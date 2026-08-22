// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A minimal in-process LE controller (`SimController`) and shared medium
//! ([`Link`]) — enough of the Link Layer, modeled at the HCI boundary, to let
//! several Simble host stacks discover, connect to, and exchange data with one
//! another **in a single process, with no netsim, no Rootcanal, and no radio**.
//!
//! This is the lowest rung of Simble's controller ladder. It is deliberately a
//! thin HCI *matchmaker*, not a faithful controller: it routes advertising to
//! scanners, completes connections, and shuttles ACL data between peers, but it
//! models none of the PHY (channel hopping, timing, encryption, ISO). For that
//! fidelity, point a host at a real Rootcanal over the WebSocket transport; for
//! ranging and device movement, at netsim. Because it is pure Rust with no FFI,
//! it runs the same natively and on `wasm32`, so a single web page can host a
//! whole scene of devices.
//!
//! HCI packets are parsed and built with zero-copy `#[repr(C)]` structs (the
//! same idiom as [`crate::packets`]), so the wire layouts are explicit rather
//! than hand-indexed byte offsets.
//!
//! ```
//! use simble::controller::sim::Link;
//! use simble::types::Address;
//!
//! let mut link = Link::new();
//! let adv = link.add_device("AA:BB:CC:00:00:01".parse::<Address>().unwrap());
//! let scan = link.add_device("AA:BB:CC:00:00:02".parse::<Address>().unwrap());
//!
//! adv.send_command(&[0x08, 0x20, 0x04, 0x03, 0x02, 0x01, 0x06]).unwrap(); // adv data
//! adv.send_command(&[0x0A, 0x20, 0x01, 0x01]).unwrap(); // LE Set Advertising Enable
//! scan.send_command(&[0x0C, 0x20, 0x02, 0x01, 0x00]).unwrap(); // LE Set Scan Enable
//!
//! link.tick(); // route advertising across the shared medium
//!
//! assert!(scan.poll_controller_packet().is_some()); // an LE Advertising Report
//! ```

use crate::controller::propagation::{
    PathLossModel, Position, Rng, channel_frequency_hz, phase_noise_sigma_rad,
    propagation_phase_rad, wrap_phase,
};
use crate::transport::HciChannel;
use crate::transport::h4_type;
use crate::types::Address;
use std::collections::VecDeque;
use std::sync::Arc;
use zerocopy::byteorder::little_endian::U16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

/// The HCI command opcodes the minimal controller acts on. Every other opcode
/// is answered with a success Command Complete so a host's bring-up sequence
/// never stalls on an unimplemented command.
mod opcode {
    /// Disconnect (OGF 0x01, OCF 0x0006).
    pub const DISCONNECT: u16 = 0x0406;
    /// Reset (OGF 0x03, OCF 0x0003).
    pub const RESET: u16 = 0x0C03;
    /// Read BD_ADDR (OGF 0x04, OCF 0x0009).
    pub const READ_BD_ADDR: u16 = 0x1009;
    /// Read RSSI (OGF 0x05, OCF 0x0005).
    pub const READ_RSSI: u16 = 0x1405;
    /// LE Set Advertising Parameters (OGF 0x08, OCF 0x0006).
    pub const LE_SET_ADV_PARAMS: u16 = 0x2006;
    /// LE Set Advertising Data (OGF 0x08, OCF 0x0008).
    pub const LE_SET_ADV_DATA: u16 = 0x2008;
    /// LE Set Advertising Enable (OGF 0x08, OCF 0x000A).
    pub const LE_SET_ADV_ENABLE: u16 = 0x200A;
    /// LE Set Scan Enable (OGF 0x08, OCF 0x000C).
    pub const LE_SET_SCAN_ENABLE: u16 = 0x200C;
    /// LE Create Connection (OGF 0x08, OCF 0x000D).
    pub const LE_CREATE_CONNECTION: u16 = 0x200D;
    /// LE Create Connection Cancel (OGF 0x08, OCF 0x000E).
    pub const LE_CREATE_CONNECTION_CANCEL: u16 = 0x200E;
    /// LE CS Security Enable (OGF 0x08, OCF 0x008C).
    pub const LE_CS_SECURITY_ENABLE: u16 = 0x208C;
    /// LE CS Create Config (OGF 0x08, OCF 0x0090).
    pub const LE_CS_CREATE_CONFIG: u16 = 0x2090;
    /// LE CS Remove Config (OGF 0x08, OCF 0x0091).
    pub const LE_CS_REMOVE_CONFIG: u16 = 0x2091;
    /// LE CS Set Procedure Parameters (OGF 0x08, OCF 0x0093).
    pub const LE_CS_SET_PROCEDURE_PARAMETERS: u16 = 0x2093;
    /// LE CS Procedure Enable (OGF 0x08, OCF 0x0094).
    pub const LE_CS_PROCEDURE_ENABLE: u16 = 0x2094;
}

/// HCI event codes the controller generates.
mod event {
    /// Disconnection Complete event (0x05).
    pub const DISCONNECTION_COMPLETE: u8 = 0x05;
    /// Command Complete event (0x0E).
    pub const COMMAND_COMPLETE: u8 = 0x0E;
    /// Command Status event (0x0F).
    pub const COMMAND_STATUS: u8 = 0x0F;
    /// LE Meta event (0x3E).
    pub const LE_META: u8 = 0x3E;
    /// LE Connection Complete subevent (0x01).
    pub const LE_CONNECTION_COMPLETE: u8 = 0x01;
    /// LE Advertising Report subevent (0x02).
    pub const LE_ADVERTISING_REPORT: u8 = 0x02;
    /// LE CS Security Enable Complete subevent (0x2E).
    pub const LE_CS_SECURITY_ENABLE_COMPLETE: u8 = 0x2E;
    /// LE CS Config Complete subevent (0x2F).
    pub const LE_CS_CONFIG_COMPLETE: u8 = 0x2F;
    /// LE CS Procedure Enable Complete subevent (0x30).
    pub const LE_CS_PROCEDURE_ENABLE_COMPLETE: u8 = 0x30;
    /// LE CS Subevent Result subevent (0x31).
    pub const LE_CS_SUBEVENT_RESULT: u8 = 0x31;
}

/// Channel Sounding parameters this controller models.
///
/// Simble's radio reports one subevent per [`Link::tick`], carrying one
/// mode-2 (Phase-Based Ranging) step per tone. The tone plan is fixed rather
/// than driven by the host's channel map, because the *spacing* is what
/// bounds the measurement and a demo that let a page choose it could quietly
/// produce nonsense — see [`TONE_SPACING_CHANNELS`].
pub mod cs_plan {
    /// The first channel index a tone is placed on.
    pub const FIRST_TONE_CHANNEL: u8 = 0;

    /// Channel indices (1 MHz each) between adjacent tones.
    ///
    /// Phase-Based Ranging recovers distance from how fast phase rotates with
    /// frequency, so it can only see rotations smaller than half a turn
    /// between neighbouring tones. That caps the unambiguous range at
    /// `c / (4·Δf)`: 37.5 m at this 2 MHz spacing. Wider spacing measures
    /// more precisely over the same number of tones and wraps sooner — the
    /// central trade-off of the technique.
    pub const TONE_SPACING_CHANNELS: u8 = 2;

    /// Tones per subevent. Nineteen mode-2 steps plus the subevent header is
    /// 245 bytes, just inside an HCI event's 255-byte parameter budget.
    pub const TONES_PER_SUBEVENT: usize = 19;

    /// Antenna paths reported. One path plus the mandatory extension slot is
    /// the 1:1 antenna configuration every CS-capable radio supports.
    pub const NUM_ANTENNA_PATHS: u8 = 1;

    /// Step mode 2: Phase-Based Ranging, the mode that carries tone PCTs.
    pub const STEP_MODE_PBR: u8 = 2;

    /// "All results complete" for the procedure/subevent done status fields.
    pub const DONE_STATUS_COMPLETE: u8 = 0x00;

    /// Tone Quality Indicator: "tone quality is high" (Vol 4, Part E,
    /// Section 7.7.65.44).
    pub const TONE_QUALITY_HIGH: u8 = 0x00;

    /// The unambiguous range of this tone plan, in metres.
    pub fn unambiguous_range_m() -> f64 {
        crate::types::SPEED_OF_LIGHT_M_PER_S
            / (4.0
                * f64::from(TONE_SPACING_CHANNELS)
                * crate::controller::propagation::CHANNEL_SPACING_HZ)
    }

    /// The channel indices tones are placed on, low frequency first.
    pub fn tone_channels() -> Vec<u8> {
        (0..TONES_PER_SUBEVENT)
            .map(|i| FIRST_TONE_CHANNEL + TONE_SPACING_CHANNELS * i as u8)
            .collect()
    }
}

/// `STATUS_SUCCESS` (0x00) HCI status code.
const STATUS_SUCCESS: u8 = 0x00;
/// Disconnection reason: "Connection Terminated By Local Host" (0x16).
const REASON_LOCAL_HOST: u8 = 0x16;
/// Disconnection reason: "Remote User Terminated Connection" (0x13).
const REASON_REMOTE_USER: u8 = 0x13;
/// `INVALID_HCI_COMMAND_PARAMETERS` (0x12).
const STATUS_INVALID_PARAMETERS: u8 = 0x12;

// --- zero-copy HCI packet layouts ------------------------------------------

/// HCI command packet header: opcode then parameter-total-length (the byte
/// after the H4 type byte).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
struct CommandHeader {
    /// Command opcode (OGF/OCF), little-endian.
    opcode: U16,
    /// Length of the parameters that follow.
    parameter_length: u8,
}

/// The leading fixed fields of LE Create Connection, up to and including the
/// peer address — enough to learn who the host wants to connect to.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
struct LeCreateConnectionPrefix {
    /// LE scan interval.
    scan_interval: U16,
    /// LE scan window.
    scan_window: U16,
    /// Initiator filter policy.
    initiator_filter_policy: u8,
    /// Peer address type.
    peer_address_type: u8,
    /// Peer device address (little-endian on the wire).
    peer_address: [u8; 6],
}

/// HCI ACL data packet header (handle + flags, then payload length).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
struct AclHeader {
    /// Lower 12 bits connection handle; upper 4 bits PB/BC flags.
    handle_and_flags: U16,
    /// Payload length in this fragment.
    data_length: U16,
}

/// Command Complete event body: `num_hci_command_packets`, the opcode, then the
/// command's return parameters (status first).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct CommandCompleteHeader {
    /// Number of HCI command packets the host may now send (always 1 here).
    num_hci_command_packets: u8,
    /// Opcode of the completed command.
    opcode: U16,
}

/// Command Status event body.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct CommandStatusBody {
    /// Status of the command.
    status: u8,
    /// Number of HCI command packets the host may now send.
    num_hci_command_packets: u8,
    /// Opcode of the command whose status this reports.
    opcode: U16,
}

/// LE Connection Complete subevent body (fixed-size).
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct LeConnectionCompleteBody {
    /// LE Meta subevent code (0x01).
    subevent_code: u8,
    /// Connection status.
    status: u8,
    /// Assigned connection handle.
    connection_handle: U16,
    /// Local role: 0x00 central, 0x01 peripheral.
    role: u8,
    /// Peer address type (0x00 public).
    peer_address_type: u8,
    /// Peer device address (little-endian).
    peer_address: [u8; 6],
    /// Connection interval (units of 1.25 ms).
    connection_interval: U16,
    /// Peripheral latency (in connection events).
    peripheral_latency: U16,
    /// Supervision timeout (units of 10 ms).
    supervision_timeout: U16,
    /// Central clock accuracy.
    central_clock_accuracy: u8,
}

/// LE CS Procedure Enable Complete subevent body (Vol 4, Part E, Section
/// 7.7.65.48), after the subevent code.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct LeCsProcedureEnableCompleteBody {
    /// Status of the enable/disable request.
    status: u8,
    /// The connection the procedure runs on.
    connection_handle: U16,
    /// Which configuration was enabled.
    config_id: u8,
    /// 0x00 disabled, 0x01 enabled.
    state: u8,
    /// Antenna configuration index (0x00 = 1:1).
    tone_antenna_config_selection: u8,
    /// Transmit power the controller selected, in dBm.
    selected_tx_power: i8,
    /// Subevent length in microseconds (24-bit).
    subevent_len: [u8; 3],
    /// Subevents per CS event.
    subevents_per_event: u8,
    /// Time between subevents, in 625 µs slots.
    subevent_interval: U16,
    /// Time between CS events, in connection intervals.
    event_interval: U16,
    /// Time between procedures, in connection intervals.
    procedure_interval: U16,
    /// How many procedures will run (0 = until disabled).
    procedure_count: U16,
    /// Maximum procedure duration, in 625 µs slots.
    max_procedure_len: U16,
}

/// LE Advertising Report subevent header (one report), before the variable data
/// and trailing RSSI byte.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct LeAdvertisingReportHeader {
    /// LE Meta subevent code (0x02).
    subevent_code: u8,
    /// Number of reports (always 1 here).
    num_reports: u8,
    /// Advertising event type (ADV_IND etc.).
    event_type: u8,
    /// Advertiser address type.
    address_type: u8,
    /// Advertiser address (little-endian).
    address: [u8; 6],
    /// Length of the advertising data that follows.
    data_length: u8,
}

/// Disconnection Complete event body.
#[repr(C)]
#[derive(IntoBytes, Immutable)]
struct DisconnectionCompleteBody {
    /// Status of the disconnection.
    status: u8,
    /// Handle of the now-closed connection.
    connection_handle: U16,
    /// Reason code.
    reason: u8,
}

// --- controller + link -----------------------------------------------------

/// A live connection as seen by one controller: the shared handle and the index
/// of the peer controller within the [`Link`].
#[derive(Clone, Copy)]
struct Connection {
    handle: u16,
    peer: usize,
}

/// A Channel Sounding configuration a host has created on one connection, and
/// whether it is currently producing measurements.
#[derive(Clone, Copy)]
struct CsSession {
    /// The connection the procedure runs on.
    handle: u16,
    /// The configuration identifier the host chose.
    config_id: u8,
    /// 0x00 initiator, 0x01 reflector. Only the initiator's session drives
    /// the measurement; the reflector's exists so its host is told about the
    /// tones it received.
    role: u8,
    /// Whether LE CS Procedure Enable has been accepted.
    enabled: bool,
    /// Increments once per completed procedure, and is how a host pairs its
    /// own subevent results with the ones the peer sends over the Ranging
    /// Service.
    procedure_counter: u16,
}

/// One device's simulated controller: it owns the controller side of an
/// [`HciChannel`], tracks the minimal advertising/scanning/connection state a
/// host drives over HCI, and buffers the events it will hand back.
struct SimController {
    address: Address,
    channel: Arc<HciChannel>,
    advertising: bool,
    adv_data: Vec<u8>,
    adv_event_type: u8,
    own_adv_addr_type: u8,
    scanning: bool,
    pending_connect: Option<Address>,
    connections: Vec<Connection>,
    /// Where this device stands on the simulated floor plan, in metres. This
    /// is the ground truth every RSSI and every Channel Sounding tone is
    /// derived from, and no host ever sees it.
    position: Position,
    /// The Channel Sounding configuration on this device, if a host created
    /// one.
    cs: Option<CsSession>,
    /// H4 packets to deliver to this device's host at the end of the tick.
    outbox: VecDeque<Vec<u8>>,
}

impl SimController {
    fn new(address: Address, channel: Arc<HciChannel>) -> Self {
        Self {
            address,
            channel,
            advertising: false,
            adv_data: Vec::new(),
            adv_event_type: 0x00,
            own_adv_addr_type: 0x00,
            scanning: false,
            pending_connect: None,
            connections: Vec::new(),
            position: Position::default(),
            cs: None,
            outbox: VecDeque::new(),
        }
    }

    /// Reset to power-on defaults (HCI Reset).
    ///
    /// Position survives: it is a property of the simulated room, not of the
    /// controller's register file, and a host that resets its chip does not
    /// teleport.
    fn reset(&mut self) {
        self.advertising = false;
        self.adv_data.clear();
        self.scanning = false;
        self.pending_connect = None;
        self.connections.clear();
        self.cs = None;
    }
}

/// Cross-controller effects collected while handling one controller's commands,
/// applied in a later phase that can touch two controllers at once.
enum Action {
    Disconnect {
        from: usize,
        handle: u16,
    },
    Acl {
        from: usize,
        handle: u16,
        data: Vec<u8>,
    },
    /// An isochronous SDU (LE Audio media plane), routed like ACL data on
    /// the same connection handle — Simble carries ISO over the established
    /// connection rather than modeling CIG/CIS setup.
    Iso {
        from: usize,
        handle: u16,
        data: Vec<u8>,
    },
    /// LE CS Create Config with Create_Context = 0x01, which the spec defines
    /// as "write the configuration in both the local and the remote
    /// controller" (Vol 4, Part E, Section 7.8.137). The peer's host is told
    /// with its own LE CS Config Complete, and gets the mirrored role — this
    /// is how a reflector learns it is in a procedure at all.
    CsConfigure {
        from: usize,
        handle: u16,
        config_id: u8,
        role: u8,
        propagate_to_peer: bool,
    },
    /// LE CS Procedure Enable. Enabling on the initiator enables the
    /// reflector's matching configuration too, since a procedure has two
    /// ends by definition.
    CsEnable {
        from: usize,
        handle: u16,
        config_id: u8,
        enable: bool,
    },
}

/// The shared medium. Holds every `SimController` on the "air" and, on each
/// [`tick`](Self::tick), drains their hosts' HCI, routes advertising and data
/// between them, and delivers the resulting events — the same role as Bumble's
/// `LocalLink`, sized for an in-process scene of any number of devices.
#[derive(Default)]
pub struct Link {
    controllers: Vec<SimController>,
    next_handle: u16,
    /// How the medium attenuates: one model for the whole scene, since it
    /// describes the *room* rather than any device.
    path_loss: PathLossModel,
    /// Shadowing and phase noise. Seeded, so a test that asserts on a noisy
    /// measurement gets the same noise every run.
    rng: Rng,
}

impl Link {
    /// Creates an empty medium with no devices.
    pub fn new() -> Self {
        Self {
            controllers: Vec::new(),
            next_handle: 0x0001,
            path_loss: PathLossModel::default(),
            rng: Rng::default(),
        }
    }

    /// The propagation model advertising reports and Channel Sounding tones
    /// are generated through.
    pub fn path_loss(&self) -> PathLossModel {
        self.path_loss
    }

    /// Replaces the propagation model — transmit power, reference loss,
    /// path-loss exponent, shadowing.
    pub fn set_path_loss(&mut self, model: PathLossModel) {
        self.path_loss = model;
    }

    /// Reseeds the medium's noise. Same seed, same shadowing and phase noise.
    pub fn set_noise_seed(&mut self, seed: u64) {
        self.rng = Rng::new(seed);
    }

    /// Moves the device at `address` to `position`. Returns false if no such
    /// device is on the medium.
    pub fn set_position(&mut self, address: Address, position: Position) -> bool {
        match self.controllers.iter_mut().find(|c| c.address == address) {
            Some(controller) => {
                controller.position = position;
                true
            }
            None => false,
        }
    }

    /// Where the device at `address` stands.
    pub fn position(&self, address: Address) -> Option<Position> {
        self.controllers
            .iter()
            .find(|c| c.address == address)
            .map(|c| c.position)
    }

    /// The true separation of two devices in metres — the ground truth a
    /// ranging demo compares its estimate against, and which no host on the
    /// medium can see.
    pub fn distance_between(&self, a: Address, b: Address) -> Option<f64> {
        Some(self.position(a)?.distance_to(self.position(b)?))
    }

    /// Adds a device with `address` and returns the host side of its HCI
    /// channel: send commands and ACL to it, and poll it for events, exactly as
    /// if it were a real controller. The returned handle and the [`Link`] share
    /// the channel; [`tick`](Self::tick) services it.
    pub fn add_device(&mut self, address: Address) -> Arc<HciChannel> {
        let channel = Arc::new(HciChannel::new());
        self.controllers
            .push(SimController::new(address, Arc::clone(&channel)));
        channel
    }

    /// The number of devices on the medium.
    pub fn device_count(&self) -> usize {
        self.controllers.len()
    }

    fn alloc_handle(&mut self) -> u16 {
        let h = self.next_handle;
        self.next_handle = self.next_handle.wrapping_add(1).max(0x0001);
        h
    }

    /// Advances the simulation by one step: handle every host's queued HCI,
    /// route advertising to scanners and data between connected peers, and hand
    /// the resulting events back to each host. Non-blocking; call it in a loop
    /// or on a timer to keep the scene live.
    pub fn tick(&mut self) {
        // Phase A: drain and handle each host's queued commands / ACL.
        let mut actions: Vec<Action> = Vec::new();
        for i in 0..self.controllers.len() {
            while let Some(pkt) = self.controllers[i].channel.poll_host_packet() {
                self.handle_packet(i, &pkt, &mut actions);
            }
        }

        // Phase B: deliver an advertising report from each advertiser to every
        // other scanning device, stamped with the RSSI that advertiser's
        // signal actually arrives with at that scanner's position.
        let advertisers: Vec<(Address, u8, u8, Vec<u8>, Position)> = self
            .controllers
            .iter()
            .filter(|c| c.advertising)
            .map(|c| {
                (
                    c.address,
                    c.adv_event_type,
                    c.own_adv_addr_type,
                    c.adv_data.clone(),
                    c.position,
                )
            })
            .collect();
        let path_loss = self.path_loss;
        for i in 0..self.controllers.len() {
            if !self.controllers[i].scanning {
                continue;
            }
            let scanner_position = self.controllers[i].position;
            let scanner_address = self.controllers[i].address;
            for (addr, event_type, addr_type, data, advertiser_position) in &advertisers {
                if *addr == scanner_address {
                    continue; // a device never hears its own advertisement
                }
                // Shadowing is redrawn per report, which is exactly why an
                // RSSI reading jitters while the devices sit still.
                let distance = advertiser_position.distance_to(scanner_position);
                let shadowing = self.rng.normal_scaled(path_loss.shadowing_sigma_db);
                let rssi = quantize_rssi(path_loss.rssi_dbm(distance, shadowing));
                self.controllers[i].outbox.push_back(le_advertising_report(
                    *event_type,
                    *addr_type,
                    *addr,
                    data,
                    rssi,
                ));
            }
        }

        // Phase C: pending connections — a scanner that asked to connect to an
        // advertiser's address is joined to it once that advertiser is on air.
        for i in 0..self.controllers.len() {
            if let Some(target) = self.controllers[i].pending_connect
                && let Some(a) = self
                    .controllers
                    .iter()
                    .position(|c| c.address == target && c.advertising)
            {
                self.establish_connection(i, a);
                self.controllers[i].pending_connect = None;
            }
        }

        // Phase D: apply disconnects and ACL routing (touch two controllers).
        for action in actions {
            match action {
                Action::Disconnect { from, handle } => self.route_disconnect(from, handle),
                Action::Acl { from, handle, data } => self.route_acl(from, handle, &data),
                Action::Iso { from, handle, data } => self.route_iso(from, handle, &data),
                Action::CsConfigure {
                    from,
                    handle,
                    config_id,
                    role,
                    propagate_to_peer,
                } => self.route_cs_configure(from, handle, config_id, role, propagate_to_peer),
                Action::CsEnable {
                    from,
                    handle,
                    config_id,
                    enable,
                } => self.route_cs_enable(from, handle, config_id, enable),
            }
        }

        // Phase D2: run one Channel Sounding procedure per enabled session.
        self.tick_channel_sounding();

        // Phase E: flush every outbox to its host.
        for c in &mut self.controllers {
            while let Some(pkt) = c.outbox.pop_front() {
                let _ = c.channel.receive_from_controller(pkt);
            }
        }
    }

    /// Handle one H4 packet a host sent to controller `i`.
    fn handle_packet(&mut self, i: usize, pkt: &[u8], actions: &mut Vec<Action>) {
        match pkt.first().copied() {
            Some(h4_type::HCI_COMMAND) => {
                if let Ok((hdr, params)) = Ref::<_, CommandHeader>::from_prefix(&pkt[1..]) {
                    self.handle_command(i, hdr.opcode.get(), params, actions);
                }
            }
            Some(h4_type::HCI_ACL_DATA) => {
                if let Ok((hdr, _)) = Ref::<_, AclHeader>::from_prefix(&pkt[1..]) {
                    actions.push(Action::Acl {
                        from: i,
                        handle: hdr.handle_and_flags.get() & 0x0FFF,
                        data: pkt[1..].to_vec(), // handle+flags+len+payload, forwarded verbatim
                    });
                }
            }
            Some(h4_type::HCI_ISO_DATA) => {
                if let Ok((hdr, _)) = Ref::<_, AclHeader>::from_prefix(&pkt[1..]) {
                    // An ISO header shares the ACL header's shape (handle +
                    // flags, then a length), so the same view reads it.
                    actions.push(Action::Iso {
                        from: i,
                        handle: hdr.handle_and_flags.get() & 0x0FFF,
                        data: pkt[1..].to_vec(),
                    });
                }
            }
            _ => {}
        }
    }

    /// Handle one parsed HCI command from controller `i`'s host.
    fn handle_command(&mut self, i: usize, opcode: u16, params: &[u8], actions: &mut Vec<Action>) {
        // Read RSSI needs the *peer's* position, so it is answered before the
        // borrow of this one controller that the rest of the match works
        // under.
        if opcode == opcode::READ_RSSI {
            self.handle_read_rssi(i, le_u16(params, 0));
            return;
        }
        let c = &mut self.controllers[i];
        match opcode {
            opcode::RESET => {
                c.reset();
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::READ_BD_ADDR => {
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&addr_le(c.address));
                c.outbox.push_back(command_complete(opcode, &ret));
            }
            opcode::LE_SET_ADV_PARAMS => {
                // interval_min(2) interval_max(2) adv_type(1) own_addr_type(1) …
                if params.len() >= 6 {
                    c.adv_event_type = params[4];
                    c.own_adv_addr_type = params[5];
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_ADV_DATA => {
                // length(1) data(31)
                if let Some(&len) = params.first() {
                    let len = (len as usize).min(params.len().saturating_sub(1));
                    c.adv_data = params[1..1 + len].to_vec();
                }
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_ADV_ENABLE => {
                c.advertising = params.first().copied() == Some(0x01);
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_SET_SCAN_ENABLE => {
                c.scanning = params.first().copied() == Some(0x01);
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_CREATE_CONNECTION => {
                if let Ok((prefix, _)) = Ref::<_, LeCreateConnectionPrefix>::from_prefix(params) {
                    let mut be = prefix.peer_address;
                    be.reverse(); // wire is little-endian; Address is big-endian
                    c.pending_connect = Some(Address::from_be_bytes(be));
                }
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
            }
            opcode::LE_CREATE_CONNECTION_CANCEL => {
                c.pending_connect = None;
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
            opcode::LE_CS_SECURITY_ENABLE => {
                // The CS security start procedure derives the keys that
                // randomize tone phases against an attacker. Simble models no
                // CS encryption, so this succeeds immediately — a host that
                // waits for the completion event gets one, and a host that
                // skips the step is not stopped, which is the difference that
                // matters for a demo. Vol 4, Part E, Section 7.8.133.
                let handle = le_u16(params, 0);
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                let mut body = vec![event::LE_CS_SECURITY_ENABLE_COMPLETE, STATUS_SUCCESS];
                body.extend_from_slice(&handle.to_le_bytes());
                c.outbox.push_back(event_packet(event::LE_META, &body));
            }
            opcode::LE_CS_CREATE_CONFIG => {
                // connection_handle(2) config_id(1) create_context(1)
                // main_mode_type(1) sub_mode_type(1) … role at offset 10.
                if params.len() >= 11 {
                    let handle = le_u16(params, 0);
                    let config_id = params[2];
                    let propagate_to_peer = params[3] == 0x01;
                    let role = params[10];
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    actions.push(Action::CsConfigure {
                        from: i,
                        handle,
                        config_id,
                        role,
                        propagate_to_peer,
                    });
                } else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                }
            }
            opcode::LE_CS_SET_PROCEDURE_PARAMETERS => {
                // Timing and tone-antenna choices Simble does not model: it
                // reports one subevent per tick regardless. Answered with the
                // Command Complete the spec specifies (status, then the
                // connection handle) so a host's sequencing still works.
                let handle = le_u16(params, 0);
                let mut ret = vec![STATUS_SUCCESS];
                ret.extend_from_slice(&handle.to_le_bytes());
                c.outbox.push_back(command_complete(opcode, &ret));
            }
            opcode::LE_CS_PROCEDURE_ENABLE => {
                if params.len() >= 4 {
                    let handle = le_u16(params, 0);
                    let config_id = params[2];
                    let enable = params[3] == 0x01;
                    c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                    actions.push(Action::CsEnable {
                        from: i,
                        handle,
                        config_id,
                        enable,
                    });
                } else {
                    c.outbox
                        .push_back(command_status(STATUS_INVALID_PARAMETERS, opcode));
                }
            }
            opcode::LE_CS_REMOVE_CONFIG => {
                c.cs = None;
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
            }
            opcode::DISCONNECT => {
                let handle = params
                    .get(0..2)
                    .map(|b| u16::from_le_bytes([b[0], b[1]]))
                    .unwrap_or(0);
                c.outbox.push_back(command_status(STATUS_SUCCESS, opcode));
                actions.push(Action::Disconnect { from: i, handle });
            }
            // Set Event Mask, LE Set Event Mask, scan/adv params, scan-response
            // data, and anything else: accept with a success Command Complete so
            // the host's bring-up never stalls on an unimplemented command.
            _ => {
                c.outbox
                    .push_back(command_complete(opcode, &[STATUS_SUCCESS]));
            }
        }
    }

    /// Join controller `central` to advertiser `peripheral`: allocate a shared
    /// handle, record the connection on both, stop the advertiser, and emit an
    /// LE Connection Complete to each host with the correct role.
    fn establish_connection(&mut self, central: usize, peripheral: usize) {
        let handle = self.alloc_handle();
        let central_addr = self.controllers[central].address;
        let peripheral_addr = self.controllers[peripheral].address;

        self.controllers[central].connections.push(Connection {
            handle,
            peer: peripheral,
        });
        self.controllers[peripheral].connections.push(Connection {
            handle,
            peer: central,
        });
        self.controllers[peripheral].advertising = false;

        // Role 0x00 = Central, 0x01 = Peripheral.
        self.controllers[central]
            .outbox
            .push_back(le_connection_complete(handle, 0x00, peripheral_addr));
        self.controllers[peripheral]
            .outbox
            .push_back(le_connection_complete(handle, 0x01, central_addr));
    }

    /// Tear down the connection on `handle` for controller `from`, notifying
    /// both ends with a Disconnection Complete.
    fn route_disconnect(&mut self, from: usize, handle: u16) {
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        self.controllers[from]
            .connections
            .retain(|c| c.handle != handle);
        self.controllers[peer]
            .connections
            .retain(|c| c.handle != handle);
        self.controllers[from]
            .outbox
            .push_back(disconnection_complete(handle, REASON_LOCAL_HOST));
        self.controllers[peer]
            .outbox
            .push_back(disconnection_complete(handle, REASON_REMOTE_USER));
    }

    /// Forward an ACL packet from `from` to the peer on `handle`.
    fn route_acl(&mut self, from: usize, handle: u16, data: &[u8]) {
        if let Some(peer) = self.peer_of(from, handle) {
            let mut pkt = vec![h4_type::HCI_ACL_DATA];
            pkt.extend_from_slice(data);
            self.controllers[peer].outbox.push_back(pkt);
        }
    }

    /// Delivers an isochronous SDU to the connection's peer — the media
    /// plane's counterpart to [`Self::route_acl`].
    fn route_iso(&mut self, from: usize, handle: u16, data: &[u8]) {
        if let Some(peer) = self.peer_of(from, handle) {
            let mut pkt = vec![h4_type::HCI_ISO_DATA];
            pkt.extend_from_slice(data);
            self.controllers[peer].outbox.push_back(pkt);
        }
    }

    /// Answers HCI Read RSSI (Vol 4, Part E, Section 7.5.4) for one of this
    /// controller's connections.
    ///
    /// This is how a *connected* device measures its peer's signal. A
    /// controller stops advertising once a connection is up, so there are no
    /// more advertising reports to read an RSSI out of — and a proximity
    /// feature that stopped working the moment you connected would be a
    /// strange thing to ship. Shadowing is redrawn per call, exactly as it is
    /// for an advertising report: a real Read RSSI moves between calls too.
    fn handle_read_rssi(&mut self, i: usize, handle: u16) {
        let rssi = match self.peer_of(i, handle) {
            Some(peer) => {
                let distance = self.controllers[i]
                    .position
                    .distance_to(self.controllers[peer].position);
                let shadowing = self.rng.normal_scaled(self.path_loss.shadowing_sigma_db);
                quantize_rssi(self.path_loss.rssi_dbm(distance, shadowing))
            }
            // 127 is HCI's "RSSI is not available", which is the honest
            // answer for a handle that is not connected.
            None => 127,
        };
        let mut ret = vec![STATUS_SUCCESS];
        ret.extend_from_slice(&handle.to_le_bytes());
        ret.push(rssi as u8);
        self.controllers[i]
            .outbox
            .push_back(command_complete(opcode::READ_RSSI, &ret));
    }

    /// Records a Channel Sounding configuration on `from`, and — when the
    /// host asked for it — the mirrored configuration on the peer, telling
    /// both hosts with an LE CS Config Complete.
    fn route_cs_configure(
        &mut self,
        from: usize,
        handle: u16,
        config_id: u8,
        role: u8,
        propagate_to_peer: bool,
    ) {
        self.controllers[from].cs = Some(CsSession {
            handle,
            config_id,
            role,
            enabled: false,
            procedure_counter: 0,
        });
        self.controllers[from]
            .outbox
            .push_back(cs_config_complete(handle, config_id, role));

        if !propagate_to_peer {
            return;
        }
        let Some(peer) = self.peer_of(from, handle) else {
            return;
        };
        // The peer takes the opposite role: one end transmits the tone the
        // other measures, and both measure.
        let peer_role = u8::from(role == 0x00);
        self.controllers[peer].cs = Some(CsSession {
            handle,
            config_id,
            role: peer_role,
            enabled: false,
            procedure_counter: 0,
        });
        self.controllers[peer]
            .outbox
            .push_back(cs_config_complete(handle, config_id, peer_role));
    }

    /// Enables or disables the configuration on both ends of `handle`.
    fn route_cs_enable(&mut self, from: usize, handle: u16, config_id: u8, enable: bool) {
        let peer = self.peer_of(from, handle);
        for index in [Some(from), peer].into_iter().flatten() {
            let Some(session) = self.controllers[index].cs.as_mut() else {
                continue;
            };
            if session.handle != handle || session.config_id != config_id {
                continue;
            }
            session.enabled = enable;
            let tx_power = self.path_loss.tx_power_dbm;
            self.controllers[index]
                .outbox
                .push_back(cs_procedure_enable_complete(
                    handle, config_id, enable, tx_power,
                ));
        }
    }

    /// Runs one Channel Sounding procedure per enabled initiator session and
    /// reports the tones **both** ends measured.
    ///
    /// This is where the physics happens, and where the honesty of the whole
    /// ranging demo lives. Each end measures the same propagation phase
    /// `2π·f·d/c`, but offset by its own local oscillator's phase, which it
    /// does not know:
    ///
    /// ```text
    ///   initiator sees:  φ_prop(f) + Δθ(f) + noise
    ///   reflector sees:  φ_prop(f) − Δθ(f) + noise
    /// ```
    ///
    /// `Δθ(f)` is the difference between the two local oscillators' phases,
    /// and it is redrawn **per tone**, not once per procedure: each radio
    /// re-locks its synthesizer on every hop and comes back with an arbitrary
    /// phase. So one end's measurements are, on their own, uniform noise —
    /// no amount of fitting recovers a distance from them. Their **sum** is
    /// `2·φ_prop(f)`, with `Δθ` gone. That per-tone cancellation is the whole
    /// reason Channel Sounding needs the Ranging Service: the reflector's
    /// tones have to reach the initiator's *host* before any distance exists.
    fn tick_channel_sounding(&mut self) {
        let channels = cs_plan::tone_channels();
        for i in 0..self.controllers.len() {
            let Some(session) = self.controllers[i].cs else {
                continue;
            };
            // Only the initiator's session drives a procedure; the reflector
            // is reported to as a side effect, never twice.
            if !session.enabled || session.role != 0x00 {
                continue;
            }
            let Some(peer) = self.peer_of(i, session.handle) else {
                continue;
            };
            if !self.controllers[peer].cs.is_some_and(|s| s.enabled) {
                continue;
            }

            let distance = self.controllers[i]
                .position
                .distance_to(self.controllers[peer].position);
            let sigma = phase_noise_sigma_rad(self.path_loss.snr_linear(distance));
            let reference_power = quantize_rssi(self.path_loss.rssi_dbm(distance, 0.0));

            let mut initiator_steps = Vec::with_capacity(channels.len());
            let mut reflector_steps = Vec::with_capacity(channels.len());
            for &channel in &channels {
                let phase = propagation_phase_rad(distance, channel_frequency_hz(channel));
                // A fresh oscillator-phase difference on every hop, applied
                // to the two ends with opposite sign so it cancels in the sum.
                let lo_offset = self.rng.uniform_phase();
                let initiator_phase = wrap_phase(phase + lo_offset + self.rng.normal_scaled(sigma));
                let reflector_phase = wrap_phase(phase - lo_offset + self.rng.normal_scaled(sigma));
                initiator_steps.push(pbr_step(channel, initiator_phase));
                reflector_steps.push(pbr_step(channel, reflector_phase));
            }

            let counter = session.procedure_counter;
            for (index, steps) in [(i, initiator_steps), (peer, reflector_steps)] {
                let config_id = self.controllers[index]
                    .cs
                    .map_or(session.config_id, |s| s.config_id);
                self.controllers[index]
                    .outbox
                    .push_back(cs_subevent_result(
                        session.handle,
                        config_id,
                        counter,
                        reference_power,
                        &steps,
                    ));
                if let Some(peer_session) = self.controllers[index].cs.as_mut() {
                    peer_session.procedure_counter = counter.wrapping_add(1);
                }
            }
        }
    }

    /// The peer controller index for `from`'s connection on `handle`, if any.
    fn peer_of(&self, from: usize, handle: u16) -> Option<usize> {
        self.controllers[from]
            .connections
            .iter()
            .find(|c| c.handle == handle)
            .map(|c| c.peer)
    }
}

/// A Bluetooth address as it appears on the wire in HCI (little-endian, LSB
/// first) — [`Address`] stores the big-endian display order.
fn addr_le(address: Address) -> [u8; 6] {
    let mut b = address.to_be_bytes();
    b.reverse();
    b
}

/// Wrap an event body as an H4 event packet: `0x04, code, len, body…`.
fn event_packet(code: u8, body: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(3 + body.len());
    p.push(h4_type::HCI_EVENT);
    p.push(code);
    p.push(body.len() as u8);
    p.extend_from_slice(body);
    p
}

/// Command Complete for `opcode` carrying `return_params` (status first).
fn command_complete(opcode: u16, return_params: &[u8]) -> Vec<u8> {
    let hdr = CommandCompleteHeader {
        num_hci_command_packets: 1,
        opcode: U16::new(opcode),
    };
    let mut body = hdr.as_bytes().to_vec();
    body.extend_from_slice(return_params);
    event_packet(event::COMMAND_COMPLETE, &body)
}

/// Command Status for `opcode` with `status`.
fn command_status(status: u8, opcode: u16) -> Vec<u8> {
    let body = CommandStatusBody {
        status,
        num_hci_command_packets: 1,
        opcode: U16::new(opcode),
    };
    event_packet(event::COMMAND_STATUS, body.as_bytes())
}

/// LE Connection Complete subevent for the given handle, role, and peer.
fn le_connection_complete(handle: u16, role: u8, peer: Address) -> Vec<u8> {
    let body = LeConnectionCompleteBody {
        subevent_code: event::LE_CONNECTION_COMPLETE,
        status: STATUS_SUCCESS,
        connection_handle: U16::new(handle),
        role,
        peer_address_type: 0x00, // public
        peer_address: addr_le(peer),
        connection_interval: U16::new(0x0018), // 30 ms
        peripheral_latency: U16::new(0),
        supervision_timeout: U16::new(0x002A),
        central_clock_accuracy: 0x00,
    };
    event_packet(event::LE_META, body.as_bytes())
}

/// LE Advertising Report subevent carrying one report for `addr`, stamped
/// with the RSSI the scanner measured.
fn le_advertising_report(
    event_type: u8,
    addr_type: u8,
    addr: Address,
    data: &[u8],
    rssi_dbm: i8,
) -> Vec<u8> {
    let hdr = LeAdvertisingReportHeader {
        subevent_code: event::LE_ADVERTISING_REPORT,
        num_reports: 1,
        event_type,
        address_type: addr_type,
        address: addr_le(addr),
        data_length: data.len() as u8,
    };
    let mut body = hdr.as_bytes().to_vec();
    body.extend_from_slice(data);
    body.push(rssi_dbm as u8);
    event_packet(event::LE_META, &body)
}

/// Rounds a received power to the signed whole dBm an HCI report carries.
///
/// A controller reports RSSI as one byte, so a demo cannot show more
/// precision than that however finely the model computes it. Clamped to the
/// range HCI defines as meaningful (Vol 4, Part E, Section 7.7.65.2); 127
/// means "not available" and is never produced here.
fn quantize_rssi(dbm: f64) -> i8 {
    dbm.round().clamp(-127.0, 20.0) as i8
}

/// Reads a little-endian u16 at `offset`, or 0 if the parameters are short.
fn le_u16(params: &[u8], offset: usize) -> u16 {
    params
        .get(offset..offset + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .unwrap_or(0)
}

/// LE CS Config Complete (Vol 4, Part E, Section 7.7.65.47) for a
/// configuration that was just created.
fn cs_config_complete(handle: u16, config_id: u8, role: u8) -> Vec<u8> {
    let body = crate::packets::hci::LeCsConfigCompleteEvent {
        status: STATUS_SUCCESS,
        connection_handle: U16::new(handle),
        config_id,
        action: 0x01, // the configuration was created (0x00 would be removed)
        main_mode_type: cs_plan::STEP_MODE_PBR,
        sub_mode_type: 0xFF, // unused
        min_main_mode_steps: cs_plan::TONES_PER_SUBEVENT as u8,
        max_main_mode_steps: cs_plan::TONES_PER_SUBEVENT as u8,
        main_mode_repetition: 0,
        mode_0_steps: 0,
        role,
        rtt_type: 0x00,
        cs_sync_phy: 0x01, // LE 1M
        channel_map: cs_channel_map(),
        channel_map_repetition: 1,
        channel_selection_type: 0x00,
        ch3c_shape: 0x00,
        ch3c_jump: 0x00,
        companion_signal_status: 0x00,
    };
    let mut packet_body = vec![event::LE_CS_CONFIG_COMPLETE];
    packet_body.extend_from_slice(body.as_bytes());
    event_packet(event::LE_META, &packet_body)
}

/// The 79-bit channel map, as a bitmask over channel indices, marking the
/// channels [`cs_plan::tone_channels`] places tones on.
fn cs_channel_map() -> [u8; 10] {
    let mut map = [0u8; 10];
    for channel in cs_plan::tone_channels() {
        map[channel as usize / 8] |= 1 << (channel % 8);
    }
    map
}

/// LE CS Procedure Enable Complete (Vol 4, Part E, Section 7.7.65.48).
fn cs_procedure_enable_complete(
    handle: u16,
    config_id: u8,
    enabled: bool,
    tx_power_dbm: f64,
) -> Vec<u8> {
    let body = LeCsProcedureEnableCompleteBody {
        status: STATUS_SUCCESS,
        connection_handle: U16::new(handle),
        config_id,
        state: u8::from(enabled),
        tone_antenna_config_selection: 0x00, // 1:1
        selected_tx_power: quantize_rssi(tx_power_dbm),
        subevent_len: [0x40, 0x0D, 0x00], // 3392 µs
        subevents_per_event: 1,
        subevent_interval: U16::new(0),
        event_interval: U16::new(1),
        procedure_interval: U16::new(1),
        procedure_count: U16::new(0), // repeat until disabled
        max_procedure_len: U16::new(0x0040),
    };
    let mut packet_body = vec![event::LE_CS_PROCEDURE_ENABLE_COMPLETE];
    packet_body.extend_from_slice(body.as_bytes());
    event_packet(event::LE_META, &packet_body)
}

/// One mode-2 (Phase-Based Ranging) step, as it appears in a subevent
/// result: step mode, step channel, step data length, then the step data.
///
/// Mode-2 step data is an antenna permutation index followed by one
/// `Tone_PCT` + `Tone_Quality_Indicator` pair per antenna path *plus one*
/// for the extension slot (Vol 4, Part E, Section 7.7.65.44). The PCT is a
/// 24-bit value: 12-bit signed I in the low bits, 12-bit signed Q above.
fn pbr_step(channel: u8, phase_rad: f64) -> Vec<u8> {
    let pct = pct_bytes(phase_rad);
    let mut step_data = vec![0x00]; // antenna permutation index
    for _ in 0..=cs_plan::NUM_ANTENNA_PATHS {
        step_data.extend_from_slice(&pct);
        step_data.push(cs_plan::TONE_QUALITY_HIGH);
    }
    let mut step = vec![cs_plan::STEP_MODE_PBR, channel, step_data.len() as u8];
    step.extend_from_slice(&step_data);
    step
}

/// Packs a unit-amplitude phasor at `phase_rad` into the three bytes of a
/// Phase Correction Term: I in bits 0–11, Q in bits 12–23, both 12-bit
/// two's-complement.
fn pct_bytes(phase_rad: f64) -> [u8; 3] {
    /// The largest magnitude a 12-bit two's-complement sample can carry.
    const FULL_SCALE: f64 = 2047.0;
    let quantize = |v: f64| ((v * FULL_SCALE).round() as i32).clamp(-2048, 2047) as u32 & 0x0FFF;
    let packed = quantize(phase_rad.cos()) | (quantize(phase_rad.sin()) << 12);
    [packed as u8, (packed >> 8) as u8, (packed >> 16) as u8]
}

/// LE CS Subevent Result (Vol 4, Part E, Section 7.7.65.49) carrying `steps`.
fn cs_subevent_result(
    handle: u16,
    config_id: u8,
    procedure_counter: u16,
    reference_power_level: i8,
    steps: &[Vec<u8>],
) -> Vec<u8> {
    let header = crate::packets::hci::LeCsSubeventResultHeader {
        connection_handle: U16::new(handle),
        config_id,
        start_acl_conn_event: U16::new(procedure_counter),
        procedure_counter: U16::new(procedure_counter),
        // No crystal-offset compensation is modelled; 0xFFFF is the spec's
        // "not available".
        frequency_compensation: U16::new(0xFFFF),
        reference_power_level,
        procedure_done_status: cs_plan::DONE_STATUS_COMPLETE,
        subevent_done_status: cs_plan::DONE_STATUS_COMPLETE,
        procedure_abort_reason: 0x00,
        subevent_abort_reason: 0x00,
        num_antenna_paths: cs_plan::NUM_ANTENNA_PATHS,
        num_steps_reported: steps.len() as u8,
    };
    let mut body = vec![event::LE_CS_SUBEVENT_RESULT];
    body.extend_from_slice(header.as_bytes());
    for step in steps {
        body.extend_from_slice(step);
    }
    debug_assert!(
        body.len() <= 255,
        "an HCI event carries at most 255 parameter bytes; got {}",
        body.len()
    );
    event_packet(event::LE_META, &body)
}

/// Disconnection Complete event for `handle` with the given reason.
fn disconnection_complete(handle: u16, reason: u8) -> Vec<u8> {
    let body = DisconnectionCompleteBody {
        status: STATUS_SUCCESS,
        connection_handle: U16::new(handle),
        reason,
    };
    event_packet(event::DISCONNECTION_COMPLETE, body.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> Address {
        s.parse().unwrap()
    }

    /// LE Set Advertising Data (Flags 0x06) then LE Set Advertising Enable.
    fn enable_adv(ch: &HciChannel) {
        ch.send_command(&[0x08, 0x20, 0x04, 0x03, 0x02, 0x01, 0x06])
            .unwrap();
        ch.send_command(&[0x0A, 0x20, 0x01, 0x01]).unwrap();
    }
    /// LE Set Scan Enable (enable = on).
    fn enable_scan(ch: &HciChannel) {
        ch.send_command(&[0x0C, 0x20, 0x02, 0x01, 0x00]).unwrap();
    }
    /// Drain a host channel and return only the LE Meta subevents of `subevent`.
    fn le_subevents(ch: &HciChannel, subevent: u8) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(p) = ch.poll_controller_packet() {
            if p.len() >= 4
                && p[0] == h4_type::HCI_EVENT
                && p[1] == event::LE_META
                && p[3] == subevent
            {
                out.push(p);
            }
        }
        out
    }

    #[test]
    fn test_advertising_reaches_every_scanner() {
        let mut link = Link::new();
        let a = link.add_device(addr("AA:BB:CC:00:00:01"));
        let s1 = link.add_device(addr("AA:BB:CC:00:00:02"));
        let s2 = link.add_device(addr("AA:BB:CC:00:00:03"));
        enable_adv(&a);
        enable_scan(&s1);
        enable_scan(&s2);

        link.tick();

        for s in [&s1, &s2] {
            let reports = le_subevents(s, event::LE_ADVERTISING_REPORT);
            assert_eq!(reports.len(), 1);
            let r = &reports[0];
            // p: 04 3E len | 02 num event_type addr_type | addr(6) | data_len data… rssi
            assert_eq!(&r[7..13], &addr_le(addr("AA:BB:CC:00:00:01")));
            let data_len = r[13] as usize;
            assert_eq!(&r[14..14 + data_len], &[0x02, 0x01, 0x06]);
        }
        assert!(le_subevents(&a, event::LE_ADVERTISING_REPORT).is_empty());
    }

    #[test]
    fn test_many_advertisers_one_scanner() {
        let mut link = Link::new();
        let scanner = link.add_device(addr("AA:BB:CC:00:00:FF"));
        for i in 1..=5u8 {
            let adv = link.add_device(addr(&format!("AA:BB:CC:00:00:0{i}")));
            enable_adv(&adv);
        }
        enable_scan(&scanner);
        link.tick();
        assert_eq!(link.device_count(), 6);
        assert_eq!(
            le_subevents(&scanner, event::LE_ADVERTISING_REPORT).len(),
            5
        );
    }

    /// LE CS Create Config as a host sends it: 28 parameter bytes, with
    /// `create_context = 1` so the peer is configured too and `role` at
    /// offset 10.
    fn cs_create_config(handle: u16, config_id: u8, role: u8) -> Vec<u8> {
        let mut params = Vec::with_capacity(28);
        params.extend_from_slice(&handle.to_le_bytes());
        params.push(config_id);
        params.push(0x01); // create context: both controllers
        params.push(0x02); // main mode: PBR
        params.push(0xFF); // sub mode: none
        params.push(0x03); // min main mode steps
        params.push(0x13); // max main mode steps
        params.push(0x00); // main mode repetition
        params.push(0x03); // mode 0 steps
        params.push(role);
        params.push(0x00); // RTT type
        params.push(0x01); // CS sync PHY: LE 1M
        params.extend_from_slice(&[0xFF; 10]); // channel map
        params.push(0x01); // channel map repetition
        params.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // selection / ch3c / companion
        let mut command = vec![0x90, 0x20, params.len() as u8];
        command.extend_from_slice(&params);
        command
    }

    /// Connects `central` to `peripheral` and returns the connection handle.
    fn connect(link: &mut Link, central: &HciChannel, peripheral: &HciChannel, to: Address) -> u16 {
        enable_adv(peripheral);
        let mut cmd = vec![0x0D, 0x20, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00];
        cmd.extend_from_slice(&addr_le(to));
        central.send_command(&cmd).unwrap();
        link.tick();
        let cc = le_subevents(central, event::LE_CONNECTION_COMPLETE);
        let _ = le_subevents(peripheral, event::LE_CONNECTION_COMPLETE);
        u16::from_le_bytes([cc[0][5], cc[0][6]])
    }

    /// The RSSI byte an advertising report ends with.
    fn report_rssi(report: &[u8]) -> i8 {
        *report.last().unwrap() as i8
    }

    #[test]
    fn test_advertising_reports_carry_the_rssi_the_geometry_implies() {
        let mut link = Link::new();
        link.set_path_loss(PathLossModel {
            shadowing_sigma_db: 0.0, // isolate the distance term
            ..PathLossModel::default()
        });
        let advertiser_address = addr("AA:BB:CC:00:00:01");
        let adv = link.add_device(advertiser_address);
        let scan = link.add_device(addr("AA:BB:CC:00:00:02"));
        enable_adv(&adv);
        enable_scan(&scan);

        let mut readings = Vec::new();
        for distance in [1.0, 4.0, 16.0] {
            assert!(link.set_position(advertiser_address, Position::new(distance, 0.0)));
            link.tick();
            let reports = le_subevents(&scan, event::LE_ADVERTISING_REPORT);
            readings.push(report_rssi(&reports[0]));
        }
        assert!(
            readings.windows(2).all(|w| w[1] < w[0]),
            "RSSI must fall as the advertiser moves away: {readings:?}"
        );
        // Two doublings at n = 2.7 is 10·2.7·log10(4) ≈ 16.3 dB.
        assert!(
            (f64::from(readings[0] - readings[1]) - 16.3).abs() < 1.5,
            "{readings:?}"
        );
    }

    #[test]
    fn test_shadowing_makes_a_stationary_devices_rssi_jitter() {
        // The single most misleading thing the old constant did: hold still
        // and RSSI never moved, so any estimate looked rock solid.
        let mut link = Link::new();
        link.set_noise_seed(4);
        let advertiser = addr("AA:BB:CC:00:00:01");
        let adv = link.add_device(advertiser);
        let scan = link.add_device(addr("AA:BB:CC:00:00:02"));
        link.set_position(advertiser, Position::new(5.0, 0.0));
        enable_adv(&adv);
        enable_scan(&scan);

        let mut readings = Vec::new();
        for _ in 0..24 {
            link.tick();
            for report in le_subevents(&scan, event::LE_ADVERTISING_REPORT) {
                readings.push(report_rssi(&report));
            }
        }
        let distinct: std::collections::BTreeSet<i8> = readings.iter().copied().collect();
        assert!(
            distinct.len() > 3,
            "a stationary device's RSSI should still move: {distinct:?}"
        );
    }

    #[test]
    fn test_a_device_that_never_moved_is_at_the_origin() {
        let mut link = Link::new();
        let a = addr("AA:BB:CC:00:00:01");
        let b = addr("AA:BB:CC:00:00:02");
        link.add_device(a);
        link.add_device(b);
        assert_eq!(link.distance_between(a, b), Some(0.0));
        link.set_position(b, Position::new(3.0, 4.0));
        assert_eq!(link.distance_between(a, b), Some(5.0));
        assert!(!link.set_position(addr("AA:BB:CC:00:00:09"), Position::default()));
        assert!(link.distance_between(a, addr("AA:BB:CC:00:00:09")).is_none());
    }

    #[test]
    fn test_channel_sounding_tones_recover_the_true_separation() {
        // The end-to-end claim of the whole ranging path: the radio is told
        // where the devices are, the two hosts are told nothing but their own
        // tones, and combining the two sets recovers the distance.
        let mut link = Link::new();
        link.set_noise_seed(2);
        let initiator_address = addr("AA:BB:CC:00:00:01");
        let reflector_address = addr("AA:BB:CC:00:00:02");
        let initiator = link.add_device(initiator_address);
        let reflector = link.add_device(reflector_address);

        let truth = 7.25;
        link.set_position(reflector_address, Position::new(truth, 0.0));
        let handle = connect(&mut link, &initiator, &reflector, reflector_address);

        initiator.send_command(&cs_create_config(handle, 1, 0x00)).unwrap();
        link.tick();
        assert_eq!(
            le_subevents(&initiator, event::LE_CS_CONFIG_COMPLETE).len(),
            1
        );
        assert_eq!(
            le_subevents(&reflector, event::LE_CS_CONFIG_COMPLETE).len(),
            1,
            "the reflector's host must be told it is in a procedure"
        );

        // LE CS Procedure Enable: handle(2) config_id(1) enable(1).
        let mut enable = vec![0x94, 0x20, 0x04];
        enable.extend_from_slice(&handle.to_le_bytes());
        enable.extend_from_slice(&[0x01, 0x01]);
        initiator.send_command(&enable).unwrap();
        link.tick();
        assert_eq!(
            le_subevents(&initiator, event::LE_CS_PROCEDURE_ENABLE_COMPLETE).len(),
            1
        );
        let _ = le_subevents(&reflector, event::LE_CS_PROCEDURE_ENABLE_COMPLETE);

        link.tick();
        let local = subevent_tones(&initiator);
        let remote = subevent_tones(&reflector);
        assert_eq!(local.tones.len(), cs_plan::TONES_PER_SUBEVENT);
        assert_eq!(remote.tones.len(), cs_plan::TONES_PER_SUBEVENT);
        assert_eq!(
            local.procedure_counter, remote.procedure_counter,
            "both ends must label the same procedure the same way"
        );

        let estimate = crate::cs::estimate_from_tones(&local.tones, &remote.tones)
            .expect("an estimate from the radio's own tones");
        assert!(
            (estimate.distance_m - truth).abs() < 0.25,
            "true {truth} m, estimated {} m (±{})",
            estimate.distance_m,
            estimate.std_error_m
        );
    }

    #[test]
    fn test_one_ends_tones_alone_say_nothing_about_distance() {
        // Why the Ranging Service exists, asserted against the radio: the
        // initiator's own subevent results contain no recoverable distance,
        // because the oscillator offset is redrawn on every hop.
        let mut link = Link::new();
        link.set_noise_seed(6);
        let reflector_address = addr("AA:BB:CC:00:00:02");
        let initiator = link.add_device(addr("AA:BB:CC:00:00:01"));
        let reflector = link.add_device(reflector_address);
        link.set_position(reflector_address, Position::new(9.0, 0.0));
        let handle = connect(&mut link, &initiator, &reflector, reflector_address);
        initiator.send_command(&cs_create_config(handle, 1, 0x00)).unwrap();
        let mut enable = vec![0x94, 0x20, 0x04];
        enable.extend_from_slice(&handle.to_le_bytes());
        enable.extend_from_slice(&[0x01, 0x01]);
        initiator.send_command(&enable).unwrap();
        link.tick();
        link.tick();

        let local = subevent_tones(&initiator);
        // Pretend the peer reported a flat zero phase — i.e. skip the RAS
        // transfer and fit the local tones alone.
        let flat: Vec<crate::cs::Tone> = local
            .tones
            .iter()
            .map(|t| crate::cs::Tone {
                i: 2047,
                q: 0,
                ..*t
            })
            .collect();
        let alone = crate::cs::estimate_from_tones(&local.tones, &flat).expect("a fit");
        assert!(
            (alone.distance_m - 9.0).abs() > 1.0,
            "one end alone landed at {} m, which would mean the model is wrong",
            alone.distance_m
        );
    }

    #[test]
    fn test_no_measurements_are_produced_until_a_procedure_is_enabled() {
        let mut link = Link::new();
        let reflector_address = addr("AA:BB:CC:00:00:02");
        let initiator = link.add_device(addr("AA:BB:CC:00:00:01"));
        let reflector = link.add_device(reflector_address);
        link.set_position(reflector_address, Position::new(3.0, 0.0));
        let handle = connect(&mut link, &initiator, &reflector, reflector_address);

        link.tick();
        assert!(
            le_subevents(&initiator, event::LE_CS_SUBEVENT_RESULT).is_empty(),
            "a connection alone is not a Channel Sounding procedure"
        );

        initiator.send_command(&cs_create_config(handle, 1, 0x00)).unwrap();
        link.tick();
        let _ = le_subevents(&initiator, event::LE_CS_CONFIG_COMPLETE);
        link.tick();
        assert!(
            le_subevents(&initiator, event::LE_CS_SUBEVENT_RESULT).is_empty(),
            "a configuration alone is not a procedure either"
        );
    }

    /// Drains `channel` and parses the first LE CS Subevent Result on it.
    fn subevent_tones(channel: &HciChannel) -> crate::cs::SubeventResult {
        let events = le_subevents(channel, event::LE_CS_SUBEVENT_RESULT);
        let body = &events.first().expect("a subevent result")[3..];
        crate::cs::parse_subevent_result(body).expect("parsed")
    }

    #[test]
    fn test_connection_and_acl_roundtrip() {
        let mut link = Link::new();
        let central = link.add_device(addr("AA:BB:CC:00:00:01"));
        let peripheral = link.add_device(addr("AA:BB:CC:00:00:02"));
        enable_adv(&peripheral);

        // Central issues LE Create Connection to the peripheral's address.
        // params: scan_interval(2) scan_window(2) filter_policy(1)
        //         peer_addr_type(1) peer_addr(6) …
        let mut cmd = vec![0x0D, 0x20, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00];
        cmd.extend_from_slice(&addr_le(addr("AA:BB:CC:00:00:02")));
        central.send_command(&cmd).unwrap();

        link.tick();

        let cc = le_subevents(&central, event::LE_CONNECTION_COMPLETE);
        let pc = le_subevents(&peripheral, event::LE_CONNECTION_COMPLETE);
        assert_eq!(cc.len(), 1);
        assert_eq!(pc.len(), 1);
        let handle = u16::from_le_bytes([cc[0][5], cc[0][6]]);
        assert_eq!(handle, u16::from_le_bytes([pc[0][5], pc[0][6]]));
        assert_eq!(cc[0][7], 0x00); // central role
        assert_eq!(pc[0][7], 0x01); // peripheral role

        // Central sends ACL on the connection; the peripheral's host receives it.
        let payload = [0xDE, 0xAD, 0xBE, 0xEF];
        let mut acl = vec![handle as u8, (handle >> 8) as u8, 0x04, 0x00];
        acl.extend_from_slice(&payload);
        central.send_acl_data(&acl).unwrap();
        link.tick();
        let got = peripheral.poll_controller_packet().expect("acl delivered");
        assert_eq!(got[0], h4_type::HCI_ACL_DATA);
        assert_eq!(&got[5..9], &payload);
    }
}
