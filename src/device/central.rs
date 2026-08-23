// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The LE **central** host: connect, discover, read, write, subscribe.
//!
//! This is the client half of GATT as a transport-free state machine, in the
//! same shape as [`LeHost`](super::LeHost) and [`CisCentral`](super::CisCentral):
//! H4 packets in, H4 packets out, no sockets and no clock of its own. It owns
//! the progression a real client makes —
//!
//! 1. controller bring-up (Reset + both event masks, or LE Meta Events never
//!    arrive and the connection completion is silently dropped),
//! 2. **LE Create Connection** (Vol 4, Part E, Section 7.8.12),
//! 3. ATT MTU exchange, primary service discovery, characteristic discovery
//!    (Vol 3, Part G, Sections 4.3–4.6),
//! 4. reads, writes and subscriptions, one outstanding ATT request at a time
//!    as Vol 3, Part F, Section 3.3.2 requires,
//!
//! — and reports what happened as [`CentralEvent`]s, which is what a caller
//! turns into callbacks. The protocol below the state machine is
//! [`GattClient`], which already builds and parses the PDUs.
//!
//! Two things here are deliberately more careful than the scene central this
//! was lifted from, because both are invisible when both endpoints are
//! simble's and fatal against a foreign one:
//!
//! - **The CCCD is discovered, not assumed.** `value_handle + 1` is the
//!   common layout, not a rule: a characteristic may carry a Characteristic
//!   Extended Properties or User Description descriptor first (Vol 3, Part G,
//!   Section 3.3.3). A Find Information Request over the characteristic's
//!   descriptor range asks the peer where its CCCD is.
//! - **Indications are confirmed.** Only one indication may be outstanding
//!   (Vol 3, Part F, Section 3.4.7.2), so a client that never sends Handle
//!   Value Confirmation stalls the server's indication path after the first
//!   one.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use zerocopy::IntoBytes;

use crate::client::gatt_client::{DiscoveredDescriptor, DiscoveredService, GattClient};
use crate::device::host::{acl_packets, command, init_commands};
use crate::l2cap::{AclReassembler, HciAclHeader, L2capHeader};
use crate::packets::HciEvent;
use crate::packets::att::opcode as att_op;
use crate::packets::{AttFindInformationReq, AttFindInformationRspHeader};
use crate::transport::h4_type;
use crate::types::{Address, Uuid};

/// LE Create Connection (Vol 4, Part E, Section 7.8.12).
const LE_CREATE_CONNECTION: [u8; 2] = [0x0D, 0x20];
/// LE Set Scan Parameters (Vol 4, Part E, Section 7.8.10).
const LE_SET_SCAN_PARAMETERS: [u8; 2] = [0x0B, 0x20];
/// LE Set Scan Enable (Vol 4, Part E, Section 7.8.11).
const LE_SET_SCAN_ENABLE: [u8; 2] = [0x0C, 0x20];
/// Disconnect (Vol 4, Part E, Section 7.1.6).
const DISCONNECT: [u8; 2] = [0x06, 0x04];
/// Remote User Terminated Connection — the reason a host gives when the
/// application asked to disconnect (Vol 1, Part F, Section 1.3).
const REASON_REMOTE_USER_TERMINATED: u8 = 0x13;

/// The ATT MTU this client asks for. 517 is the largest an ATT_MTU field can
/// express (Vol 3, Part F, Section 3.2.9).
const CLIENT_MTU: u16 = 517;

/// Where the central is in its connect → discover progression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CentralPhase {
    /// No target yet — `connect` has not been called.
    Idle,
    /// Controller bring-up issued; waiting for it to complete.
    Initializing,
    /// Scanning for the target, to learn which address type it advertises
    /// with before initiating.
    Scanning,
    /// LE Create Connection sent; waiting for LE Connection Complete.
    Connecting,
    /// Connected; exchanging ATT MTU.
    ExchangingMtu,
    /// Reading the peer's primary services.
    DiscoveringServices,
    /// Reading the characteristics of `services[i]`.
    DiscoveringCharacteristics(usize),
    /// Discovery complete — reads, writes and subscriptions run now.
    Ready,
    /// The link is gone.
    Disconnected,
}

impl CentralPhase {
    /// A short human label, used by status JSON and by page headers.
    pub fn label(self) -> &'static str {
        match self {
            CentralPhase::Idle => "idle",
            CentralPhase::Initializing => "initializing",
            CentralPhase::Scanning => "scanning for the peer",
            CentralPhase::Connecting => "connecting",
            CentralPhase::ExchangingMtu => "exchanging MTU",
            CentralPhase::DiscoveringServices => "discovering services",
            CentralPhase::DiscoveringCharacteristics(_) => "discovering characteristics",
            CentralPhase::Ready => "ready",
            CentralPhase::Disconnected => "disconnected",
        }
    }
}

/// Something that happened on the client side, in the vocabulary a callback
/// speaks. Plain owned data, so a caller can queue it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CentralEvent {
    /// The link came up or went away. `status` is the HCI status byte.
    ConnectionStateChange {
        /// The peer this central was pointed at.
        peer: Address,
        /// Whether the link is now up.
        connected: bool,
        /// HCI status (0 on success).
        status: u8,
    },
    /// The ATT MTU was negotiated.
    MtuChanged {
        /// The MTU both sides settled on.
        mtu: u16,
    },
    /// Service and characteristic discovery finished.
    ServicesDiscovered {
        /// How many primary services the peer exposes.
        services: usize,
    },
    /// A read completed (or failed, with a non-zero ATT error code).
    CharacteristicRead {
        /// The characteristic read.
        uuid: Uuid,
        /// Its value handle.
        handle: u16,
        /// The bytes read; empty on failure.
        value: Vec<u8>,
        /// ATT error code, 0 on success.
        status: u8,
    },
    /// A write completed (or failed). A write *command* reports success as
    /// soon as it is sent — the peer never answers one.
    CharacteristicWrite {
        /// The characteristic written.
        uuid: Uuid,
        /// Its value handle.
        handle: u16,
        /// ATT error code, 0 on success.
        status: u8,
    },
    /// A notification or indication arrived.
    CharacteristicChanged {
        /// The characteristic that changed.
        uuid: Uuid,
        /// Its value handle.
        handle: u16,
        /// The notified bytes.
        value: Vec<u8>,
    },
    /// A subscription was enabled or disabled (the CCCD write completed).
    SubscriptionChanged {
        /// The characteristic subscribed to.
        uuid: Uuid,
        /// Its value handle.
        handle: u16,
        /// Whether notifications/indications are now on.
        enabled: bool,
        /// ATT error code, 0 on success.
        status: u8,
    },
    /// A requested operation could not be started: the peer has no such
    /// characteristic, or it lacks the property the operation needs. Raised
    /// once discovery has finished, so a script that names a UUID the device
    /// does not have hears about it rather than waiting forever.
    OperationFailed {
        /// The characteristic that was asked for.
        uuid: Uuid,
        /// What was attempted ("read", "write", "subscribe", "unsubscribe").
        operation: &'static str,
        /// Why it could not be started.
        reason: String,
    },
}

/// A queued client operation. Targets are UUIDs, not handles: a script names
/// a characteristic before discovery has found it, and the handle is only
/// known afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    /// Read Request.
    Read(Uuid),
    /// Write Request (`with_response`) or Write Command.
    Write {
        uuid: Uuid,
        value: Vec<u8>,
        with_response: bool,
    },
    /// Write the characteristic's CCCD.
    Subscribe { uuid: Uuid, enable: bool },
    /// Find Information over a characteristic's descriptor range, so the
    /// Subscribe that follows writes the CCCD the peer actually has rather
    /// than guessing at `value_handle + 1`.
    DiscoverDescriptors { uuid: Uuid, enable: bool },
}

/// The central half of a GATT link. Transport-free: feed it controller
/// packets, send whatever it hands back.
pub struct LeCentral {
    /// The advertiser to connect to.
    target: Address,
    /// Protocol engine: PDU construction, discovery bookkeeping.
    client: GattClient,
    reassembler: AclReassembler,
    phase: CentralPhase,
    /// Operations awaiting their turn on the link.
    pending: VecDeque<Op>,
    /// The operation whose response is outstanding, and the attribute handle
    /// it addressed.
    in_flight: Option<(Op, u16)>,
    /// Latest value seen per value handle, from a read or a notification.
    values: BTreeMap<u16, Vec<u8>>,
    /// Value handles with an active subscription.
    subscribed: BTreeSet<u16>,
    /// What the caller has not drained yet.
    events: Vec<CentralEvent>,
    /// The peer's address type for LE Create Connection: `Some` when the
    /// caller stated it, `None` until an advertising report reveals it.
    ///
    /// This is not cosmetic. A peer advertising with a random address is
    /// unreachable by a Create Connection that says "public", and the
    /// in-process controller ignores the field entirely — so the mistake is
    /// invisible in every simble-against-simble test and total against a
    /// real one. (The same field, dropped from LE Connection Complete, broke
    /// every pairing attempt against Android; see `docs/HANDOFF-2026-08-22.md`.)
    peer_address_type: Option<u8>,
}

impl LeCentral {
    /// Creates a central with no target. `connect` points it at one.
    pub fn new() -> Self {
        Self {
            target: Address::from_be_bytes([0; 6]),
            client: GattClient::new(0, Address::from_be_bytes([0; 6])),
            reassembler: AclReassembler::new(),
            phase: CentralPhase::Idle,
            pending: VecDeque::new(),
            in_flight: None,
            values: BTreeMap::new(),
            subscribed: BTreeSet::new(),
            events: Vec::new(),
            peer_address_type: None,
        }
    }

    /// Points the central at `target` and issues controller bring-up.
    ///
    /// Two things are deliberately sequenced rather than fired at once:
    ///
    /// - LE Create Connection waits for bring-up to finish, because a
    ///   controller whose LE event mask is still at its post-Reset default
    ///   never reports the connection (Vol 4, Part E, Section 7.3.1).
    /// - It then waits to *hear* the target, so the peer address type in the
    ///   command is the one the peer actually advertises with. Guessing
    ///   "public" reaches a random-address advertiser never, and the
    ///   in-process controller does not read the field — so the failure only
    ///   shows against a real peer.
    ///
    /// Use [`Self::connect_with_type`] when the type is already known (from
    /// a scan the caller did, or a bond) and the scan step is unwanted.
    pub fn connect(&mut self, target: Address) -> Vec<Vec<u8>> {
        self.begin(target, None)
    }

    /// As [`Self::connect`], but states the peer's address type (0 public,
    /// 1 random) instead of learning it, skipping the scan.
    pub fn connect_with_type(&mut self, target: Address, address_type: u8) -> Vec<Vec<u8>> {
        self.begin(target, Some(address_type))
    }

    fn begin(&mut self, target: Address, address_type: Option<u8>) -> Vec<Vec<u8>> {
        self.target = target;
        self.client = GattClient::new(0, target);
        self.peer_address_type = address_type;
        self.phase = CentralPhase::Initializing;
        init_commands()
    }

    /// Passive scanning, wide open: 10 ms window in a 10 ms interval, no
    /// filter policy, so the target's next advertisement is heard at once.
    fn scan_commands() -> Vec<Vec<u8>> {
        vec![
            command(
                LE_SET_SCAN_PARAMETERS,
                // type: passive, interval, window, own address type: public,
                // filter policy: accept all.
                &[0x00, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00],
            ),
            // enable, no duplicate filtering (a filtered duplicate is a
            // report we never see, and one report is all this needs).
            command(LE_SET_SCAN_ENABLE, &[0x01, 0x00]),
        ]
    }

    /// Tears the link down.
    pub fn disconnect(&mut self) -> Vec<Vec<u8>> {
        let handle = self.client.connection_handle;
        if handle == 0 {
            return Vec::new();
        }
        let mut params = handle.to_le_bytes().to_vec();
        params.push(REASON_REMOTE_USER_TERMINATED);
        vec![command(DISCONNECT, &params)]
    }

    /// The peer this central is pointed at.
    pub fn target(&self) -> Address {
        self.target
    }

    /// Where the connect → discover progression has got to.
    pub fn phase(&self) -> CentralPhase {
        self.phase
    }

    /// True once the link is up and the peer's GATT has been discovered.
    pub fn is_ready(&self) -> bool {
        self.phase == CentralPhase::Ready
    }

    /// True when every queued operation has been sent *and* answered — the
    /// only moment at which the results of a sequence of operations are all
    /// in, since a queued operation has not reached the peer yet.
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.in_flight.is_none()
    }

    /// The ACL connection handle, or 0 when there is no link.
    pub fn connection_handle(&self) -> u16 {
        self.client.connection_handle
    }

    /// The negotiated ATT MTU.
    pub fn mtu(&self) -> u16 {
        self.client.mtu
    }

    /// The peer's discovered services.
    pub fn services(&self) -> &[DiscoveredService] {
        &self.client.services
    }

    /// The value handle of a discovered characteristic, by UUID.
    pub fn value_handle(&self, uuid: Uuid) -> Option<u16> {
        self.client.find_characteristic(uuid).map(|c| c.value_handle)
    }

    /// The last value seen for a characteristic — read or notified — or
    /// `None` if nothing has arrived on it.
    pub fn value(&self, uuid: Uuid) -> Option<&[u8]> {
        let handle = self.value_handle(uuid)?;
        self.values.get(&handle).map(Vec::as_slice)
    }

    /// The last value seen at a value handle.
    pub fn value_at(&self, handle: u16) -> Option<&[u8]> {
        self.values.get(&handle).map(Vec::as_slice)
    }

    /// Whether a characteristic currently has notifications or indications
    /// enabled.
    pub fn is_subscribed(&self, uuid: Uuid) -> bool {
        self.value_handle(uuid)
            .is_some_and(|h| self.subscribed.contains(&h))
    }

    /// Queues a read of `uuid`.
    pub fn queue_read(&mut self, uuid: Uuid) {
        self.pending.push_back(Op::Read(uuid));
    }

    /// Queues a write of `value` to `uuid`. `with_response` picks Write
    /// Request (acknowledged) over Write Command (fire-and-forget).
    pub fn queue_write(&mut self, uuid: Uuid, value: Vec<u8>, with_response: bool) {
        self.pending.push_back(Op::Write {
            uuid,
            value,
            with_response,
        });
    }

    /// Queues enabling (or disabling) notifications on `uuid`. The peer's
    /// descriptors are discovered first if they are not known yet, so the
    /// CCCD write lands on the handle the peer actually published.
    pub fn queue_subscribe(&mut self, uuid: Uuid, enable: bool) {
        self.pending.push_back(Op::DiscoverDescriptors { uuid, enable });
    }

    /// Drains the events raised since the last call.
    pub fn take_events(&mut self) -> Vec<CentralEvent> {
        std::mem::take(&mut self.events)
    }

    /// Sends the next queued operation, if the link is ready and nothing is
    /// outstanding. ATT allows one request per connection at a time (Vol 3,
    /// Part F, Section 3.3.2), which is what the queue is for.
    pub fn pump(&mut self) -> Vec<Vec<u8>> {
        if self.phase != CentralPhase::Ready {
            return Vec::new();
        }
        let mut out = Vec::new();
        // A write command is unacknowledged, so it never becomes the
        // in-flight operation and the loop may send the next one at once.
        while self.in_flight.is_none() {
            let Some(op) = self.pending.pop_front() else {
                break;
            };
            match self.start(op, &mut out) {
                Started::InFlight => {}
                Started::Complete => continue,
            }
        }
        out
    }

    /// Builds the PDU for one operation and records it as in flight.
    fn start(&mut self, op: Op, out: &mut Vec<Vec<u8>>) -> Started {
        let handle = self.client.connection_handle;
        match &op {
            Op::Read(uuid) => {
                let Some(characteristic) = self.client.find_characteristic(*uuid) else {
                    self.fail(*uuid, "read", "no such characteristic on this peer");
                    return Started::Complete;
                };
                let value_handle = characteristic.value_handle;
                let pdu = self.client.create_read_request(value_handle);
                out.extend(acl_packets(handle, &pdu));
                self.in_flight = Some((op, value_handle));
                Started::InFlight
            }
            Op::Write {
                uuid,
                value,
                with_response,
            } => {
                let Some(characteristic) = self.client.find_characteristic(*uuid) else {
                    self.fail(*uuid, "write", "no such characteristic on this peer");
                    return Started::Complete;
                };
                let value_handle = characteristic.value_handle;
                if *with_response {
                    let pdu = self.client.create_write_request(value_handle, value);
                    out.extend(acl_packets(handle, &pdu));
                    self.in_flight = Some((op, value_handle));
                    Started::InFlight
                } else {
                    let pdu = self.client.create_write_command(value_handle, value);
                    out.extend(acl_packets(handle, &pdu));
                    // Nothing will ever answer a Write Command, so the
                    // result is reported now rather than never.
                    self.events.push(CentralEvent::CharacteristicWrite {
                        uuid: *uuid,
                        handle: value_handle,
                        status: 0,
                    });
                    Started::Complete
                }
            }
            Op::DiscoverDescriptors { uuid, enable } => {
                let Some(range) = self.descriptor_range(*uuid) else {
                    let operation = if *enable { "subscribe" } else { "unsubscribe" };
                    self.fail(*uuid, operation, "no such characteristic on this peer");
                    return Started::Complete;
                };
                // Descriptors already known (a second subscribe on the same
                // characteristic): go straight to the CCCD write.
                if self
                    .client
                    .find_characteristic(*uuid)
                    .is_some_and(|c| !c.descriptors.is_empty())
                {
                    return self.start(
                        Op::Subscribe {
                            uuid: *uuid,
                            enable: *enable,
                        },
                        out,
                    );
                }
                let (start, end) = range;
                if start > end {
                    // A characteristic with no room for descriptors cannot
                    // have a CCCD; say so instead of writing over whatever
                    // attribute follows it.
                    let operation = if *enable { "subscribe" } else { "unsubscribe" };
                    self.fail(*uuid, operation, "characteristic has no descriptors");
                    return Started::Complete;
                }
                let pdu = find_information_request(start, end);
                out.extend(acl_packets(handle, &pdu));
                self.in_flight = Some((op, start));
                Started::InFlight
            }
            Op::Subscribe { uuid, enable } => {
                let Some(characteristic) = self.client.find_characteristic(*uuid) else {
                    let operation = if *enable { "subscribe" } else { "unsubscribe" };
                    self.fail(*uuid, operation, "no such characteristic on this peer");
                    return Started::Complete;
                };
                let value_handle = characteristic.value_handle;
                let properties = characteristic.properties;
                let Some(cccd) = characteristic
                    .descriptors
                    .iter()
                    .find(|d| d.uuid == Uuid::CCCD)
                    .map(|d| d.handle)
                else {
                    let operation = if *enable { "subscribe" } else { "unsubscribe" };
                    self.fail(
                        *uuid,
                        operation,
                        "characteristic has no Client Characteristic Configuration descriptor",
                    );
                    return Started::Complete;
                };
                // Bit 1 (indicate) when the characteristic only indicates:
                // several SIG profiles mandate Indicate, and writing 0x0001
                // to those subscribes to nothing (Vol 3, Part G, 3.3.3.3).
                let bits: u16 = if !*enable {
                    0x0000
                } else if properties & 0x20 != 0 && properties & 0x10 == 0 {
                    0x0002
                } else {
                    0x0001
                };
                let pdu = self.client.create_write_request(cccd, &bits.to_le_bytes());
                out.extend(acl_packets(handle, &pdu));
                self.in_flight = Some((op, value_handle));
                Started::InFlight
            }
        }
    }

    fn fail(&mut self, uuid: Uuid, operation: &'static str, reason: &str) {
        self.events.push(CentralEvent::OperationFailed {
            uuid,
            operation,
            reason: reason.to_string(),
        });
    }

    /// The handle range a characteristic's descriptors live in: everything
    /// after its value up to the next characteristic declaration, or the end
    /// of the service (Vol 3, Part G, Section 3.3).
    fn descriptor_range(&self, uuid: Uuid) -> Option<(u16, u16)> {
        for service in &self.client.services {
            for (i, characteristic) in service.characteristics.iter().enumerate() {
                if characteristic.uuid != uuid {
                    continue;
                }
                let end = service
                    .characteristics
                    .get(i + 1)
                    .map(|next| next.declaration_handle.saturating_sub(1))
                    .unwrap_or(service.end_handle);
                return Some((characteristic.value_handle.saturating_add(1), end));
            }
        }
        None
    }

    /// Feeds one controller→host H4 packet in and returns what to send back.
    pub fn on_packet(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        match packet.first() {
            Some(&h4_type::HCI_EVENT) => self.on_event(packet, &mut out),
            Some(&h4_type::HCI_ACL_DATA) => {
                if let Some((header, payload)) = HciAclHeader::parse(&packet[1..]) {
                    let handle = header.handle();
                    let is_first = header.is_first_fragment();
                    if let Ok(Some(frame)) =
                        self.reassembler.push_fragment(handle, is_first, payload)
                        && let Some((_, att)) = L2capHeader::parse(&frame)
                    {
                        let att = att.to_vec();
                        self.dispatch_att(&att, &mut out);
                    }
                }
            }
            _ => {}
        }
        out.extend(self.pump());
        out
    }

    fn on_event(&mut self, packet: &[u8], out: &mut Vec<Vec<u8>>) {
        let Some(event) = HciEvent::parse_h4(packet) else {
            return;
        };
        match event {
            // Bring-up is complete when the last init command is answered.
            // Waiting for it is what keeps LE Create Connection from being
            // issued before the LE event mask is open.
            HciEvent::CommandComplete { header, .. }
                if self.phase == CentralPhase::Initializing
                    && header.command_opcode.get() == last_init_opcode() =>
            {
                match self.peer_address_type {
                    Some(_) => {
                        self.phase = CentralPhase::Connecting;
                        out.push(command(
                            LE_CREATE_CONNECTION,
                            &self.create_connection_params(),
                        ));
                    }
                    None => {
                        self.phase = CentralPhase::Scanning;
                        out.extend(Self::scan_commands());
                    }
                }
            }
            // The advertisement is where the peer's address type comes from.
            HciEvent::Other { code, parameters }
                if self.phase == CentralPhase::Scanning
                    && code == crate::packets::hci_events::event_code::LE_META
                    && parameters.first()
                        == Some(&crate::packets::hci_events::le_subevent::ADVERTISING_REPORT) =>
            {
                let mut wire = self.target.to_be_bytes();
                wire.reverse();
                let Some(report) = crate::packets::hci_events::advertising_reports(parameters)
                    .into_iter()
                    .find(|report| report.header.address == wire)
                else {
                    return;
                };
                self.peer_address_type = Some(report.header.address_type);
                self.phase = CentralPhase::Connecting;
                // Scanning and initiating at once is legal but wasteful, and
                // some controllers refuse it outright.
                out.push(command(LE_SET_SCAN_ENABLE, &[0x00, 0x00]));
                out.push(command(
                    LE_CREATE_CONNECTION,
                    &self.create_connection_params(),
                ));
            }
            HciEvent::LeConnectionComplete(event) => {
                if event.status != 0x00 {
                    self.phase = CentralPhase::Disconnected;
                    self.events.push(CentralEvent::ConnectionStateChange {
                        peer: self.target,
                        connected: false,
                        status: event.status,
                    });
                    return;
                }
                let handle = event.connection_handle.get() & 0x0FFF;
                self.client = GattClient::new(handle, self.target);
                self.phase = CentralPhase::ExchangingMtu;
                self.events.push(CentralEvent::ConnectionStateChange {
                    peer: self.target,
                    connected: true,
                    status: 0,
                });
                let pdu = self.client.create_exchange_mtu_request(CLIENT_MTU);
                out.extend(acl_packets(handle, &pdu));
            }
            HciEvent::DisconnectionComplete(event) => {
                self.phase = CentralPhase::Disconnected;
                self.client.connection_handle = 0;
                self.in_flight = None;
                self.subscribed.clear();
                self.events.push(CentralEvent::ConnectionStateChange {
                    peer: self.target,
                    connected: false,
                    status: event.reason,
                });
            }
            _ => {}
        }
    }

    /// LE Create Connection's 25 parameter bytes (Vol 4, Part E, Section
    /// 7.8.12). All 25 matter: a controller rejects a short command outright,
    /// and the central then sits in "connecting" having never transmitted.
    fn create_connection_params(&self) -> Vec<u8> {
        let mut params = Vec::with_capacity(25);
        params.extend_from_slice(&0x0060u16.to_le_bytes()); // scan interval, 60 ms
        params.extend_from_slice(&0x0030u16.to_le_bytes()); // scan window, 30 ms
        params.push(0x00); // initiator filter policy: use the peer address
        // Peer address type, as heard on the air (or as the caller stated).
        params.push(self.peer_address_type.unwrap_or(0x00));
        let mut peer = self.target.to_be_bytes();
        peer.reverse(); // little-endian on the wire
        params.extend_from_slice(&peer);
        params.push(0x00); // own address type: public
        params.extend_from_slice(&0x0018u16.to_le_bytes()); // min interval, 30 ms
        params.extend_from_slice(&0x0028u16.to_le_bytes()); // max interval, 50 ms
        params.extend_from_slice(&0x0000u16.to_le_bytes()); // max latency
        params.extend_from_slice(&0x00C8u16.to_le_bytes()); // supervision timeout, 2 s
        params.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
        params.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length
        debug_assert_eq!(params.len(), 25, "LE Create Connection is 25 bytes");
        params
    }

    /// Advances the discovery state machine, or completes an outstanding
    /// operation, on one ATT PDU.
    fn dispatch_att(&mut self, att: &[u8], out: &mut Vec<Vec<u8>>) {
        let handle = self.client.connection_handle;
        let Some(&op) = att.first() else { return };
        let is_error = op == att_op::ERROR_RSP;

        // Server-initiated PDUs arrive whenever the server likes and are
        // unrelated to the request FSM.
        if (op == att_op::HANDLE_VALUE_NTF || op == att_op::HANDLE_VALUE_IND) && att.len() >= 3 {
            let value_handle = u16::from_le_bytes([att[1], att[2]]);
            let value = att[3..].to_vec();
            self.values.insert(value_handle, value.clone());
            self.events.push(CentralEvent::CharacteristicChanged {
                uuid: self.uuid_for_handle(value_handle).unwrap_or(Uuid::Uuid16(0)),
                handle: value_handle,
                value,
            });
            if op == att_op::HANDLE_VALUE_IND {
                // Vol 3, Part F, Section 3.4.7.2: one indication at a time.
                // Without this the server's next indication is never sent.
                let pdu = L2capHeader::serialize(crate::l2cap::cid::ATT, &[att_op::HANDLE_VALUE_CFM]);
                out.extend(acl_packets(handle, &pdu));
            }
            return;
        }

        match self.phase {
            CentralPhase::ExchangingMtu => {
                if op == att_op::EXCHANGE_MTU_RSP && att.len() >= 3 {
                    let server_mtu = u16::from_le_bytes([att[1], att[2]]);
                    self.client.on_exchange_mtu_response(server_mtu, CLIENT_MTU);
                }
                // An MTU the peer refuses is not fatal — the default 23 is
                // always legal — so discovery starts either way.
                self.events.push(CentralEvent::MtuChanged {
                    mtu: self.client.mtu,
                });
                self.phase = CentralPhase::DiscoveringServices;
                let pdu = self.client.create_discover_services_request(0x0001, 0xFFFF);
                out.extend(acl_packets(handle, &pdu));
            }
            CentralPhase::DiscoveringServices => {
                if is_error {
                    // Attribute Not Found ends the sweep: every service has
                    // been reported (Vol 3, Part G, Section 4.4.1).
                    self.start_characteristic_discovery(out);
                } else if op == att_op::READ_BY_GROUP_TYPE_RSP {
                    let _ = self.client.on_discover_services_response(att);
                    let last_end = self.client.services.last().map_or(0xFFFF, |s| s.end_handle);
                    if last_end < 0xFFFF {
                        let pdu = self
                            .client
                            .create_discover_services_request(last_end + 1, 0xFFFF);
                        out.extend(acl_packets(handle, &pdu));
                    } else {
                        self.start_characteristic_discovery(out);
                    }
                }
            }
            CentralPhase::DiscoveringCharacteristics(i) => {
                if is_error {
                    self.next_characteristic_service(i, out);
                } else if op == att_op::READ_BY_TYPE_RSP {
                    let service_uuid = self.client.services[i].uuid;
                    let _ = self
                        .client
                        .on_discover_characteristics_response(service_uuid, att);
                    let service = &self.client.services[i];
                    let end = service.end_handle;
                    let last = service
                        .characteristics
                        .last()
                        .map_or(service.start_handle, |c| c.value_handle);
                    if last < end {
                        let pdu = self
                            .client
                            .create_discover_characteristics_request(last + 1, end);
                        out.extend(acl_packets(handle, &pdu));
                    } else {
                        self.next_characteristic_service(i, out);
                    }
                }
            }
            CentralPhase::Ready => self.complete_operation(att, is_error, out),
            CentralPhase::Idle
            | CentralPhase::Initializing
            | CentralPhase::Scanning
            | CentralPhase::Connecting
            | CentralPhase::Disconnected => {}
        }
    }

    /// Finishes the in-flight operation on its response.
    fn complete_operation(&mut self, att: &[u8], is_error: bool, out: &mut Vec<Vec<u8>>) {
        let op = att.first().copied().unwrap_or(0);
        let Some((pending, attribute_handle)) = self.in_flight.take() else {
            return;
        };
        let status = if is_error {
            // Error Response: opcode, request opcode, handle(2), error code.
            att.get(4).copied().unwrap_or(0xFF)
        } else {
            0
        };
        match pending {
            Op::Read(uuid) => {
                let value = if !is_error && op == att_op::READ_RSP {
                    att[1..].to_vec()
                } else {
                    Vec::new()
                };
                if !value.is_empty() || !is_error {
                    self.values.insert(attribute_handle, value.clone());
                }
                self.events.push(CentralEvent::CharacteristicRead {
                    uuid,
                    handle: attribute_handle,
                    value,
                    status,
                });
            }
            Op::Write { uuid, .. } => {
                self.events.push(CentralEvent::CharacteristicWrite {
                    uuid,
                    handle: attribute_handle,
                    status,
                });
            }
            Op::Subscribe { uuid, enable } => {
                if !is_error {
                    if enable {
                        self.subscribed.insert(attribute_handle);
                    } else {
                        self.subscribed.remove(&attribute_handle);
                    }
                }
                self.events.push(CentralEvent::SubscriptionChanged {
                    uuid,
                    handle: attribute_handle,
                    enabled: enable && !is_error,
                    status,
                });
            }
            Op::DiscoverDescriptors { uuid, enable } => {
                if !is_error && op == att_op::FIND_INFORMATION_RSP {
                    self.record_descriptors(uuid, att);
                }
                // Either way, try the CCCD write next: if the sweep found
                // nothing the Subscribe arm reports it as a failure with the
                // reason, which is more useful than a silent stall.
                let next = Op::Subscribe { uuid, enable };
                if let Started::Complete = self.start(next, out) {
                    // Nothing outstanding; the queue keeps moving in `pump`.
                }
            }
        }
    }

    /// Stores the descriptors a Find Information Response reported against
    /// the characteristic they belong to (Vol 3, Part F, Section 3.4.3.2:
    /// format 0x01 is 16-bit UUIDs, 0x02 is 128-bit).
    fn record_descriptors(&mut self, uuid: Uuid, att: &[u8]) {
        let Some((header, information_data)) = AttFindInformationRspHeader::parse(att) else {
            return;
        };
        let Some(items) = header.items(information_data) else {
            return;
        };
        let mut found = Vec::new();
        for (entry, uuid_bytes) in items {
            let Some(descriptor_uuid) = Uuid::from_bytes(uuid_bytes) else {
                continue;
            };
            found.push(DiscoveredDescriptor {
                handle: entry.attribute_handle.get(),
                uuid: descriptor_uuid,
            });
        }
        for service in &mut self.client.services {
            for characteristic in &mut service.characteristics {
                if characteristic.uuid == uuid {
                    characteristic.descriptors = found;
                    return;
                }
            }
        }
    }

    /// The UUID of the characteristic whose value sits at `handle`.
    fn uuid_for_handle(&self, handle: u16) -> Option<Uuid> {
        self.client
            .services
            .iter()
            .flat_map(|s| s.characteristics.iter())
            .find(|c| c.value_handle == handle)
            .map(|c| c.uuid)
    }

    fn start_characteristic_discovery(&mut self, out: &mut Vec<Vec<u8>>) {
        if self.client.services.is_empty() {
            self.finish_discovery();
        } else {
            self.phase = CentralPhase::DiscoveringCharacteristics(0);
            self.discover_characteristics_for(0, out);
        }
    }

    fn next_characteristic_service(&mut self, i: usize, out: &mut Vec<Vec<u8>>) {
        let next = i + 1;
        if next < self.client.services.len() {
            self.phase = CentralPhase::DiscoveringCharacteristics(next);
            self.discover_characteristics_for(next, out);
        } else {
            self.finish_discovery();
        }
    }

    fn finish_discovery(&mut self) {
        self.phase = CentralPhase::Ready;
        self.events.push(CentralEvent::ServicesDiscovered {
            services: self.client.services.len(),
        });
    }

    fn discover_characteristics_for(&mut self, i: usize, out: &mut Vec<Vec<u8>>) {
        let handle = self.client.connection_handle;
        let service = &self.client.services[i];
        let pdu = self
            .client
            .create_discover_characteristics_request(service.start_handle, service.end_handle);
        out.extend(acl_packets(handle, &pdu));
    }
}

impl Default for LeCentral {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether starting an operation left a request outstanding.
enum Started {
    /// A request was sent; its response is awaited.
    InFlight,
    /// The operation finished without a response (a write command, or a
    /// failure raised as an event).
    Complete,
}

/// Find Information Request over a handle range (Vol 3, Part F, 3.4.3.1) —
/// how a client learns where a characteristic's descriptors are.
fn find_information_request(start: u16, end: u16) -> Vec<u8> {
    let request = AttFindInformationReq::new(start, end);
    L2capHeader::serialize(crate::l2cap::cid::ATT, request.as_bytes())
}

/// The opcode of the last command [`init_commands`] issues — the completion
/// this central waits for before connecting.
fn last_init_opcode() -> u16 {
    let commands = init_commands();
    let last = commands.last().expect("init_commands is never empty");
    // H4 type, then the opcode, little-endian.
    u16::from_le_bytes([last[1], last[2]])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::host::opcode;

    fn command_complete(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
        let mut packet = vec![h4_type::HCI_EVENT, 0x0E, (3 + params.len()) as u8, 0x01];
        packet.extend_from_slice(&opcode);
        packet.extend_from_slice(params);
        packet
    }

    /// One LE Advertising Report event for `address` with `address_type`.
    fn advertising_report(address: Address, address_type: u8) -> Vec<u8> {
        let mut wire = address.to_be_bytes();
        wire.reverse();
        let mut body = vec![
            crate::packets::hci_events::le_subevent::ADVERTISING_REPORT,
            0x01,
            0x00, // ADV_IND
            address_type,
        ];
        body.extend_from_slice(&wire);
        body.push(0x00); // no AD data
        body.push(0xC0); // RSSI
        let mut packet = vec![
            h4_type::HCI_EVENT,
            crate::packets::hci_events::event_code::LE_META,
            body.len() as u8,
        ];
        packet.extend_from_slice(&body);
        packet
    }

    #[test]
    fn le_create_connection_is_withheld_until_the_event_masks_are_open() {
        let mut central = LeCentral::new();
        let target: Address = "AA:BB:CC:00:00:01".parse().unwrap();
        let bringup = central.connect_with_type(target, 0x00);
        assert!(!bringup.is_empty(), "connect issues controller bring-up");
        assert_eq!(central.phase(), CentralPhase::Initializing);
        // Anything other than the last init command's completion leaves the
        // central waiting: connecting first would lose the LE Meta Event.
        let out = central.on_packet(&command_complete(opcode::RESET, &[0x00]));
        assert!(out.is_empty());
        assert_eq!(central.phase(), CentralPhase::Initializing);

        let last = last_init_opcode().to_le_bytes();
        let out = central.on_packet(&command_complete(last, &[0x00]));
        assert_eq!(central.phase(), CentralPhase::Connecting);
        assert_eq!(out.len(), 1);
        // H4 type, opcode(2), parameter length, then 25 parameter bytes.
        assert_eq!(out[0][1..3], LE_CREATE_CONNECTION);
        assert_eq!(out[0][3], 25, "LE Create Connection is 25 parameter bytes");
    }

    #[test]
    fn the_peers_address_type_is_taken_from_its_advertisement_not_assumed() {
        // A peer advertising with a random address is unreachable by a
        // Create Connection that claims "public" — and the in-process
        // controller never reads the field, so only a real one notices.
        let mut central = LeCentral::new();
        let target: Address = "F0:F1:F2:F3:F4:D2".parse().unwrap();
        central.connect(target);
        let last = last_init_opcode().to_le_bytes();
        let out = central.on_packet(&command_complete(last, &[0x00]));
        assert_eq!(central.phase(), CentralPhase::Scanning);
        assert_eq!(out[0][1..3], LE_SET_SCAN_PARAMETERS);
        assert_eq!(out[1][1..3], LE_SET_SCAN_ENABLE);

        // An advertisement from someone else is not the peer.
        let other: Address = "11:22:33:44:55:66".parse().unwrap();
        assert!(central.on_packet(&advertising_report(other, 0x01)).is_empty());
        assert_eq!(central.phase(), CentralPhase::Scanning);

        let out = central.on_packet(&advertising_report(target, 0x01));
        assert_eq!(central.phase(), CentralPhase::Connecting);
        let create = out
            .iter()
            .find(|packet| packet[1..3] == LE_CREATE_CONNECTION)
            .expect("the connection was initiated");
        // scan interval(2) window(2) filter policy(1), then the peer type.
        assert_eq!(create[4 + 5], 0x01, "peer address type: random");
    }

    #[test]
    fn a_failed_connection_reports_its_hci_status_rather_than_hanging() {
        let mut central = LeCentral::new();
        central.connect_with_type("AA:BB:CC:00:00:02".parse().unwrap(), 0x00);
        let last = last_init_opcode().to_le_bytes();
        central.on_packet(&command_complete(last, &[0x00]));
        // LE Connection Complete with status 0x3E (Connection Failed to be
        // Established).
        let mut event = vec![h4_type::HCI_EVENT, 0x3E, 19, 0x01, 0x3E];
        event.extend_from_slice(&[0x00; 17]);
        central.on_packet(&event);
        assert_eq!(central.phase(), CentralPhase::Disconnected);
        assert_eq!(
            central.take_events(),
            vec![CentralEvent::ConnectionStateChange {
                peer: "AA:BB:CC:00:00:02".parse().unwrap(),
                connected: false,
                status: 0x3E,
            }]
        );
    }

    #[test]
    fn a_read_of_an_undiscovered_uuid_fails_loudly_instead_of_waiting_forever() {
        let mut central = LeCentral::new();
        central.phase = CentralPhase::Ready;
        central.client = GattClient::new(0x0040, "AA:BB:CC:00:00:03".parse().unwrap());
        central.queue_read(Uuid::Uuid16(0x2A37));
        let out = central.pump();
        assert!(out.is_empty(), "nothing goes on the wire");
        assert!(matches!(
            central.take_events().as_slice(),
            [CentralEvent::OperationFailed {
                operation: "read",
                ..
            }]
        ));
    }

    #[test]
    fn find_information_request_carries_the_descriptor_range() {
        let pdu = find_information_request(0x0010, 0x0014);
        // L2CAP header (4 bytes) then the ATT PDU.
        assert_eq!(&pdu[4..], &[att_op::FIND_INFORMATION_REQ, 0x10, 0x00, 0x14, 0x00]);
    }
}
