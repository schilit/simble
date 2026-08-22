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
}

/// `STATUS_SUCCESS` (0x00) HCI status code.
const STATUS_SUCCESS: u8 = 0x00;
/// Disconnection reason: "Connection Terminated By Local Host" (0x16).
const REASON_LOCAL_HOST: u8 = 0x16;
/// Disconnection reason: "Remote User Terminated Connection" (0x13).
const REASON_REMOTE_USER: u8 = 0x13;

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
            outbox: VecDeque::new(),
        }
    }

    /// Reset to power-on defaults (HCI Reset).
    fn reset(&mut self) {
        self.advertising = false;
        self.adv_data.clear();
        self.scanning = false;
        self.pending_connect = None;
        self.connections.clear();
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
}

/// The shared medium. Holds every `SimController` on the "air" and, on each
/// [`tick`](Self::tick), drains their hosts' HCI, routes advertising and data
/// between them, and delivers the resulting events — the same role as Bumble's
/// `LocalLink`, sized for an in-process scene of any number of devices.
#[derive(Default)]
pub struct Link {
    controllers: Vec<SimController>,
    next_handle: u16,
}

impl Link {
    /// Creates an empty medium with no devices.
    pub fn new() -> Self {
        Self {
            controllers: Vec::new(),
            next_handle: 0x0001,
        }
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
        // other scanning device.
        let advertisers: Vec<(Address, u8, u8, Vec<u8>)> = self
            .controllers
            .iter()
            .filter(|c| c.advertising)
            .map(|c| {
                (
                    c.address,
                    c.adv_event_type,
                    c.own_adv_addr_type,
                    c.adv_data.clone(),
                )
            })
            .collect();
        for scanner in self.controllers.iter_mut().filter(|c| c.scanning) {
            for (addr, event_type, addr_type, data) in &advertisers {
                if *addr == scanner.address {
                    continue; // a device never hears its own advertisement
                }
                scanner.outbox.push_back(le_advertising_report(
                    *event_type,
                    *addr_type,
                    *addr,
                    data,
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
            }
        }

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

/// LE Advertising Report subevent carrying one report for `addr`.
fn le_advertising_report(event_type: u8, addr_type: u8, addr: Address, data: &[u8]) -> Vec<u8> {
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
    body.push(0xC3); // RSSI -61 dBm (0xC3 as i8)
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
