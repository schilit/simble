// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The scene central: the client half of a scripted scene.
//!
//! `CentralDevice` connects to a peripheral by address over the shared link,
//! exchanges MTU, discovers its GATT, and supports interactive read / write /
//! subscribe — the natively-testable engine the browser bindings wrap. Split
//! out of the wasm transport so the scene engine and tests can drive it
//! directly.

use crate::client::gatt_client::GattClient;
use crate::l2cap::{AclReassembler, HciAclHeader, L2capHeader};
use crate::packets::att::{AttExchangeMtuRsp, AttHandleValueHeader, opcode as att_op};
use crate::packets::hci_events::HciEvent;
use crate::transport::hci_adapter::{HciChannel, h4_type};
use crate::transport::scan_report::{queue_command, send_acl};
use crate::types::Address;

/// The central's connect → discover progression.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CentralPhase {
    /// Waiting to connect to the target advertiser.
    Connecting,
    /// Connected; exchanging ATT MTU.
    ExchangingMtu,
    /// Reading the peer's primary services.
    DiscoveringServices,
    /// Reading characteristics of `services[i]`.
    DiscoveringCharacteristics(usize),
    /// Discovery complete.
    Ready,
}

impl CentralPhase {
    /// A short human label for the current phase.
    fn label(self) -> &'static str {
        match self {
            CentralPhase::Connecting => "connecting",
            CentralPhase::ExchangingMtu => "exchanging MTU",
            CentralPhase::DiscoveringServices => "discovering services",
            CentralPhase::DiscoveringCharacteristics(_) => "discovering characteristics",
            CentralPhase::Ready => "ready",
        }
    }
}

/// A client-initiated GATT operation, queued from the UI and sent when the
/// central is idle (one outstanding request at a time).
#[derive(Clone)]
enum CentralOp {
    /// Read a characteristic value handle.
    Read(u16),
    /// Write a value to a characteristic value handle.
    Write(u16, Vec<u8>),
    /// Enable notifications by writing 0x0001 to the CCCD after a value handle.
    Subscribe(u16),
}

/// A scene central: connects to a peripheral by address over the shared
/// [`Link`], exchanges MTU, and discovers its GATT — the *client* half of the
/// server/client view. It drives a [`GattClient`], framing ATT over L2CAP over
/// ACL, the mirror of the peripheral's inbound path, and supports interactive
/// read / write / subscribe once discovery is complete.
pub(crate) struct CentralDevice {
    target: Address,
    pub(crate) client: GattClient,
    reassembler: AclReassembler,
    pub(crate) phase: CentralPhase,
    connect_requested: bool,
    /// Operations requested from the UI, awaiting their turn on the link.
    pending_ops: std::collections::VecDeque<CentralOp>,
    /// The operation whose response we're waiting for, if any.
    in_flight: Option<CentralOp>,
    /// Latest value seen per handle (from a read response or a notification).
    values: std::collections::BTreeMap<u16, Vec<u8>>,
    /// Value handles with notifications enabled.
    subscribed: std::collections::BTreeSet<u16>,
    /// Outgoing isochronous SDU sequence number (media plane).
    pub(crate) audio_tx_sequence: u16,
    /// Decodes HID input reports as they are notified. Inert unless
    /// `hid_start` has found a HID service on this peer.
    hid: crate::device::HidHost,
}

impl CentralDevice {
    pub(crate) fn new(target: Address) -> Self {
        Self {
            target,
            client: GattClient::new(0, target),
            reassembler: AclReassembler::new(),
            phase: CentralPhase::Connecting,
            connect_requested: false,
            pending_ops: std::collections::VecDeque::new(),
            in_flight: None,
            values: std::collections::BTreeMap::new(),
            subscribed: std::collections::BTreeSet::new(),
            audio_tx_sequence: 0,
            hid: crate::device::HidHost::new(),
        }
    }

    /// Queue a read of the given value handle.
    pub(crate) fn queue_read(&mut self, value_handle: u16) {
        self.pending_ops.push_back(CentralOp::Read(value_handle));
    }

    /// Queue a write of `value` to the given value handle.
    pub(crate) fn queue_write(&mut self, value_handle: u16, value: Vec<u8>) {
        self.pending_ops
            .push_back(CentralOp::Write(value_handle, value));
    }

    /// Queue enabling notifications on the given value handle (writes its CCCD).
    pub(crate) fn queue_subscribe(&mut self, value_handle: u16) {
        self.pending_ops
            .push_back(CentralOp::Subscribe(value_handle));
    }

    /// True once the connection is up and its GATT has been discovered.
    ///
    /// These four accessors exist for [`WebSource`](web::WebSource), which is
    /// browser-only, so they are gated with it rather than carried as dead
    /// code in native builds.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn is_ready(&self) -> bool {
        self.phase == CentralPhase::Ready
    }

    /// True when every queued operation has been sent *and* answered — the
    /// only safe moment to act on the result of a sequence of writes, since
    /// a queued operation has not reached the peer yet.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn is_idle(&self) -> bool {
        self.pending_ops.is_empty() && self.in_flight.is_none()
    }

    /// The ACL connection handle, which a CIS is opened against.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn connection_handle(&self) -> u16 {
        self.client.connection_handle
    }

    /// The value handle of a discovered characteristic, by UUID.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn characteristic_handle(&self, uuid: crate::types::Uuid) -> Option<u16> {
        self.client
            .find_characteristic(uuid)
            .map(|characteristic| characteristic.value_handle)
    }

    /// Send the next queued operation when discovery is done and nothing is in
    /// flight.
    fn pump_ops(&mut self, channel: &HciChannel) {
        if self.phase != CentralPhase::Ready || self.in_flight.is_some() {
            return;
        }
        let Some(op) = self.pending_ops.pop_front() else {
            return;
        };
        let handle = self.client.connection_handle;
        let req = match &op {
            CentralOp::Read(h) => self.client.create_read_request(*h),
            CentralOp::Write(h, v) => self.client.create_write_request(*h, v),
            // The CCCD sits at value_handle + 1 in the standard layout.
            CentralOp::Subscribe(h) => self.client.create_write_request(h + 1, &[0x01, 0x00]),
        };
        let _ = send_acl(channel, handle, &req);
        self.in_flight = Some(op);
    }

    /// On the first tick, request a connection to the target advertiser.
    ///
    /// LE Create Connection takes 25 parameter bytes (Vol 4, Part E, Section
    /// 7.8.12). This used to send only the first 12 — everything from
    /// Own_Address_Type onwards was missing. The in-process controller reads
    /// the fields it needs at their fixed offsets and connected regardless,
    /// so every scene test passed; a real controller rejects the command
    /// outright and the central then sits in "connecting" having never
    /// transmitted.
    pub(crate) fn produce(&mut self, channel: &HciChannel) {
        if !self.connect_requested && self.phase == CentralPhase::Connecting {
            // A Zephyr/nRF controller has no public address in ROM; connecting
            // as own=public from 00:00:00:00:00:00 makes the peer drop the link
            // during negotiate (HCI status 0x3E). Set a static random address
            // (two MSBs = 0b11) and initiate as own=random, matching LeCentral.
            const RANDOM_ADDRESS: [u8; 6] = [0xC1, 0x00, 0x00, 0xC0, 0xDE, 0xF0];
            let _ = queue_command(channel, [0x05, 0x20], &RANDOM_ADDRESS); // LE Set Random Address
            let mut params = Vec::with_capacity(25);
            // Scan window == interval: initiate with a continuous scan so the
            // peer's connectable advert is never missed.
            params.extend_from_slice(&0x0060u16.to_le_bytes()); // scan interval, 60 ms
            params.extend_from_slice(&0x0060u16.to_le_bytes()); // scan window, 60 ms (continuous)
            params.push(0x00); // initiator filter policy: use the peer address
            params.push(0x00); // peer address type: public
            let mut peer = self.target.to_be_bytes();
            peer.reverse(); // little-endian on the wire
            params.extend_from_slice(&peer);
            params.push(0x01); // own address type: random (the address set above)
            params.extend_from_slice(&0x0018u16.to_le_bytes()); // min interval, 30 ms
            params.extend_from_slice(&0x0028u16.to_le_bytes()); // max interval, 50 ms
            params.extend_from_slice(&0x0000u16.to_le_bytes()); // max latency
            params.extend_from_slice(&0x0190u16.to_le_bytes()); // supervision timeout, 4 s
            params.extend_from_slice(&0x0000u16.to_le_bytes()); // min CE length
            params.extend_from_slice(&0x0000u16.to_le_bytes()); // max CE length
            debug_assert_eq!(params.len(), 25, "LE Create Connection is 25 bytes");
            let _ = queue_command(channel, [0x0D, 0x20], &params); // LE Create Connection
            self.connect_requested = true;
        }
        self.pump_ops(channel);
    }

    /// Consume one controller→host packet: a connection completion starts the
    /// discovery flow; ATT responses advance it.
    pub(crate) fn consume(&mut self, channel: &HciChannel, packet: &[u8]) {
        match packet.first() {
            Some(&h4_type::HCI_EVENT) => {
                // Parsed through the same typed view the peripheral host
                // uses, rather than a second hand-rolled copy: this event
                // was where a dropped field silently broke pairing, and one
                // parser is easier to keep right than two. The Enhanced
                // variant is accepted too — a resolving central gets that
                // one, and the old byte test (`packet[3] == 0x01`) ignored
                // it, so such a connection never started discovery.
                if let Some(HciEvent::LeConnectionComplete(event)) = HciEvent::parse_h4(packet)
                    && event.status == 0x00
                {
                    let handle = event.connection_handle.get() & 0x0FFF;
                    self.client = GattClient::new(handle, self.target);
                    self.phase = CentralPhase::ExchangingMtu;
                    let req = self.client.create_exchange_mtu_request(517);
                    let _ = send_acl(channel, handle, &req);
                }
            }
            Some(&h4_type::HCI_ACL_DATA) => {
                if let Some((header, payload)) = HciAclHeader::parse(&packet[1..]) {
                    let handle = header.handle();
                    let is_first = header.is_first_fragment();
                    if let Ok(Some(frame)) =
                        self.reassembler.push_fragment(handle, is_first, payload)
                        && let Some((_, att)) = L2capHeader::parse(&frame)
                    {
                        self.dispatch_att(channel, att);
                    }
                }
            }
            _ => {}
        }
    }

    /// Advance the discovery state machine, or handle a read/write/notification,
    /// on one ATT response.
    fn dispatch_att(&mut self, channel: &HciChannel, att: &[u8]) {
        let handle = self.client.connection_handle;
        let Some(&op) = att.first() else { return };
        let is_error = op == att_op::ERROR_RSP;

        // Notifications can arrive at any time, independent of the request FSM.
        // The typed header splits handle from value, so the value slice cannot
        // drift out of step with the offset the handle was read from.
        if op == att_op::HANDLE_VALUE_NTF
            && let Some((header, value)) = AttHandleValueHeader::parse(att)
        {
            let value_handle = header.handle.get();
            self.values.insert(value_handle, value.to_vec());
            // Decoded here rather than by polling `values`: consecutive input
            // reports overwrite each other in that map, and a HID host that
            // missed one would lose a keystroke or a click edge.
            self.hid.on_notification(value_handle, value);
            return;
        }

        match self.phase {
            CentralPhase::ExchangingMtu => {
                if op == att_op::EXCHANGE_MTU_RSP
                    && let Some((rsp, _)) = AttExchangeMtuRsp::parse(att)
                {
                    self.client
                        .on_exchange_mtu_response(rsp.server_rx_mtu.get(), 517);
                }
                self.phase = CentralPhase::DiscoveringServices;
                let req = self.client.create_discover_services_request(0x0001, 0xFFFF);
                let _ = send_acl(channel, handle, &req);
            }
            CentralPhase::DiscoveringServices => {
                if is_error {
                    self.start_characteristic_discovery(channel);
                } else if op == att_op::READ_BY_GROUP_TYPE_RSP {
                    let _ = self.client.on_discover_services_response(att);
                    let last_end = self.client.services.last().map_or(0xFFFF, |s| s.end_handle);
                    if last_end < 0xFFFF {
                        let req = self
                            .client
                            .create_discover_services_request(last_end + 1, 0xFFFF);
                        let _ = send_acl(channel, handle, &req);
                    } else {
                        self.start_characteristic_discovery(channel);
                    }
                }
            }
            CentralPhase::DiscoveringCharacteristics(i) => {
                if is_error {
                    self.next_characteristic_service(channel, i);
                } else if op == att_op::READ_BY_TYPE_RSP {
                    let svc_uuid = self.client.services[i].uuid;
                    let _ = self
                        .client
                        .on_discover_characteristics_response(svc_uuid, att);
                    let svc = &self.client.services[i];
                    let end = svc.end_handle;
                    let last = svc
                        .characteristics
                        .last()
                        .map_or(svc.start_handle, |c| c.value_handle);
                    if last < end {
                        let req = self
                            .client
                            .create_discover_characteristics_request(last + 1, end);
                        let _ = send_acl(channel, handle, &req);
                    } else {
                        self.next_characteristic_service(channel, i);
                    }
                }
            }
            CentralPhase::Ready => {
                // Response to the in-flight read / write / subscribe.
                if let Some(pending) = self.in_flight.take()
                    && !is_error
                {
                    match pending {
                        CentralOp::Read(h) if op == att_op::READ_RSP => {
                            self.values.insert(h, att[1..].to_vec());
                            self.hid.on_read(h, &att[1..]);
                        }
                        CentralOp::Subscribe(h) => {
                            self.subscribed.insert(h);
                        }
                        CentralOp::Read(_) | CentralOp::Write(..) => {}
                    }
                }
            }
            CentralPhase::Connecting => {}
        }
    }

    /// Begin characteristic discovery on the first service (or finish).
    fn start_characteristic_discovery(&mut self, channel: &HciChannel) {
        if self.client.services.is_empty() {
            self.phase = CentralPhase::Ready;
        } else {
            self.phase = CentralPhase::DiscoveringCharacteristics(0);
            self.discover_characteristics_for(channel, 0);
        }
    }

    /// Move to the next service's characteristics (or finish).
    fn next_characteristic_service(&mut self, channel: &HciChannel, i: usize) {
        let next = i + 1;
        if next < self.client.services.len() {
            self.phase = CentralPhase::DiscoveringCharacteristics(next);
            self.discover_characteristics_for(channel, next);
        } else {
            self.phase = CentralPhase::Ready;
        }
    }

    /// Send the characteristic-discovery request for `services[i]`.
    fn discover_characteristics_for(&mut self, channel: &HciChannel, i: usize) {
        let handle = self.client.connection_handle;
        let svc = &self.client.services[i];
        let req = self
            .client
            .create_discover_characteristics_request(svc.start_handle, svc.end_handle);
        let _ = send_acl(channel, handle, &req);
    }

    /// The discovered GATT as JSON: `{connected, peer, phase, services:[…]}`.
    pub(crate) fn status_json(&self) -> String {
        #[derive(serde::Serialize)]
        struct View {
            connected: bool,
            peer: String,
            phase: &'static str,
            services: Vec<Svc>,
        }
        #[derive(serde::Serialize)]
        struct Svc {
            uuid: String,
            characteristics: Vec<Chr>,
        }
        #[derive(serde::Serialize)]
        struct Chr {
            uuid: String,
            value_handle: u16,
            properties: u8,
            /// Latest value (read or notified) as uppercase hex, if any.
            value: Option<String>,
            /// Whether notifications are enabled on this characteristic.
            subscribed: bool,
        }
        let hex = |v: &[u8]| v.iter().map(|b| format!("{b:02X}")).collect::<String>();
        let view = View {
            connected: self.client.connection_handle != 0,
            peer: self.target.to_string(),
            phase: self.phase.label(),
            services: self
                .client
                .services
                .iter()
                .map(|s| Svc {
                    uuid: s.uuid.to_string(),
                    characteristics: s
                        .characteristics
                        .iter()
                        .map(|c| Chr {
                            uuid: c.uuid.to_string(),
                            value_handle: c.value_handle,
                            properties: c.properties,
                            value: self.values.get(&c.value_handle).map(|v| hex(v)),
                            subscribed: self.subscribed.contains(&c.value_handle),
                        })
                        .collect(),
                })
                .collect(),
        };
        serde_json::to_string(&view).unwrap_or_else(|_| "{}".to_string())
    }

    // --- HID host ----------------------------------------------------------
    // The HOGP client half: a real computer's response to discovering a
    // keyboard. Kept here rather than in a second central because everything
    // it needs — the discovered GATT, the read/write queue, the notification
    // path — already exists on this type; only the decoding is new, and that
    // lives in [`crate::device::HidHost`].

    /// Starts driving the peer as a HID device: reads its Report Map and
    /// subscribes to every input Report. Returns false if discovery has not
    /// finished or the peer exposes no HID service.
    pub(crate) fn hid_start(&mut self) -> bool {
        if self.phase != CentralPhase::Ready {
            return false;
        }
        let plan = self.hid.plan(&self.client.services);
        if plan.is_empty() {
            return false;
        }
        // The Report Map is read first so the reports that follow can be
        // decoded; the operation queue preserves that order.
        for handle in plan.read {
            self.queue_read(handle);
        }
        for handle in plan.subscribe {
            self.queue_subscribe(handle);
        }
        true
    }

    /// The input decoded since the last call, plus what the host has learned
    /// about the device: `{kind, ready, report_map, report, report_handle,
    /// events:[…]}`. Draining, so a page polling each frame sees every event
    /// exactly once. `report` is the raw bytes those events were decoded
    /// from, so a caller can show the wire beside its meaning.
    pub(crate) fn hid_events_json(&mut self) -> String {
        #[derive(serde::Serialize)]
        struct View {
            kind: &'static str,
            ready: bool,
            /// The Report Map as uppercase hex, once read.
            report_map: Option<String>,
            /// The most recent input report, space-separated hex.
            report: Option<String>,
            /// The value handle that report arrived on.
            report_handle: Option<u16>,
            events: Vec<crate::device::HidEvent>,
        }
        let last = self.hid.last_report();
        let view = View {
            kind: self.hid.kind().label(),
            ready: self.hid.is_ready(),
            report_map: self
                .hid
                .report_map()
                .map(|m| m.iter().map(|b| format!("{b:02X}")).collect()),
            report: last.map(|(_, bytes)| {
                bytes
                    .iter()
                    .map(|b| format!("{b:02X}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
            report_handle: last.map(|(handle, _)| handle),
            events: self.hid.drain_events(),
        };
        serde_json::to_string(&view).unwrap_or_else(|_| "{}".to_string())
    }
}
