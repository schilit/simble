// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The **BR/EDR host**: the layer that turns simble's Classic protocol
//! modules ([`crate::classic`]) and its L2CAP Classic channel manager
//! ([`crate::l2cap::classic::ClassicChannelManager`]) into a device a real stack can
//! find, connect to, and use.
//!
//! Those protocol modules were complete but unreachable: nothing accepted a
//! page, nothing routed ACL data to L2CAP signalling, and nothing answered
//! SDP. This host closes that gap, mirroring [`crate::device::LeHost`] — it
//! is transport-free, taking H4 packets in and returning the H4 packets to
//! send back, so the same host runs over netsim, a dongle, or a test harness.
//!
//! What it covers today: discoverable/connectable bring-up, accepting an
//! inbound ACL connection, the L2CAP signalling handshake (connect →
//! configure → open), and dispatching channel data to a protocol handler —
//! with SDP wired up, which is what lets a peer discover any profile at all.
//! Profile channels above SDP (RFCOMM/SPP, HID, AVDTP) plug into the same
//! [`ProtocolHandler`] seam; see the module tests for the shape.

use crate::classic::sdp::{
    DataElement, SDP_PSM, SdpServer, SdpUuid, Service, ServiceAttribute, attribute_id,
};
use crate::l2cap::classic::ClassicChannelManager;
use crate::l2cap::{L2capHeader, cid};
use crate::packets::{
    ConfigurationRequestHeader, ConfigurationResponseHeader, ConnectionRequestHeader,
    ConnectionResponseHeader, HciEvent, L2capSignalingHeader, signaling_code,
};
use crate::types::{Address, SimbleError};
use zerocopy::{FromBytes, IntoBytes};

/// HCI command opcodes this host issues (Vol 4, Part E, Section 7.3).
mod opcode {
    /// Reset.
    pub const RESET: [u8; 2] = [0x03, 0x0C];
    /// Set Event Mask.
    pub const SET_EVENT_MASK: [u8; 2] = [0x01, 0x0C];
    /// Write Local Name.
    pub const WRITE_LOCAL_NAME: [u8; 2] = [0x13, 0x0C];
    /// Write Class of Device.
    pub const WRITE_CLASS_OF_DEVICE: [u8; 2] = [0x24, 0x0C];
    /// Write Scan Enable.
    pub const WRITE_SCAN_ENABLE: [u8; 2] = [0x1A, 0x0C];
    /// Write Simple Pairing Mode.
    pub const WRITE_SIMPLE_PAIRING_MODE: [u8; 2] = [0x56, 0x0C];
    /// Accept Connection Request.
    pub const ACCEPT_CONNECTION_REQUEST: [u8; 2] = [0x09, 0x04];
    /// Inquiry — the initiator's half of discovery.
    pub const INQUIRY: [u8; 2] = [0x01, 0x04];
    /// Create Connection: page a device found by inquiry.
    pub const CREATE_CONNECTION: [u8; 2] = [0x05, 0x04];
    /// Disconnect.
    pub const DISCONNECT: [u8; 2] = [0x06, 0x04];
    /// Remote Name Request: ask a discovered device what it is called.
    pub const REMOTE_NAME_REQUEST: [u8; 2] = [0x19, 0x04];
    /// Write Inquiry Mode: choose which of the three inquiry-result event
    /// forms the controller reports with.
    pub const WRITE_INQUIRY_MODE: [u8; 2] = [0x45, 0x0C];
}

/// Inquiry Mode values (Vol 4, Part E, Section 7.3.49) — which event form
/// the controller reports inquiry results in. This is a *host* setting: it
/// changes nothing on the air, only which of three event layouts arrives.
pub mod inquiry_mode {
    /// Inquiry Result (event 0x02). The reset default.
    pub const STANDARD: u8 = 0x00;
    /// Inquiry Result with RSSI (event 0x22).
    pub const WITH_RSSI: u8 = 0x01;
    /// Extended Inquiry Result (event 0x2F), which carries the peer's EIR —
    /// its name and service UUIDs — without a Remote Name Request. This is
    /// what Android asks for, which is why a phone can list device names
    /// before it has paged anything.
    pub const WITH_EXTENDED: u8 = 0x02;
}

/// HCI event codes this host reacts to beyond the ones [`HciEvent`] gives a
/// typed variant for.
mod event_code {
    /// Inquiry Complete — discovery is over.
    pub const INQUIRY_COMPLETE: u8 = 0x01;
    /// Inquiry Result — one or more devices answered.
    pub const INQUIRY_RESULT: u8 = 0x02;
    /// Remote Name Request Complete.
    pub const REMOTE_NAME_REQUEST_COMPLETE: u8 = 0x07;
    /// Inquiry Result with RSSI — the same information one byte to the left,
    /// plus an RSSI octet.
    pub const INQUIRY_RESULT_WITH_RSSI: u8 = 0x22;
    /// Extended Inquiry Result — one response, with 240 octets of EIR.
    pub const EXTENDED_INQUIRY_RESULT: u8 = 0x2F;
}

/// The General/Unlimited Inquiry Access Code, 0x9E8B33 — the LAP every
/// discoverable device listens on. A host that inquires on any other LAP
/// finds only devices in limited discoverable mode.
const GIAC: [u8; 3] = [0x33, 0x8B, 0x9E];

/// The name carried in an Extended Inquiry Response, if it carries one.
///
/// EIR uses the same AD structure encoding as LE advertising data (CSS Part
/// A), which is why this walks it with the GAP parser rather than a second
/// copy. A shortened name counts: it is what the peer chose to fit, and a
/// list entry reading "Bumble SP" beats one reading "unknown device".
fn name_from_eir(eir: &[u8]) -> Option<String> {
    use crate::gap::ad_type;
    let mut shortened = None;
    for (kind, value) in crate::gap::advertising::ad_structures(eir) {
        match kind {
            ad_type::COMPLETE_LOCAL_NAME => {
                return Some(String::from_utf8_lossy(value).into_owned());
            }
            ad_type::SHORTENED_LOCAL_NAME if shortened.is_none() => {
                shortened = Some(String::from_utf8_lossy(value).into_owned());
            }
            _ => {}
        }
    }
    shortened
}

/// A device this host found by inquiry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// The device's address.
    pub address: Address,
    /// Its Class of Device, which is what a UI turns into an icon.
    pub class_of_device: [u8; 3],
    /// Its name, once a Remote Name Request has been answered. An inquiry
    /// alone never carries a name — that is why phones show "unknown device"
    /// while they are still resolving one.
    pub name: Option<String>,
}

/// Scan Enable values (Vol 4, Part E, Section 7.3.18). Inquiry scan makes a
/// device *discoverable*; page scan makes it *connectable*. A peripheral
/// needs both to appear in Android's pairing list and then be connected to.
pub mod scan_enable {
    /// No scans — invisible and unconnectable.
    pub const NONE: u8 = 0x00;
    /// Inquiry scan only: discoverable, not connectable.
    pub const INQUIRY_ONLY: u8 = 0x01;
    /// Page scan only: connectable, not discoverable.
    pub const PAGE_ONLY: u8 = 0x02;
    /// Both — what a pairable peripheral advertises with.
    pub const INQUIRY_AND_PAGE: u8 = 0x03;
}

/// The default L2CAP MTU this host offers on its channels.
const DEFAULT_L2CAP_MTU: u16 = 672;

/// Builds one H4 HCI command packet.
fn command(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(4 + params.len());
    packet.push(crate::transport::h4_type::HCI_COMMAND);
    packet.extend_from_slice(&opcode);
    packet.push(params.len() as u8);
    packet.extend_from_slice(params);
    packet
}

/// Wraps an L2CAP PDU in an H4 ACL packet for `handle`.
fn acl_packet(handle: u16, l2cap: &[u8]) -> Vec<u8> {
    use crate::l2cap::{AclPacketBoundary, HciAclHeader};
    let header = HciAclHeader::new(
        handle,
        AclPacketBoundary::FirstNonFlushable,
        l2cap.len() as u16,
    );
    let mut packet = Vec::with_capacity(5 + l2cap.len());
    packet.push(crate::transport::h4_type::HCI_ACL_DATA);
    packet.extend_from_slice(header.as_bytes());
    packet.extend_from_slice(l2cap);
    packet
}

/// Frames an L2CAP signalling PDU (code + identifier + payload) on the
/// BR/EDR signalling channel.
fn signaling_pdu(code: u8, identifier: u8, payload: &[u8]) -> Vec<u8> {
    let header = L2capSignalingHeader {
        code,
        identifier,
        length: (payload.len() as u16).into(),
    };
    let mut body = Vec::with_capacity(4 + payload.len());
    body.extend_from_slice(header.as_bytes());
    body.extend_from_slice(payload);
    L2capHeader::serialize(cid::BR_SIGNALING, &body)
}

/// What a profile does with data on an open L2CAP channel: it sees the
/// payload and returns whatever should be sent back on the same channel.
/// SDP is one of these; RFCOMM, HID and AVDTP fit the same seam.
pub trait ProtocolHandler: std::fmt::Debug {
    /// The PSM this handler serves.
    fn psm(&self) -> u16;

    /// Handles one inbound SDU; returns the SDUs to reply with, in order.
    ///
    /// A reply is a *list* because a single inbound frame can oblige a
    /// profile to send several: an RFCOMM PN command is answered with a PN
    /// response, and an MSC exchange crosses in both directions, so one SDU
    /// in can mean two or three out.
    fn on_data(&mut self, data: &[u8], peer_mtu: u16) -> Vec<Vec<u8>>;

    /// SDUs the profile wants to send without having been prompted by the
    /// peer — a device writing bytes to an open serial port, say. The host
    /// drains this after every packet and whenever it is polled, so data a
    /// device queues is not stuck waiting for the peer to speak first.
    ///
    /// This is also how a *client* profile speaks first at all: an RFCOMM
    /// initiator's SABM and an SDP query are both unprompted, so they leave
    /// here. `peer_mtu` is the negotiated L2CAP MTU of the channel being
    /// polled, which a profile that must size frames to the channel (RFCOMM)
    /// needs and cannot learn any other way before the first inbound frame.
    fn poll_output(&mut self, peer_mtu: u16) -> Vec<Vec<u8>> {
        let _ = peer_mtu;
        Vec::new()
    }

    /// The L2CAP channel this profile was speaking on has gone away — the
    /// peer disconnected it, or the ACL dropped underneath it.
    ///
    /// A profile that keeps per-session state must discard it here. RFCOMM
    /// is the reason this exists: a multiplexer session is bound to the
    /// L2CAP connection that carries it (RFCOMM spec §5.1), so a session
    /// left over from a departed peer would swallow the next peer's SABM on
    /// DLCI 0 and the device would answer nothing at all.
    fn on_channel_closed(&mut self) {}
}

/// The SDP server as a channel handler — the one profile every other
/// profile depends on, since a peer discovers services through it.
#[derive(Debug, Default)]
pub struct SdpHandler {
    server: SdpServer,
}

impl SdpHandler {
    /// Wraps a configured SDP server (its records are already registered).
    pub fn new(server: SdpServer) -> Self {
        Self { server }
    }

    /// The underlying server, for registering records.
    pub fn server_mut(&mut self) -> &mut SdpServer {
        &mut self.server
    }
}

impl std::fmt::Debug for SdpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdpServer")
            .field("records", &self.service_records.len())
            .finish()
    }
}

impl ProtocolHandler for SdpHandler {
    fn psm(&self) -> u16 {
        SDP_PSM
    }

    fn on_data(&mut self, data: &[u8], peer_mtu: u16) -> Vec<Vec<u8>> {
        vec![self.server.handle_request(data, peer_mtu)]
    }
}

/// What an [`SdpQueryHandler`] learned from a peer's SDP server.
#[derive(Debug, Default)]
pub struct SdpQueryResults {
    /// Whether a response — even an empty or failed one — has arrived. This
    /// is what tells a driver "discovery finished and found nothing" apart
    /// from "discovery has not answered yet", which are the same thing to
    /// anyone only looking at `rfcomm_channels`.
    pub answered: bool,
    /// The error code, if the peer's server answered with an SDP error.
    pub error: Option<u16>,
    /// RFCOMM server channel numbers advertised, with their service classes.
    pub rfcomm_channels: Vec<(u8, Vec<SdpUuid>)>,
    /// The peer kept asking us to continue past the watchdog, so what is in
    /// `rfcomm_channels` is a prefix of the answer rather than the answer.
    /// Reported rather than hidden: a truncated SDP answer looks exactly
    /// like a peer with fewer services, and acting on it opens a DLC on a
    /// channel nobody is listening to.
    pub truncated: bool,
    /// The profile version from the matched record's
    /// BluetoothProfileDescriptorList — the *profile* the peer implements,
    /// which is not the same claim as the feature bits it later makes over
    /// the channel, and worth being able to compare against them.
    pub profile_version: Option<u16>,
    /// Bytes of request written, and of response read. Cheap to record and
    /// the only evidence a UI has that the search was a real transaction
    /// rather than a lookup in a table.
    pub request_bytes: usize,
    /// See [`Self::request_bytes`].
    pub response_bytes: usize,
}

impl SdpQueryResults {
    /// The server channel of the service advertising `uuid`, if any. This is
    /// the number an RFCOMM client must open a DLC on; opening any other is
    /// refused with DM.
    pub fn channel_for(&self, uuid: SdpUuid) -> Option<u8> {
        self.rfcomm_channels
            .iter()
            .find(|(_, classes)| classes.contains(&uuid))
            .map(|(channel, _)| *channel)
    }
}

/// A shared [`SdpQueryResults`]: the handler writes, the driver reads.
pub type SharedSdpQueryResults = std::sync::Arc<std::sync::Mutex<SdpQueryResults>>;

/// The SDP **client** as a channel handler: it asks the peer what it offers
/// and records the answer.
///
/// It builds [`SdpPdu`]s directly rather than using [`crate::classic::sdp::SdpClient`].
/// That client takes a `FnMut(&[u8]) -> Vec<u8>` transport — it *blocks* on
/// each response — which cannot work above an event loop where the answer
/// comes back several ticks later on an HCI channel. What it does share with
/// that client is **continuation**: a server whose answer does not fit in
/// one response returns a prefix plus a continuation state, and expects the
/// identical request back with those bytes echoed in. Bumble's SDP server
/// caps each response at the negotiated L2CAP MTU less nine, so any peer
/// with more than a couple of dozen records continues — as every phone
/// does. Ignoring the field yields a truncated byte string that still looks
/// like a well-formed response, which is exactly why the omission survived
/// every test against a peer whose records happened to fit.
#[derive(Debug)]
pub struct SdpQueryHandler {
    /// The service class UUID to search for.
    uuid: SdpUuid,
    /// Whether the query has been sent.
    sent: bool,
    /// Attribute-list bytes accumulated so far, across continuations. The
    /// data element they form is only parseable once the last chunk has
    /// arrived — a prefix of a SEQUENCE is not a SEQUENCE.
    partial: Vec<u8>,
    /// How many continuation round-trips this query has taken.
    continuations: u32,
    /// Where the answer is put for the driver to read.
    results: SharedSdpQueryResults,
    /// Whether to ask for the BluetoothProfileDescriptorList as well.
    ///
    /// Off by default, and deliberately: `tests/classic_foreign_bytes_test`
    /// pins the exact request bytes a real Bumble server accepted, and that
    /// session asked for two attributes. A third would still be a valid
    /// request, but nothing has ever proved a foreign server answers it —
    /// so the verified request stays the default and the extra attribute is
    /// opt-in for callers that need the version and can live with that.
    want_profile_version: bool,
}

impl SdpQueryHandler {
    /// A client that searches for services advertising `uuid`, and the
    /// results handle to read the answer from.
    pub fn searching(uuid: SdpUuid) -> (Self, SharedSdpQueryResults) {
        let results: SharedSdpQueryResults =
            std::sync::Arc::new(std::sync::Mutex::new(SdpQueryResults::default()));
        (
            Self {
                uuid,
                sent: false,
                partial: Vec::new(),
                continuations: 0,
                results: results.clone(),
                want_profile_version: false,
            },
            results,
        )
    }

    /// As [`Self::searching`], but also asks for the profile version. See
    /// [`Self::want_profile_version`] for why that is not the default.
    pub fn searching_with_profile_version(uuid: SdpUuid) -> (Self, SharedSdpQueryResults) {
        let (mut handler, results) = Self::searching(uuid);
        handler.want_profile_version = true;
        (handler, results)
    }

    /// The ServiceSearchAttributeRequest this client asks with. The search
    /// pattern and attribute list must be *identical* on a continuation —
    /// the server matches the request, not just the state bytes — so the
    /// same builder produces both the first request and every follow-up.
    fn request(&self, transaction_id: u16, continuation_state: Vec<u8>) -> Vec<u8> {
        // Ask for the protocol descriptor (which carries the RFCOMM channel)
        // the service class list (which says what the channel is *for*) and
        // the profile descriptor (which says which version of that profile).
        // Asking for both in one request is what makes the answer actionable.
        crate::classic::sdp::SdpPdu::ServiceSearchAttributeRequest {
            transaction_id,
            service_search_pattern: DataElement::sequence(vec![DataElement::uuid(self.uuid)]),
            maximum_attribute_byte_count: 0xFFFF,
            attribute_id_list: DataElement::sequence({
                let mut ids = vec![
                    DataElement::unsigned_integer_16(attribute_id::PROTOCOL_DESCRIPTOR_LIST),
                    DataElement::unsigned_integer_16(attribute_id::SERVICE_CLASS_ID_LIST),
                ];
                if self.want_profile_version {
                    ids.push(DataElement::unsigned_integer_16(
                        attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
                    ));
                }
                ids
            }),
            continuation_state,
        }
        .to_bytes()
    }

    /// Pulls the RFCOMM server channel, service classes and profile version
    /// out of one
    /// record's attribute list. The protocol descriptor is
    /// `[[L2CAP], [RFCOMM, channel]]`, so the channel is the second element
    /// of the second layer; the profile descriptor is `[[UUID, version]]`.
    fn read_record(attributes: &[ServiceAttribute]) -> Option<(u8, Vec<SdpUuid>, Option<u16>)> {
        let mut channel = None;
        let mut classes = Vec::new();
        let mut version = None;
        for attribute in attributes {
            match attribute.id {
                attribute_id::PROTOCOL_DESCRIPTOR_LIST => {
                    if let Some(rfcomm) = attribute
                        .value
                        .as_sequence()
                        .and_then(|layers| layers.get(1))
                        .and_then(DataElement::as_sequence)
                        && let Some((value, _)) =
                            rfcomm.get(1).and_then(DataElement::as_unsigned_integer)
                    {
                        channel = Some(value as u8);
                    }
                }
                attribute_id::SERVICE_CLASS_ID_LIST => {
                    if let Some(list) = attribute.value.as_sequence() {
                        classes = list.iter().filter_map(DataElement::as_uuid).collect();
                    }
                }
                attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST => {
                    if let Some((value, _)) = attribute
                        .value
                        .as_sequence()
                        .and_then(<[DataElement]>::first)
                        .and_then(DataElement::as_sequence)
                        .and_then(|profile| profile.get(1))
                        .and_then(DataElement::as_unsigned_integer)
                    {
                        version = Some(value as u16);
                    }
                }
                _ => {}
            }
        }
        channel.map(|channel| (channel, classes, version))
    }
}

impl ProtocolHandler for SdpQueryHandler {
    fn psm(&self) -> u16 {
        SDP_PSM
    }

    fn on_data(&mut self, data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        use crate::classic::sdp::{SDP_CONTINUATION_WATCHDOG, SdpPdu, is_final_continuation};
        // Clone the handle before locking: the guard would otherwise borrow
        // `self` for as long as it lives, and accumulating into `self.partial`
        // needs `&mut self`.
        let shared = self.results.clone();
        let Ok(mut results) = shared.lock() else {
            return Vec::new();
        };
        match SdpPdu::parse(data) {
            Some(SdpPdu::ServiceSearchAttributeResponse {
                transaction_id,
                attribute_lists,
                continuation_state,
            }) => {
                self.partial.extend_from_slice(&attribute_lists);
                if !is_final_continuation(&continuation_state) {
                    self.continuations += 1;
                    if self.continuations > SDP_CONTINUATION_WATCHDOG {
                        // A server that never finishes is a server we stop
                        // asking. Say the answer is partial rather than
                        // pretending the prefix is the whole database.
                        results.answered = true;
                        results.truncated = true;
                        return Vec::new();
                    }
                    // The same request again, with the server's state echoed
                    // back. This is the whole of the fix: without it the
                    // prefix above is treated as the entire answer.
                    return vec![self.request(transaction_id, continuation_state)];
                }
                results.answered = true;
                results.response_bytes += data.len();
                let records = DataElement::from_bytes(&self.partial)
                    .as_ref()
                    .and_then(DataElement::as_sequence)
                    .map(<[DataElement]>::to_vec)
                    .unwrap_or_default();
                self.partial = Vec::new();
                for record in &records {
                    let Some(items) = record.as_sequence() else {
                        continue;
                    };
                    let attributes = ServiceAttribute::list_from_data_elements(items);
                    if let Some((channel, classes, version)) = Self::read_record(&attributes) {
                        results.rfcomm_channels.push((channel, classes));
                        results.profile_version = results.profile_version.or(version);
                    }
                }
            }
            Some(SdpPdu::ErrorResponse { error_code, .. }) => {
                results.answered = true;
                results.response_bytes += data.len();
                results.error = Some(error_code);
            }
            // Anything else is not an answer to what we asked. Saying so
            // beats recording a success we did not get.
            _ => {}
        }
        Vec::new()
    }

    fn poll_output(&mut self, _peer_mtu: u16) -> Vec<Vec<u8>> {
        if self.sent {
            return Vec::new();
        }
        self.sent = true;
        // A null continuation state — one zero byte — opens the exchange.
        let bytes = self.request(1, vec![0x00]);
        if let Ok(mut results) = self.results.lock() {
            results.request_bytes += bytes.len();
        }
        vec![bytes]
    }

    fn on_channel_closed(&mut self) {
        // A fresh channel means a fresh peer, so the question must be asked
        // again — a stale `sent` would leave the next peer never queried,
        // and a stale `partial` would splice one peer's records onto
        // another's.
        self.sent = false;
        self.partial = Vec::new();
        self.continuations = 0;
    }
}

/// What the DLC underneath a port negotiated, and how much of its credit
/// window is left right now.
///
/// The [`Dlc`](crate::classic::rfcomm::Dlc) itself belongs to the
/// [`RfcommHandler`], which a port holder cannot reach — the handler is a
/// `Box<dyn ProtocolHandler>` inside the host. So the handler mirrors these
/// numbers out to the port after every frame. Without this, credit
/// accounting is a thing the stack does that nothing can observe.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DlcWindow {
    /// The data link's DLCI — server channel × 2, plus the direction bit.
    pub dlci: u8,
    /// Largest frame this end may send, as PN settled it.
    pub tx_max_frame_size: u16,
    /// Largest frame this end will accept.
    pub rx_max_frame_size: u16,
    /// Credits granted to the peer when the DLC opened.
    pub rx_initial_credits: u8,
    /// Credits this end holds now: how many more frames it may send before
    /// the peer has to top it up.
    pub tx_credits: u8,
    /// Credits the peer holds now.
    pub rx_credits: u8,
}

/// The device-facing end of an RFCOMM serial port: what arrived from the
/// peer, and what the device wants to send back.
///
/// This mirrors the LE side's `VirtualDevice::audio_rx` — a queue plus an
/// accessor — but lives behind an `Arc<Mutex<_>>` because the handler
/// itself is owned as a `Box<dyn ProtocolHandler>` inside the host, where a
/// caller cannot reach it. Holding a clone of the port is how a scripted
/// device, a test, or an example talks to the serial connection.
#[derive(Debug, Default)]
pub struct RfcommPort {
    /// Payloads received from the peer, oldest first.
    received: std::collections::VecDeque<Vec<u8>>,
    /// Payloads the device wants to send, oldest first.
    outbound: std::collections::VecDeque<Vec<u8>>,
    /// The DLCI of the open data link, once the peer has opened one.
    open_dlci: Option<u8>,
    /// When set, every received payload is queued straight back — the
    /// simplest demonstrable behaviour for a serial port, and what a
    /// terminal app on a phone will show working.
    echo: bool,
    /// Total payloads received, which survives draining so a test or a UI
    /// can tell "nothing yet" from "nothing since I last looked".
    received_count: usize,
    /// The data link's negotiated parameters and live credit window, as the
    /// handler last mirrored them out.
    window: Option<DlcWindow>,
}

impl RfcommPort {
    /// A port that echoes everything it receives.
    pub fn echoing() -> Self {
        Self {
            echo: true,
            ..Self::default()
        }
    }

    /// Whether a peer currently has a data link open.
    pub fn is_open(&self) -> bool {
        self.open_dlci.is_some()
    }

    /// How many payloads have arrived in total, including drained ones.
    pub fn received_count(&self) -> usize {
        self.received_count
    }

    /// Drains what the peer sent, oldest first.
    pub fn take_received(&mut self) -> Vec<Vec<u8>> {
        self.received.drain(..).collect()
    }

    /// Queues `data` to send to the peer. It leaves on the next host packet
    /// or poll; nothing is sent if no data link is open.
    pub fn write(&mut self, data: impl Into<Vec<u8>>) {
        self.outbound.push_back(data.into());
    }

    /// The DLC's negotiated frame sizes and its credit window as of the last
    /// frame, or `None` before a data link exists.
    pub fn window(&self) -> Option<DlcWindow> {
        self.window
    }
}

/// A shared [`RfcommPort`]: the handler holds one clone, the device another.
pub type SharedRfcommPort = std::sync::Arc<std::sync::Mutex<RfcommPort>>;

/// RFCOMM as a channel handler — the serial transport that SPP, and every
/// other RFCOMM-based profile, rides on (ETSI TS 07.10; Bluetooth RFCOMM
/// 1.1).
///
/// The multiplexer is created lazily on the first frame because it must be
/// sized to the L2CAP channel's negotiated MTU, which is not known until the
/// channel is open.
#[derive(Debug)]
pub struct RfcommHandler {
    multiplexer: Option<crate::classic::rfcomm::Multiplexer>,
    /// Server channel numbers to accept DLC opens on, with their frame size
    /// and initial credits.
    listen: Vec<(u8, u16, u8)>,
    port: SharedRfcommPort,
    /// When set, this end *drives* the session rather than answering one: it
    /// starts the multiplexer and opens a DLC on this server channel without
    /// waiting to be asked. `None` is the responder — the original
    /// behaviour, and what a peripheral wants.
    open_channel: Option<u8>,
    /// Whether the initiator has sent its SABM on DLCI 0 yet.
    started: bool,
    /// Whether the initiator has asked for its data DLC yet.
    dlc_requested: bool,
}

impl RfcommHandler {
    /// Listens on `channel` (a server channel number, 1-30) — the same
    /// number the SDP record advertises, or a peer will open a DLC that is
    /// refused with DM.
    pub fn new(channel: u8, port: SharedRfcommPort) -> Self {
        Self {
            multiplexer: None,
            // 127-byte frames and 7 credits: within the smallest L2CAP MTU a
            // peer may negotiate, so a DLC never has to fragment.
            listen: vec![(channel, 127, 7)],
            port,
            open_channel: None,
            started: false,
            dlc_requested: false,
        }
    }

    /// The initiating end: as soon as the L2CAP channel opens this brings the
    /// multiplexer up and opens a DLC on `channel` — the server channel the
    /// peer's SDP record advertised.
    pub fn initiating(channel: u8, port: SharedRfcommPort) -> Self {
        Self {
            open_channel: Some(channel),
            ..Self::new(channel, port)
        }
    }

    /// A handler and the port it serves, for the common case where the
    /// caller wants both.
    pub fn echoing(channel: u8) -> (Self, SharedRfcommPort) {
        let port: SharedRfcommPort =
            std::sync::Arc::new(std::sync::Mutex::new(RfcommPort::echoing()));
        (Self::new(channel, port.clone()), port)
    }

    /// An initiating handler and its port. The port does not echo: an
    /// initiator is the one asking, so echoing would bounce its own bytes
    /// back for ever.
    pub fn initiator(channel: u8) -> (Self, SharedRfcommPort) {
        let port: SharedRfcommPort =
            std::sync::Arc::new(std::sync::Mutex::new(RfcommPort::default()));
        (Self::initiating(channel, port.clone()), port)
    }

    /// This end's multiplexer, created on first use — it must be sized to the
    /// L2CAP channel's negotiated MTU, which is not known until the channel
    /// is open.
    fn multiplexer_for(&mut self, peer_mtu: u16) -> &mut crate::classic::rfcomm::Multiplexer {
        use crate::classic::rfcomm::{Multiplexer, Role};
        let role = if self.open_channel.is_some() {
            Role::Initiator
        } else {
            Role::Responder
        };
        let listen = self.listen.clone();
        self.multiplexer.get_or_insert_with(|| {
            let mut multiplexer = Multiplexer::new(role, peer_mtu);
            for (channel, max_frame_size, credits) in listen {
                multiplexer.listen(channel, max_frame_size, credits);
            }
            multiplexer
        })
    }

    /// Move the initiating end's handshake along one step: bring the
    /// multiplexer up, then — once DLCI 0 is established — open the data DLC.
    ///
    /// Split into steps rather than done in one go because each step needs
    /// the peer's answer first: the SABM must be acknowledged with UA before
    /// a PN on any other DLCI means anything.
    fn drive_initiator(&mut self, peer_mtu: u16) -> Vec<Vec<u8>> {
        let Some(channel) = self.open_channel else {
            return Vec::new();
        };
        if !self.started {
            self.started = true;
            let multiplexer = self.multiplexer_for(peer_mtu);
            return multiplexer
                .start()
                .map(|sabm| vec![sabm])
                .unwrap_or_default();
        }
        if self.dlc_requested {
            return Vec::new();
        }
        let Some(multiplexer) = self.multiplexer.as_mut() else {
            return Vec::new();
        };
        if !multiplexer.is_connected() {
            return Vec::new();
        }
        self.dlc_requested = true;
        multiplexer
            .open_dlc(channel, 127, 7)
            .map(|pn| vec![pn])
            .unwrap_or_default()
    }

    /// Applies the multiplexer's events to the port, returning any frames
    /// the events themselves oblige us to send.
    fn apply_events(&mut self, events: Vec<crate::classic::rfcomm::MultiplexerEvent>) {
        use crate::classic::rfcomm::MultiplexerEvent;
        let Ok(mut port) = self.port.lock() else {
            return;
        };
        for event in events {
            match event {
                MultiplexerEvent::DlcOpened(dlci) => port.open_dlci = Some(dlci),
                MultiplexerEvent::DlcClosed(dlci) if port.open_dlci == Some(dlci) => {
                    port.open_dlci = None;
                }
                MultiplexerEvent::Disconnected => port.open_dlci = None,
                MultiplexerEvent::DataReceived(_, data) => {
                    port.received_count += 1;
                    if port.echo {
                        port.outbound.push_back(data.clone());
                    }
                    port.received.push_back(data);
                }
                _ => {}
            }
        }
    }

    /// Copies the open DLC's negotiated sizes and live credit counts out to
    /// the port, so the device end can see the flow control it is subject to.
    fn mirror_window(&mut self) {
        let Some(multiplexer) = self.multiplexer.as_ref() else {
            return;
        };
        let Ok(mut port) = self.port.lock() else {
            return;
        };
        port.window = port.open_dlci.and_then(|dlci| {
            multiplexer.dlcs.get(&dlci).map(|dlc| DlcWindow {
                dlci,
                tx_max_frame_size: dlc.tx_max_frame_size,
                rx_max_frame_size: dlc.rx_max_frame_size,
                rx_initial_credits: dlc.rx_initial_credits,
                tx_credits: dlc.tx_credits,
                rx_credits: dlc.rx_credits,
            })
        });
    }

    /// Turns whatever the device queued into UIH frames on the open DLC.
    fn drain_outbound(&mut self) -> Vec<Vec<u8>> {
        let Some(multiplexer) = self.multiplexer.as_mut() else {
            return Vec::new();
        };
        let (dlci, payloads) = {
            let Ok(mut port) = self.port.lock() else {
                return Vec::new();
            };
            // Without an open data link there is nowhere to send; the data
            // stays queued rather than being dropped.
            let Some(dlci) = port.open_dlci else {
                return Vec::new();
            };
            (dlci, port.outbound.drain(..).collect::<Vec<_>>())
        };
        payloads
            .iter()
            .filter_map(|payload| multiplexer.write(dlci, payload).ok())
            .flatten()
            .collect()
    }
}

impl ProtocolHandler for RfcommHandler {
    fn psm(&self) -> u16 {
        crate::classic::rfcomm::RFCOMM_PSM
    }

    fn on_data(&mut self, data: &[u8], peer_mtu: u16) -> Vec<Vec<u8>> {
        let multiplexer = self.multiplexer_for(peer_mtu);
        // A malformed frame or FCS mismatch is the peer's problem: drop it
        // rather than tearing down a working session.
        let Ok((mut frames, events)) = multiplexer.receive(data) else {
            return Vec::new();
        };
        self.apply_events(events);
        // An initiator's handshake advances on what just arrived: the UA(0)
        // answering its SABM is what lets it ask for a DLC.
        frames.extend(self.drive_initiator(peer_mtu));
        frames.extend(self.drain_outbound());
        self.mirror_window();
        frames
    }

    fn poll_output(&mut self, peer_mtu: u16) -> Vec<Vec<u8>> {
        let mut out = self.drive_initiator(peer_mtu);
        out.extend(self.drain_outbound());
        self.mirror_window();
        out
    }

    fn on_channel_closed(&mut self) {
        // Drop the session so the next peer's SABM builds a fresh one. The
        // port survives: it is the device's, not the peer's, and anything a
        // device queued while nobody was connected stays queued for whoever
        // connects next.
        self.multiplexer = None;
        self.started = false;
        self.dlc_requested = false;
        if let Ok(mut port) = self.port.lock() {
            port.open_dlci = None;
            port.window = None;
        }
    }
}

/// Builds a Serial Port Profile service record for `rfcomm_channel`, named
/// `name` — the record a peer reads to learn that this device speaks SPP and
/// on which RFCOMM channel. The shape (ServiceClassIDList +
/// ProtocolDescriptorList of L2CAP then RFCOMM) is what every SPP record
/// carries; SPP spec §5.1.
pub fn spp_service_record(handle: u32, rfcomm_channel: u8, name: &str) -> Service {
    /// Serial Port service class (Assigned Numbers).
    const SERIAL_PORT_SERVICE_CLASS: u16 = 0x1101;

    vec![
        ServiceAttribute {
            id: attribute_id::SERVICE_RECORD_HANDLE,
            value: DataElement::unsigned_integer_32(handle),
        },
        ServiceAttribute {
            id: attribute_id::SERVICE_CLASS_ID_LIST,
            value: DataElement::sequence(vec![DataElement::uuid(SdpUuid::Uuid16(
                SERIAL_PORT_SERVICE_CLASS,
            ))]),
        },
        ServiceAttribute {
            id: attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            value: DataElement::sequence(vec![
                DataElement::sequence(vec![DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID)]),
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::Uuid16(0x0003)), // RFCOMM
                    DataElement::unsigned_integer(u64::from(rfcomm_channel), 1),
                ]),
            ]),
        },
        ServiceAttribute {
            id: attribute_id::BROWSE_GROUP_LIST,
            value: DataElement::sequence(vec![DataElement::uuid(SdpUuid::SDP_PUBLIC_BROWSE_ROOT)]),
        },
        ServiceAttribute {
            // ServiceName sits at the primary language base (0x0100) rather
            // than having a fixed id of its own.
            id: 0x0100,
            value: DataElement::text_string(name.as_bytes().to_vec()),
        },
    ]
}

/// A BR/EDR host: discoverability, inbound connections, L2CAP signalling,
/// and per-PSM protocol dispatch.
#[derive(Debug)]
pub struct ClassicHost {
    /// The device's advertised name, returned on a Remote Name Request.
    name: String,
    /// Class of Device (3 octets, little-endian on the wire).
    class_of_device: [u8; 3],
    channels: ClassicChannelManager,
    handlers: Vec<Box<dyn ProtocolHandler>>,
    /// The current ACL connection, if any.
    connection: Option<(u16, Address)>,
    /// Next signalling identifier to use for host-initiated requests.
    next_identifier: u8,
    /// Local CIDs this host has accepted or opened, so channel state can be
    /// inspected (the CID allocator does not expose iteration).
    local_cids: Vec<u16>,
    /// Devices seen during inquiry, in the order they answered.
    discovered: Vec<DiscoveredDevice>,
    /// Whether an Inquiry Complete has arrived since the last inquiry began.
    inquiry_finished: bool,
}

impl ClassicHost {
    /// Creates a host advertising `name` with `class_of_device`. A common
    /// Class of Device for an audio peripheral is `[0x04, 0x04, 0x24]`
    /// (rendering / audio-video, wearable headset).
    pub fn new(name: impl Into<String>, class_of_device: [u8; 3]) -> Self {
        Self {
            name: name.into(),
            class_of_device,
            channels: ClassicChannelManager::new(),
            handlers: Vec::new(),
            connection: None,
            next_identifier: 1,
            local_cids: Vec::new(),
            discovered: Vec::new(),
            inquiry_finished: false,
        }
    }

    /// Registers a protocol handler and its PSM, so an inbound connection
    /// request for that PSM is accepted and its data routed here.
    pub fn register_handler(
        &mut self,
        handler: Box<dyn ProtocolHandler>,
    ) -> Result<(), SimbleError> {
        self.channels.register_server(handler.psm())?;
        self.handlers.push(handler);
        Ok(())
    }

    /// The current ACL connection as `(handle, peer address)`, if any.
    pub fn connection(&self) -> Option<(u16, Address)> {
        self.connection
    }

    /// The name this host answers a Remote Name Request with.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Whether any L2CAP channel is open (i.e. a peer has completed the
    /// connect + configure handshake).
    pub fn has_open_channel(&self) -> bool {
        self.local_cids
            .iter()
            .filter_map(|cid| self.channels.get_channel(*cid))
            .any(|channel| channel.is_open())
    }

    /// The controller bring-up that makes this device visible: reset, event
    /// mask, name, Class of Device, Simple Pairing, then inquiry+page scan.
    /// Without Write Scan Enable a BR/EDR device is neither discoverable nor
    /// connectable, which is the usual reason a peer never sees it.
    pub fn start_commands(&self) -> Vec<Vec<u8>> {
        let mut name_param = [0u8; 248];
        let bytes = self.name.as_bytes();
        let len = bytes.len().min(name_param.len());
        name_param[..len].copy_from_slice(&bytes[..len]);

        vec![
            command(opcode::RESET, &[]),
            command(opcode::SET_EVENT_MASK, &[0xFF; 8]),
            command(opcode::WRITE_LOCAL_NAME, &name_param),
            command(opcode::WRITE_CLASS_OF_DEVICE, &self.class_of_device),
            command(opcode::WRITE_SIMPLE_PAIRING_MODE, &[0x01]),
            command(opcode::WRITE_SCAN_ENABLE, &[scan_enable::INQUIRY_AND_PAGE]),
        ]
    }

    // -- the initiator side ------------------------------------------------
    //
    // Everything above this point answers a peer. Everything below starts
    // something: discovery, paging, and opening an L2CAP channel as a client.
    // A device that only responds cannot be half of a two-device scene, and
    // until there was a simulated controller to talk to there was never a
    // reason for this host to page anyone.

    /// HCI Inquiry on the General Inquiry Access Code for `length` × 1.28 s,
    /// reporting every device that answers. Clears whatever the last inquiry
    /// found, so a caller cannot mistake a stale result for a fresh one.
    pub fn start_inquiry(&mut self, length: u8) -> Vec<Vec<u8>> {
        self.discovered.clear();
        self.inquiry_finished = false;
        let mut params = GIAC.to_vec();
        params.push(length.max(1));
        params.push(0x00); // Num_Responses: 0 = unlimited
        vec![command(opcode::INQUIRY, &params)]
    }

    /// The devices this inquiry has found so far.
    pub fn discovered(&self) -> &[DiscoveredDevice] {
        &self.discovered
    }

    /// Whether an Inquiry Complete has arrived — i.e. whether
    /// [`Self::discovered`] is the whole answer rather than a partial one.
    pub fn inquiry_finished(&self) -> bool {
        self.inquiry_finished
    }

    /// HCI Remote Name Request: ask a discovered device what it is called.
    /// The answer lands in [`Self::discovered`].
    pub fn request_remote_name(&self, address: Address) -> Vec<Vec<u8>> {
        let mut params = address.as_slice().to_vec();
        params.push(0x01); // Page_Scan_Repetition_Mode R1
        params.push(0x00); // Reserved
        params.extend_from_slice(&[0x00, 0x00]); // Clock_Offset
        vec![command(opcode::REMOTE_NAME_REQUEST, &params)]
    }

    /// HCI Create Connection: page `address` and open an ACL link to it.
    ///
    /// The answer is a Command Status, and then — much later — a Connection
    /// Complete once the peer's host has accepted. Nothing here waits for it;
    /// [`Self::connection`] becomes `Some` when it arrives.
    pub fn create_connection(&self, address: Address) -> Vec<Vec<u8>> {
        let mut params = address.as_slice().to_vec();
        params.extend_from_slice(&[
            0x18, 0xCC, // Packet_Type: the usual DM1/DH1/DM3/DH3/DM5/DH5 set
            0x01, // Page_Scan_Repetition_Mode R1
            0x00, // Reserved
            0x00, 0x00, // Clock_Offset
            0x01, // Allow_Role_Switch
        ]);
        vec![command(opcode::CREATE_CONNECTION, &params)]
    }

    /// Opens an L2CAP channel to the peer's `psm` as a client: allocates a
    /// CID and sends the Connection Request. The channel is not usable until
    /// both sides have configured — see [`Self::channel_is_open`].
    pub fn open_channel(&mut self, psm: u16) -> Result<Vec<Vec<u8>>, SimbleError> {
        let Some((handle, _)) = self.connection else {
            return Err(SimbleError::DeviceError(
                "L2CAP: no ACL connection to open a channel on".into(),
            ));
        };
        let spec = crate::l2cap::classic::ClassicChannelSpec::with_mtu(psm, DEFAULT_L2CAP_MTU);
        let (local_cid, request) = self.channels.connect(&spec)?;
        self.local_cids.push(local_cid);
        Ok(vec![acl_packet(
            handle,
            &signaling_pdu(
                signaling_code::CONNECTION_REQUEST,
                self.take_identifier(),
                request.as_bytes(),
            ),
        )])
    }

    /// Whether the channel for `psm` has completed configuration in both
    /// directions and can carry data.
    pub fn channel_is_open(&self, psm: u16) -> bool {
        self.local_cids
            .iter()
            .filter_map(|cid| self.channels.get_channel(*cid))
            .any(|channel| channel.psm == psm && channel.is_open())
    }

    /// HCI Disconnect on the current ACL link.
    pub fn disconnect(&self) -> Vec<Vec<u8>> {
        let Some((handle, _)) = self.connection else {
            return Vec::new();
        };
        let mut params = handle.to_le_bytes().to_vec();
        params.push(0x13); // Remote User Terminated Connection
        vec![command(opcode::DISCONNECT, &params)]
    }

    /// HCI Write Scan Enable on its own — for a device that wants to be
    /// something other than discoverable *and* connectable. A pure client
    /// needs neither; a device that should be findable but not connectable
    /// wants [`scan_enable::INQUIRY_ONLY`].
    pub fn set_scan_enable(&self, value: u8) -> Vec<Vec<u8>> {
        vec![command(opcode::WRITE_SCAN_ENABLE, &[value])]
    }

    /// The peer's name, if a Remote Name Request for `address` has been
    /// answered.
    pub fn name_of(&self, address: Address) -> Option<&str> {
        self.discovered
            .iter()
            .find(|d| d.address == address)
            .and_then(|d| d.name.as_deref())
    }

    /// Handles one H4 packet from the controller, returning what to send back.
    pub fn handle_packet(&mut self, packet: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let mut out = match packet.first() {
            Some(&crate::transport::h4_type::HCI_EVENT) => self.handle_event(packet),
            Some(&crate::transport::h4_type::HCI_ACL_DATA) => self.handle_acl(packet)?,
            _ => Vec::new(),
        };
        // Anything a profile queued while handling this packet leaves with
        // it, so a device that replies to a peer does so in one round trip.
        out.extend(self.poll());
        Ok(out)
    }

    fn handle_event(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        let Some(event) = HciEvent::parse_h4(packet) else {
            return Vec::new();
        };
        match event {
            HciEvent::ConnectionRequest(request) => {
                // Answer the page, or the peer's connection attempt times
                // out: Accept Connection Request with role 0x01 (remain
                // peripheral, letting the initiator stay central).
                let mut params = Vec::with_capacity(7);
                params.extend_from_slice(&request.bd_addr);
                params.push(0x01);
                vec![command(opcode::ACCEPT_CONNECTION_REQUEST, &params)]
            }
            HciEvent::ConnectionComplete(complete) if complete.status == 0x00 => {
                self.connection = Some((
                    complete.connection_handle.get(),
                    Address::new(complete.bd_addr),
                ));
                Vec::new()
            }
            HciEvent::DisconnectionComplete(_) => {
                self.connection = None;
                // The ACL is gone, so every channel riding it is gone too —
                // and a profile holding session state must be told, or it
                // meets the next peer still believing the last one is there.
                for cid in std::mem::take(&mut self.local_cids) {
                    self.channels.remove_channel(cid);
                }
                for handler in &mut self.handlers {
                    handler.on_channel_closed();
                }
                // Re-enable scanning so the device is findable again after
                // the peer goes away.
                vec![command(
                    opcode::WRITE_SCAN_ENABLE,
                    &[scan_enable::INQUIRY_AND_PAGE],
                )]
            }
            HciEvent::Other { code, parameters } => {
                self.handle_discovery_event(code, parameters);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Records what an inquiry turned up. These arrive as `HciEvent::Other`
    /// because they carry no typed variant; the layouts are Vol 4, Part E,
    /// Sections 7.7.1, 7.7.2, 7.7.7, 7.7.33 and 7.7.38.
    ///
    /// All **three** inquiry-result forms are handled, because which one
    /// arrives is not this host's choice alone: it follows the controller's
    /// Inquiry Mode, and a host that understands only the reset default goes
    /// blind — the inquiry completes, `discovered` stays empty, and the
    /// symptom is "the device is not there" with no error anywhere.
    fn handle_discovery_event(&mut self, code: u8, parameters: &[u8]) {
        match code {
            event_code::INQUIRY_COMPLETE => self.inquiry_finished = true,
            // The three forms differ only in the fixed part's length and in
            // where Class_of_Device sits — the standard form has *two*
            // reserved octets after Page_Scan_Repetition_Mode, the other two
            // have one. Reading the standard offset out of an RSSI result
            // shifts the Class of Device by a byte, which turns a headset
            // into whatever the clock offset's low byte says it is.
            event_code::INQUIRY_RESULT => self.record_inquiry_results(parameters, 14, 9, false),
            event_code::INQUIRY_RESULT_WITH_RSSI => {
                // BD_ADDR(6), PSRM(1), Reserved(1), CoD(3), Clock_Offset(2),
                // RSSI(1) — the same 14 octets as the standard form, with
                // one reserved octet traded for the RSSI.
                self.record_inquiry_results(parameters, 14, 8, false);
            }
            event_code::EXTENDED_INQUIRY_RESULT => {
                // Num_Responses is always 1 here, and the response carries
                // 240 octets of EIR after the RSSI — which is how a phone
                // shows a name it never asked for. 14 + 240 = 254.
                self.record_inquiry_results(parameters, 254, 8, true);
            }
            event_code::REMOTE_NAME_REQUEST_COMPLETE => {
                // Status(1), BD_ADDR(6), Remote_Name(248, NUL-padded).
                if parameters.first() != Some(&0x00) || parameters.len() < 7 {
                    return;
                }
                let address = Address::new([
                    parameters[1],
                    parameters[2],
                    parameters[3],
                    parameters[4],
                    parameters[5],
                    parameters[6],
                ]);
                let name = String::from_utf8_lossy(&parameters[7..])
                    .trim_end_matches('\0')
                    .to_string();
                match self.discovered.iter_mut().find(|d| d.address == address) {
                    Some(device) => device.name = Some(name),
                    // A name can be asked for without an inquiry first — a
                    // host reconnecting to a bonded device does exactly that.
                    None => self.discovered.push(DiscoveredDevice {
                        address,
                        class_of_device: [0; 3],
                        name: Some(name),
                    }),
                }
            }
            _ => {}
        }
    }

    /// The body all three inquiry-result forms share: `Num_Responses`, then
    /// that many fixed-size responses, each starting with a BD_ADDR and
    /// carrying its Class of Device at `cod_offset`.
    ///
    /// `response_len` is the whole per-response record — for an Extended
    /// Inquiry Result that includes the 240 EIR octets, and `has_eir` says
    /// to read a name out of them.
    fn record_inquiry_results(
        &mut self,
        parameters: &[u8],
        response_len: usize,
        cod_offset: usize,
        has_eir: bool,
    ) {
        let Some((&count, rest)) = parameters.split_first() else {
            return;
        };
        for index in 0..usize::from(count) {
            let Some(response) = rest.get(index * response_len..(index + 1) * response_len) else {
                return; // truncated: stop rather than misread past it
            };
            let address = Address::new([
                response[0],
                response[1],
                response[2],
                response[3],
                response[4],
                response[5],
            ]);
            let class_of_device = [
                response[cod_offset],
                response[cod_offset + 1],
                response[cod_offset + 2],
            ];
            // The EIR sits after the RSSI octet: BD_ADDR(6) + PSRM(1) +
            // Reserved(1) + CoD(3) + Clock_Offset(2) + RSSI(1) = 14.
            let eir_name = if has_eir {
                response.get(14..).and_then(name_from_eir)
            } else {
                None
            };
            // A real controller reports the same device repeatedly for as
            // long as the inquiry runs, so deduping is the host's job — not
            // the simulator's.
            if let Some(existing) = self.discovered.iter_mut().find(|d| d.address == address) {
                existing.class_of_device = class_of_device;
                // A name already resolved by a Remote Name Request is the
                // authoritative one; an EIR name only fills a blank.
                if existing.name.is_none() {
                    existing.name = eir_name;
                }
                continue;
            }
            self.discovered.push(DiscoveredDevice {
                address,
                class_of_device,
                name: eir_name,
            });
        }
    }

    /// HCI Write Inquiry Mode: ask the controller for a richer inquiry
    /// result. [`inquiry_mode::WITH_EXTENDED`] is what a phone sets, and the
    /// reason its device list shows names before anything is paired.
    ///
    /// Not part of [`Self::start_commands`]: the reset default is the
    /// standard form, and a device that only accepts connections has no
    /// inquiry results to shape.
    pub fn set_inquiry_mode(&self, mode: u8) -> Vec<Vec<u8>> {
        vec![command(opcode::WRITE_INQUIRY_MODE, &[mode])]
    }

    fn handle_acl(&mut self, packet: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        use crate::l2cap::HciAclHeader;
        let Some((header, payload)) = HciAclHeader::parse(&packet[1..]) else {
            return Err(SimbleError::PacketParseError("Invalid ACL header".into()));
        };
        let handle = header.handle();
        let Some((l2cap_header, body)) = L2capHeader::ref_from_prefix(payload).ok() else {
            return Ok(Vec::new());
        };
        let channel_id = l2cap_header.cid.get();
        let length = usize::from(l2cap_header.length.get()).min(body.len());
        let body = &body[..length];

        if channel_id == cid::BR_SIGNALING {
            return Ok(self.handle_signaling(handle, body));
        }
        Ok(self.handle_channel_data(handle, channel_id, body))
    }

    /// Drives the L2CAP connect/configure/disconnect handshake through the
    /// channel manager, which owns the state machine.
    fn handle_signaling(&mut self, handle: u16, body: &[u8]) -> Vec<Vec<u8>> {
        let Ok((header, params)) = L2capSignalingHeader::ref_from_prefix(body) else {
            return Vec::new();
        };
        let identifier = header.identifier;
        let length = usize::from(header.length.get()).min(params.len());
        let params = &params[..length];
        let mut out = Vec::new();

        match header.code {
            signaling_code::CONNECTION_REQUEST => {
                let Ok((request, _)) = ConnectionRequestHeader::ref_from_prefix(params) else {
                    return out;
                };
                let Ok(response) = self
                    .channels
                    .on_connection_request(request, DEFAULT_L2CAP_MTU)
                else {
                    return out;
                };
                let local_cid = response.destination_cid.get();
                if local_cid != 0 {
                    self.local_cids.push(local_cid);
                }
                out.push(acl_packet(
                    handle,
                    &signaling_pdu(
                        signaling_code::CONNECTION_RESPONSE,
                        identifier,
                        response.as_bytes(),
                    ),
                ));
                // A server sends its own Configuration Request straight
                // after accepting; the channel opens once both sides have
                // configured.
                if local_cid != 0
                    && let Ok((config, mtu_option)) =
                        self.channels.make_configuration_request(local_cid)
                {
                    let mut payload = config.as_bytes().to_vec();
                    payload.extend_from_slice(&mtu_option);
                    out.push(acl_packet(
                        handle,
                        &signaling_pdu(
                            signaling_code::CONFIGURATION_REQUEST,
                            self.take_identifier(),
                            &payload,
                        ),
                    ));
                }
            }
            // The peer answered a channel *we* opened. Without this arm a
            // client's Connection Request is a dead end: the channel stays in
            // WaitConnectRsp for ever and never learns the peer's CID, so
            // nothing can be sent on it and nothing arriving on it matches.
            signaling_code::CONNECTION_RESPONSE => {
                let Ok((response, _)) = ConnectionResponseHeader::ref_from_prefix(params) else {
                    return out;
                };
                // source_cid echoes the CID we chose — ours.
                let local_cid = response.source_cid.get();
                if self
                    .channels
                    .on_connection_response(local_cid, response)
                    .is_err()
                {
                    // Refused, and the channel manager has dropped it.
                    self.local_cids.retain(|cid| *cid != local_cid);
                    return out;
                }
                // A client configures as soon as it is accepted; the channel
                // opens once both sides have.
                if let Ok((config, mtu_option)) =
                    self.channels.make_configuration_request(local_cid)
                {
                    let mut payload = config.as_bytes().to_vec();
                    payload.extend_from_slice(&mtu_option);
                    out.push(acl_packet(
                        handle,
                        &signaling_pdu(
                            signaling_code::CONFIGURATION_REQUEST,
                            self.take_identifier(),
                            &payload,
                        ),
                    ));
                }
            }
            signaling_code::CONFIGURATION_REQUEST => {
                let Ok((request, options)) = ConfigurationRequestHeader::ref_from_prefix(params)
                else {
                    return out;
                };
                // The peer addresses our local CID in destination_cid.
                let local_cid = request.destination_cid.get();
                if let Ok(response) = self.channels.on_configuration_request(local_cid, options) {
                    out.push(acl_packet(
                        handle,
                        &signaling_pdu(
                            signaling_code::CONFIGURATION_RESPONSE,
                            identifier,
                            response.as_bytes(),
                        ),
                    ));
                }
            }
            signaling_code::CONFIGURATION_RESPONSE => {
                let Ok((response, _)) = ConfigurationResponseHeader::ref_from_prefix(params) else {
                    return out;
                };
                // source_cid echoes the CID we put in our request — ours.
                let local_cid = response.source_cid.get();
                let _ = self.channels.on_configuration_response(local_cid, response);
            }
            // Echo the request body back as the response, per spec, and
            // drop the channel.
            signaling_code::DISCONNECTION_REQUEST if params.len() >= 4 => {
                let local_cid = u16::from_le_bytes([params[0], params[1]]);
                let psm = self.channels.get_channel(local_cid).map(|c| c.psm);
                self.channels.remove_channel(local_cid);
                self.local_cids.retain(|cid| *cid != local_cid);
                if let Some(psm) = psm
                    && let Some(handler) = self.handlers.iter_mut().find(|h| h.psm() == psm)
                {
                    handler.on_channel_closed();
                }
                out.push(acl_packet(
                    handle,
                    &signaling_pdu(signaling_code::DISCONNECTION_RESPONSE, identifier, params),
                ));
            }
            _ => {}
        }
        out
    }

    /// Routes an SDU on an open channel to the handler for its PSM.
    fn handle_channel_data(&mut self, handle: u16, cid: u16, data: &[u8]) -> Vec<Vec<u8>> {
        let Some(channel) = self.channels.get_channel(cid) else {
            return Vec::new();
        };
        let (psm, peer_cid, peer_mtu) = (channel.psm, channel.peer_cid, channel.peer_mtu);
        let Some(handler) = self.handlers.iter_mut().find(|h| h.psm() == psm) else {
            return Vec::new();
        };
        handler
            .on_data(data, peer_mtu)
            .into_iter()
            .filter(|reply| !reply.is_empty())
            .map(|reply| acl_packet(handle, &L2capHeader::serialize(peer_cid, &reply)))
            .collect()
    }

    /// Collects anything the profiles want to send unprompted — bytes a
    /// device wrote to an open RFCOMM port, say — as H4 packets.
    ///
    /// [`Self::handle_packet`] already drains this after each inbound
    /// packet; a runtime that ticks should also call it directly, or a
    /// device that speaks first waits for the peer to speak.
    pub fn poll(&mut self) -> Vec<Vec<u8>> {
        let Some((handle, _)) = self.connection else {
            return Vec::new();
        };
        // Map each open channel to its handler once, so a handler is polled
        // against the channel it actually serves.
        let open: Vec<(u16, u16, u16)> = self
            .local_cids
            .iter()
            .filter_map(|cid| self.channels.get_channel(*cid))
            .filter(|channel| channel.is_open())
            .map(|channel| (channel.psm, channel.peer_cid, channel.peer_mtu))
            .collect();

        let mut out = Vec::new();
        for (psm, peer_cid, peer_mtu) in open {
            let Some(handler) = self.handlers.iter_mut().find(|h| h.psm() == psm) else {
                continue;
            };
            for sdu in handler.poll_output(peer_mtu) {
                if sdu.is_empty() {
                    continue;
                }
                out.push(acl_packet(handle, &L2capHeader::serialize(peer_cid, &sdu)));
            }
        }
        out
    }

    fn take_identifier(&mut self) -> u8 {
        let id = self.next_identifier;
        self.next_identifier = self.next_identifier.wrapping_add(1).max(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packets::ConnectionResponseHeader;

    fn connection_request_event(addr: [u8; 6]) -> Vec<u8> {
        let mut packet = vec![0x04, 0x04, 0x0A];
        packet.extend_from_slice(&addr);
        packet.extend_from_slice(&[0x04, 0x04, 0x24]); // class of device
        packet.push(0x01); // ACL
        packet
    }

    fn connection_complete_event(handle: u16, addr: [u8; 6]) -> Vec<u8> {
        let mut packet = vec![0x04, 0x03, 0x0B, 0x00];
        packet.extend_from_slice(&handle.to_le_bytes());
        packet.extend_from_slice(&addr);
        packet.push(0x01); // ACL
        packet.push(0x00); // encryption off
        packet
    }

    fn host() -> ClassicHost {
        let mut host = ClassicHost::new("SimbleClassic", [0x04, 0x04, 0x24]);
        host.register_handler(Box::new(SdpHandler::default()))
            .unwrap();
        host
    }

    #[test]
    fn test_bring_up_makes_the_device_discoverable_and_connectable() {
        let commands = host().start_commands();
        // The last command must enable both scans, or a peer never sees it.
        let scan = commands.last().expect("bring-up is not empty");
        assert_eq!(&scan[1..3], &opcode::WRITE_SCAN_ENABLE);
        assert_eq!(scan[4], scan_enable::INQUIRY_AND_PAGE);
        // The name and class of device are set before scanning starts.
        assert!(commands.iter().any(|c| c[1..3] == opcode::WRITE_LOCAL_NAME));
        assert!(
            commands
                .iter()
                .any(|c| c[1..3] == opcode::WRITE_CLASS_OF_DEVICE)
        );
    }

    #[test]
    fn test_inbound_page_is_accepted_and_tracked() {
        let mut host = host();
        let addr = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

        let out = host.handle_packet(&connection_request_event(addr)).unwrap();
        assert_eq!(
            &out[0][1..3],
            &opcode::ACCEPT_CONNECTION_REQUEST,
            "an unanswered page times out"
        );

        assert!(host.connection().is_none());
        host.handle_packet(&connection_complete_event(0x0080, addr))
            .unwrap();
        let (handle, peer) = host.connection().expect("connection tracked");
        assert_eq!(handle, 0x0080);
        assert_eq!(peer, Address::new(addr));
    }

    #[test]
    fn test_l2cap_handshake_opens_an_sdp_channel() {
        let mut host = host();
        let addr = [0x11; 6];
        host.handle_packet(&connection_request_event(addr)).unwrap();
        host.handle_packet(&connection_complete_event(0x0080, addr))
            .unwrap();

        // Peer opens SDP: Connection Request for PSM 0x0001, source CID 0x0040.
        let request = ConnectionRequestHeader {
            psm: SDP_PSM.into(),
            source_cid: 0x0040u16.into(),
        };
        let pdu = signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes());
        // signaling_pdu already wraps in an L2CAP header; feed it as ACL.
        let out = host.handle_packet(&acl_packet(0x0080, &pdu)).unwrap();
        assert_eq!(out.len(), 2, "connection response, then our config request");

        // The response must accept (result 0x0000) and name our local CID.
        // H4(1) + ACL header(4) + L2CAP header(4) + signalling header(4).
        let response_body = &out[0][13..];
        let (response, _) = ConnectionResponseHeader::ref_from_prefix(response_body).unwrap();
        assert_eq!(response.result.get(), 0x0000, "SDP PSM must be accepted");
        let local_cid = response.destination_cid.get();
        assert_ne!(local_cid, 0);

        // Peer configures us, and acks our configuration: channel opens.
        let mut config = ConfigurationRequestHeader {
            destination_cid: local_cid.into(),
            flags: 0u16.into(),
        }
        .as_bytes()
        .to_vec();
        config.extend_from_slice(&[0x01, 0x02, 0xA0, 0x02]); // MTU option, 672
        host.handle_packet(&acl_packet(
            0x0080,
            &signaling_pdu(signaling_code::CONFIGURATION_REQUEST, 2, &config),
        ))
        .unwrap();

        let ack = ConfigurationResponseHeader {
            source_cid: local_cid.into(),
            flags: 0u16.into(),
            result: 0u16.into(),
        };
        host.handle_packet(&acl_packet(
            0x0080,
            &signaling_pdu(signaling_code::CONFIGURATION_RESPONSE, 1, ack.as_bytes()),
        ))
        .unwrap();

        assert!(
            host.has_open_channel(),
            "both sides configured — the channel must be open"
        );
    }

    #[test]
    fn test_sdp_request_on_an_open_channel_is_answered() {
        let mut host = host();
        let addr = [0x11; 6];
        host.handle_packet(&connection_request_event(addr)).unwrap();
        host.handle_packet(&connection_complete_event(0x0080, addr))
            .unwrap();
        let request = ConnectionRequestHeader {
            psm: SDP_PSM.into(),
            source_cid: 0x0040u16.into(),
        };
        let out = host
            .handle_packet(&acl_packet(
                0x0080,
                &signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes()),
            ))
            .unwrap();
        let (response, _) = ConnectionResponseHeader::ref_from_prefix(&out[0][13..]).unwrap();
        let local_cid = response.destination_cid.get();

        // A malformed SDP request still gets an SDP error response, which
        // proves the data path reaches the server and comes back.
        let out = host.handle_channel_data(0x0080, local_cid, &[0xFF, 0x00, 0x00]);
        assert_eq!(out.len(), 1, "SDP must answer on the same channel");
        let reply = &out[0][9..];
        assert_eq!(
            reply[0], 0x01,
            "SDP ErrorResponse PDU id, i.e. the server ran"
        );
    }

    #[test]
    fn test_spp_record_is_discoverable_through_sdp() {
        // A peer finds SPP by searching for the Serial Port service class;
        // the record must carry the class and the RFCOMM channel.
        let mut handler = SdpHandler::default();
        handler
            .server_mut()
            .service_records
            .insert(0x00010001, spp_service_record(0x00010001, 3, "Simble SPP"));

        let record = &handler.server_mut().service_records[&0x00010001];
        let class_list = record
            .iter()
            .find(|a| a.id == attribute_id::SERVICE_CLASS_ID_LIST)
            .expect("record names its service class");
        assert_eq!(
            class_list.value,
            DataElement::sequence(vec![DataElement::uuid(SdpUuid::Uuid16(0x1101))])
        );

        // The protocol descriptor must be L2CAP then RFCOMM/channel 3, or a
        // peer cannot work out where to connect.
        let protocols = record
            .iter()
            .find(|a| a.id == attribute_id::PROTOCOL_DESCRIPTOR_LIST)
            .expect("record names its protocol stack");
        let DataElement::Sequence(layers) = &protocols.value else {
            panic!("protocol descriptor list must be a sequence");
        };
        assert_eq!(layers.len(), 2);
        assert_eq!(
            layers[1],
            DataElement::sequence(vec![
                DataElement::uuid(SdpUuid::Uuid16(0x0003)),
                DataElement::unsigned_integer(3, 1),
            ])
        );
    }

    /// A peer that answers every continuation with another continuation
    /// would keep an SDP client asking for ever. The watchdog stops it, and
    /// — this is the part that matters — says the answer is a *prefix*
    /// rather than letting a partial record list pass for the whole
    /// database. Acting on a truncated answer opens a DLC on a channel
    /// nobody is listening to.
    #[test]
    fn test_an_endless_sdp_continuation_is_stopped_and_declared_partial() {
        use crate::classic::sdp::{SDP_CONTINUATION_WATCHDOG, SdpPdu};

        let (mut query, results) = SdpQueryHandler::searching(SdpUuid::Uuid16(0x1101));
        assert_eq!(query.poll_output(672).len(), 1, "the query goes out once");

        // A response that never ends: an empty attribute list and a
        // continuation state that is never the null one.
        let endless = SdpPdu::ServiceSearchAttributeResponse {
            transaction_id: 1,
            attribute_lists: Vec::new(),
            continuation_state: vec![0x01, 0x00],
        }
        .to_bytes();

        let mut asked_again = 0;
        for _ in 0..=SDP_CONTINUATION_WATCHDOG {
            asked_again += query.on_data(&endless, 672).len();
        }
        assert_eq!(
            asked_again as u32, SDP_CONTINUATION_WATCHDOG,
            "the client asks again exactly {SDP_CONTINUATION_WATCHDOG} times, \
             then gives up"
        );

        let results = results.lock().expect("results readable");
        assert!(results.answered, "giving up is still an outcome to report");
        assert!(
            results.truncated,
            "and it must be reported as partial, not as a peer with no services"
        );
    }

    #[test]
    fn test_disconnect_restores_discoverability() {
        let mut host = host();
        let addr = [0x11; 6];
        host.handle_packet(&connection_request_event(addr)).unwrap();
        host.handle_packet(&connection_complete_event(0x0080, addr))
            .unwrap();

        let out = host
            .handle_packet(&[0x04, 0x05, 0x04, 0x00, 0x80, 0x00, 0x13])
            .unwrap();
        assert!(host.connection().is_none());
        assert_eq!(&out[0][1..3], &opcode::WRITE_SCAN_ENABLE);
        assert_eq!(out[0][4], scan_enable::INQUIRY_AND_PAGE);
    }

    // ---------------------------------------------------------------------
    // RFCOMM / SPP
    // ---------------------------------------------------------------------

    use crate::classic::rfcomm::{Multiplexer, MultiplexerEvent, RFCOMM_PSM, Role};

    /// Shuttles RFCOMM frames between a peer multiplexer and our handler
    /// until neither has anything more to say, returning the events the peer
    /// saw. Both sides are synchronous, so a bounded loop settles.
    fn shuttle(
        peer: &mut Multiplexer,
        handler: &mut RfcommHandler,
        start: Vec<Vec<u8>>,
    ) -> Vec<MultiplexerEvent> {
        let mut to_handler = start;
        let mut peer_events = Vec::new();
        for _ in 0..8 {
            if to_handler.is_empty() {
                break;
            }
            let mut to_peer = Vec::new();
            for frame in to_handler.drain(..) {
                to_peer.extend(handler.on_data(&frame, 672));
            }
            for frame in to_peer {
                if let Ok((out, events)) = peer.receive(&frame) {
                    to_handler.extend(out);
                    peer_events.extend(events);
                }
            }
        }
        peer_events
    }

    /// Opens a multiplexer session and a DLC on `channel`, the way a peer
    /// does: SABM(0), then PN, then SABM on the data DLCI.
    fn open_session(channel: u8) -> (Multiplexer, RfcommHandler, SharedRfcommPort) {
        let (mut handler, port) = RfcommHandler::echoing(channel);
        let mut peer = Multiplexer::new(Role::Initiator, 672);
        let sabm = peer.start().expect("initiator starts the multiplexer");
        shuttle(&mut peer, &mut handler, vec![sabm]);
        let pn = peer.open_dlc(channel, 127, 7).expect("PN for the channel");
        shuttle(&mut peer, &mut handler, vec![pn]);
        (peer, handler, port)
    }

    #[test]
    fn test_rfcomm_handler_serves_the_rfcomm_psm() {
        let (handler, _port) = RfcommHandler::echoing(3);
        assert_eq!(handler.psm(), RFCOMM_PSM, "SPP rides RFCOMM on PSM 3");
    }

    #[test]
    fn test_rfcomm_session_and_dlc_open() {
        let (peer, _handler, port) = open_session(3);
        assert!(peer.is_connected(), "the multiplexer session must come up");
        assert!(
            port.lock().unwrap().is_open(),
            "the DLC the SDP record advertises must open"
        );
    }

    #[test]
    fn test_rfcomm_dlc_open_is_refused_on_an_unadvertised_channel() {
        // A peer that opens a channel we never listened on must get DM, not
        // a half-open DLC — otherwise the SDP record and reality diverge.
        let (mut peer, mut handler, port) = open_session(3);
        let pn = peer.open_dlc(9, 127, 7).expect("PN for a stray channel");
        let events = shuttle(&mut peer, &mut handler, vec![pn]);
        assert!(
            events
                .iter()
                .any(|e| matches!(e, MultiplexerEvent::DlcOpenRejected(_))),
            "an unlistened channel must be rejected: {events:?}"
        );
        // The channel we do serve is unaffected.
        assert!(port.lock().unwrap().is_open());
    }

    #[test]
    fn test_rfcomm_carries_data_from_the_peer_to_the_device() {
        let (mut peer, mut handler, port) = open_session(3);
        let dlci = port.lock().unwrap().open_dlci.expect("a DLC is open");

        let frames = peer.write(dlci, b"AT+HELLO\r").expect("peer writes");
        shuttle(&mut peer, &mut handler, frames);

        let received = port.lock().unwrap().take_received();
        assert_eq!(
            received,
            vec![b"AT+HELLO\r".to_vec()],
            "the device must see exactly what the peer sent"
        );
        assert_eq!(port.lock().unwrap().received_count(), 1);
        assert!(
            port.lock().unwrap().take_received().is_empty(),
            "draining is destructive"
        );
    }

    #[test]
    fn test_rfcomm_carries_data_from_the_device_to_the_peer() {
        let (mut peer, mut handler, port) = open_session(3);

        // The device speaks first: nothing inbound prompted this.
        port.lock().unwrap().write(b"READY\r\n".to_vec());
        let frames = handler.poll_output(672);
        assert!(!frames.is_empty(), "queued data must leave on a poll");

        let events = shuttle(&mut peer, &mut handler, frames);
        let delivered: Vec<Vec<u8>> = events
            .iter()
            .filter_map(|e| match e {
                MultiplexerEvent::DataReceived(_, data) => Some(data.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            delivered,
            vec![b"READY\r\n".to_vec()],
            "the peer must receive what the device wrote"
        );
    }

    #[test]
    fn test_rfcomm_echo_answers_the_peer() {
        // The default port behaviour, and what a terminal app on a phone
        // sees: type a line, get it back.
        let (mut peer, mut handler, port) = open_session(3);
        let dlci = port.lock().unwrap().open_dlci.unwrap();

        let frames = peer.write(dlci, b"ping").expect("peer writes");
        let events = shuttle(&mut peer, &mut handler, frames);

        let echoed: Vec<Vec<u8>> = events
            .iter()
            .filter_map(|e| match e {
                MultiplexerEvent::DataReceived(_, data) => Some(data.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            echoed,
            vec![b"ping".to_vec()],
            "the port echoes: {events:?}"
        );
    }

    #[test]
    fn test_rfcomm_data_queued_before_the_dlc_opens_is_not_lost() {
        // A device may write before a peer has opened the port; that data
        // must wait rather than being dropped on the floor.
        let (mut handler, port) = RfcommHandler::echoing(3);
        port.lock().unwrap().write(b"early".to_vec());
        assert!(
            handler.poll_output(672).is_empty(),
            "nothing can be sent with no DLC open"
        );

        let mut peer = Multiplexer::new(Role::Initiator, 672);
        let sabm = peer.start().unwrap();
        shuttle(&mut peer, &mut handler, vec![sabm]);
        let pn = peer.open_dlc(3, 127, 7).unwrap();
        let events = shuttle(&mut peer, &mut handler, vec![pn]);

        // Opening the DLC flushes what was waiting.
        let delivered: Vec<Vec<u8>> = events
            .iter()
            .filter_map(|e| match e {
                MultiplexerEvent::DataReceived(_, data) => Some(data.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(delivered, vec![b"early".to_vec()], "queued data survives");
    }

    /// The whole path an Android terminal app takes: page the device, open
    /// the RFCOMM L2CAP channel, run the multiplexer handshake, exchange
    /// data, and disconnect — driven through [`ClassicHost`] as H4 packets
    /// rather than by calling the handler directly.
    #[test]
    fn test_spp_end_to_end_through_the_host() {
        let mut host = ClassicHost::new("SimbleClassic", [0x04, 0x04, 0x24]);
        host.register_handler(Box::new(SdpHandler::default()))
            .unwrap();
        let (rfcomm, port) = RfcommHandler::echoing(3);
        host.register_handler(Box::new(rfcomm)).unwrap();

        let addr = [0x22; 6];
        let handle = 0x0081;
        let peer_cid = 0x0041u16;
        host.handle_packet(&connection_request_event(addr)).unwrap();
        host.handle_packet(&connection_complete_event(handle, addr))
            .unwrap();

        // L2CAP: connect to the RFCOMM PSM, then configure in both directions.
        let request = ConnectionRequestHeader {
            psm: RFCOMM_PSM.into(),
            source_cid: peer_cid.into(),
        };
        let out = host
            .handle_packet(&acl_packet(
                handle,
                &signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes()),
            ))
            .unwrap();
        let (response, _) = ConnectionResponseHeader::ref_from_prefix(&out[0][13..]).unwrap();
        assert_eq!(response.result.get(), 0x0000, "RFCOMM PSM must be accepted");
        let local_cid = response.destination_cid.get();

        let mut config = ConfigurationRequestHeader {
            destination_cid: local_cid.into(),
            flags: 0u16.into(),
        }
        .as_bytes()
        .to_vec();
        config.extend_from_slice(&[0x01, 0x02, 0xA0, 0x02]); // MTU 672
        host.handle_packet(&acl_packet(
            handle,
            &signaling_pdu(signaling_code::CONFIGURATION_REQUEST, 2, &config),
        ))
        .unwrap();
        let ack = ConfigurationResponseHeader {
            source_cid: local_cid.into(),
            flags: 0u16.into(),
            result: 0u16.into(),
        };
        host.handle_packet(&acl_packet(
            handle,
            &signaling_pdu(signaling_code::CONFIGURATION_RESPONSE, 1, ack.as_bytes()),
        ))
        .unwrap();
        assert!(host.has_open_channel(), "the RFCOMM channel must be open");

        // RFCOMM over that channel: peer frames go in as ACL packets and the
        // host's replies come back the same way. Strip H4 + ACL + L2CAP
        // headers (1 + 4 + 4) to recover each SDU.
        let mut peer = Multiplexer::new(Role::Initiator, 672);
        let feed = |host: &mut ClassicHost, peer: &mut Multiplexer, sdus: Vec<Vec<u8>>| {
            let mut events = Vec::new();
            let mut next = Vec::new();
            for sdu in sdus {
                // An inbound frame is addressed to *our* CID; the host looks
                // the channel up by it.
                let out = host
                    .handle_packet(&acl_packet(
                        handle,
                        &L2capHeader::serialize(local_cid, &sdu),
                    ))
                    .unwrap();
                for packet in out {
                    if let Ok((frames, evts)) = peer.receive(&packet[9..]) {
                        next.extend(frames);
                        events.extend(evts);
                    }
                }
            }
            (next, events)
        };

        // Multiplexer up.
        let start = peer.start().unwrap();
        let (mut pending, _) = feed(&mut host, &mut peer, vec![start]);
        assert!(peer.is_connected(), "multiplexer up through the real host");
        assert!(pending.is_empty(), "UA(0) needs no further reply");

        // DLC open on the advertised channel; the handshake settles in a few
        // exchanges (PN response, SABM, UA + MSC).
        pending.push(peer.open_dlc(3, 127, 7).unwrap());
        for _ in 0..6 {
            if pending.is_empty() {
                break;
            }
            let (next, _) = feed(&mut host, &mut peer, std::mem::take(&mut pending));
            pending = next;
        }
        let dlci = port
            .lock()
            .unwrap()
            .open_dlci
            .expect("the DLC must open through the host");

        // Data from the peer reaches the device, and the echo comes back.
        let frames = peer.write(dlci, b"hello serial").unwrap();
        let (_, events) = feed(&mut host, &mut peer, frames);
        assert_eq!(
            port.lock().unwrap().take_received(),
            vec![b"hello serial".to_vec()],
            "the device sees the peer's bytes through the full stack"
        );
        let echoed: Vec<Vec<u8>> = events
            .iter()
            .filter_map(|e| match e {
                MultiplexerEvent::DataReceived(_, data) => Some(data.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(
            echoed,
            vec![b"hello serial".to_vec()],
            "and the echo returns over the same path"
        );

        // Tearing down the ACL link closes everything.
        host.handle_packet(&[0x04, 0x05, 0x04, 0x00, 0x81, 0x00, 0x13])
            .unwrap();
        assert!(host.connection().is_none());
    }

    #[test]
    fn test_rfcomm_dlc_close_marks_the_port_closed() {
        let (mut peer, mut handler, port) = open_session(3);
        let dlci = port.lock().unwrap().open_dlci.unwrap();

        let disc = peer.close_dlc(dlci).expect("peer closes the DLC");
        shuttle(&mut peer, &mut handler, vec![disc]);

        assert!(
            !port.lock().unwrap().is_open(),
            "the port must close when the peer disconnects the DLC"
        );
    }

    /// A multiplexer session belongs to the L2CAP channel carrying it
    /// (RFCOMM spec §5.1). Keeping one past the channel's death makes the
    /// device look dead to the *next* peer: the stale session drops the new
    /// SABM on DLCI 0 and nothing is ever answered. Caught live — a second
    /// Bumble client could not open a session until the handler was reset.
    #[test]
    fn test_rfcomm_session_is_reset_when_the_channel_closes() {
        let (mut first, mut handler, port) = open_session(3);
        assert!(first.is_connected(), "the first peer's session comes up");
        let disconnect = first.disconnect().expect("first peer tears down");
        shuttle(&mut first, &mut handler, vec![disconnect]);

        // What the host does when the channel or the ACL goes away.
        handler.on_channel_closed();
        assert!(
            !port.lock().unwrap().is_open(),
            "no DLC survives the channel that carried it"
        );

        // A second peer, arriving on a fresh channel, must be answered.
        let mut second = Multiplexer::new(Role::Initiator, 672);
        let sabm = second.start().expect("second peer starts a session");
        shuttle(&mut second, &mut handler, vec![sabm]);
        assert!(
            second.is_connected(),
            "a new peer must get a session after the previous one left"
        );

        let pn = second.open_dlc(3, 127, 7).expect("PN for the channel");
        shuttle(&mut second, &mut handler, vec![pn]);
        assert!(
            port.lock().unwrap().is_open(),
            "and must be able to open the advertised DLC again"
        );
    }

    /// The mirror of the reset: a device that queued bytes with nobody
    /// connected still has them when someone does connect. The port outlives
    /// the session because it belongs to the device, not to the peer.
    #[test]
    fn test_a_write_survives_the_session_that_was_not_there_to_carry_it() {
        let (mut peer, mut handler, port) = open_session(3);
        let disconnect = peer.disconnect().expect("peer tears down");
        shuttle(&mut peer, &mut handler, vec![disconnect]);
        handler.on_channel_closed();

        port.lock().unwrap().write(b"queued while alone".to_vec());
        assert!(
            handler.poll_output(672).is_empty(),
            "with no DLC open there is nowhere to send it, so it waits"
        );

        let mut next = Multiplexer::new(Role::Initiator, 672);
        let sabm = next.start().expect("a new peer arrives");
        shuttle(&mut next, &mut handler, vec![sabm]);
        let pn = next.open_dlc(3, 127, 7).expect("PN for the channel");
        let events = shuttle(&mut next, &mut handler, vec![pn]);

        let delivered: Vec<Vec<u8>> = events
            .into_iter()
            .filter_map(|event| match event {
                MultiplexerEvent::DataReceived(_, data) => Some(data),
                _ => None,
            })
            .collect();
        assert_eq!(
            delivered,
            vec![b"queued while alone".to_vec()],
            "the queued write reaches the peer that eventually connects"
        );
    }
}
