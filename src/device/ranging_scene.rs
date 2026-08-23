// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A two-device scene for comparing the two ways Bluetooth answers "how far
//! away is that?": inverting a path-loss model on RSSI, and Channel Sounding.
//!
//! # The scene
//!
//! * a **tag** — a peripheral advertising the Ranging Service, which is the
//!   reflector of the Channel Sounding procedure and publishes its half of
//!   every measurement over RAS;
//! * a **locator** — a central that connects, discovers the Ranging Service,
//!   subscribes to Real-Time Ranging Data, and runs the procedure.
//!
//! Both hosts sit on one [`Link`], which is the only thing that knows where
//! they are. Move the tag and every number downstream moves with it: the RSSI
//! the locator reads, the phase of every Channel Sounding tone, both
//! estimates, and both errors.
//!
//! # Why both devices are in one object
//!
//! Chrome throttles hidden tabs to roughly one timer callback per second, so
//! a scene split across two tabs stalls silently and looks like a Bluetooth
//! bug. One [`tick`](RangingScene::tick) advances everything.
//!
//! # What is derived and what is assumed
//!
//! Every number [`RangingScene::status_json`] reports is derived from bytes
//! the simulated stack produced — the RSSI from a Command Complete for HCI
//! Read RSSI, the tones from LE CS Subevent Result events and from Ranging
//! Data notified over ATT. The one thing that is *assumed* rather than
//! measured is the pair of constants the RSSI estimator inverts the model
//! with, and that is the point: a receiver never knows them.

use crate::client::gatt_client::GattClient;
use crate::controller::propagation::{PathLossModel, Position};
use crate::controller::sim::Link;
use crate::cs::path_loss::{RssiRanger, RssiRangingParams};
use crate::device::VirtualDevice;
use crate::device::channel_sounding::{CsInitiator, CsReflector, CsState};
use crate::device::host::{LeHost, acl_packets, command};
use crate::l2cap::{AclReassembler, HciAclHeader, L2capHeader};
use crate::packets::att::opcode as att_op;
use crate::packets::hci_events::HciEvent;
use crate::profiles::ras::{self, RangingService, ras_uuid};
use crate::transport::{HciChannel, h4_type};
use crate::types::{Address, AddressType};
use std::sync::Arc;

/// HCI Read RSSI (Vol 4, Part E, Section 7.5.4).
const OPCODE_READ_RSSI: [u8; 2] = [0x05, 0x14];

/// How many RSSI samples the locator averages over. About two seconds at the
/// page's tick rate: long enough to be a fair representation of what a
/// proximity app does, short enough to still follow a moving tag.
const RSSI_WINDOW: usize = 16;

/// The ATT MTU the locator asks for. RAS segments to fit it.
const CLIENT_MTU: u16 = 517;

/// Where the locator's GATT work has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocatorPhase {
    /// Scanning; advertising reports are arriving but nothing is connected.
    Scanning,
    /// LE Create Connection sent.
    Connecting,
    /// ATT MTU exchange in flight.
    ExchangingMtu,
    /// Reading the tag's primary services.
    DiscoveringServices,
    /// Reading the characteristics of `services[i]`.
    DiscoveringCharacteristics(usize),
    /// Writing the Real-Time Ranging Data CCCD.
    Subscribing,
    /// Subscribed; the Channel Sounding procedure is being set up or is
    /// running.
    Ranging,
    /// The tag has no Ranging Service, so there is nothing to range with.
    NoRangingService,
}

impl LocatorPhase {
    /// A short label for a status readout.
    fn label(self) -> &'static str {
        match self {
            LocatorPhase::Scanning => "scanning",
            LocatorPhase::Connecting => "connecting",
            LocatorPhase::ExchangingMtu => "exchanging MTU",
            LocatorPhase::DiscoveringServices => "discovering services",
            LocatorPhase::DiscoveringCharacteristics(_) => "discovering characteristics",
            LocatorPhase::Subscribing => "subscribing to ranging data",
            LocatorPhase::Ranging => "ranging",
            LocatorPhase::NoRangingService => "no Ranging Service",
        }
    }
}

/// The peripheral: advertises, serves the Ranging Service, and reflects.
struct Tag {
    channel: Arc<HciChannel>,
    device: VirtualDevice,
    host: LeHost,
    ras: RangingService,
    reflector: CsReflector,
    /// Ranging Data segments queued for notification, oldest first.
    outbound: std::collections::VecDeque<Vec<u8>>,
    /// Ranging Data bodies actually notified.
    published: u32,
}

/// The central: connects, discovers, subscribes, and estimates.
struct Locator {
    channel: Arc<HciChannel>,
    client: GattClient,
    reassembler: AclReassembler,
    phase: LocatorPhase,
    rssi: RssiRanger,
    initiator: CsInitiator,
    /// Reassembles the Ranging Data body from RAS notification segments.
    ras_segments: ras::Reassembler,
    /// Value handle of Real-Time Ranging Data, once discovered.
    ranging_data_handle: Option<u16>,
    /// Ranging Data bodies received and accepted.
    accepted: u32,
    /// Notifications whose reassembly or parse failed.
    rejected: u32,
    connect_requested: bool,
}

/// A tag and a locator on one simulated medium.
pub struct RangingScene {
    link: Link,
    tag: Tag,
    locator: Locator,
    tag_address: Address,
    locator_address: Address,
    started: bool,
}

impl RangingScene {
    /// Builds the scene with the tag at the origin and the locator two metres
    /// away, so a page that never moves anything still shows a real distance.
    pub fn new(tag_address: Address, locator_address: Address) -> Self {
        let mut link = Link::new();
        let tag_channel = link.add_device(tag_address);
        let locator_channel = link.add_device(locator_address);
        link.set_position(locator_address, Position::new(0.0, 0.0));
        link.set_position(tag_address, Position::new(2.0, 0.0));

        let mut device = VirtualDevice::new("Ranging Tag", tag_address, AddressType::Public);
        device.gatt_db = crate::gatt::GattDatabase::new();
        let ras = RangingService::register(&mut device.gatt_db);

        Self {
            link,
            tag: Tag {
                channel: tag_channel,
                device,
                host: LeHost::new(),
                ras,
                reflector: CsReflector::new(),
                outbound: std::collections::VecDeque::new(),
                published: 0,
            },
            locator: Locator {
                channel: locator_channel,
                client: GattClient::new(0, tag_address),
                reassembler: AclReassembler::new(),
                phase: LocatorPhase::Scanning,
                rssi: RssiRanger::new(RssiRangingParams::default(), RSSI_WINDOW),
                initiator: CsInitiator::new(1),
                ras_segments: ras::Reassembler::new(),
                ranging_data_handle: None,
                accepted: 0,
                rejected: 0,
                connect_requested: false,
            },
            tag_address,
            locator_address,
            started: false,
        }
    }

    /// Moves the tag on the floor plan. Everything downstream follows.
    pub fn set_tag_position(&mut self, position: Position) {
        self.link.set_position(self.tag_address, position);
    }

    /// Where the tag stands.
    pub fn tag_position(&self) -> Position {
        self.link.position(self.tag_address).unwrap_or_default()
    }

    /// Where the locator stands.
    pub fn locator_position(&self) -> Position {
        self.link.position(self.locator_address).unwrap_or_default()
    }

    /// The true separation in metres — the number both estimates are judged
    /// against, and which neither host can see.
    pub fn true_distance_m(&self) -> f64 {
        self.link
            .distance_between(self.tag_address, self.locator_address)
            .unwrap_or(0.0)
    }

    /// The room the radio propagates through.
    pub fn path_loss(&self) -> PathLossModel {
        self.link.path_loss()
    }

    /// Replaces the room's propagation model.
    pub fn set_path_loss(&mut self, model: PathLossModel) {
        self.link.set_path_loss(model);
    }

    /// What the locator's RSSI estimator *assumes* about the room. Changing
    /// it re-derives the estimate from samples already collected, which is
    /// how much of the error was the assumption rather than the radio.
    pub fn rssi_assumptions(&self) -> RssiRangingParams {
        self.locator.rssi.params()
    }

    /// Replaces the locator's assumptions.
    pub fn set_rssi_assumptions(&mut self, params: RssiRangingParams) {
        self.locator.rssi.set_params(params);
    }

    /// Reseeds the medium's noise, so a run repeats exactly.
    pub fn set_noise_seed(&mut self, seed: u64) {
        self.link.set_noise_seed(seed);
    }

    /// Advances the whole scene one step: both hosts produce, the medium
    /// routes, both hosts consume.
    pub fn tick(&mut self) {
        if !self.started {
            self.start();
            self.started = true;
        }
        self.tag_produce();
        self.locator_produce();
        self.link.tick();
        self.tag_consume();
        self.locator_consume();
    }

    /// Queues both devices' controller bring-up.
    fn start(&mut self) {
        if let Ok(commands) = self.tag.host.start_advertising(&self.tag.device, &[0x185B]) {
            for packet in commands {
                let _ = self.tag.channel.send_command(&packet[1..]);
            }
        }
        for packet in crate::device::host::init_commands() {
            let _ = self.locator.channel.send_command(&packet[1..]);
        }
        // A scanner hears the tag's advertisements, which is where the RSSI
        // half of the demo starts — before anything is connected.
        let _ = self
            .locator
            .channel
            .send_command(&[0x0C, 0x20, 0x02, 0x01, 0x00]);
    }

    /// Publishes any Ranging Data the reflector has produced.
    fn tag_produce(&mut self) {
        let Some((handle, _)) = self.tag.host.connection() else {
            self.tag.outbound.clear();
            return;
        };
        if let Some(body) = self.tag.reflector.take_ranging_data() {
            // One byte of the notification is the ATT opcode and two the
            // handle, so the value capacity is MTU − 3.
            let mtu = self
                .tag
                .device
                .connections
                .get(&handle)
                .map_or(self.tag.device.default_mtu, |c| c.mtu);
            let capacity = usize::from(mtu.saturating_sub(3));
            self.tag
                .outbound
                .extend(ras::segment(&body, capacity.max(2)));
        }
        // Only notify what a subscriber will actually receive; a notification
        // to nobody is dropped by the server and would inflate the count.
        while let Some(segment) = self.tag.outbound.pop_front() {
            let pdu = self.tag.device.create_notification_for(
                handle,
                self.tag.ras.realtime_data_value_handle,
                &segment,
            );
            for packet in acl_packets(handle, &pdu) {
                let _ = self.tag.channel.send_acl_data(&packet[1..]);
            }
            self.tag.published += 1;
        }
    }

    /// Issues the locator's connection request, its per-tick RSSI read, and
    /// whatever the Channel Sounding state machine wants next.
    fn locator_produce(&mut self) {
        if !self.locator.connect_requested && self.locator.phase == LocatorPhase::Scanning {
            let _ = self
                .locator
                .channel
                .send_command(&create_connection_params(self.tag_address));
            self.locator.connect_requested = true;
            self.locator.phase = LocatorPhase::Connecting;
        }
        let handle = self.locator.client.connection_handle;
        if handle != 0 {
            // Vol 4, Part E, Section 7.5.4 — the connected device's way of
            // reading its peer's signal strength.
            let packet = command(OPCODE_READ_RSSI, &handle.to_le_bytes());
            let _ = self.locator.channel.send_command(&packet[1..]);
        }
    }

    /// Delivers the tag's controller events to its host.
    fn tag_consume(&mut self) {
        while let Some(packet) = self.tag.channel.poll_controller_packet() {
            self.tag.reflector.on_packet(&packet);
            if let Ok(replies) = self.tag.host.handle_packet(&mut self.tag.device, &packet) {
                for reply in replies {
                    let _ = match reply.first() {
                        Some(&h4_type::HCI_ACL_DATA) => self.tag.channel.send_acl_data(&reply[1..]),
                        _ => self.tag.channel.send_command(&reply[1..]),
                    };
                }
            }
        }
    }

    /// Delivers the locator's controller events to its host.
    fn locator_consume(&mut self) {
        while let Some(packet) = self.locator.channel.poll_controller_packet() {
            for reply in self.locator.initiator.on_packet(&packet) {
                let _ = self.locator.channel.send_command(&reply[1..]);
            }
            match packet.first() {
                Some(&h4_type::HCI_EVENT) => self.locator_on_event(&packet),
                Some(&h4_type::HCI_ACL_DATA) => self.locator_on_acl(&packet),
                _ => {}
            }
        }
    }

    /// Handles one HCI event on the locator: advertising reports feed the
    /// RSSI window, Read RSSI's completion feeds it while connected, and a
    /// connection completion starts GATT discovery.
    fn locator_on_event(&mut self, packet: &[u8]) {
        let Some(event) = HciEvent::parse_h4(packet) else {
            return;
        };
        match event {
            HciEvent::CommandComplete {
                header,
                return_parameters,
            } if header.command_opcode.get() == u16::from_le_bytes(OPCODE_READ_RSSI) => {
                // status(1) connection_handle(2) rssi(1); 127 means the
                // controller had no measurement, and must not be recorded as
                // an extremely strong signal.
                if let [0x00, _, _, rssi] = return_parameters[..]
                    && rssi as i8 != 127
                {
                    self.locator.rssi.push(rssi as i8);
                }
            }
            HciEvent::LeConnectionComplete(event) if event.status == 0x00 => {
                let handle = event.connection_handle.get() & 0x0FFF;
                self.locator.client = GattClient::new(handle, self.tag_address);
                self.locator.phase = LocatorPhase::ExchangingMtu;
                let request = self.locator.client.create_exchange_mtu_request(CLIENT_MTU);
                self.locator_send(&request);
            }
            HciEvent::DisconnectionComplete(_) => {
                self.locator.phase = LocatorPhase::Scanning;
                self.locator.connect_requested = false;
                self.locator.initiator.reset();
                self.locator.ras_segments.reset();
                self.tag.reflector.reset();
            }
            HciEvent::Other {
                code: 0x3E,
                parameters,
            } if parameters.first() == Some(&0x02) => {
                // An LE Advertising Report ends with the RSSI byte: this is
                // how a proximity app ranges a device it has not connected to.
                if let Some(&rssi) = parameters.last() {
                    self.locator.rssi.push(rssi as i8);
                }
            }
            _ => {}
        }
    }

    /// Reassembles ACL into ATT and dispatches it.
    fn locator_on_acl(&mut self, packet: &[u8]) {
        let Some((header, payload)) = HciAclHeader::parse(&packet[1..]) else {
            return;
        };
        let Ok(Some(frame)) = self.locator.reassembler.push_fragment(
            header.handle(),
            header.is_first_fragment(),
            payload,
        ) else {
            return;
        };
        let Some((_, att)) = L2capHeader::parse(&frame) else {
            return;
        };
        let att = att.to_vec(); // the reassembler's buffer is borrowed until this copy
        self.locator_on_att(&att);
    }

    /// Advances GATT discovery, or takes in a Ranging Data notification.
    fn locator_on_att(&mut self, att: &[u8]) {
        let Some(&op) = att.first() else { return };
        let is_error = op == att_op::ERROR_RSP;

        if op == att_op::HANDLE_VALUE_NTF && att.len() >= 3 {
            let value_handle = u16::from_le_bytes([att[1], att[2]]);
            if Some(value_handle) == self.locator.ranging_data_handle {
                self.locator_on_ranging_segment(&att[3..]);
            }
            return;
        }

        match self.locator.phase {
            LocatorPhase::ExchangingMtu => {
                if op == att_op::EXCHANGE_MTU_RSP && att.len() >= 3 {
                    let server_mtu = u16::from_le_bytes([att[1], att[2]]);
                    self.locator
                        .client
                        .on_exchange_mtu_response(server_mtu, CLIENT_MTU);
                }
                self.locator.phase = LocatorPhase::DiscoveringServices;
                let request = self
                    .locator
                    .client
                    .create_discover_services_request(0x0001, 0xFFFF);
                self.locator_send(&request);
            }
            LocatorPhase::DiscoveringServices => {
                if is_error {
                    self.locator_start_characteristics();
                } else if op == att_op::READ_BY_GROUP_TYPE_RSP {
                    let _ = self.locator.client.on_discover_services_response(att);
                    let last = self
                        .locator
                        .client
                        .services
                        .last()
                        .map_or(0xFFFF, |s| s.end_handle);
                    if last < 0xFFFF {
                        let request = self
                            .locator
                            .client
                            .create_discover_services_request(last + 1, 0xFFFF);
                        self.locator_send(&request);
                    } else {
                        self.locator_start_characteristics();
                    }
                }
            }
            LocatorPhase::DiscoveringCharacteristics(index) => {
                if is_error {
                    self.locator_next_service(index);
                } else if op == att_op::READ_BY_TYPE_RSP {
                    let uuid = self.locator.client.services[index].uuid;
                    let _ = self
                        .locator
                        .client
                        .on_discover_characteristics_response(uuid, att);
                    let service = &self.locator.client.services[index];
                    let end = service.end_handle;
                    let last = service
                        .characteristics
                        .last()
                        .map_or(service.start_handle, |c| c.value_handle);
                    if last < end {
                        let request = self
                            .locator
                            .client
                            .create_discover_characteristics_request(last + 1, end);
                        self.locator_send(&request);
                    } else {
                        self.locator_next_service(index);
                    }
                }
            }
            LocatorPhase::Subscribing => {
                // The CCCD write has been answered; the procedure can start.
                // Starting before it would waste measurements: the reflector
                // has no subscriber to publish its half to, so nothing could
                // be combined.
                self.locator.phase = LocatorPhase::Ranging;
                let handle = self.locator.client.connection_handle;
                for packet in self.locator.initiator.start(handle) {
                    let _ = self.locator.channel.send_command(&packet[1..]);
                }
            }
            LocatorPhase::Scanning
            | LocatorPhase::Connecting
            | LocatorPhase::Ranging
            | LocatorPhase::NoRangingService => {}
        }
    }

    /// Feeds one RAS notification segment into the reassembler, and the
    /// completed body into the initiator.
    fn locator_on_ranging_segment(&mut self, segment: &[u8]) {
        let Some(body) = self.locator.ras_segments.push(segment) else {
            return;
        };
        if self.locator.initiator.on_ranging_data(&body) {
            self.locator.accepted += 1;
        } else {
            self.locator.rejected += 1;
        }
    }

    /// Begins characteristic discovery, or gives up if there are no services.
    fn locator_start_characteristics(&mut self) {
        if self.locator.client.services.is_empty() {
            self.locator.phase = LocatorPhase::NoRangingService;
            return;
        }
        self.locator.phase = LocatorPhase::DiscoveringCharacteristics(0);
        self.locator_discover_characteristics(0);
    }

    /// Moves to the next service, or subscribes once every one is walked.
    fn locator_next_service(&mut self, index: usize) {
        let next = index + 1;
        if next < self.locator.client.services.len() {
            self.locator.phase = LocatorPhase::DiscoveringCharacteristics(next);
            self.locator_discover_characteristics(next);
            return;
        }
        let Some(characteristic) = self
            .locator
            .client
            .find_characteristic(ras_uuid::RANGING_REALTIME_DATA)
        else {
            // Without Real-Time Ranging Data the peer cannot send its half of
            // a measurement, so Channel Sounding is not possible against it —
            // better said than quietly attempted.
            self.locator.phase = LocatorPhase::NoRangingService;
            return;
        };
        self.locator.ranging_data_handle = Some(characteristic.value_handle);
        self.locator.phase = LocatorPhase::Subscribing;
        // The CCCD sits directly after the value handle in RAS's layout.
        let request = self
            .locator
            .client
            .create_write_request(characteristic.value_handle + 1, &[0x01, 0x00]);
        self.locator_send(&request);
    }

    /// Sends the characteristic-discovery request for `services[index]`.
    fn locator_discover_characteristics(&mut self, index: usize) {
        let service = &self.locator.client.services[index];
        let request = self
            .locator
            .client
            .create_discover_characteristics_request(service.start_handle, service.end_handle);
        self.locator_send(&request);
    }

    /// Frames an ATT PDU over L2CAP and ACL and sends it.
    fn locator_send(&self, att: &[u8]) {
        let handle = self.locator.client.connection_handle;
        for packet in acl_packets(handle, att) {
            let _ = self.locator.channel.send_acl_data(&packet[1..]);
        }
    }

    /// Everything a page needs, as JSON: the ground truth, the room, the raw
    /// measurements, and both estimates with their errors.
    pub fn status_json(&self) -> String {
        serde_json::to_string(&self.status()).unwrap_or_else(|_| "{}".to_string())
    }

    /// The structured status behind [`Self::status_json`].
    fn status(&self) -> RangingStatus {
        let truth = self.true_distance_m();
        let model = self.link.path_loss();
        let assumptions = self.locator.rssi.params();
        let rssi_distance = self.locator.rssi.distance_m();
        let cs = self.locator.initiator.estimate();
        let (completed, mismatched) = self.locator.initiator.procedure_counts();

        RangingStatus {
            truth: TruthView {
                tag: self.tag_position(),
                locator: self.locator_position(),
                distance_m: truth,
            },
            room: RoomView {
                tx_power_dbm: model.tx_power_dbm,
                reference_loss_db: model.reference_loss_db,
                path_loss_exponent: model.path_loss_exponent,
                shadowing_sigma_db: model.shadowing_sigma_db,
            },
            link: LinkView {
                phase: self.locator.phase.label(),
                connected: self.locator.client.connection_handle != 0,
                connection_handle: self.locator.client.connection_handle,
                ras_subscribed: self.locator.ranging_data_handle.is_some()
                    && self.locator.phase == LocatorPhase::Ranging,
                ranging_data_handle: self.locator.ranging_data_handle,
                notifications_sent: self.tag.published,
                ranging_data_accepted: self.locator.accepted,
                ranging_data_rejected: self.locator.rejected,
            },
            rssi: RssiView {
                latest_dbm: self.locator.rssi.latest_rssi_dbm(),
                mean_dbm: self.locator.rssi.mean_rssi_dbm(),
                std_dev_db: self.locator.rssi.rssi_std_dev_db(),
                samples: self.locator.rssi.sample_count(),
                assumed_reference_dbm: assumptions.reference_rssi_dbm,
                assumed_exponent: assumptions.path_loss_exponent,
                distance_m: rssi_distance,
                instantaneous_distance_m: self.locator.rssi.instantaneous_distance_m(),
                error_m: rssi_distance.map(|d| d - truth),
            },
            cs: CsView {
                state: cs_state_label(self.locator.initiator.state()),
                measuring: self.locator.initiator.is_measuring(),
                procedures_combined: completed,
                procedures_mismatched: mismatched,
                local_tones: self.locator.initiator.local_tones().len(),
                remote_tones: self.locator.initiator.remote_tones().len(),
                distance_m: cs.map(|e| e.distance_m),
                std_error_m: cs.map(|e| e.std_error_m),
                residual_rad: cs.map(|e| e.residual_rad),
                bandwidth_hz: cs.map(|e| e.bandwidth_hz),
                unambiguous_range_m: cs.map(|e| e.unambiguous_range_m),
                error_m: cs.map(|e| e.distance_m - truth),
                ranging_counter: self.locator.initiator.measured_counter(),
                tones: self.tone_views(),
            },
        }
    }

    /// The raw per-tone measurements, so a page can show what the estimate
    /// was computed from rather than only its output.
    ///
    /// All three columns come from one stored measurement, so the sums shown
    /// are the sums the estimate was actually fitted to. Pairing the latest
    /// local half with the latest remote half instead would, for most of
    /// every procedure, add tones from two different measurements — and their
    /// sums are noise, since the oscillator offset only cancels within a
    /// procedure.
    fn tone_views(&self) -> Vec<ToneView> {
        let local = self.locator.initiator.local_tones();
        let remote = self.locator.initiator.remote_tones();
        let combined = self.locator.initiator.combined_tones();
        // The estimator's own unwrapped sequence. The wrapped sums look like
        // scatter at any real distance; the straight line the distance was
        // fitted to only exists here.
        let unwrapped = crate::cs::ranging::unwrap_sequence(combined);
        local
            .iter()
            .map(|tone| ToneView {
                channel: tone.channel,
                i: tone.i,
                q: tone.q,
                local_phase_deg: tone.phase_rad().to_degrees(),
                remote_phase_deg: remote
                    .iter()
                    .find(|t| t.channel == tone.channel)
                    .map(|t| t.phase_rad().to_degrees()),
                combined_phase_deg: combined
                    .iter()
                    .find(|t| t.channel == tone.channel)
                    .map(|t| t.phase_rad.to_degrees()),
                unwrapped_phase_deg: combined
                    .iter()
                    .position(|t| t.channel == tone.channel)
                    .and_then(|index| unwrapped.get(index))
                    .map(|radians| radians.to_degrees()),
            })
            .collect()
    }
}

/// A short label for the initiator's setup state.
fn cs_state_label(state: CsState) -> String {
    match state {
        CsState::Idle => "idle".to_string(),
        CsState::Securing => "securing".to_string(),
        CsState::Configuring => "creating config".to_string(),
        CsState::SettingParameters => "setting procedure parameters".to_string(),
        CsState::Enabling => "enabling procedure".to_string(),
        CsState::Measuring => "measuring".to_string(),
        CsState::Failed(status) => format!("failed (HCI status 0x{status:02X})"),
    }
}

/// LE Create Connection's 25 parameter bytes (Vol 4, Part E, Section 7.8.12),
/// as an H4 command packet without its type byte.
fn create_connection_params(target: Address) -> Vec<u8> {
    let mut params = Vec::with_capacity(25);
    params.extend_from_slice(&0x0060u16.to_le_bytes()); // scan interval
    params.extend_from_slice(&0x0030u16.to_le_bytes()); // scan window
    params.push(0x00); // initiator filter policy: use the peer address
    params.push(0x00); // peer address type: public
    let mut peer = target.to_be_bytes();
    peer.reverse();
    params.extend_from_slice(&peer);
    params.push(0x00); // own address type: public
    params.extend_from_slice(&0x0018u16.to_le_bytes()); // min connection interval
    params.extend_from_slice(&0x0028u16.to_le_bytes()); // max connection interval
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // max latency
    params.extend_from_slice(&0x00C8u16.to_le_bytes()); // supervision timeout
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
    params.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length
    debug_assert_eq!(params.len(), 25, "LE Create Connection is 25 bytes");
    let packet = command([0x0D, 0x20], &params);
    packet[1..].to_vec()
}

// --- status views ----------------------------------------------------------

/// The whole scene, as a page sees it.
#[derive(serde::Serialize)]
struct RangingStatus {
    truth: TruthView,
    room: RoomView,
    link: LinkView,
    rssi: RssiView,
    cs: CsView,
}

/// Ground truth, which only the medium knows.
#[derive(serde::Serialize)]
struct TruthView {
    tag: Position,
    locator: Position,
    distance_m: f64,
}

/// The propagation model the radio actually used.
#[derive(serde::Serialize)]
struct RoomView {
    tx_power_dbm: f64,
    reference_loss_db: f64,
    path_loss_exponent: f64,
    shadowing_sigma_db: f64,
}

/// Where the GATT connection and the RAS subscription have got to.
#[derive(serde::Serialize)]
struct LinkView {
    phase: &'static str,
    connected: bool,
    connection_handle: u16,
    ras_subscribed: bool,
    ranging_data_handle: Option<u16>,
    notifications_sent: u32,
    ranging_data_accepted: u32,
    ranging_data_rejected: u32,
}

/// The RSSI method: its raw input, its assumptions, and its answer.
#[derive(serde::Serialize)]
struct RssiView {
    latest_dbm: Option<i8>,
    mean_dbm: Option<f64>,
    std_dev_db: Option<f64>,
    samples: usize,
    assumed_reference_dbm: f64,
    assumed_exponent: f64,
    distance_m: Option<f64>,
    instantaneous_distance_m: Option<f64>,
    error_m: Option<f64>,
}

/// The Channel Sounding method, likewise.
#[derive(serde::Serialize)]
struct CsView {
    state: String,
    measuring: bool,
    procedures_combined: u32,
    procedures_mismatched: u32,
    local_tones: usize,
    remote_tones: usize,
    distance_m: Option<f64>,
    std_error_m: Option<f64>,
    residual_rad: Option<f64>,
    bandwidth_hz: Option<f64>,
    unambiguous_range_m: Option<f64>,
    error_m: Option<f64>,
    ranging_counter: Option<u16>,
    tones: Vec<ToneView>,
}

/// One tone, at both ends and combined.
#[derive(serde::Serialize)]
struct ToneView {
    channel: u8,
    i: i16,
    q: i16,
    local_phase_deg: f64,
    remote_phase_deg: Option<f64>,
    combined_phase_deg: Option<f64>,
    unwrapped_phase_deg: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> RangingScene {
        let mut scene = RangingScene::new(
            "AA:BB:CC:00:00:01".parse().unwrap(),
            "AA:BB:CC:00:00:02".parse().unwrap(),
        );
        scene.set_noise_seed(11);
        scene
    }

    /// Runs the scene until the Channel Sounding estimate exists, or gives up.
    fn run_until_ranging(scene: &mut RangingScene, ticks: usize) -> bool {
        for _ in 0..ticks {
            scene.tick();
            if scene.locator.initiator.estimate().is_some() {
                return true;
            }
        }
        false
    }

    #[test]
    fn test_the_scene_connects_discovers_ras_and_subscribes() {
        let mut scene = scene();
        assert!(run_until_ranging(&mut scene, 40), "never started ranging");
        assert_eq!(scene.locator.phase, LocatorPhase::Ranging);
        assert!(scene.locator.ranging_data_handle.is_some());
        assert!(
            scene.tag.published > 0,
            "the tag must actually notify its half"
        );
        assert_eq!(scene.locator.rejected, 0, "every notification parsed");
    }

    #[test]
    fn test_channel_sounding_tracks_the_true_distance() {
        let mut scene = scene();
        assert!(run_until_ranging(&mut scene, 40));
        for truth in [1.0, 4.5, 12.0, 25.0] {
            scene.set_tag_position(Position::new(truth, 0.0));
            // Two procedures: one to measure at the new position, one for the
            // tag's half to cross the link and be combined.
            for _ in 0..6 {
                scene.tick();
            }
            let estimate = scene
                .locator
                .initiator
                .estimate()
                .expect("an estimate")
                .distance_m;
            assert!(
                (estimate - truth).abs() < 0.5,
                "true {truth} m, Channel Sounding said {estimate} m"
            );
        }
    }

    #[test]
    fn test_rssi_ranging_is_much_worse_than_channel_sounding() {
        // The claim the whole page is built on, as a test. Not "RSSI is
        // noisy" — that it is *systematically* wrong, because it inverts a
        // model whose constants it does not know.
        let mut scene = scene();
        assert!(run_until_ranging(&mut scene, 40));
        let truth = 10.0;
        scene.set_tag_position(Position::new(truth, 0.0));
        for _ in 0..40 {
            scene.tick();
        }
        let cs = scene.locator.initiator.estimate().unwrap().distance_m;
        let rssi = scene.locator.rssi.distance_m().expect("samples");
        assert!(
            (cs - truth).abs() < 0.5,
            "Channel Sounding: {cs} m for a true {truth} m"
        );
        assert!(
            (rssi - truth).abs() > 3.0 * (cs - truth).abs().max(0.1),
            "RSSI ({rssi} m) should be far worse than CS ({cs} m)"
        );
    }

    #[test]
    fn test_rssi_falls_as_the_tag_moves_away() {
        let mut scene = scene();
        for _ in 0..20 {
            scene.tick();
        }
        let mut means = Vec::new();
        for distance in [1.0, 8.0, 25.0] {
            scene.set_tag_position(Position::new(distance, 0.0));
            for _ in 0..20 {
                scene.tick();
            }
            means.push(scene.locator.rssi.mean_rssi_dbm().expect("samples"));
        }
        assert!(
            means.windows(2).all(|w| w[1] < w[0]),
            "RSSI must weaken with distance: {means:?}"
        );
    }

    #[test]
    fn test_the_reported_status_carries_the_raw_inputs_not_only_the_answers() {
        let mut scene = scene();
        assert!(run_until_ranging(&mut scene, 40));
        let status = scene.status_json();
        let parsed: serde_json::Value = serde_json::from_str(&status).unwrap();
        assert!(parsed["rssi"]["latest_dbm"].is_i64(), "{status}");
        assert!(parsed["rssi"]["mean_dbm"].is_f64());
        assert_eq!(parsed["cs"]["local_tones"], 19);
        assert_eq!(parsed["cs"]["remote_tones"], 19);
        assert_eq!(
            parsed["cs"]["tones"].as_array().unwrap().len(),
            19,
            "the per-tone measurements must be visible"
        );
        assert!(parsed["cs"]["tones"][0]["combined_phase_deg"].is_f64());
        assert!(parsed["cs"]["tones"][0]["unwrapped_phase_deg"].is_f64());
        assert!(parsed["truth"]["distance_m"].is_f64());
        assert!(parsed["room"]["path_loss_exponent"].is_f64());
    }

    #[test]
    fn test_changing_the_assumed_exponent_moves_only_the_rssi_estimate() {
        let mut scene = scene();
        assert!(run_until_ranging(&mut scene, 40));
        scene.set_tag_position(Position::new(12.0, 0.0));
        for _ in 0..30 {
            scene.tick();
        }
        let cs_before = scene.locator.initiator.estimate().unwrap().distance_m;
        let rssi_before = scene.locator.rssi.distance_m().unwrap();

        scene.set_rssi_assumptions(RssiRangingParams {
            path_loss_exponent: 2.7,
            ..RssiRangingParams::default()
        });
        let rssi_after = scene.locator.rssi.distance_m().unwrap();
        let cs_after = scene.locator.initiator.estimate().unwrap().distance_m;
        assert!(rssi_after < rssi_before, "{rssi_after} vs {rssi_before}");
        assert_eq!(
            cs_before, cs_after,
            "Channel Sounding does not depend on the room's constants"
        );
    }

    #[test]
    fn test_no_estimate_before_the_procedure_runs() {
        let mut scene = scene();
        scene.tick();
        let parsed: serde_json::Value = serde_json::from_str(&scene.status_json()).unwrap();
        assert!(parsed["cs"]["distance_m"].is_null());
        assert_eq!(parsed["cs"]["procedures_combined"], 0);
    }
}
