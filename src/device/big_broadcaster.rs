// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! An **Auracast broadcast source**: the transmit half of LE Audio's
//! connectionless media plane.
//!
//! Where [`CisCentral`](super::CisCentral) opens one stream to one peer over
//! an ACL link, a broadcaster talks to nobody in particular. Everything a
//! receiver needs to find and join the stream has to be *published*, which is
//! why the setup sequence is mostly advertising:
//!
//! 1. **LE Set Extended Advertising Parameters / Data** — a non-connectable,
//!    non-scannable extended advertising set carrying the Broadcast Audio
//!    Announcement (Service Data for UUID 0x1852, holding the 24-bit
//!    Broadcast_ID) and a Broadcast Name. This is what a scanner sees first.
//! 2. **LE Set Periodic Advertising Parameters / Data** — the periodic train
//!    carrying the BASE (Service Data for UUID 0x1851): presentation delay,
//!    codec configuration, and the BIS indices. A receiver cannot decode
//!    anything without it.
//! 3. **LE Set Extended Advertising Enable**, then **LE Set Periodic
//!    Advertising Enable** — in that order; the periodic train rides on the
//!    extended set's secondary channel.
//! 4. **LE Create BIG** on that advertising handle. The controller starts
//!    putting BIGInfo into the periodic train's ACAD, and reports the BIS
//!    connection handles in LE Create BIG Complete — the only way to learn
//!    them.
//! 5. **LE Setup ISO Data Path** (Input) once per BIS, after which SDUs may be
//!    written.
//!
//! Like `CisCentral` this is transport-free — HCI packets in, HCI packets out —
//! and carries no codec: SDUs are opaque bytes, so the same broadcaster
//! carries LC3 from `crate::audio::lc3` or anything else a caller encodes.
//!
//! Encryption is wired through (`broadcast_code`) but untested against a
//! foreign peer; see the crate's interop notes.

use crate::device::host::{command, iso_data_path};
use crate::gap::advertising::ad_type;
use crate::packets::big::{
    LeCreateBig, LeCreateBigCompleteEventHeader, LeTerminateBig, big_encryption, big_framing,
    big_opcode, big_packing, big_subevent_code,
};
use crate::packets::ext_adv::{
    AdvertisingEnableEntry, LeSetExtendedAdvertisingDataHeader,
    LeSetExtendedAdvertisingEnableHeader, LeSetExtendedAdvertisingParameters,
    LeSetPeriodicAdvertisingDataHeader, LeSetPeriodicAdvertisingEnable,
    LeSetPeriodicAdvertisingParameters, U24, adv_phy, data_operation, ext_adv_opcode,
};
use crate::packets::hci::OpCode;
use crate::packets::{HciEvent, build_iso_packet, hci_event_code};
use crate::profiles::bap::{
    self, Bis, BroadcastAudioAnnouncement, CodecSpecificConfiguration, FrameDuration,
    SamplingFrequency, Subgroup,
};
use zerocopy::{IntoBytes, byteorder::U16};

/// Builds one H4 HCI command packet from a typed [`OpCode`].
pub(crate) fn hci_command(op_code: OpCode, params: &[u8]) -> Vec<u8> {
    command(op_code.get().to_le_bytes(), params)
}

/// Everything that has to agree between the BASE a receiver reads and the BIG
/// its controller joins. The defaults are the configuration Bumble's
/// `auracast transmit` uses — 48 kHz, 10 ms frames, 100 octets, stereo across
/// two BISes — so a Bumble receiver needs no arguments to decode this source.
#[derive(Debug, Clone)]
pub struct BroadcastConfig {
    /// Advertising set the periodic train and the BIG hang off.
    pub advertising_handle: u8,
    /// Advertising Set ID published in the extended advertisement.
    pub advertising_sid: u8,
    /// Identifier the host assigns to the BIG.
    pub big_handle: u8,
    /// 24-bit Broadcast_ID, the value a receiver filters on.
    pub broadcast_id: u32,
    /// Broadcast Name (AD type 0x30), shown in a scanner's UI.
    pub broadcast_name: String,
    /// Number of BISes; one per audio channel.
    pub num_bis: u8,
    /// Time between SDUs on each BIS, in microseconds.
    pub sdu_interval_us: u32,
    /// Largest SDU written to a BIS, in octets. Must equal the codec's
    /// octets-per-frame for a single-frame-per-SDU configuration.
    pub max_sdu: u16,
    /// Transport latency budget, in milliseconds.
    pub max_transport_latency_ms: u16,
    /// Retransmission number.
    pub rtn: u8,
    /// PHY bitfield: 0x01 = 1M, 0x02 = 2M, 0x04 = Coded.
    pub phy: u8,
    /// Presentation delay published in the BASE, in microseconds.
    pub presentation_delay_us: u32,
    /// Codec sampling frequency published in the BASE.
    pub sampling_frequency: SamplingFrequency,
    /// Codec frame duration published in the BASE.
    pub frame_duration: FrameDuration,
    /// Octets per codec frame published in the BASE.
    pub octets_per_codec_frame: u16,
    /// Subgroup metadata (an LTV blob) published in the BASE.
    pub metadata: Vec<u8>,
    /// Broadcast code, if the BISes are to be encrypted.
    pub broadcast_code: Option<[u8; 16]>,
    /// Primary advertising interval, in units of 0.625 ms.
    pub primary_advertising_interval: u32,
    /// Periodic advertising interval, in units of 1.25 ms.
    pub periodic_advertising_interval: u16,
}

impl Default for BroadcastConfig {
    fn default() -> Self {
        Self {
            advertising_handle: 0,
            advertising_sid: 0,
            big_handle: 0,
            broadcast_id: 0x0000_0001,
            broadcast_name: "Simble Auracast".to_string(),
            num_bis: 2,
            sdu_interval_us: 10_000,
            max_sdu: 100,
            max_transport_latency_ms: 65,
            rtn: 4,
            phy: adv_phy::LE_2M,
            presentation_delay_us: 40_000,
            sampling_frequency: SamplingFrequency::Freq48000,
            frame_duration: FrameDuration::Duration10000Us,
            octets_per_codec_frame: 100,
            // Streaming_Audio_Contexts = Media, the context a general-purpose
            // Auracast source declares.
            metadata: vec![0x03, 0x02, 0x04, 0x00],
            broadcast_code: None,
            primary_advertising_interval: 160, // 100 ms
            periodic_advertising_interval: 80, // 100 ms
        }
    }
}

impl BroadcastConfig {
    /// The Basic Audio Announcement (BASE) this configuration publishes: one
    /// subgroup, one BIS per audio channel, each BIS carrying only its own
    /// channel allocation as BAP requires.
    pub fn base(&self) -> bap::BasicAudioAnnouncement {
        let bis = (1..=self.num_bis)
            .map(|index| Bis {
                index,
                codec_specific_configuration: CodecSpecificConfiguration {
                    audio_channel_allocation: Some(channel_allocation(index, self.num_bis)),
                    ..Default::default()
                },
            })
            .collect();
        bap::BasicAudioAnnouncement {
            presentation_delay: self.presentation_delay_us,
            subgroups: vec![Subgroup {
                codec_id: bap::LC3_CODEC_ID,
                codec_specific_configuration: CodecSpecificConfiguration {
                    sampling_frequency: Some(self.sampling_frequency),
                    frame_duration: Some(self.frame_duration),
                    octets_per_codec_frame: Some(self.octets_per_codec_frame),
                    ..Default::default()
                },
                metadata: self.metadata.clone(),
                bis,
            }],
        }
    }

    /// The extended advertising payload: Broadcast Audio Announcement (with
    /// the Broadcast_ID a receiver filters on) plus the Broadcast Name.
    pub fn advertising_data(&self) -> Vec<u8> {
        let announcement = BroadcastAudioAnnouncement {
            broadcast_id: self.broadcast_id,
        }
        .to_bytes();
        let mut data = service_data_16(
            bap::bap_uuid::BROADCAST_AUDIO_ANNOUNCEMENT_UUID16,
            &announcement,
        );
        let name = self.broadcast_name.as_bytes();
        data.push(name.len() as u8 + 1);
        data.push(ad_type::BROADCAST_NAME);
        data.extend_from_slice(name);
        data
    }

    /// The periodic advertising payload: the BASE, as Service Data.
    pub fn periodic_advertising_data(&self) -> Vec<u8> {
        service_data_16(
            bap::bap_uuid::BASIC_AUDIO_ANNOUNCEMENT_UUID16,
            &self.base().to_bytes(),
        )
    }
}

/// The channel a BIS carries: stereo splits front left / front right, and any
/// wider configuration falls back to numbering the standard locations.
fn channel_allocation(index: u8, num_bis: u8) -> u32 {
    if num_bis == 1 {
        return bap::audio_location::FRONT_CENTER;
    }
    match index {
        1 => bap::audio_location::FRONT_LEFT,
        2 => bap::audio_location::FRONT_RIGHT,
        n => 1u32 << (u32::from(n) + 1),
    }
}

/// Wraps `data` in a 16-bit Service Data AD structure.
fn service_data_16(uuid: u16, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    out.push((data.len() + 3) as u8);
    out.push(ad_type::SERVICE_DATA_16BIT);
    out.extend_from_slice(&uuid.to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// Where the broadcaster's setup has got to. The advertising steps are
/// enumerated rather than counted so a failure names the command that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BroadcastState {
    /// Nothing requested yet.
    Idle,
    /// LE Set Extended Advertising Parameters sent.
    SettingAdvertisingParameters,
    /// LE Set Extended Advertising Data sent.
    SettingAdvertisingData,
    /// LE Set Periodic Advertising Parameters sent.
    SettingPeriodicParameters,
    /// LE Set Periodic Advertising Data (the BASE) sent.
    SettingPeriodicData,
    /// LE Set Extended Advertising Enable sent.
    EnablingAdvertising,
    /// LE Set Periodic Advertising Enable sent.
    EnablingPeriodicAdvertising,
    /// LE Create BIG sent; waiting for LE Create BIG Complete.
    CreatingBig,
    /// The BIG exists; opening one ISO data path per BIS.
    OpeningDataPaths,
    /// Ready — SDUs written now go out over the air.
    Streaming,
    /// The BIG was torn down.
    Terminated,
    /// A command or the BIG creation failed, with the status byte given.
    Failed(u8),
}

/// The transmit side of a Broadcast Isochronous Group.
#[derive(Debug)]
pub struct BigBroadcaster {
    config: BroadcastConfig,
    state: BroadcastState,
    /// BIS connection handles, in BIS index order (index 1 is `[0]`).
    bis_handles: Vec<u16>,
    /// How many ISO data paths have been opened so far.
    data_paths_open: usize,
    /// One packet sequence number per BIS.
    tx_sequence: Vec<u16>,
    /// Whether an [`Self::update_metadata`] re-publish is awaiting its
    /// Command Complete. Only ever set while [`BroadcastState::Streaming`],
    /// which is why it cannot be confused with the setup sequence's own
    /// LE Set Periodic Advertising Data.
    update_in_flight: bool,
    /// The status of the last completed metadata update, until taken.
    last_update_status: Option<u8>,
}

impl BigBroadcaster {
    /// Creates a broadcaster that will publish `config` when started.
    pub fn new(config: BroadcastConfig) -> Self {
        Self {
            config,
            state: BroadcastState::Idle,
            bis_handles: Vec::new(),
            data_paths_open: 0,
            tx_sequence: Vec::new(),
            update_in_flight: false,
            last_update_status: None,
        }
    }

    /// The configuration being broadcast.
    pub fn config(&self) -> &BroadcastConfig {
        &self.config
    }

    /// Begins the setup sequence, returning the first command to send.
    pub fn start(&mut self) -> Vec<Vec<u8>> {
        self.state = BroadcastState::SettingAdvertisingParameters;
        vec![self.extended_advertising_parameters()]
    }

    /// Feeds one HCI packet in, and returns the commands to send back.
    pub fn on_packet(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
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

    /// Advances the setup sequence on each command's Command Complete.
    fn on_command_complete(&mut self, opcode: u16, return_parameters: &[u8]) -> Vec<Vec<u8>> {
        // Every command in this sequence returns status as its first octet.
        let Some(&status) = return_parameters.first() else {
            return Vec::new();
        };
        // A metadata re-publish is answered by the same opcode as the setup
        // sequence's periodic-data write, so it is matched first — it can only
        // be outstanding while streaming, where the setup match below is inert.
        if self.update_in_flight && opcode == ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA.get()
        {
            self.update_in_flight = false;
            self.last_update_status = Some(status);
            return Vec::new();
        }
        let expected = match self.state {
            BroadcastState::SettingAdvertisingParameters => {
                ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_PARAMETERS
            }
            BroadcastState::SettingAdvertisingData => {
                ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_DATA
            }
            BroadcastState::SettingPeriodicParameters => {
                ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_PARAMETERS
            }
            BroadcastState::SettingPeriodicData => ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA,
            BroadcastState::EnablingAdvertising => {
                ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_ENABLE
            }
            BroadcastState::EnablingPeriodicAdvertising => {
                ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_ENABLE
            }
            BroadcastState::OpeningDataPaths => {
                OpCode::from_bytes(super::host::opcode::LE_SETUP_ISO_DATA_PATH)
            }
            _ => return Vec::new(),
        };
        if opcode != expected.get() {
            return Vec::new();
        }
        if status != 0x00 {
            self.state = BroadcastState::Failed(status);
            return Vec::new();
        }

        match self.state {
            BroadcastState::SettingAdvertisingParameters => {
                self.state = BroadcastState::SettingAdvertisingData;
                vec![hci_command(
                    ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_DATA,
                    &LeSetExtendedAdvertisingDataHeader::serialize(
                        self.config.advertising_handle,
                        data_operation::COMPLETE,
                        0x00,
                        &self.config.advertising_data(),
                    ),
                )]
            }
            BroadcastState::SettingAdvertisingData => {
                self.state = BroadcastState::SettingPeriodicParameters;
                vec![self.periodic_advertising_parameters()]
            }
            BroadcastState::SettingPeriodicParameters => {
                self.state = BroadcastState::SettingPeriodicData;
                vec![hci_command(
                    ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA,
                    &LeSetPeriodicAdvertisingDataHeader::serialize(
                        self.config.advertising_handle,
                        data_operation::COMPLETE,
                        &self.config.periodic_advertising_data(),
                    ),
                )]
            }
            BroadcastState::SettingPeriodicData => {
                self.state = BroadcastState::EnablingAdvertising;
                vec![hci_command(
                    ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_ENABLE,
                    &LeSetExtendedAdvertisingEnableHeader::serialize(
                        true,
                        &[AdvertisingEnableEntry {
                            advertising_handle: self.config.advertising_handle,
                            duration: U16::new(0),
                            max_extended_advertising_events: 0,
                        }],
                    ),
                )]
            }
            BroadcastState::EnablingAdvertising => {
                self.state = BroadcastState::EnablingPeriodicAdvertising;
                let enable = LeSetPeriodicAdvertisingEnable {
                    enable: 0x01,
                    advertising_handle: self.config.advertising_handle,
                };
                vec![hci_command(
                    ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_ENABLE,
                    enable.as_bytes(),
                )]
            }
            BroadcastState::EnablingPeriodicAdvertising => {
                self.state = BroadcastState::CreatingBig;
                vec![self.create_big()]
            }
            BroadcastState::OpeningDataPaths => {
                self.data_paths_open += 1;
                if self.data_paths_open >= self.bis_handles.len() {
                    self.state = BroadcastState::Streaming;
                    Vec::new()
                } else {
                    vec![self.setup_data_path(self.data_paths_open)]
                }
            }
            _ => Vec::new(),
        }
    }

    /// LE Create BIG answers Command Status, not Command Complete; a non-zero
    /// status there means no LE Create BIG Complete will ever arrive.
    fn on_command_status(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        if self.state != BroadcastState::CreatingBig || parameters.len() < 4 {
            return Vec::new();
        }
        let opcode = u16::from_le_bytes([parameters[2], parameters[3]]);
        if opcode == big_opcode::LE_CREATE_BIG.get() && parameters[0] != 0x00 {
            self.state = BroadcastState::Failed(parameters[0]);
        }
        Vec::new()
    }

    /// Handles LE Create BIG Complete and LE Terminate BIG Complete.
    fn on_le_meta(&mut self, parameters: &[u8]) -> Vec<Vec<u8>> {
        let Some((&subevent, rest)) = parameters.split_first() else {
            return Vec::new();
        };
        match subevent {
            big_subevent_code::LE_CREATE_BIG_COMPLETE
                if self.state == BroadcastState::CreatingBig =>
            {
                let Some((header, handles)) = LeCreateBigCompleteEventHeader::parse(rest) else {
                    self.state = BroadcastState::Failed(0xFF);
                    return Vec::new();
                };
                if header.status != 0x00 {
                    self.state = BroadcastState::Failed(header.status);
                    return Vec::new();
                }
                if handles.is_empty() {
                    self.state = BroadcastState::Failed(0xFF);
                    return Vec::new();
                }
                self.tx_sequence = vec![0; handles.len()];
                self.bis_handles = handles;
                self.data_paths_open = 0;
                self.state = BroadcastState::OpeningDataPaths;
                vec![self.setup_data_path(0)]
            }
            big_subevent_code::LE_TERMINATE_BIG_COMPLETE => {
                self.state = BroadcastState::Terminated;
                self.bis_handles.clear();
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// A non-connectable, non-scannable extended advertising set — the only
    /// kind periodic advertising, and therefore a BIG, may hang off.
    fn extended_advertising_parameters(&self) -> Vec<u8> {
        let params = LeSetExtendedAdvertisingParameters {
            advertising_handle: self.config.advertising_handle,
            // No properties at all: not connectable, not scannable, not
            // directed, not legacy, not anonymous.
            advertising_event_properties: U16::new(0),
            primary_advertising_interval_min: U24::new(self.config.primary_advertising_interval),
            primary_advertising_interval_max: U24::new(self.config.primary_advertising_interval),
            primary_advertising_channel_map: 0x07,
            own_address_type: 0x00, // Public.
            peer_address_type: 0x00,
            peer_address: [0; 6],
            advertising_filter_policy: 0x00,
            advertising_tx_power: 0x7F, // No preference.
            primary_advertising_phy: adv_phy::LE_1M,
            secondary_advertising_max_skip: 0,
            secondary_advertising_phy: adv_phy::LE_2M,
            advertising_sid: self.config.advertising_sid,
            scan_request_notification_enable: 0x00,
        };
        hci_command(
            ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_PARAMETERS,
            params.as_bytes(),
        )
    }

    fn periodic_advertising_parameters(&self) -> Vec<u8> {
        let params = LeSetPeriodicAdvertisingParameters {
            advertising_handle: self.config.advertising_handle,
            periodic_advertising_interval_min: U16::new(self.config.periodic_advertising_interval),
            periodic_advertising_interval_max: U16::new(self.config.periodic_advertising_interval),
            periodic_advertising_properties: U16::new(0),
        };
        hci_command(
            ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_PARAMETERS,
            params.as_bytes(),
        )
    }

    fn create_big(&self) -> Vec<u8> {
        let params = LeCreateBig {
            big_handle: self.config.big_handle,
            advertising_handle: self.config.advertising_handle,
            num_bis: self.config.num_bis,
            sdu_interval: U24::new(self.config.sdu_interval_us),
            max_sdu: U16::new(self.config.max_sdu),
            max_transport_latency: U16::new(self.config.max_transport_latency_ms),
            rtn: self.config.rtn,
            phy: self.config.phy,
            packing: big_packing::SEQUENTIAL,
            framing: big_framing::UNFRAMED,
            encryption: if self.config.broadcast_code.is_some() {
                big_encryption::ENCRYPTED
            } else {
                big_encryption::UNENCRYPTED
            },
            broadcast_code: self.config.broadcast_code.unwrap_or([0; 16]),
        };
        hci_command(big_opcode::LE_CREATE_BIG, params.as_bytes())
    }

    /// LE Setup ISO Data Path for BIS `slot` in the Input direction. Codec ID
    /// 0x03 is "transparent": the codec runs in the host.
    fn setup_data_path(&self, slot: usize) -> Vec<u8> {
        let handle = self.bis_handles.get(slot).copied().unwrap_or(0);
        let mut params = Vec::with_capacity(13);
        params.extend_from_slice(&handle.to_le_bytes());
        params.push(iso_data_path::INPUT);
        params.push(iso_data_path::HCI);
        params.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x00]); // transparent
        params.extend_from_slice(&[0x00, 0x00, 0x00]); // controller delay
        params.push(0x00); // codec configuration length
        command(super::host::opcode::LE_SETUP_ISO_DATA_PATH, &params)
    }

    /// Wraps one SDU for BIS index `bis_index` (1-based, as in the BASE), or
    /// `None` if the data path is not open — audio written early is dropped by
    /// the controller, so this refuses rather than pretending to send.
    pub fn send_sdu(&mut self, bis_index: u8, sdu: &[u8]) -> Option<Vec<u8>> {
        if self.state != BroadcastState::Streaming || bis_index == 0 {
            return None;
        }
        let slot = usize::from(bis_index - 1);
        let handle = *self.bis_handles.get(slot)?;
        let sequence = *self.tx_sequence.get(slot)?;
        self.tx_sequence[slot] = sequence.wrapping_add(1);
        Some(build_iso_packet(handle, sequence, sdu))
    }

    /// Re-publishes the BASE with new subgroup metadata, without disturbing
    /// the BIG. Returns the command to send, or `None` if the broadcast is
    /// not streaming — there is no periodic train to rewrite before then.
    ///
    /// This existed nowhere until a second consumer needed it: the setup
    /// sequence wrote LE Set Periodic Advertising Data once and the BASE was
    /// frozen for the life of the broadcast, so a source could start and stop
    /// but never *change*. Only the metadata is settable, because everything
    /// else in the BASE (codec configuration, BIS count, channel allocation)
    /// has to agree with the `LE Create BIG` the controller already acted on;
    /// changing those means tearing the BIG down and building another.
    ///
    /// The result arrives as a Command Complete and is read back with
    /// [`Self::take_update_status`].
    pub fn update_metadata(&mut self, metadata: Vec<u8>) -> Option<Vec<u8>> {
        if self.state != BroadcastState::Streaming {
            return None;
        }
        self.config.metadata = metadata;
        self.update_in_flight = true;
        Some(hci_command(
            ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA,
            &LeSetPeriodicAdvertisingDataHeader::serialize(
                self.config.advertising_handle,
                data_operation::COMPLETE,
                &self.config.periodic_advertising_data(),
            ),
        ))
    }

    /// Takes the controller's answer to the last [`Self::update_metadata`],
    /// once, or `None` while none has arrived.
    pub fn take_update_status(&mut self) -> Option<u8> {
        self.last_update_status.take()
    }

    /// Tears the BIG down. 0x16 is Connection Terminated by Local Host, the
    /// reason a host gives for its own decision.
    pub fn terminate(&self) -> Vec<u8> {
        let params = LeTerminateBig {
            big_handle: self.config.big_handle,
            reason: 0x16,
        };
        hci_command(big_opcode::LE_TERMINATE_BIG, params.as_bytes())
    }

    /// Where setup has got to.
    pub fn state(&self) -> BroadcastState {
        self.state
    }

    /// True once SDUs will actually go out.
    pub fn is_streaming(&self) -> bool {
        self.state == BroadcastState::Streaming
    }

    /// The BIS connection handles, in BIS index order.
    pub fn bis_handles(&self) -> &[u16] {
        &self.bis_handles
    }
}

#[cfg(test)]
#[path = "big_broadcaster_tests.rs"]
mod tests;
