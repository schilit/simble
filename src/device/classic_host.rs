// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The **BR/EDR host**: the layer that turns simble's Classic protocol
//! modules ([`crate::classic`]) and its L2CAP Classic channel manager
//! (`crate::l2cap::classic::ClassicChannelManager`) into a device a real stack can
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
use crate::l2cap::{AclReassembler, L2capHeader, cid};
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
    /// Write Extended Inquiry Response: the 240 octets an inquiring peer
    /// gets *with* the inquiry result, before it has connected to anything.
    pub const WRITE_EXTENDED_INQUIRY_RESPONSE: [u8; 2] = [0x52, 0x0C];
    /// Read Buffer Size: how large an ACL packet the controller accepts and
    /// how many it can hold — the two numbers outbound ACL must obey.
    pub const READ_BUFFER_SIZE: [u8; 2] = [0x05, 0x10];

    // --- security ---------------------------------------------------------
    //
    // Opcodes are written little-endian here (OCF byte first), which is the
    // order they go on the wire. `WRITE_SIMPLE_PAIRING_MODE` above is 0x0C56
    // and *not* 0x0C45 — 0x0C45 is `WRITE_INQUIRY_MODE`, right beside it.
    /// Link Key Request Reply — "yes, I am bonded to this device".
    pub const LINK_KEY_REQUEST_REPLY: [u8; 2] = [0x0B, 0x04];
    /// Link Key Request Negative Reply — "no, and this is what starts SSP".
    pub const LINK_KEY_REQUEST_NEGATIVE_REPLY: [u8; 2] = [0x0C, 0x04];
    /// Authentication Requested.
    pub const AUTHENTICATION_REQUESTED: [u8; 2] = [0x11, 0x04];
    /// Set Connection Encryption.
    pub const SET_CONNECTION_ENCRYPTION: [u8; 2] = [0x13, 0x04];
    /// IO Capability Request Reply.
    pub const IO_CAPABILITY_REQUEST_REPLY: [u8; 2] = [0x2B, 0x04];
    /// User Confirmation Request Reply.
    pub const USER_CONFIRMATION_REQUEST_REPLY: [u8; 2] = [0x2C, 0x04];
    /// User Confirmation Request Negative Reply.
    pub const USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY: [u8; 2] = [0x2D, 0x04];
    /// User Passkey Request Reply.
    pub const USER_PASSKEY_REQUEST_REPLY: [u8; 2] = [0x2E, 0x04];
    /// User Passkey Request Negative Reply.
    pub const USER_PASSKEY_REQUEST_NEGATIVE_REPLY: [u8; 2] = [0x2F, 0x04];
    /// Setup Synchronous Connection: open the SCO/eSCO link that carries
    /// call audio, over an ACL that already exists.
    pub const SETUP_SYNCHRONOUS_CONNECTION: [u8; 2] = [0x28, 0x04];
    /// Accept Synchronous Connection Request — the answer to a Connection
    /// Request whose link type is SCO or eSCO. Answering one of those with
    /// plain Accept Connection Request gets silence.
    pub const ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST: [u8; 2] = [0x29, 0x04];
    /// Reject Synchronous Connection Request.
    pub const REJECT_SYNCHRONOUS_CONNECTION_REQUEST: [u8; 2] = [0x2A, 0x04];
}

/// Link types, as Connection Request and the connection-complete events
/// report them (Vol 4, Part E, Section 7.7.4).
pub mod link_type {
    /// SCO — a plain synchronous link.
    pub const SCO: u8 = 0x00;
    /// ACL — the asynchronous link everything else rides.
    pub const ACL: u8 = 0x01;
    /// eSCO — an extended synchronous link, which wideband speech needs.
    pub const ESCO: u8 = 0x02;
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

    // --- security ---------------------------------------------------------
    /// Authentication Complete — the answer to Authentication Requested.
    pub const AUTHENTICATION_COMPLETE: u8 = 0x06;
    /// Link Key Request — the controller asking whether we are bonded.
    pub const LINK_KEY_REQUEST: u8 = 0x17;
    /// Link Key Notification — a new key to store.
    pub const LINK_KEY_NOTIFICATION: u8 = 0x18;
    /// IO Capability Request.
    pub const IO_CAPABILITY_REQUEST: u8 = 0x31;
    /// IO Capability Response — the peer's capabilities.
    pub const IO_CAPABILITY_RESPONSE: u8 = 0x32;
    /// User Confirmation Request, carrying six digits.
    pub const USER_CONFIRMATION_REQUEST: u8 = 0x33;
    /// User Passkey Request — type the digits the peer is showing.
    pub const USER_PASSKEY_REQUEST: u8 = 0x34;
    /// Simple Pairing Complete.
    pub const SIMPLE_PAIRING_COMPLETE: u8 = 0x36;
    /// User Passkey Notification — the digits to show.
    pub const USER_PASSKEY_NOTIFICATION: u8 = 0x3B;
}

/// IO capabilities this host can claim in IO Capability Request Reply
/// (Vol 4, Part E, Section 7.7.40).
///
/// The value chosen decides which association model the two controllers
/// select, so it is the one knob that changes whether a bond is
/// MITM-protected. It is *not* a description of the hardware simble is
/// running on — it is a description of what the application will do when
/// asked, which is why it is set per-host rather than globally.
pub mod io_capability {
    /// Shows a number, cannot answer yes/no.
    pub const DISPLAY_ONLY: u8 = 0x00;
    /// Shows a number and can answer yes/no. With MITM asked for at either
    /// end this is what gets Numeric Comparison.
    pub const DISPLAY_YES_NO: u8 = 0x01;
    /// Can type digits, shows nothing.
    pub const KEYBOARD_ONLY: u8 = 0x02;
    /// Neither, so nothing a person does can protect the link: every model
    /// involving one of these is Just Works.
    pub const NO_INPUT_NO_OUTPUT: u8 = 0x03;
}

/// `Authentication_Requirements` values (Vol 4, Part E, Section 7.7.40). The
/// odd values are the MITM ones.
pub mod authentication_requirements {
    /// No bonding, MITM protection not required.
    pub const NO_BONDING: u8 = 0x00;
    /// No bonding, MITM protection required.
    pub const NO_BONDING_MITM: u8 = 0x01;
    /// Dedicated bonding, MITM protection not required.
    pub const DEDICATED_BONDING: u8 = 0x02;
    /// Dedicated bonding, MITM protection required.
    pub const DEDICATED_BONDING_MITM: u8 = 0x03;
    /// General bonding, MITM protection not required — the usual default for
    /// a device that wants to keep the key and does not need a person.
    pub const GENERAL_BONDING: u8 = 0x04;
    /// General bonding, MITM protection required.
    pub const GENERAL_BONDING_MITM: u8 = 0x05;
}

/// A link key this host has stored for a peer, as Link Key Notification
/// delivered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinkKey {
    /// The sixteen key octets, in the order the controller gave them and the
    /// order Link Key Request Reply must give them back.
    pub value: [u8; 16],
    /// `Key_Type` (Vol 4, Part E, Section 7.7.24). 0x04/0x08 are the
    /// unauthenticated combination keys Just Works produces, 0x05/0x07 the
    /// authenticated ones — which is how a service that requires MITM
    /// protection can tell a bond it may trust from one it may not.
    pub key_type: u8,
}

impl LinkKey {
    /// Whether a person took part in creating this key, so it resists a
    /// man-in-the-middle. Both the P-192 (0x05) and P-256 (0x07) authenticated
    /// combination key types count.
    pub fn is_authenticated(&self) -> bool {
        matches!(self.key_type, 0x05 | 0x07)
    }
}

/// How far a link has got through security.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkSecurity {
    /// Whether an Authentication Complete arrived with a success status — the
    /// link has a key both ends agree on.
    pub authenticated: bool,
    /// Whether an Encryption Change said encryption is on.
    pub encrypted: bool,
    /// The status of the last Simple Pairing Complete, if pairing ran. `None`
    /// means it did not — which, on a link that authenticated anyway, is the
    /// signature of a bonded reconnect.
    pub pairing_status: Option<u8>,
    /// The peer's IO capability, as its IO Capability Response reported it.
    pub peer_io_capability: Option<u8>,
    /// The last six-digit value a User Confirmation Request showed.
    pub numeric_value: Option<u32>,
}

/// The BD_ADDR at `offset` in an event's parameters. HCI carries addresses
/// least-significant octet first, which is the order [`Address::new`] takes,
/// so this is a copy and not a reversal — the mistake it exists to prevent is
/// reaching for `from_be_bytes` here.
fn address_at(parameters: &[u8], offset: usize) -> Option<Address> {
    Some(Address::new(
        parameters.get(offset..offset + 6)?.try_into().ok()?,
    ))
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
///
/// One packet, whatever the size — callers whose PDUs can exceed the
/// controller's ACL buffer use [`acl_packets`] instead. The signalling
/// PDUs built through this are tens of bytes; no controller's buffer is
/// that small.
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

/// Fragments an L2CAP PDU across as many HCI ACL packets as the
/// controller's `mtu` (HC_ACL_Data_Packet_Length) requires: the first
/// carries the boundary flag saying so, the rest say Continuing.
///
/// A controller does not transmit an ACL packet larger than the buffer it
/// declared in Read Buffer Size, and it does not report the discard. The
/// symptom is a stream that "reached STREAMING" and delivered almost
/// nothing — see [`ClassicHost::handle_acl`] for the receive-side mirror
/// of the same lesson.
fn acl_packets(handle: u16, l2cap: &[u8], mtu: usize) -> Vec<Vec<u8>> {
    use crate::l2cap::{AclPacketBoundary, HciAclHeader};
    if l2cap.len() <= mtu {
        return vec![acl_packet(handle, l2cap)];
    }
    let mut out = Vec::with_capacity(l2cap.len().div_ceil(mtu));
    let mut boundary = AclPacketBoundary::FirstNonFlushable;
    let mut rest = l2cap;
    while !rest.is_empty() {
        let take = rest.len().min(mtu);
        let (chunk, remaining) = rest.split_at(take);
        let header = HciAclHeader::new(handle, boundary, chunk.len() as u16);
        let mut packet = Vec::with_capacity(5 + chunk.len());
        packet.push(crate::transport::h4_type::HCI_ACL_DATA);
        packet.extend_from_slice(header.as_bytes());
        packet.extend_from_slice(chunk);
        out.push(packet);
        boundary = AclPacketBoundary::Continuing;
        rest = remaining;
    }
    out
}

/// Wraps a call-audio payload in an H4 synchronous (SCO) packet for
/// `sco_handle` (Vol 4, Part E, Section 5.4.3).
///
/// `sco_handle` is the **synchronous** link's handle, from Synchronous
/// Connection Complete — not the ACL handle it was set up over. The two come
/// out of the same 12-bit space and a mix-up produces a perfectly valid
/// packet that reaches nobody.
///
/// The Packet_Status_Flag is left at 0b00, "correctly received data": this
/// host models no loss, and any other value would be a claim about an air
/// interface that is not simulated.
fn sco_packet(sco_handle: u16, payload: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(4 + payload.len());
    packet.push(crate::transport::h4_type::HCI_SCO_DATA);
    packet.extend_from_slice(&(sco_handle & 0x0FFF).to_le_bytes());
    packet.push(payload.len() as u8);
    packet.extend_from_slice(payload);
    packet
}

/// The audio connection as this host holds it: the handle SCO packets are
/// addressed to, and what the controller said it agreed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScoConnection {
    /// The synchronous link's own connection handle.
    pub handle: u16,
    /// [`link_type::SCO`] or [`link_type::ESCO`].
    pub link_type: u8,
    /// Air mode: 0x02 CVSD, 0x03 transparent (what wideband speech uses).
    pub air_mode: u8,
}

/// What this host does with an inbound synchronous Connection Request.
///
/// A headset that has not been told there is a call has a legitimate reason
/// to refuse audio, and "reject" must be reachable — a host that can only
/// accept cannot be tested for the half-open handle a refusal must not leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScoPolicy {
    /// Answer with Accept Synchronous Connection Request.
    #[default]
    Accept,
    /// Answer with Reject Synchronous Connection Request and this reason.
    Reject(u8),
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

/// One open L2CAP channel, as a profile sees it.
///
/// The local CID is the identity: it is what distinguishes AVDTP's media
/// transport channel from its signalling channel, which share a PSM and are
/// told apart by nothing else. `psm` is here too because a handler serving
/// more than one PSM — Classic HID's control (0x0011) and interrupt (0x0013)
/// — needs to know which of its channels spoke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HandlerChannel {
    /// The PSM this channel was opened on.
    pub psm: u16,
    /// The local CID — this channel's stable identity for the handler.
    pub cid: u16,
    /// The peer's MTU: the largest SDU this channel may carry outbound.
    pub peer_mtu: u16,
}

/// What a profile does with data on an open L2CAP channel: it sees the
/// payload and returns whatever should be sent back on the same channel.
/// SDP is one of these; RFCOMM, HID and AVDTP fit the same seam.
///
/// # One handler, several channels
///
/// The single-channel methods ([`Self::psm`], [`Self::on_data`],
/// [`Self::poll_output`]) are the whole trait for a profile that runs one
/// channel — SDP and RFCOMM both do. Two profiles do not:
///
/// * **Classic HID** runs a control channel on PSM 0x0011 and an interrupt
///   channel on PSM 0x0013. One device, two PSMs.
/// * **AVDTP** runs signalling and every media transport on PSM 0x0019, as
///   separate L2CAP channels. One device, one PSM, several *channels*.
///
/// So the host's routing table stays keyed on **PSM** — [`Self::psms`] is a
/// set rather than a single value — and the channel's identity is handed
/// *through* to the handler by [`Self::on_channel_data`], which keys its own
/// per-channel state on the CID. The alternative, keying the host's table on
/// `(psm, cid)`, was rejected: at the moment a second 0x0019 channel is
/// accepted the host has no way to know what role it plays. Only the profile
/// knows, and only because an AVDTP OPEN just succeeded. Routing decisions
/// belong where the knowledge is.
/// `Any` is a supertrait so a caller can get its handler back out of the
/// host: registering one *moves* it into a `Box<dyn ProtocolHandler>`, and
/// without a downcast the only way to read a profile's state would be to
/// mirror every field of it through an `Arc<Mutex<_>>` — which is what
/// [`SharedRfcommPort`] does, and is worth it there because the port is a
/// seam two owners write to. A speaker's stream state has one owner.
pub trait ProtocolHandler: std::fmt::Debug + std::any::Any {
    /// The PSM this handler serves. For a multi-PSM handler this is the
    /// *primary* one — the channel a peer connects first.
    fn psm(&self) -> u16;

    /// Every PSM this handler serves, in the order a peer meets them.
    ///
    /// Defaults to just [`Self::psm`], which is the whole story for SDP and
    /// RFCOMM. Classic HID overrides it with control *and* interrupt.
    fn psms(&self) -> Vec<u16> {
        vec![self.psm()]
    }

    /// A channel this handler serves has finished configuring and can carry
    /// data. This is how a handler learns the CID of a channel it asked for
    /// with [`Self::poll_channel_requests`], and how an AVDTP sink learns
    /// that the second 0x0019 channel — the media transport — has arrived.
    fn on_channel_open(&mut self, channel: HandlerChannel) {
        let _ = channel;
    }

    /// One of this handler's channels went away, named by its local CID.
    ///
    /// [`Self::on_channel_closed`] is the coarse form: *every* channel gone,
    /// so discard the session. This is the fine one, and the difference
    /// matters — an AVDTP media transport closing must detach that transport
    /// and nothing else, while the signalling channel closing ends the
    /// session.
    fn on_channel_lost(&mut self, cid: u16) {
        let _ = cid;
    }

    /// Handles one inbound SDU, told which channel it arrived on; returns
    /// the SDUs to reply with **on that same channel**.
    ///
    /// Defaults to [`Self::on_data`], which is why every single-channel
    /// handler is unaffected by any of this.
    fn on_channel_data(&mut self, channel: HandlerChannel, data: &[u8]) -> Vec<Vec<u8>> {
        self.on_data(data, channel.peer_mtu)
    }

    /// Unprompted SDUs for one specific channel; see [`Self::poll_output`].
    ///
    /// Called once per open channel this handler serves, so a multi-channel
    /// handler must answer for the channel it is asked about and no other —
    /// returning a signalling PDU when polled for the media channel would
    /// put it on the wrong CID.
    fn poll_channel_output(&mut self, channel: HandlerChannel) -> Vec<Vec<u8>> {
        self.poll_output(channel.peer_mtu)
    }

    /// PSMs this handler wants a *new* outbound L2CAP channel on, drained by
    /// the host after every packet. The CID it gets back arrives at
    /// [`Self::on_channel_open`].
    ///
    /// This exists because a profile cannot open an L2CAP channel itself:
    /// the channel manager and the ACL handle belong to the host. AVDTP is
    /// the reason — its media transport is a second channel that only the
    /// profile knows it is time to open, at the moment OPEN succeeds, with
    /// no peer traffic to hang the decision off.
    fn poll_channel_requests(&mut self) -> Vec<u16> {
        Vec::new()
    }

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

/// One record's worth of what [`SdpQueryHandler::read_record`] understood.
/// Private: [`SdpQueryResults`] is the public shape.
struct ReadRecord {
    psm: Option<u16>,
    channel: Option<u8>,
    classes: Vec<SdpUuid>,
    version: Option<u16>,
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
    /// L2CAP PSMs advertised, with their service classes. This is where a
    /// profile that runs straight over L2CAP rather than over RFCOMM shows
    /// up — A2DP, AVRCP, HID — and such a record contributes nothing to
    /// `rfcomm_channels`, so a caller looking for one must look here.
    pub l2cap_psms: Vec<(u16, Vec<SdpUuid>)>,
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

    /// The L2CAP PSM of the service advertising `uuid`, if any — the AVDTP
    /// PSM for an Audio Sink, the AVCTP PSM for a remote control. The
    /// counterpart of [`Self::channel_for`] for profiles that do not use
    /// RFCOMM.
    pub fn psm_for(&self, uuid: SdpUuid) -> Option<u16> {
        self.l2cap_psms
            .iter()
            .find(|(_, classes)| classes.contains(&uuid))
            .map(|(psm, _)| *psm)
    }
}

/// A shared [`SdpQueryResults`]: the handler writes, the driver reads.
pub type SharedSdpQueryResults = std::sync::Arc<std::sync::Mutex<SdpQueryResults>>;

/// The SDP **client** as a channel handler: it asks the peer what it offers
/// and records the answer.
///
/// It builds `SdpPdu`s directly rather than using `classic::sdp::SdpClient`.
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
    /// `Self::want_profile_version` for why that is not the default.
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

    /// Pulls the L2CAP PSM, RFCOMM server channel, service classes and
    /// profile version out of one record's attribute list.
    ///
    /// A ProtocolDescriptorList is a sequence of layers, each `[UUID,
    /// parameters…]`. Layers are identified **by their UUID, not by their
    /// position**: this used to read "the second element of the second
    /// layer" and call it an RFCOMM channel, which is only true for a record
    /// that happens to stack RFCOMM over L2CAP. An A2DP record stacks AVDTP
    /// instead — `[[L2CAP, PSM], [AVDTP, version]]` — so the positional read
    /// reported the *AVDTP version's* low byte as an RFCOMM server channel
    /// (0x0103 became "channel 3"), and the PSM the record exists to publish
    /// was never read at all. Both halves of that mattered: a caller looking
    /// for an Audio Sink found a plausible-looking wrong number, and a
    /// caller looking for the AVDTP PSM found nothing.
    fn read_record(attributes: &[ServiceAttribute]) -> Option<ReadRecord> {
        let mut psm = None;
        let mut channel = None;
        let mut classes = Vec::new();
        let mut version = None;
        for attribute in attributes {
            match attribute.id {
                attribute_id::PROTOCOL_DESCRIPTOR_LIST => {
                    let Some(layers) = attribute.value.as_sequence() else {
                        continue;
                    };
                    for layer in layers {
                        let Some(items) = layer.as_sequence() else {
                            continue;
                        };
                        let Some(protocol) = items.first().and_then(DataElement::as_uuid) else {
                            continue;
                        };
                        let Some((value, _)) =
                            items.get(1).and_then(DataElement::as_unsigned_integer)
                        else {
                            continue;
                        };
                        if protocol == SdpUuid::BT_L2CAP_PROTOCOL_ID {
                            psm = Some(value as u16);
                        } else if protocol == SdpUuid::BT_RFCOMM_PROTOCOL_ID {
                            channel = Some(value as u8);
                        }
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
        // A record with neither a PSM nor a channel names nothing openable,
        // so there is nothing to report. Either one alone is enough: an
        // A2DP sink publishes only a PSM.
        if psm.is_none() && channel.is_none() {
            return None;
        }
        Some(ReadRecord {
            psm,
            channel,
            classes,
            version,
        })
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
                    if let Some(record) = Self::read_record(&attributes) {
                        if let Some(channel) = record.channel {
                            results
                                .rfcomm_channels
                                .push((channel, record.classes.clone()));
                        }
                        if let Some(psm) = record.psm {
                            results.l2cap_psms.push((psm, record.classes));
                        }
                        results.profile_version = results.profile_version.or(record.version);
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
    /// Why the last paging attempt failed, if it did.
    ///
    /// A Connection Complete carrying a non-zero status used to be dropped on
    /// the floor: the arm below matched only `status == 0x00`, so a refused
    /// page left `connection` as `None` forever and the only symptom was a
    /// host that waited. Found on real hardware, where a dongle still
    /// page-scanning under a dead host answered with `0x10`, Connection
    /// Accept Timeout — twenty seconds of silence that should have been one
    /// line.
    connection_failure: Option<u8>,
    /// Next signalling identifier to use for host-initiated requests.
    next_identifier: u8,
    /// Local CIDs this host has accepted or opened, so channel state can be
    /// inspected (the CID allocator does not expose iteration).
    local_cids: Vec<u16>,
    /// Devices seen during inquiry, in the order they answered.
    discovered: Vec<DiscoveredDevice>,
    /// Whether an Inquiry Complete has arrived since the last inquiry began.
    inquiry_finished: bool,
    /// The audio connection riding the ACL, if one is up.
    sco: Option<ScoConnection>,
    /// What to answer an inbound synchronous Connection Request with.
    sco_policy: ScoPolicy,
    /// The Voice Setting and packet types the next Setup Synchronous
    /// Connection asks for — the codec seam, filled in from
    /// [`crate::classic::hfp::AudioCodec`] by whoever runs the profile.
    sco_voice_setting: u16,
    /// Packet-type mask: HV1|HV2|HV3 for narrowband, EV3 for wideband.
    sco_packet_type: u16,
    /// Call audio that arrived on the synchronous link, oldest first.
    sco_received: Vec<Vec<u8>>,
    /// Why the last audio setup failed, if it did.
    sco_failure: Option<u8>,
    /// Link keys this host has stored, by peer address. This is the bond
    /// database: what it holds is the difference between a reconnect that
    /// pairs again and one that does not, and it survives disconnection
    /// because that is the entire point of a bond.
    link_keys: Vec<(Address, LinkKey)>,
    /// What this host answers an IO Capability Request with.
    io_capability: u8,
    /// The `Authentication_Requirements` it asks for.
    authentication_requirements: u8,
    /// Whether a User Confirmation Request is accepted. A host with no
    /// person attached has to decide this in advance; setting it false is how
    /// a test makes a peer refuse.
    accept_pairing: bool,
    /// The digits to answer a User Passkey Request with, if this host claims
    /// a keyboard. Without one it answers the negative reply, which is the
    /// honest answer for a device that cannot type.
    passkey: Option<u32>,
    /// Security state of the current link.
    security: LinkSecurity,
    /// Local CIDs whose handler has already been told the channel is open,
    /// so `on_channel_open` fires exactly once per channel. A channel is not
    /// open when it is created — it opens when both sides have configured —
    /// and there is no event for that, only a state the poll loop notices.
    announced_cids: Vec<u16>,
    /// Reassembles inbound L2CAP frames that the controller split across
    /// several HCI ACL packets. See [`Self::handle_acl`] for why a Classic
    /// host cannot do without one.
    acl_reassembler: AclReassembler,
    /// The controller's HC_ACL_Data_Packet_Length, learned from Read Buffer
    /// Size. Outbound L2CAP frames larger than this are fragmented across
    /// HCI ACL packets; sending one oversized packet instead is how 1390
    /// RTP media packets became 5 decoded SBC frames on a CSR8510 — the
    /// controller cannot transmit what does not fit its buffer, and it does
    /// not say so. `usize::MAX` until learned, which every simulated
    /// controller effectively has.
    acl_mtu: usize,
    /// HC_Total_Num_ACL_Data_Packets: how many ACL packets the controller
    /// can hold at once. The other half of the same lesson: a real
    /// controller drops what it has no buffer for, silently.
    acl_credits_total: usize,
    /// ACL packets sent and not yet returned by Number Of Completed Packets.
    acl_in_flight: usize,
    /// Outbound ACL packets awaiting credit. Filled by [`Self::poll`]'s
    /// channel-output path (the only firehose); drained as credits return.
    pending_acl: std::collections::VecDeque<Vec<u8>>,
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
            connection_failure: None,
            next_identifier: 1,
            local_cids: Vec::new(),
            discovered: Vec::new(),
            inquiry_finished: false,
            sco: None,
            sco_policy: ScoPolicy::default(),
            // CVSD over HV1|HV2|HV3: the narrowband call every HFP pair can
            // fall back to, and what a host asks for before a codec has been
            // negotiated.
            sco_voice_setting: 0x0060,
            sco_packet_type: 0x0007,
            sco_received: Vec::new(),
            sco_failure: None,
            link_keys: Vec::new(),
            // DisplayYesNo with general bonding and no MITM demand is what a
            // phone or a laptop claims. Two of these pair with Just Works —
            // asking for MITM is what escalates it to Numeric Comparison, and
            // that is the host's decision to make, not this constructor's.
            io_capability: io_capability::DISPLAY_YES_NO,
            authentication_requirements: authentication_requirements::GENERAL_BONDING,
            accept_pairing: true,
            passkey: None,
            security: LinkSecurity::default(),
            announced_cids: Vec::new(),
            acl_reassembler: AclReassembler::new(),
            acl_mtu: usize::MAX,
            acl_credits_total: usize::MAX,
            acl_in_flight: 0,
            pending_acl: std::collections::VecDeque::new(),
        }
    }

    // -- security ----------------------------------------------------------
    //
    // Everything from here to `handle_packet` is Secure Simple Pairing, link
    // keys and encryption. The events themselves are answered in
    // `handle_security_event`; this is the policy the answers come from.

    /// Sets what this host claims it can show and type, and what it asks the
    /// peer for. Together these choose the association model — see
    /// [`io_capability`] and [`authentication_requirements`].
    pub fn set_io_capability(&mut self, io_capability: u8, requirements: u8) {
        self.io_capability = io_capability;
        self.authentication_requirements = requirements;
    }

    /// Whether to accept a User Confirmation Request. Setting it false makes
    /// this host refuse every pairing, which is the only way to test that the
    /// *other* end fails cleanly.
    pub fn set_accept_pairing(&mut self, accept: bool) {
        self.accept_pairing = accept;
    }

    /// The digits to answer a User Passkey Request with. A host that never
    /// sets one answers User Passkey Request Negative Reply, because a device
    /// with no keyboard genuinely cannot answer.
    pub fn set_passkey(&mut self, passkey: Option<u32>) {
        self.passkey = passkey;
    }

    /// Stores a link key for `peer` as though a Link Key Notification had
    /// delivered it — how a bond survives a process restart, and how a test
    /// arranges a device that is already paired.
    pub fn insert_link_key(&mut self, peer: Address, key: LinkKey) {
        match self.link_keys.iter_mut().find(|(a, _)| *a == peer) {
            Some(entry) => entry.1 = key,
            None => self.link_keys.push((peer, key)),
        }
    }

    /// The link key stored for `peer`, if this host is bonded to it.
    pub fn link_key(&self, peer: Address) -> Option<LinkKey> {
        self.link_keys
            .iter()
            .find(|(a, _)| *a == peer)
            .map(|(_, key)| *key)
    }

    /// Forgets the bond with `peer`. The next connection to it pairs again.
    pub fn remove_link_key(&mut self, peer: Address) -> bool {
        let before = self.link_keys.len();
        self.link_keys.retain(|(a, _)| *a != peer);
        self.link_keys.len() != before
    }

    /// How far the current link has got through security.
    pub fn security(&self) -> LinkSecurity {
        self.security
    }

    /// HCI Authentication Requested on the current ACL link: make the
    /// controller establish a link key, pairing if it has to.
    ///
    /// The answer is a Command Status and then, much later, an Authentication
    /// Complete — with an IO Capability Request, a User Confirmation Request
    /// and a Link Key Notification in between if pairing runs. Nothing here
    /// waits; [`Self::security`] reports what has arrived.
    pub fn authenticate(&self) -> Vec<Vec<u8>> {
        let Some((handle, _)) = self.connection else {
            return Vec::new();
        };
        vec![command(
            opcode::AUTHENTICATION_REQUESTED,
            &handle.to_le_bytes(),
        )]
    }

    /// HCI Set Connection Encryption on the current ACL link. Only legal once
    /// the link has a key: a controller with nothing to encrypt with answers
    /// an Encryption Change carrying an error, and the link stays clear.
    pub fn encrypt(&self, enable: bool) -> Vec<Vec<u8>> {
        let Some((handle, _)) = self.connection else {
            return Vec::new();
        };
        let mut params = handle.to_le_bytes().to_vec();
        params.push(u8::from(enable));
        vec![command(opcode::SET_CONNECTION_ENCRYPTION, &params)]
    }

    /// Registers a protocol handler and **every** PSM it serves, so an
    /// inbound connection request for any of them is accepted and its data
    /// routed here.
    ///
    /// A handler claiming several PSMs is registered atomically: if the
    /// second one is already taken, the first is released again rather than
    /// leaving the host advertising half a profile.
    pub fn register_handler(
        &mut self,
        handler: Box<dyn ProtocolHandler>,
    ) -> Result<(), SimbleError> {
        let psms = handler.psms();
        for (index, psm) in psms.iter().enumerate() {
            if let Err(e) = self.channels.register_server(*psm) {
                for done in &psms[..index] {
                    self.channels.unregister_server(*done);
                }
                return Err(e);
            }
        }
        self.handlers.push(handler);
        Ok(())
    }

    /// The first registered handler of type `T`, for a caller that wants to
    /// read a profile's state back out — an A2DP sink's received frames, an
    /// AVDTP stream's state. `None` if no handler of that type is here.
    pub fn handler<T: ProtocolHandler>(&self) -> Option<&T> {
        self.handlers
            .iter()
            .find_map(|handler| (handler.as_ref() as &dyn std::any::Any).downcast_ref::<T>())
    }

    /// The first registered handler of type `T`, mutably — for a caller that
    /// drives a profile: queueing audio into a source, draining frames from
    /// a sink.
    pub fn handler_mut<T: ProtocolHandler>(&mut self) -> Option<&mut T> {
        self.handlers
            .iter_mut()
            .find_map(|handler| (handler.as_mut() as &mut dyn std::any::Any).downcast_mut::<T>())
    }

    /// The status of the last failed paging attempt, if the last one failed.
    /// Cleared when a new page starts.
    #[must_use]
    pub fn connection_failure(&self) -> Option<u8> {
        self.connection_failure
    }

    /// The current ACL connection as `(handle, peer address)`, if any.
    pub fn connection(&self) -> Option<(u16, Address)> {
        self.connection
    }

    /// The name this host answers a Remote Name Request with.
    pub fn name(&self) -> &str {
        &self.name
    }

    // --- the audio connection (SCO / eSCO) ---------------------------------

    /// The audio connection riding the ACL, if one is up.
    pub fn sco(&self) -> Option<ScoConnection> {
        self.sco
    }

    /// The status the last Synchronous Connection Complete carried, if it
    /// carried a failure. Cleared by a setup that succeeds.
    pub fn sco_failure(&self) -> Option<u8> {
        self.sco_failure
    }

    /// Chooses the Voice Setting and packet types the next Setup Synchronous
    /// Connection will ask for.
    ///
    /// This is where the codec seam lands: HFP settles on a codec, and
    /// `AudioCodec::voice_setting()` / `AudioCodec::esco_packet_type()` turn
    /// that into the two numbers HCI actually carries. Nothing here encodes
    /// anything — the payload crosses the link byte for byte.
    pub fn set_sco_parameters(&mut self, voice_setting: u16, packet_type: u16) {
        self.sco_voice_setting = voice_setting;
        self.sco_packet_type = packet_type;
    }

    /// What to answer an inbound synchronous Connection Request with.
    pub fn set_sco_policy(&mut self, policy: ScoPolicy) {
        self.sco_policy = policy;
    }

    /// Opens the audio connection over the current ACL: HCI Setup
    /// Synchronous Connection (Vol 4, Part E, Section 7.1.26).
    ///
    /// Empty when there is no ACL to hang it off, or when audio is already
    /// up — a second setup on the same link is refused by the controller
    /// anyway, and sending one is how a host ends up processing a failure
    /// event for a link that is working.
    pub fn setup_sco(&self) -> Vec<Vec<u8>> {
        let Some((handle, _)) = self.connection else {
            return Vec::new();
        };
        if self.sco.is_some() {
            return Vec::new();
        }
        let mut params = Vec::with_capacity(17);
        params.extend_from_slice(&handle.to_le_bytes());
        params.extend_from_slice(&8000u32.to_le_bytes()); // Transmit_Bandwidth
        params.extend_from_slice(&8000u32.to_le_bytes()); // Receive_Bandwidth
        params.extend_from_slice(&0xFFFFu16.to_le_bytes()); // Max_Latency: don't care
        params.extend_from_slice(&self.sco_voice_setting.to_le_bytes());
        params.push(0xFF); // Retransmission_Effort: don't care
        params.extend_from_slice(&self.sco_packet_type.to_le_bytes());
        vec![command(opcode::SETUP_SYNCHRONOUS_CONNECTION, &params)]
    }

    /// Hangs up the audio connection while leaving the ACL — and so the
    /// call's AT signalling — alone.
    pub fn disconnect_sco(&self) -> Vec<Vec<u8>> {
        let Some(sco) = self.sco else {
            return Vec::new();
        };
        let mut params = sco.handle.to_le_bytes().to_vec();
        params.push(0x13); // Remote User Terminated Connection
        vec![command(opcode::DISCONNECT, &params)]
    }

    /// Frames `payload` as one HCI synchronous data packet on the audio
    /// connection. Empty when there is no audio connection to put it on,
    /// which is the honest answer: there is nowhere for it to go.
    pub fn send_sco(&self, payload: &[u8]) -> Vec<Vec<u8>> {
        match self.sco {
            Some(sco) => vec![sco_packet(sco.handle, payload)],
            None => Vec::new(),
        }
    }

    /// Takes the call audio that has arrived since the last call.
    pub fn take_sco_received(&mut self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.sco_received)
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
            // Bits 0..=61, not 0xFF x8: bits 62-63 are reserved and a real
            // controller rejects the whole command for setting them (see
            // `host::EVENT_MASK_ALL`).
            command(opcode::SET_EVENT_MASK, &crate::device::host::EVENT_MASK_ALL),
            command(opcode::READ_BUFFER_SIZE, &[]),
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
    pub fn create_connection(&mut self, address: Address) -> Vec<Vec<u8>> {
        // A new attempt clears the last one's verdict, so a stale failure
        // cannot be read as this page's outcome.
        self.connection_failure = None;
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
            Some(&crate::transport::h4_type::HCI_SCO_DATA) => {
                self.handle_sco(packet);
                Vec::new()
            }
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
            // --- SCO / eSCO ---
            //
            // A Connection Request whose link type is SCO or eSCO is a
            // request for *audio*, not for a second ACL, and it is answered
            // with a different command. This host used to answer every
            // Connection Request with Accept Connection Request regardless;
            // that is a silent hang the moment a peer asks for call audio,
            // because the controller has no page to match it against.
            HciEvent::ConnectionRequest(request) if request.link_type != link_type::ACL => {
                self.answer_synchronous_request(request.bd_addr)
            }
            HciEvent::SynchronousConnectionComplete(complete) => {
                if complete.status == 0x00 {
                    self.sco = Some(ScoConnection {
                        handle: complete.connection_handle.get(),
                        link_type: complete.link_type,
                        air_mode: complete.air_mode,
                    });
                    self.sco_failure = None;
                } else {
                    // A refusal is an answer. Recording it is what lets the
                    // layer above stop asking — a host that only ever
                    // watched for success re-sends the setup forever and
                    // reports nothing, which looks exactly like a link that
                    // is merely slow.
                    self.sco_failure = Some(complete.status);
                }
                Vec::new()
            }
            // --- end SCO / eSCO ---
            HciEvent::ConnectionRequest(request) => {
                // Answer the page, or the peer's connection attempt times
                // out: Accept Connection Request with role 0x01 (remain
                // peripheral, letting the initiator stay central).
                let mut params = Vec::with_capacity(7);
                params.extend_from_slice(&request.bd_addr);
                params.push(0x01);
                vec![command(opcode::ACCEPT_CONNECTION_REQUEST, &params)]
            }
            // A refused page is reported, not discarded. See
            // `connection_failure`.
            HciEvent::ConnectionComplete(complete) if complete.status != 0x00 => {
                self.connection_failure = Some(complete.status);
                Vec::new()
            }
            HciEvent::ConnectionComplete(complete) => {
                self.connection = Some((
                    complete.connection_handle.get(),
                    Address::new(complete.bd_addr),
                ));
                // A fresh link is unauthenticated and unencrypted whatever the
                // last one was. Carrying the old link's security across would
                // be the worst possible bug in this file: a profile would ask
                // "is this encrypted?" and be told about a connection that
                // has already ended.
                self.security = LinkSecurity::default();
                self.acl_reassembler
                    .on_disconnected(complete.connection_handle.get());
                Vec::new()
            }
            HciEvent::CommandComplete {
                header,
                return_parameters,
            } if header.command_opcode.get() == u16::from_le_bytes(opcode::READ_BUFFER_SIZE) => {
                // status(1) acl_len(2) sco_len(1) acl_num(2) sco_num(2) —
                // Vol 4, Part E, Section 7.4.5.
                if return_parameters.len() >= 8 && return_parameters[0] == 0x00 {
                    let acl_len =
                        u16::from_le_bytes([return_parameters[1], return_parameters[2]]) as usize;
                    let acl_num =
                        u16::from_le_bytes([return_parameters[4], return_parameters[5]]) as usize;
                    if acl_len > 0 {
                        self.acl_mtu = acl_len;
                    }
                    if acl_num > 0 {
                        self.acl_credits_total = acl_num;
                    }
                }
                Vec::new()
            }
            // Number Of Completed Packets (Vol 4, Part E, Section 7.7.19):
            // the controller returning the buffers our sends occupied. The
            // only thing that lets a media stream outrun neither the radio
            // nor the buffer pool.
            HciEvent::Other {
                code: 0x13,
                parameters,
            } => {
                let mut released = 0usize;
                if let Some((&count, rest)) = parameters.split_first() {
                    for pair in rest.as_chunks::<4>().0.iter().take(count as usize) {
                        released += u16::from_le_bytes([pair[2], pair[3]]) as usize;
                    }
                }
                self.acl_in_flight = self.acl_in_flight.saturating_sub(released);
                Vec::new()
            }
            HciEvent::EncryptionChange(change) => {
                // Status first. An Encryption Change carrying an error is the
                // controller saying encryption did *not* start, and reading
                // the enabled byte past it is how a link ends up believing it
                // is encrypted when it is not.
                if change.status == 0x00 {
                    self.security.encrypted = change.encryption_enabled != 0x00;
                }
                Vec::new()
            }
            // Hanging up the audio must not hang up the call. A Disconnection
            // Complete on the SCO handle takes the audio and nothing else —
            // the ACL, its L2CAP channels and the RFCOMM session carrying AT
            // are all still there, and tearing them down here would end the
            // call every time the audio route changed.
            HciEvent::DisconnectionComplete(complete)
                if self
                    .sco
                    .is_some_and(|sco| sco.handle == complete.connection_handle.get()) =>
            {
                self.sco = None;
                self.sco_received.clear();
                Vec::new()
            }
            HciEvent::DisconnectionComplete(ended) => {
                self.connection = None;
                // The audio rode the ACL and cannot outlive it.
                self.sco = None;
                self.sco_received.clear();
                // The link is gone, so its security is gone. The *link keys*
                // are not: they outlive the connection, which is what makes
                // them a bond rather than a session.
                self.security = LinkSecurity::default();
                // A frame half-reassembled when the link dropped must not be
                // completed by the next link's first fragment.
                self.acl_reassembler
                    .on_disconnected(ended.connection_handle.get());
                // The ACL is gone, so every channel riding it is gone too —
                // and a profile holding session state must be told, or it
                // meets the next peer still believing the last one is there.
                self.announced_cids.clear();
                for cid in std::mem::take(&mut self.local_cids) {
                    let psm = self.channels.remove_channel(cid).map(|c| c.psm);
                    if let Some(psm) = psm
                        && let Some(handler) = Self::handler_for(&mut self.handlers, psm)
                    {
                        handler.on_channel_lost(cid);
                    }
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
                if let Some(reply) = self.handle_security_event(code, parameters) {
                    return reply;
                }
                self.handle_discovery_event(code, parameters);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    /// Answers a Connection Request whose link type is SCO or eSCO, per
    /// [`Self::set_sco_policy`].
    ///
    /// Accept carries the same fifteen parameters Setup Synchronous
    /// Connection does, with a BD_ADDR in front instead of a handle — the
    /// acceptor states its own bandwidth, Voice Setting and packet types
    /// rather than inheriting the initiator's.
    fn answer_synchronous_request(&self, bd_addr: [u8; 6]) -> Vec<Vec<u8>> {
        match self.sco_policy {
            ScoPolicy::Accept => {
                let mut params = bd_addr.to_vec();
                params.extend_from_slice(&8000u32.to_le_bytes()); // Transmit_Bandwidth
                params.extend_from_slice(&8000u32.to_le_bytes()); // Receive_Bandwidth
                params.extend_from_slice(&0xFFFFu16.to_le_bytes()); // Max_Latency
                params.extend_from_slice(&self.sco_voice_setting.to_le_bytes());
                params.push(0xFF); // Retransmission_Effort: don't care
                params.extend_from_slice(&self.sco_packet_type.to_le_bytes());
                vec![command(
                    opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST,
                    &params,
                )]
            }
            ScoPolicy::Reject(reason) => {
                let mut params = bd_addr.to_vec();
                params.push(reason);
                vec![command(
                    opcode::REJECT_SYNCHRONOUS_CONNECTION_REQUEST,
                    &params,
                )]
            }
        }
    }

    /// Takes in one HCI synchronous data packet: call audio from the peer.
    ///
    /// Audio on a handle this host does not hold is dropped rather than
    /// queued. A packet on the *ACL* handle looks exactly like this one and
    /// means something else entirely.
    fn handle_sco(&mut self, packet: &[u8]) {
        let Some(sco) = self.sco else {
            return;
        };
        // H4(1) handle+flags(2) length(1), then the payload.
        let Some(header) = packet.get(1..4) else {
            return;
        };
        let handle = u16::from_le_bytes([header[0], header[1]]) & 0x0FFF;
        if handle != sco.handle {
            return;
        }
        let length = usize::from(header[2]);
        let Some(payload) = packet.get(4..4 + length) else {
            return;
        };
        self.sco_received.push(payload.to_vec());
    }

    /// Answers the controller's security questions, returning `None` for an
    /// event that is not one.
    ///
    /// These arrive as [`HciEvent::Other`] because none of them has a typed
    /// variant; the layouts are Vol 4, Part E, Sections 7.7.6, 7.7.23, 7.7.24
    /// and 7.7.40–7.7.48.
    ///
    /// **Every request here must be answered.** A controller that asks a
    /// question and hears nothing does not fail — it sits there, and the
    /// pairing that was supposed to happen simply never does, with no error
    /// anywhere. That is the same failure shape as answering a command with
    /// the wrong event kind, one layer up.
    fn handle_security_event(&mut self, code: u8, parameters: &[u8]) -> Option<Vec<Vec<u8>>> {
        match code {
            event_code::LINK_KEY_REQUEST => {
                let peer = address_at(parameters, 0)?;
                // The bond database is what decides whether SSP runs at all.
                // A negative reply here is not an error: it is the normal
                // answer for a device met for the first time, and it is what
                // starts pairing.
                Some(match self.link_key(peer) {
                    Some(key) => {
                        let mut params = peer.as_slice().to_vec();
                        params.extend_from_slice(&key.value);
                        vec![command(opcode::LINK_KEY_REQUEST_REPLY, &params)]
                    }
                    None => vec![command(
                        opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY,
                        peer.as_slice(),
                    )],
                })
            }
            event_code::LINK_KEY_NOTIFICATION => {
                // BD_ADDR(6), Link_Key(16), Key_Type(1).
                let peer = address_at(parameters, 0)?;
                let value: [u8; 16] = parameters.get(6..22)?.try_into().ok()?;
                let key_type = parameters.get(22).copied()?;
                self.insert_link_key(peer, LinkKey { value, key_type });
                Some(Vec::new())
            }
            event_code::IO_CAPABILITY_REQUEST => {
                let peer = address_at(parameters, 0)?;
                let mut params = peer.as_slice().to_vec();
                params.push(self.io_capability);
                params.push(0x00); // OOB_Data_Present: none, and none is real
                params.push(self.authentication_requirements);
                Some(vec![command(opcode::IO_CAPABILITY_REQUEST_REPLY, &params)])
            }
            event_code::IO_CAPABILITY_RESPONSE => {
                // BD_ADDR(6), IO_Capability(1), OOB(1), Auth_Requirements(1).
                // Nothing to answer — this event exists only to tell a host
                // what the peer said, which is what lets it work out which
                // model is coming before the request for it arrives.
                self.security.peer_io_capability = parameters.get(6).copied();
                Some(Vec::new())
            }
            event_code::USER_CONFIRMATION_REQUEST => {
                // BD_ADDR(6), Numeric_Value(4, little-endian).
                let peer = address_at(parameters, 0)?;
                self.security.numeric_value = parameters
                    .get(6..10)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                let opcode = if self.accept_pairing {
                    opcode::USER_CONFIRMATION_REQUEST_REPLY
                } else {
                    opcode::USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY
                };
                Some(vec![command(opcode, peer.as_slice())])
            }
            event_code::USER_PASSKEY_REQUEST => {
                let peer = address_at(parameters, 0)?;
                Some(match self.passkey.filter(|_| self.accept_pairing) {
                    Some(passkey) => {
                        let mut params = peer.as_slice().to_vec();
                        params.extend_from_slice(&passkey.to_le_bytes());
                        vec![command(opcode::USER_PASSKEY_REQUEST_REPLY, &params)]
                    }
                    None => vec![command(
                        opcode::USER_PASSKEY_REQUEST_NEGATIVE_REPLY,
                        peer.as_slice(),
                    )],
                })
            }
            event_code::USER_PASSKEY_NOTIFICATION => {
                // BD_ADDR(6), Passkey(4). Nothing to answer: this side has
                // only a display, so being told is the whole of its part.
                self.security.numeric_value = parameters
                    .get(6..10)
                    .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]));
                Some(Vec::new())
            }
            event_code::SIMPLE_PAIRING_COMPLETE => {
                // Status(1), BD_ADDR(6). A non-zero status means no key was
                // made; the Link Key Notification that would have carried one
                // never arrives, so there is nothing to undo.
                self.security.pairing_status = parameters.first().copied();
                // A successful pairing means this link now has a key,
                // whichever side asked for it. The *acceptor* is never sent
                // an Authentication Complete — that goes only to the host
                // that issued Authentication Requested — so without this the
                // side that answered a pairing would report a link it just
                // paired as unauthenticated.
                if parameters.first() == Some(&0x00) {
                    self.security.authenticated = true;
                }
                Some(Vec::new())
            }
            event_code::AUTHENTICATION_COMPLETE => {
                // Status(1), Connection_Handle(2).
                self.security.authenticated = parameters.first() == Some(&0x00);
                Some(Vec::new())
            }
            _ => None,
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

    /// HCI Write Extended Inquiry Response: publish this device's name and
    /// the service classes it offers *in the inquiry result itself*.
    ///
    /// Without one, an inquiry result carries an address and a Class of
    /// Device and nothing else — a peer must page and run a Remote Name
    /// Request to learn the name, and an SDP query to learn the services.
    /// Phones do not wait for that to decide what to show: a device is
    /// offered as an audio device largely on its Class of Device and the
    /// service-class UUID list in its EIR, both of which are readable before
    /// any connection exists. A speaker that publishes neither is a nameless
    /// row in a scan list.
    ///
    /// `uuids` are 16-bit service class UUIDs — 0x110B for A2DP Audio Sink.
    /// The payload is the same AD-structure encoding LE advertising uses
    /// (Core Vol 3, Part C, §8), zero-padded to the 240 octets the command
    /// always carries. FEC is requested, which is the usual choice: the
    /// payload here is far short of the size where the encoding cost bites.
    ///
    /// Not part of [`Self::start_commands`] — a device that only ever
    /// initiates has no inquiry response to shape, and the service list is
    /// the caller's to know.
    pub fn set_extended_inquiry_response(&self, name: &str, uuids: &[u16]) -> Vec<Vec<u8>> {
        /// Vol 4, Part E, §7.3.56: FEC_Required(1) then a fixed 240 octets.
        const EIR_DATA_LEN: usize = 240;
        let mut data = crate::gap::advertising::AdvertisingData::new().with_name(name);
        for uuid in uuids {
            data = data.with_service_uuid_16(*uuid);
        }
        let mut parameters = vec![0x01];
        let mut payload = data.to_bytes();
        // A name long enough to overflow is truncated by the builder, not
        // here; anything still over length would make the controller reject
        // the whole command, which would cost the name *and* the services.
        payload.truncate(EIR_DATA_LEN);
        payload.resize(EIR_DATA_LEN, 0x00);
        parameters.extend_from_slice(&payload);
        vec![command(
            opcode::WRITE_EXTENDED_INQUIRY_RESPONSE,
            &parameters,
        )]
    }

    /// One inbound HCI ACL packet, reassembled into an L2CAP frame and
    /// routed to the channel it belongs to.
    ///
    /// The reassembly is not optional. An L2CAP frame larger than the
    /// controller's ACL data packet length arrives as several HCI ACL
    /// packets: the first carries the L2CAP header and the rest are bare
    /// continuation bytes, told apart only by the header's Packet Boundary
    /// flag. This used to parse *every* ACL packet as a fresh L2CAP frame,
    /// which reads a continuation fragment's audio bytes as a length and a
    /// CID and routes the result to a channel that does not exist.
    ///
    /// Nothing in the test suite could see it. Both simulated controllers
    /// carry a 672-byte A2DP media SDU in one piece, so the fragmented path
    /// never ran. A CSR8510 with a real phone streaming into it fragments on
    /// the first media packet, and the symptom is not a dropped packet but a
    /// *corrupt* one: `cid=0xdbb6`, invented out of two bytes of SBC.
    fn handle_acl(&mut self, packet: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        use crate::l2cap::HciAclHeader;
        let Some((header, payload)) = HciAclHeader::parse(&packet[1..]) else {
            return Err(SimbleError::PacketParseError("Invalid ACL header".into()));
        };
        let handle = header.handle();
        let is_first = header.is_first_fragment();
        let Some(frame) = self
            .acl_reassembler
            .push_fragment(handle, is_first, payload)?
        else {
            return Ok(Vec::new());
        };
        let Some((l2cap_header, body)) = L2capHeader::ref_from_prefix(frame.as_slice()).ok() else {
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
                self.announced_cids.retain(|cid| *cid != local_cid);
                if let Some(psm) = psm {
                    // Does this handler still have a channel? A profile that
                    // runs several at once — AVDTP signalling plus a media
                    // transport — must not discard its whole session because
                    // one of them went away. Only the last one ends it.
                    let last = !self.has_channel_served_by(psm);
                    if let Some(handler) = Self::handler_for(&mut self.handlers, psm) {
                        handler.on_channel_lost(local_cid);
                        if last {
                            handler.on_channel_closed();
                        }
                    }
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

    /// Routes an SDU on an open channel to the handler that serves its PSM,
    /// telling that handler which channel it arrived on.
    ///
    /// The lookup is by PSM *set*, not by a single PSM, so one handler can
    /// own several — Classic HID's control and interrupt channels reach the
    /// same device. The CID travels with the data because a PSM alone does
    /// not identify a channel: AVDTP runs signalling and media on 0x0019.
    fn handle_channel_data(&mut self, handle: u16, cid: u16, data: &[u8]) -> Vec<Vec<u8>> {
        let Some(channel) = self.channels.get_channel(cid) else {
            return Vec::new();
        };
        let peer_cid = channel.peer_cid;
        let channel = HandlerChannel {
            psm: channel.psm,
            cid: channel.cid,
            peer_mtu: channel.peer_mtu,
        };
        let Some(handler) = Self::handler_for(&mut self.handlers, channel.psm) else {
            return Vec::new();
        };
        handler
            .on_channel_data(channel, data)
            .into_iter()
            .filter(|reply| !reply.is_empty())
            .map(|reply| acl_packet(handle, &L2capHeader::serialize(peer_cid, &reply)))
            .collect()
    }

    /// Whether any channel still alive belongs to the handler serving `psm`
    /// — over *all* of that handler's PSMs, not just this one. A HID device
    /// whose interrupt channel drops still has its control channel, and is
    /// not finished.
    fn has_channel_served_by(&self, psm: u16) -> bool {
        let Some(handler) = self.handlers.iter().find(|h| h.psms().contains(&psm)) else {
            return false;
        };
        let psms = handler.psms();
        self.local_cids
            .iter()
            .filter_map(|cid| self.channels.get_channel(*cid))
            .any(|channel| psms.contains(&channel.psm))
    }

    /// The handler serving `psm`, if any.
    fn handler_for(
        handlers: &mut [Box<dyn ProtocolHandler>],
        psm: u16,
    ) -> Option<&mut Box<dyn ProtocolHandler>> {
        handlers.iter_mut().find(|h| h.psms().contains(&psm))
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
        let open: Vec<(HandlerChannel, u16)> = self
            .local_cids
            .iter()
            .filter_map(|cid| self.channels.get_channel(*cid))
            .filter(|channel| channel.is_open())
            .map(|channel| {
                (
                    HandlerChannel {
                        psm: channel.psm,
                        cid: channel.cid,
                        peer_mtu: channel.peer_mtu,
                    },
                    channel.peer_cid,
                )
            })
            .collect();

        // A channel becomes usable when both sides have configured, and
        // nothing announces that — so the first poll that finds it open is
        // where a profile learns it has a channel and which CID it is. This
        // runs before any output is collected, so whatever the profile
        // queues on being told leaves in the same batch.
        for (channel, _) in &open {
            if self.announced_cids.contains(&channel.cid) {
                continue;
            }
            self.announced_cids.push(channel.cid);
            if let Some(handler) = Self::handler_for(&mut self.handlers, channel.psm) {
                handler.on_channel_open(*channel);
            }
        }

        for (channel, peer_cid) in open {
            let Some(handler) = Self::handler_for(&mut self.handlers, channel.psm) else {
                continue;
            };
            for sdu in handler.poll_channel_output(channel) {
                if sdu.is_empty() {
                    continue;
                }
                self.pending_acl.extend(acl_packets(
                    handle,
                    &L2capHeader::serialize(peer_cid, &sdu),
                    self.acl_mtu,
                ));
            }
        }
        // Release queued packets only up to the buffers the controller has
        // free. This queue is the media firehose; a Number Of Completed
        // Packets event opens it further on a later poll.
        let mut out = Vec::new();
        while self.acl_in_flight < self.acl_credits_total {
            let Some(packet) = self.pending_acl.pop_front() else {
                break;
            };
            self.acl_in_flight += 1;
            out.push(packet);
        }
        out.extend(self.open_requested_channels());
        out
    }

    /// Opens the L2CAP channels the profiles asked for since the last poll.
    ///
    /// A profile cannot do this itself — the channel manager and the ACL
    /// handle are the host's. AVDTP's media transport is why it exists: a
    /// second channel on a PSM that already has one, opened at a moment only
    /// the profile can recognise.
    fn open_requested_channels(&mut self) -> Vec<Vec<u8>> {
        let mut wanted = Vec::new();
        for handler in &mut self.handlers {
            wanted.extend(handler.poll_channel_requests());
        }
        let mut out = Vec::new();
        for psm in wanted {
            match self.open_channel(psm) {
                Ok(packets) => out.extend(packets),
                // A refused open is the profile's problem to notice: it
                // never sees `on_channel_open` for the channel it asked for.
                Err(_) => continue,
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
#[path = "classic_host_tests.rs"]
mod tests;
