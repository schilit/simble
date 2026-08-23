// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! An **Auracast broadcast sink**: the receive half of LE Audio's
//! connectionless media plane.
//!
//! A receiver has to reconstruct, purely from what is on the air, everything
//! the broadcaster published:
//!
//! 1. **Extended scanning** until an advertisement carries a Broadcast Audio
//!    Announcement (Service Data for UUID 0x1852). That gives the Broadcast_ID
//!    plus the advertiser's address and SID — the three things
//!    LE Periodic Advertising Create Sync needs.
//! 2. **LE Periodic Advertising Create Sync**, answered by LE Periodic
//!    Advertising Sync Established with a sync handle.
//! 3. **LE Periodic Advertising Report**s, whose payload is the BASE (Service
//!    Data for UUID 0x1851): presentation delay, codec configuration, BIS
//!    indices. Reports may be fragmented, so they are reassembled here.
//! 4. **LE BIGInfo Advertising Report**, which the controller raises when the
//!    periodic train's ACAD carries BIGInfo. It is the only announcement of
//!    the BIS count and the encryption flag, so a receiver waits for it.
//! 5. **LE BIG Create Sync** with the BIS indices taken from the BASE, then
//!    **LE Setup ISO Data Path** (Output) per established BIS, after which
//!    SDUs arrive as HCI ISO data.
//!
//! Like the broadcaster this is transport-free and codec-free: received SDUs
//! are handed up as opaque bytes for a caller to decode.
//!
//! This is the piece [`crate::profiles::bass`] currently fakes. Nothing here
//! is wired into the Scan Delegator yet — see that module's header comment.

use crate::device::big_broadcaster::hci_command;
use crate::device::host::iso_data_path;
use crate::gap::advertising::service_data_16;
use crate::packets::big::{
    LeBigCreateSyncHeader, LeBigInfoAdvertisingReportEvent, LeBigSyncEstablishedEventHeader,
    LeBigSyncLostEvent, LeBigTerminateSync, big_encryption, big_opcode, big_subevent_code,
};
use crate::packets::ext_adv::{
    LeExtendedAdvertisingReportEvent, LePeriodicAdvertisingCreateSync,
    LePeriodicAdvertisingReportEventHeader, LePeriodicAdvertisingSyncEstablishedEvent,
    LePeriodicAdvertisingSyncLostEvent, LeSetExtendedScanEnable, LeSetExtendedScanParametersHeader,
    ScanPhyParameters, adv_phy, ext_adv_opcode, ext_adv_subevent_code,
};
use crate::packets::{HciEvent, IsoSdu, hci_event_code, parse_iso_packet};
use crate::profiles::bap::{self, BasicAudioAnnouncement, BroadcastAudioAnnouncement};
use std::collections::VecDeque;
use zerocopy::{FromBytes, IntoBytes, byteorder::U16};

/// What the receiver is looking for and how patiently.
#[derive(Debug, Clone)]
pub struct ReceiverConfig {
    /// Only sync to this Broadcast_ID; `None` takes the first source seen.
    pub broadcast_id: Option<u32>,
    /// Identifier the host assigns to the BIG it joins.
    pub big_handle: u8,
    /// Which subgroup of the BASE to take BIS indices from.
    pub subgroup_index: usize,
    /// Periodic advertising sync timeout, in units of 10 ms.
    pub sync_timeout: u16,
    /// Periodic advertising events that may be skipped.
    pub skip: u16,
    /// BIG sync timeout, in units of 10 ms.
    pub big_sync_timeout: u16,
    /// Maximum subevents the receiver may use; 0 = no preference.
    pub mse: u8,
    /// Broadcast code, if the source's BISes are encrypted.
    pub broadcast_code: Option<[u8; 16]>,
    /// Scan interval, in units of 0.625 ms.
    pub scan_interval: u16,
    /// Scan window, in units of 0.625 ms.
    pub scan_window: u16,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            broadcast_id: None,
            big_handle: 0,
            subgroup_index: 0,
            sync_timeout: 500, // 5 s
            skip: 0,
            big_sync_timeout: 0x4000,
            mse: 0,
            broadcast_code: None,
            scan_interval: 0x0060,
            scan_window: 0x0060,
        }
    }
}

/// The source a receiver has found, as it was announced on the air.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FoundBroadcast {
    /// Advertiser address type.
    pub address_type: u8,
    /// Advertiser device address.
    pub address: [u8; 6],
    /// Advertising Set ID.
    pub advertising_sid: u8,
    /// The 24-bit Broadcast_ID from the Broadcast Audio Announcement.
    pub broadcast_id: u32,
}

/// Where the receiver's synchronization has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiverState {
    /// Nothing requested yet.
    Idle,
    /// LE Set Extended Scan Parameters sent.
    SettingScanParameters,
    /// Scanning for a Broadcast Audio Announcement.
    Scanning,
    /// LE Periodic Advertising Create Sync sent.
    SyncingToPeriodicAdvertising,
    /// Synced; waiting for the BASE and the BIGInfo to arrive on the train.
    WaitingForAnnouncement,
    /// LE BIG Create Sync sent.
    SyncingToBig,
    /// The BIG is joined; opening one ISO data path per BIS.
    OpeningDataPaths,
    /// Receiving SDUs.
    Receiving,
    /// The host left the BIG itself, and the controller confirmed it.
    Terminated,
    /// The periodic train or the BIG was lost, with the reason given.
    Lost(u8),
    /// A command failed, with the status byte given.
    Failed(u8),
}

/// The receive side of a Broadcast Isochronous Group.
#[derive(Debug)]
pub struct BigReceiver {
    config: ReceiverConfig,
    state: ReceiverState,
    found: Option<FoundBroadcast>,
    sync_handle: Option<u16>,
    /// Periodic advertising data reassembled across fragmented reports.
    periodic_data: Vec<u8>,
    base: Option<BasicAudioAnnouncement>,
    /// The BASE exactly as it arrived — the Service Data payload the parse
    /// above was run on. Kept because a consumer that shows a receiver its
    /// source's announcement wants the octets too, not only the fields: the
    /// bytes are the only way to say "what the broadcaster published is what
    /// arrived", and re-serializing `base` would compare the parser with
    /// itself.
    base_bytes: Option<Vec<u8>>,
    big_info: Option<LeBigInfoAdvertisingReportEvent>,
    bis_handles: Vec<u16>,
    data_paths_open: usize,
    received: VecDeque<IsoSdu>,
    sdu_count: u64,
}

impl BigReceiver {
    /// Creates a receiver that will look for `config`'s broadcast.
    pub fn new(config: ReceiverConfig) -> Self {
        Self {
            config,
            state: ReceiverState::Idle,
            found: None,
            sync_handle: None,
            periodic_data: Vec::new(),
            base: None,
            base_bytes: None,
            big_info: None,
            bis_handles: Vec::new(),
            data_paths_open: 0,
            received: VecDeque::new(),
            sdu_count: 0,
        }
    }

    /// Begins scanning, returning the first command to send.
    pub fn start(&mut self) -> Vec<Vec<u8>> {
        self.state = ReceiverState::SettingScanParameters;
        vec![hci_command(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS,
            &LeSetExtendedScanParametersHeader::serialize(
                0x00, // Public own address.
                0x00, // Accept all advertisements.
                adv_phy::LE_1M,
                &[ScanPhyParameters {
                    // Passive: a broadcast source is non-scannable, so a scan
                    // request would only be ignored.
                    scan_type: 0x00,
                    scan_interval: U16::new(self.config.scan_interval),
                    scan_window: U16::new(self.config.scan_window),
                }],
            ),
        )]
    }

    /// Feeds one H4 packet in — event or ISO data — and returns the commands
    /// to send back. Received SDUs are queued for [`poll_sdu`](Self::poll_sdu).
    pub fn on_packet(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        if let Some(sdu) = parse_iso_packet(packet) {
            if self.bis_handles.contains(&sdu.handle) {
                self.sdu_count += 1;
                self.received.push_back(sdu);
            }
            return Vec::new();
        }
        let Some(event) = HciEvent::parse_h4(packet) else {
            return Vec::new();
        };
        match event {
            HciEvent::CommandComplete {
                header,
                return_parameters,
            } => self.on_command_complete(header.command_opcode.get(), return_parameters),
            HciEvent::Other {
                code: hci_event_code::COMMAND_STATUS,
                parameters,
            } => self.on_command_status(parameters),
            HciEvent::Other {
                code: hci_event_code::LE_META,
                parameters,
            } => self.on_le_meta(parameters),
            _ => Vec::new(),
        }
    }

    fn on_command_complete(&mut self, opcode: u16, return_parameters: &[u8]) -> Vec<Vec<u8>> {
        let Some(&status) = return_parameters.first() else {
            return Vec::new();
        };
        match self.state {
            ReceiverState::SettingScanParameters
                if opcode == ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get() =>
            {
                if status != 0x00 {
                    self.state = ReceiverState::Failed(status);
                    return Vec::new();
                }
                self.state = ReceiverState::Scanning;
                let enable = LeSetExtendedScanEnable {
                    enable: 0x01,
                    filter_duplicates: 0x00,
                    duration: U16::new(0),
                    period: U16::new(0),
                };
                vec![hci_command(
                    ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE,
                    enable.as_bytes(),
                )]
            }
            ReceiverState::OpeningDataPaths
                if opcode
                    == u16::from_le_bytes(crate::device::host::opcode::LE_SETUP_ISO_DATA_PATH) =>
            {
                if status != 0x00 {
                    self.state = ReceiverState::Failed(status);
                    return Vec::new();
                }
                self.data_paths_open += 1;
                if self.data_paths_open >= self.bis_handles.len() {
                    self.state = ReceiverState::Receiving;
                    Vec::new()
                } else {
                    vec![self.setup_data_path(self.data_paths_open)]
                }
            }
            // Leaving a BIG is confirmed by Command Complete and by nothing
            // else — no BIG Sync Lost follows a *local* termination. Without
            // this the receiver stayed in `Receiving` forever after
            // [`terminate`](Self::terminate), holding BIS handles the
            // controller had already dropped: a caller that stops a stream and
            // then asks what state it is in was told it is still streaming.
            _ if opcode == big_opcode::LE_BIG_TERMINATE_SYNC.get() => {
                if status == 0x00 {
                    self.bis_handles.clear();
                    self.received.clear();
                    self.state = ReceiverState::Terminated;
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Both LE Periodic Advertising Create Sync and LE BIG Create Sync answer
    /// Command Status; a non-zero one means the matching Established event
    /// will never arrive.
    fn on_command_status(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        if parameters.len() < 4 || parameters[0] == 0x00 {
            return Vec::new();
        }
        let opcode = u16::from_le_bytes([parameters[2], parameters[3]]);
        let awaited = match self.state {
            ReceiverState::SyncingToPeriodicAdvertising => {
                ext_adv_opcode::LE_PERIODIC_ADVERTISING_CREATE_SYNC.get()
            }
            ReceiverState::SyncingToBig => big_opcode::LE_BIG_CREATE_SYNC.get(),
            _ => return Vec::new(),
        };
        if opcode == awaited {
            self.state = ReceiverState::Failed(parameters[0]);
        }
        Vec::new()
    }

    fn on_le_meta(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        let Some((&subevent, rest)) = parameters.split_first() else {
            return Vec::new();
        };
        match subevent {
            ext_adv_subevent_code::LE_EXTENDED_ADVERTISING_REPORT => {
                self.on_advertising_report(rest)
            }
            ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_SYNC_ESTABLISHED => {
                self.on_sync_established(rest)
            }
            ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_REPORT => self.on_periodic_report(rest),
            ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_SYNC_LOST => {
                if let Ok(event) = LePeriodicAdvertisingSyncLostEvent::ref_from_bytes(rest)
                    && Some(event.sync_handle.get()) == self.sync_handle
                {
                    self.sync_handle = None;
                    self.state = ReceiverState::Lost(0x3E);
                }
                Vec::new()
            }
            big_subevent_code::LE_BIGINFO_ADVERTISING_REPORT => self.on_big_info(rest),
            big_subevent_code::LE_BIG_SYNC_ESTABLISHED => self.on_big_sync_established(rest),
            big_subevent_code::LE_BIG_SYNC_LOST => {
                if let Ok(event) = LeBigSyncLostEvent::ref_from_bytes(rest)
                    && event.big_handle == self.config.big_handle
                {
                    self.bis_handles.clear();
                    self.state = ReceiverState::Lost(event.reason);
                }
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Looks for a Broadcast Audio Announcement in each report, and syncs to
    /// the periodic train of the first one that matches the configured
    /// Broadcast_ID.
    fn on_advertising_report(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        if self.state != ReceiverState::Scanning {
            return Vec::new();
        }
        let Some(reports) = LeExtendedAdvertisingReportEvent::parse(parameters) else {
            return Vec::new();
        };
        for (header, data) in reports {
            let Some(payload) =
                service_data_16(data, bap::bap_uuid::BROADCAST_AUDIO_ANNOUNCEMENT_UUID16)
            else {
                continue;
            };
            let Some(announcement) = BroadcastAudioAnnouncement::parse(payload) else {
                continue;
            };
            if let Some(wanted) = self.config.broadcast_id
                && wanted != announcement.broadcast_id
            {
                continue;
            }
            // A source with no ADI cannot be synced to: 0xFF means the
            // advertisement carried no SID, so there is no periodic train to
            // name in Create Sync.
            if header.advertising_sid == 0xFF {
                continue;
            }
            let found = FoundBroadcast {
                address_type: header.address_type,
                address: header.address,
                advertising_sid: header.advertising_sid,
                broadcast_id: announcement.broadcast_id,
            };
            self.found = Some(found.clone());
            self.state = ReceiverState::SyncingToPeriodicAdvertising;
            let create_sync = LePeriodicAdvertisingCreateSync {
                options: 0x00, // Use the address/SID given here; reporting on.
                advertising_sid: found.advertising_sid,
                advertiser_address_type: found.address_type,
                advertiser_address: found.address,
                skip: U16::new(self.config.skip),
                sync_timeout: U16::new(self.config.sync_timeout),
                sync_cte_type: 0x00,
            };
            return vec![hci_command(
                ext_adv_opcode::LE_PERIODIC_ADVERTISING_CREATE_SYNC,
                create_sync.as_bytes(),
            )];
        }
        Vec::new()
    }

    fn on_sync_established(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        if self.state != ReceiverState::SyncingToPeriodicAdvertising {
            return Vec::new();
        }
        let Ok(event) = LePeriodicAdvertisingSyncEstablishedEvent::ref_from_prefix(parameters)
            .map(|(event, _)| event)
        else {
            return Vec::new();
        };
        if event.status != 0x00 {
            self.state = ReceiverState::Failed(event.status);
            return Vec::new();
        }
        self.sync_handle = Some(event.sync_handle.get());
        self.periodic_data.clear();
        self.state = ReceiverState::WaitingForAnnouncement;
        Vec::new()
    }

    /// Reassembles periodic advertising data and pulls the BASE out of it.
    fn on_periodic_report(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        let Some((header, data)) = LePeriodicAdvertisingReportEventHeader::parse(parameters) else {
            return Vec::new();
        };
        if Some(header.sync_handle.get()) != self.sync_handle {
            return Vec::new();
        }
        match header.data_status {
            // Complete: this report finishes whatever came before it.
            0x00 => self.periodic_data.extend_from_slice(data),
            // More to come: keep accumulating and wait.
            0x01 => {
                self.periodic_data.extend_from_slice(data);
                return Vec::new();
            }
            // Truncated or anything else: the payload is unusable, so start
            // over rather than parsing half a BASE.
            _ => {
                self.periodic_data.clear();
                return Vec::new();
            }
        }
        let assembled = std::mem::take(&mut self.periodic_data);
        if let Some(payload) =
            service_data_16(&assembled, bap::bap_uuid::BASIC_AUDIO_ANNOUNCEMENT_UUID16)
            && let Some(base) = BasicAudioAnnouncement::parse(payload)
        {
            self.base = Some(base);
            self.base_bytes = Some(payload.to_vec());
        }
        self.big_create_sync_if_ready()
    }

    fn on_big_info(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        let Ok(report) =
            LeBigInfoAdvertisingReportEvent::ref_from_prefix(parameters).map(|(report, _)| *report)
        else {
            return Vec::new();
        };
        if Some(report.sync_handle.get()) != self.sync_handle {
            return Vec::new();
        }
        self.big_info = Some(report);
        self.big_create_sync_if_ready()
    }

    /// Issues LE BIG Create Sync once both halves of the announcement are in:
    /// the BASE names the BIS indices, the BIGInfo says whether the streams
    /// are encrypted. Asking early gets Command Disallowed.
    fn big_create_sync_if_ready(&mut self) -> Vec<Vec<u8>> {
        if self.state != ReceiverState::WaitingForAnnouncement {
            return Vec::new();
        }
        let (Some(base), Some(big_info), Some(sync_handle)) =
            (self.base.as_ref(), self.big_info.as_ref(), self.sync_handle)
        else {
            return Vec::new();
        };
        let Some(subgroup) = base.subgroups.get(self.config.subgroup_index) else {
            return Vec::new();
        };
        let indices: Vec<u8> = subgroup.bis.iter().map(|bis| bis.index).collect();
        if indices.is_empty() {
            return Vec::new();
        }
        if big_info.encryption == big_encryption::ENCRYPTED && self.config.broadcast_code.is_none()
        {
            // 0x1D = Insufficient Security: the source is encrypted and no
            // broadcast code was configured, so syncing could only produce
            // undecodable audio.
            self.state = ReceiverState::Failed(0x1D);
            return Vec::new();
        }
        self.state = ReceiverState::SyncingToBig;
        vec![hci_command(
            big_opcode::LE_BIG_CREATE_SYNC,
            &LeBigCreateSyncHeader::serialize(
                self.config.big_handle,
                sync_handle,
                big_info.encryption,
                self.config.broadcast_code.unwrap_or([0; 16]),
                self.config.mse,
                self.config.big_sync_timeout,
                &indices,
            ),
        )]
    }

    fn on_big_sync_established(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        if self.state != ReceiverState::SyncingToBig {
            return Vec::new();
        }
        let Some((header, handles)) = LeBigSyncEstablishedEventHeader::parse(parameters) else {
            self.state = ReceiverState::Failed(0xFF);
            return Vec::new();
        };
        if header.status != 0x00 {
            self.state = ReceiverState::Failed(header.status);
            return Vec::new();
        }
        if handles.is_empty() {
            self.state = ReceiverState::Failed(0xFF);
            return Vec::new();
        }
        self.bis_handles = handles;
        self.data_paths_open = 0;
        self.state = ReceiverState::OpeningDataPaths;
        vec![self.setup_data_path(0)]
    }

    /// LE Setup ISO Data Path for BIS `slot` in the Output direction.
    fn setup_data_path(&self, slot: usize) -> Vec<u8> {
        let handle = self.bis_handles.get(slot).copied().unwrap_or(0);
        let mut params = Vec::with_capacity(13);
        params.extend_from_slice(&handle.to_le_bytes());
        params.push(iso_data_path::OUTPUT);
        params.push(iso_data_path::HCI);
        params.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x00]); // transparent
        params.extend_from_slice(&[0x00, 0x00, 0x00]); // controller delay
        params.push(0x00); // codec configuration length
        crate::device::host::command(crate::device::host::opcode::LE_SETUP_ISO_DATA_PATH, &params)
    }

    /// Stops scanning. Worth issuing once the BIG is joined: rootcanal keeps
    /// delivering every advertisement in the simulation otherwise.
    pub fn stop_scanning(&self) -> Vec<u8> {
        let disable = LeSetExtendedScanEnable {
            enable: 0x00,
            filter_duplicates: 0x00,
            duration: U16::new(0),
            period: U16::new(0),
        };
        hci_command(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE,
            disable.as_bytes(),
        )
    }

    /// Leaves the BIG.
    pub fn terminate(&self) -> Vec<u8> {
        let params = LeBigTerminateSync {
            big_handle: self.config.big_handle,
        };
        hci_command(big_opcode::LE_BIG_TERMINATE_SYNC, params.as_bytes())
    }

    /// The oldest SDU not yet taken.
    pub fn poll_sdu(&mut self) -> Option<IsoSdu> {
        self.received.pop_front()
    }

    /// How many SDUs have arrived on the BIG's streams.
    pub fn sdu_count(&self) -> u64 {
        self.sdu_count
    }

    /// Where synchronization has got to.
    pub fn state(&self) -> ReceiverState {
        self.state
    }

    /// The source that was found, if scanning has matched one.
    pub fn found(&self) -> Option<&FoundBroadcast> {
        self.found.as_ref()
    }

    /// The BASE read off the periodic train, if one has arrived.
    pub fn base(&self) -> Option<&BasicAudioAnnouncement> {
        self.base.as_ref()
    }

    /// The BASE's octets as they arrived on the periodic train — the Service
    /// Data payload [`base`](Self::base) was parsed from.
    pub fn base_bytes(&self) -> Option<&[u8]> {
        self.base_bytes.as_deref()
    }

    /// The BIGInfo read off the periodic train, if one has arrived.
    pub fn big_info(&self) -> Option<&LeBigInfoAdvertisingReportEvent> {
        self.big_info.as_ref()
    }

    /// The periodic advertising sync handle, once established.
    pub fn sync_handle(&self) -> Option<u16> {
        self.sync_handle
    }

    /// The synchronized BIS connection handles, in BIS index order.
    pub fn bis_handles(&self) -> &[u16] {
        &self.bis_handles
    }

    /// True once SDUs are arriving.
    pub fn is_receiving(&self) -> bool {
        self.state == ReceiverState::Receiving
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::big_broadcaster::BroadcastConfig;
    use crate::packets::big::LeBigInfoAdvertisingReportEvent as BigInfo;
    use crate::packets::build_iso_packet;
    use crate::packets::ext_adv::ExtendedAdvertisingReportHeader;

    fn command_complete(opcode: u16, params: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x04, 0x0E, (3 + params.len()) as u8, 0x01];
        packet.extend_from_slice(&opcode.to_le_bytes());
        packet.extend_from_slice(params);
        packet
    }

    fn le_meta(subevent: u8, params: &[u8]) -> Vec<u8> {
        let mut packet = vec![0x04, 0x3E, (1 + params.len()) as u8, subevent];
        packet.extend_from_slice(params);
        packet
    }

    /// An extended advertising report carrying `data`, from a source with SID
    /// 3 at a fixed address.
    fn advertising_report(data: &[u8]) -> Vec<u8> {
        let header = ExtendedAdvertisingReportHeader {
            event_type: U16::new(0),
            address_type: 0x00,
            address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            primary_phy: adv_phy::LE_1M,
            secondary_phy: adv_phy::LE_2M,
            advertising_sid: 3,
            tx_power: 0x7F,
            rssi: -40,
            periodic_advertising_interval: U16::new(80),
            direct_address_type: 0x00,
            direct_address: [0; 6],
            data_length: data.len() as u8,
        };
        le_meta(
            ext_adv_subevent_code::LE_EXTENDED_ADVERTISING_REPORT,
            &LeExtendedAdvertisingReportEvent::serialize(&[(header, data)]),
        )
    }

    fn sync_established(sync_handle: u16) -> Vec<u8> {
        let event = LePeriodicAdvertisingSyncEstablishedEvent {
            status: 0x00,
            sync_handle: U16::new(sync_handle),
            advertising_sid: 3,
            advertiser_address_type: 0x00,
            advertiser_address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
            advertiser_phy: adv_phy::LE_2M,
            periodic_advertising_interval: U16::new(80),
            advertiser_clock_accuracy: 0x00,
        };
        le_meta(
            ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_SYNC_ESTABLISHED,
            event.as_bytes(),
        )
    }

    fn periodic_report(sync_handle: u16, data_status: u8, data: &[u8]) -> Vec<u8> {
        le_meta(
            ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_REPORT,
            &LePeriodicAdvertisingReportEventHeader::serialize(
                sync_handle,
                0x7F,
                -40,
                0xFF,
                data_status,
                data,
            ),
        )
    }

    fn big_info(sync_handle: u16, num_bis: u8, encryption: u8) -> Vec<u8> {
        let report = BigInfo {
            sync_handle: U16::new(sync_handle),
            num_bis,
            nse: 3,
            iso_interval: U16::new(8),
            bn: 1,
            pto: 0,
            irc: 2,
            max_pdu: U16::new(100),
            sdu_interval: crate::packets::ext_adv::U24::new(10_000),
            max_sdu: U16::new(100),
            phy: adv_phy::LE_2M,
            framing: 0,
            encryption,
        };
        le_meta(
            big_subevent_code::LE_BIGINFO_ADVERTISING_REPORT,
            report.as_bytes(),
        )
    }

    fn big_sync_established(status: u8, handles: &[u16]) -> Vec<u8> {
        le_meta(
            big_subevent_code::LE_BIG_SYNC_ESTABLISHED,
            &LeBigSyncEstablishedEventHeader::serialize(
                status, 0, 0x0124F8, 3, 1, 0, 2, 100, 8, handles,
            ),
        )
    }

    /// Drives a receiver from `start()` all the way to Receiving, against the
    /// advertising and periodic payloads the *broadcaster* in this crate
    /// produces. This is a self-test — see `tests/interop/` for the checks
    /// that involve a foreign stack.
    fn run_to_receiving(config: ReceiverConfig) -> BigReceiver {
        let source = BroadcastConfig {
            broadcast_id: 0x00AB_CDEF,
            ..Default::default()
        };
        let mut r = BigReceiver::new(config);
        let next = r.start();
        assert_eq!(
            u16::from_le_bytes([next[0][1], next[0][2]]),
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get()
        );
        let next = r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
            &[0x00],
        ));
        assert_eq!(
            u16::from_le_bytes([next[0][1], next[0][2]]),
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get()
        );
        assert!(
            r.on_packet(&command_complete(
                ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
                &[0x00]
            ))
            .is_empty()
        );
        assert_eq!(r.state(), ReceiverState::Scanning);

        let next = r.on_packet(&advertising_report(&source.advertising_data()));
        assert_eq!(
            u16::from_le_bytes([next[0][1], next[0][2]]),
            ext_adv_opcode::LE_PERIODIC_ADVERTISING_CREATE_SYNC.get()
        );
        assert_eq!(r.found().unwrap().broadcast_id, 0x00AB_CDEF);
        assert_eq!(r.found().unwrap().advertising_sid, 3);

        assert!(r.on_packet(&sync_established(0x0009)).is_empty());
        assert_eq!(r.state(), ReceiverState::WaitingForAnnouncement);

        // The BASE alone is not enough: without BIGInfo the receiver does not
        // know whether the streams are encrypted.
        assert!(
            r.on_packet(&periodic_report(
                0x0009,
                0x00,
                &source.periodic_advertising_data()
            ))
            .is_empty()
        );
        assert_eq!(r.state(), ReceiverState::WaitingForAnnouncement);
        assert_eq!(r.base().unwrap().subgroups[0].bis.len(), 2);

        let next = r.on_packet(&big_info(0x0009, 2, big_encryption::UNENCRYPTED));
        assert_eq!(
            u16::from_le_bytes([next[0][1], next[0][2]]),
            big_opcode::LE_BIG_CREATE_SYNC.get()
        );
        // Header plus one octet per BIS index, taken from the BASE.
        assert_eq!(next[0][3], 26, "rootcanal dies on a wrong-length block");
        assert_eq!(&next[0][next[0].len() - 2..], &[1, 2], "BIS indices");

        let mut next = r.on_packet(&big_sync_established(0x00, &[0x0E10, 0x0E11]));
        for _ in 0..2 {
            assert_eq!(
                &next[0][1..3],
                &crate::device::host::opcode::LE_SETUP_ISO_DATA_PATH
            );
            assert_eq!(next[0][6], iso_data_path::OUTPUT, "a sink opens Output");
            next = r.on_packet(&command_complete(
                u16::from_le_bytes(crate::device::host::opcode::LE_SETUP_ISO_DATA_PATH),
                &[0x00],
            ));
        }
        assert!(next.is_empty());
        assert_eq!(r.state(), ReceiverState::Receiving);
        r
    }

    #[test]
    fn test_the_full_synchronization_sequence() {
        let r = run_to_receiving(ReceiverConfig::default());
        assert_eq!(r.bis_handles(), &[0x0E10, 0x0E11]);
        assert_eq!(r.sync_handle(), Some(0x0009));
    }

    #[test]
    fn test_sdus_on_the_synced_handles_are_collected() {
        let mut r = run_to_receiving(ReceiverConfig::default());
        r.on_packet(&build_iso_packet(0x0E10, 0, &[0xAA; 100]));
        r.on_packet(&build_iso_packet(0x0E11, 0, &[0xBB; 100]));
        // A handle that is not one of ours belongs to some other stream.
        r.on_packet(&build_iso_packet(0x0E99, 0, &[0xCC; 100]));
        assert_eq!(r.sdu_count(), 2);
        assert_eq!(r.poll_sdu().unwrap().payload, vec![0xAA; 100]);
        assert_eq!(r.poll_sdu().unwrap().handle, 0x0E11);
        assert!(r.poll_sdu().is_none());
    }

    /// The octets the BASE arrived as, kept beside the parsed form: a consumer
    /// showing a receiver's view of its source compares those bytes with what
    /// the broadcaster published, which re-serializing the parse cannot do.
    #[test]
    fn test_the_base_octets_are_kept_as_they_arrived() {
        let r = run_to_receiving(ReceiverConfig::default());
        let source = BroadcastConfig {
            broadcast_id: 0x00AB_CDEF,
            ..Default::default()
        };
        // The Service Data payload, i.e. the periodic advertising data with its
        // AD length, AD type and UUID stripped.
        assert_eq!(r.base_bytes(), Some(&source.periodic_advertising_data()[4..]));
    }

    /// Leaving a BIG is answered by Command Complete and nothing else — no
    /// BIG Sync Lost follows a local termination, so if this were not handled
    /// the receiver would report `Receiving` forever after it had stopped.
    #[test]
    fn test_leaving_the_big_is_reflected_in_the_state() {
        let mut r = run_to_receiving(ReceiverConfig::default());
        let terminate = r.terminate();
        assert_eq!(
            u16::from_le_bytes([terminate[1], terminate[2]]),
            big_opcode::LE_BIG_TERMINATE_SYNC.get()
        );
        assert!(
            r.on_packet(&command_complete(
                big_opcode::LE_BIG_TERMINATE_SYNC.get(),
                &[0x00, 0x00]
            ))
            .is_empty()
        );
        assert_eq!(r.state(), ReceiverState::Terminated);
        assert!(!r.is_receiving());
        assert!(r.bis_handles().is_empty());
        // And nothing that arrives afterwards is counted as audio.
        r.on_packet(&build_iso_packet(0x0E10, 1, &[0xAA; 100]));
        assert_eq!(r.sdu_count(), 0);
    }

    #[test]
    fn test_a_broadcast_id_filter_skips_other_sources() {
        let source = BroadcastConfig {
            broadcast_id: 0x00AB_CDEF,
            ..Default::default()
        };
        let mut r = BigReceiver::new(ReceiverConfig {
            broadcast_id: Some(0x00_1234),
            ..Default::default()
        });
        r.start();
        r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
            &[0x00],
        ));
        r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
            &[0x00],
        ));
        assert!(
            r.on_packet(&advertising_report(&source.advertising_data()))
                .is_empty(),
            "a different Broadcast_ID must not be synced to"
        );
        assert_eq!(r.state(), ReceiverState::Scanning);
        assert!(r.found().is_none());
    }

    #[test]
    fn test_fragmented_periodic_reports_are_reassembled() {
        let source = BroadcastConfig::default();
        let data = source.periodic_advertising_data();
        let (first, second) = data.split_at(20);
        let mut r = BigReceiver::new(ReceiverConfig::default());
        r.start();
        r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
            &[0x00],
        ));
        r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
            &[0x00],
        ));
        r.on_packet(&advertising_report(&source.advertising_data()));
        r.on_packet(&sync_established(0x0009));

        // Half a BASE must not parse into a half-truth.
        assert!(
            r.on_packet(&periodic_report(0x0009, 0x01, first))
                .is_empty()
        );
        assert!(r.base().is_none());
        r.on_packet(&periodic_report(0x0009, 0x00, second));
        assert_eq!(r.base().unwrap().presentation_delay, 40_000);
    }

    #[test]
    fn test_a_truncated_periodic_report_is_discarded() {
        let source = BroadcastConfig::default();
        let mut r = BigReceiver::new(ReceiverConfig::default());
        r.start();
        r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
            &[0x00],
        ));
        r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
            &[0x00],
        ));
        r.on_packet(&advertising_report(&source.advertising_data()));
        r.on_packet(&sync_established(0x0009));
        r.on_packet(&periodic_report(
            0x0009,
            0x02,
            &source.periodic_advertising_data()[..10],
        ));
        assert!(r.base().is_none());
        assert_eq!(r.state(), ReceiverState::WaitingForAnnouncement);
    }

    #[test]
    fn test_an_encrypted_source_without_a_code_is_refused() {
        let source = BroadcastConfig::default();
        let mut r = BigReceiver::new(ReceiverConfig::default());
        r.start();
        r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS.get(),
            &[0x00],
        ));
        r.on_packet(&command_complete(
            ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE.get(),
            &[0x00],
        ));
        r.on_packet(&advertising_report(&source.advertising_data()));
        r.on_packet(&sync_established(0x0009));
        r.on_packet(&periodic_report(
            0x0009,
            0x00,
            &source.periodic_advertising_data(),
        ));
        assert!(
            r.on_packet(&big_info(0x0009, 2, big_encryption::ENCRYPTED))
                .is_empty()
        );
        assert_eq!(r.state(), ReceiverState::Failed(0x1D));
    }

    #[test]
    fn test_a_failed_big_sync_is_recorded() {
        let mut r = BigReceiver::new(ReceiverConfig::default());
        r.state = ReceiverState::SyncingToBig;
        // 0x3E = Connection Failed to be Established.
        assert!(r.on_packet(&big_sync_established(0x3E, &[])).is_empty());
        assert_eq!(r.state(), ReceiverState::Failed(0x3E));
    }

    #[test]
    fn test_losing_the_big_stops_the_stream() {
        let mut r = run_to_receiving(ReceiverConfig::default());
        let lost = le_meta(
            big_subevent_code::LE_BIG_SYNC_LOST,
            LeBigSyncLostEvent {
                big_handle: 0,
                reason: 0x3E,
            }
            .as_bytes(),
        );
        r.on_packet(&lost);
        assert_eq!(r.state(), ReceiverState::Lost(0x3E));
        r.on_packet(&build_iso_packet(0x0E10, 1, &[0xAA; 100]));
        assert_eq!(r.sdu_count(), 0, "a lost BIG carries nothing");
    }
}
