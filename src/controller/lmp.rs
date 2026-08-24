// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Link Manager Protocol (LMP) PDU definitions and a peer-controller link
//! establishment simulator for Bluetooth Classic (BR/EDR), Bluetooth Core Spec
//! Vol 2, Part C. Simble models both ends of a Classic ACL link establishment
//! (`LmpLink`) directly in this module: construct one `LmpLink` per side, feed
//! each side's outgoing bytes
//! into the other's [`LmpLink::receive`], and both converge on
//! `LmpLinkState::Connected` with each other's feature masks known — the same
//! plain, synchronous, no-async state-machine style as `l2cap::coc::CoCChannel`.
//! Only connection establishment and feature exchange are modeled; authentication,
//! encryption, and role switch are out of scope for this pass.

use crate::types::SimbleError;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

/// LMP opcode constants (Bluetooth Core Spec Vol 2, Part C, Appendix I).
pub mod opcode {
    /// LMP_accepted opcode.
    pub const ACCEPTED: u8 = 3;
    /// LMP_not_accepted opcode.
    pub const NOT_ACCEPTED: u8 = 4;
    /// LMP_features_req opcode.
    pub const FEATURES_REQ: u8 = 39;
    /// LMP_features_res opcode.
    pub const FEATURES_RES: u8 = 40;
    /// LMP_host_connection_req opcode.
    pub const HOST_CONNECTION_REQ: u8 = 51;
}

/// Baseband/LMP error codes usable as the reason in an `LmpNotAccepted` PDU
/// (a subset of the shared HCI error code space, Core Spec Vol 1, Part F).
pub mod reject_reason {
    /// Connection rejected due to limited resources (0x0D).
    pub const CONNECTION_REJECTED_LIMITED_RESOURCES: u8 = 0x0D;
    /// Unsupported LMP parameter value (0x20).
    pub const UNSUPPORTED_LMP_PARAMETER_VALUE: u8 = 0x20;
}

/// The first octet of every LMP PDU: bit 0 is the transaction ID (0 if the
/// transaction was initiated by the central/master, 1 if by the
/// peripheral/slave), bits 1-7 are the opcode (opcodes used here all fit in 7
/// bits, so the extended-opcode escape at 0x7F is not needed).
fn pack_header(tid: u8, opcode: u8) -> u8 {
    (tid & 0x01) | (opcode << 1)
}

fn header_opcode(header: u8) -> u8 {
    header >> 1
}

fn header_tid(header: u8) -> u8 {
    header & 0x01
}

/// LMP_host_connection_req: the central requests establishing a logical (ACL)
/// connection over an already-paged link. No parameters beyond the header.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LmpHostConnectionReq {
    /// Packed transaction-ID/opcode header octet.
    pub header: u8,
}

impl LmpHostConnectionReq {
    /// Builds a new `LMP_host_connection_req` PDU with the given transaction ID.
    pub fn new(tid: u8) -> Self {
        Self {
            header: pack_header(tid, opcode::HOST_CONNECTION_REQ),
        }
    }

    /// Returns the transaction ID carried in the header.
    pub fn tid(&self) -> u8 {
        header_tid(self.header)
    }

    /// Parses this PDU from the front of `bytes`, returning `None` on opcode mismatch.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (pkt, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if header_opcode(pkt.header) != opcode::HOST_CONNECTION_REQ {
            return None;
        }
        Some((pkt, rest))
    }
}

/// LMP_accepted: generic positive response, carries the opcode being accepted.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LmpAccepted {
    /// Packed transaction-ID/opcode header octet.
    pub header: u8,
    /// The opcode being accepted.
    pub accepted_opcode: u8,
}

impl LmpAccepted {
    /// Builds a new `LMP_accepted` PDU acknowledging `accepted_opcode`.
    pub fn new(tid: u8, accepted_opcode: u8) -> Self {
        Self {
            header: pack_header(tid, opcode::ACCEPTED),
            accepted_opcode,
        }
    }

    /// Returns the transaction ID carried in the header.
    pub fn tid(&self) -> u8 {
        header_tid(self.header)
    }

    /// Parses this PDU from the front of `bytes`, returning `None` on opcode mismatch.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (pkt, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if header_opcode(pkt.header) != opcode::ACCEPTED {
            return None;
        }
        Some((pkt, rest))
    }
}

/// LMP_not_accepted: generic negative response, carries the rejected opcode
/// and a baseband/HCI error code explaining why.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LmpNotAccepted {
    /// Packed transaction-ID/opcode header octet.
    pub header: u8,
    /// The opcode being rejected.
    pub rejected_opcode: u8,
    /// Baseband/HCI error code explaining the rejection.
    pub error_code: u8,
}

impl LmpNotAccepted {
    /// Builds a new `LMP_not_accepted` PDU rejecting `rejected_opcode` with `error_code`.
    pub fn new(tid: u8, rejected_opcode: u8, error_code: u8) -> Self {
        Self {
            header: pack_header(tid, opcode::NOT_ACCEPTED),
            rejected_opcode,
            error_code,
        }
    }

    /// Returns the transaction ID carried in the header.
    pub fn tid(&self) -> u8 {
        header_tid(self.header)
    }

    /// Parses this PDU from the front of `bytes`, returning `None` on opcode mismatch.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (pkt, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if header_opcode(pkt.header) != opcode::NOT_ACCEPTED {
            return None;
        }
        Some((pkt, rest))
    }
}

/// LMP_features_req: request the peer's page-0 supported-features bitmask.
/// No parameters beyond the header; the reply is an `LmpFeaturesRes`.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LmpFeaturesReq {
    /// Packed transaction-ID/opcode header octet.
    pub header: u8,
}

impl LmpFeaturesReq {
    /// Builds a new `LMP_features_req` PDU with the given transaction ID.
    pub fn new(tid: u8) -> Self {
        Self {
            header: pack_header(tid, opcode::FEATURES_REQ),
        }
    }

    /// Returns the transaction ID carried in the header.
    pub fn tid(&self) -> u8 {
        header_tid(self.header)
    }

    /// Parses this PDU from the front of `bytes`, returning `None` on opcode mismatch.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (pkt, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if header_opcode(pkt.header) != opcode::FEATURES_REQ {
            return None;
        }
        Some((pkt, rest))
    }
}

/// LMP_features_res: the page-0 supported-features bitmask, one bit per
/// feature, byte 0 covering features 0-7 (Core Spec Vol 2, Part C, Section 3.3).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct LmpFeaturesRes {
    /// Packed transaction-ID/opcode header octet.
    pub header: u8,
    /// Page-0 supported-features bitmask, byte 0 covering features 0-7.
    pub features: [u8; 8],
}

impl LmpFeaturesRes {
    /// Builds a new `LMP_features_res` PDU carrying the given feature bitmask.
    pub fn new(tid: u8, features: [u8; 8]) -> Self {
        Self {
            header: pack_header(tid, opcode::FEATURES_RES),
            features,
        }
    }

    /// Returns the transaction ID carried in the header.
    pub fn tid(&self) -> u8 {
        header_tid(self.header)
    }

    /// Parses this PDU from the front of `bytes`, returning `None` on opcode mismatch.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (pkt, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if header_opcode(pkt.header) != opcode::FEATURES_RES {
            return None;
        }
        Some((pkt, rest))
    }
}

/// Which end of the link this `LmpLink` represents. Central always initiates
/// the connection request; role never changes here since role switch is out
/// of scope for this pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmpRole {
    /// The connection-initiating end (master).
    Central,
    /// The connection-accepting end (slave).
    Peripheral,
}

/// Coarse link-establishment state, driven entirely by `LmpLink::receive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LmpLinkState {
    /// No connection attempt in progress.
    Idle,
    /// Central has sent `host_connection_req` and awaits a response.
    ConnectionRequested,
    /// The connection request was accepted; feature exchange is underway.
    ConnectionAccepted,
    /// Connection established and both feature masks are known.
    Connected,
    /// The connection attempt was rejected.
    Rejected,
    /// A `host_connection_req` arrived and this end is waiting for its own
    /// *host* to accept or reject it.
    ///
    /// A controller cannot answer a page by itself: the spec says it raises
    /// an HCI Connection Request event and does nothing further until the
    /// host sends Accept Connection Request or Reject Connection Request
    /// (Vol 4, Part E, Sections 7.7.4, 7.1.8, 7.1.9). Without this state an
    /// `LmpLink` answers on the host's behalf, which is fine for a
    /// peer-to-peer test but wrong for anything with an HCI boundary above
    /// it — the host would be told about a connection it never agreed to.
    ConnectionPending,
}

/// One end of a simulated Classic ACL link, driving LMP connection
/// establishment and feature exchange against a peer `LmpLink`. This has no
/// knowledge of the other instance — the caller (a test, or eventually
/// `VirtualDevice`/controller wiring) is responsible for delivering each
/// side's `receive` output to the other side, the same ownership pattern
/// `CoCManager`/`CoCChannel` use for L2CAP CoC.
#[derive(Debug, Clone)]
pub struct LmpLink {
    /// Which end of the link this instance represents.
    pub role: LmpRole,
    /// This end's page-0 supported-features bitmask.
    pub local_features: [u8; 8],
    /// The peer's advertised features, once received.
    pub peer_features: Option<[u8; 8]>,
    /// Current link-establishment state.
    pub state: LmpLinkState,
    /// Error code recorded when the link was rejected, if any.
    pub rejected_reason: Option<u8>,
    /// Whether the peripheral should accept an incoming `host_connection_req`;
    /// flip to `false` to exercise the rejection path. Ignored when
    /// `defer_to_host` is set — a deferred link has no opinion of its own.
    pub accept_connections: bool,
    /// When set, an incoming `host_connection_req` parks the link in
    /// [`LmpLinkState::ConnectionPending`] and answers nothing; the owner
    /// must then call [`Self::accept_pending_connection`] or
    /// [`Self::reject_pending_connection`].
    ///
    /// This is what a *controller* needs: the decision belongs to the host
    /// above the HCI boundary, not to the link manager. Left `false` the link
    /// behaves exactly as before, so the peer-to-peer tests are unaffected.
    pub defer_to_host: bool,
    requested_peer_features: bool,
    /// Transaction ID of the `host_connection_req` awaiting a host decision.
    pending_tid: u8,
}

impl LmpLink {
    /// Creates a new link end for `role` advertising `local_features`.
    pub fn new(role: LmpRole, local_features: [u8; 8]) -> Self {
        Self {
            role,
            local_features,
            peer_features: None,
            state: LmpLinkState::Idle,
            rejected_reason: None,
            accept_connections: true,
            defer_to_host: false,
            requested_peer_features: false,
            pending_tid: 0,
        }
    }

    /// A peripheral link end that defers every inbound `host_connection_req`
    /// to its host — the shape a simulated controller wants, since answering
    /// a page is the host's decision (HCI Accept/Reject Connection Request).
    pub fn deferring(local_features: [u8; 8]) -> Self {
        Self {
            defer_to_host: true,
            ..Self::new(LmpRole::Peripheral, local_features)
        }
    }

    /// Whether a `host_connection_req` is parked waiting for a host decision.
    pub fn has_pending_connection(&self) -> bool {
        self.state == LmpLinkState::ConnectionPending
    }

    /// The host said yes: emit the `LMP_accepted` (and this end's own
    /// `LMP_features_req`) that [`Self::receive`] would have emitted
    /// immediately had the link not been deferring.
    pub fn accept_pending_connection(&mut self) -> Result<Vec<Vec<u8>>, SimbleError> {
        if self.state != LmpLinkState::ConnectionPending {
            return Err(SimbleError::DeviceError(
                "LMP: no connection request is awaiting a host decision".into(),
            ));
        }
        self.state = LmpLinkState::ConnectionAccepted;
        self.requested_peer_features = true;
        Ok(vec![
            LmpAccepted::new(self.pending_tid, opcode::HOST_CONNECTION_REQ)
                .as_bytes()
                .to_vec(),
            LmpFeaturesReq::new(self.role_bit()).as_bytes().to_vec(),
        ])
    }

    /// The host said no: emit the `LMP_not_accepted` carrying `reason`.
    pub fn reject_pending_connection(&mut self, reason: u8) -> Result<Vec<Vec<u8>>, SimbleError> {
        if self.state != LmpLinkState::ConnectionPending {
            return Err(SimbleError::DeviceError(
                "LMP: no connection request is awaiting a host decision".into(),
            ));
        }
        self.state = LmpLinkState::Rejected;
        self.rejected_reason = Some(reason);
        Ok(vec![
            LmpNotAccepted::new(self.pending_tid, opcode::HOST_CONNECTION_REQ, reason)
                .as_bytes()
                .to_vec(),
        ])
    }

    /// Returns `true` once the link has reached `Connected`.
    pub fn is_connected(&self) -> bool {
        self.state == LmpLinkState::Connected
    }

    /// The page-0 features both sides actually support: bitwise AND of
    /// `local_features` and the peer's advertised features. `None` until the
    /// peer's features have been received.
    pub fn negotiated_features(&self) -> Option<[u8; 8]> {
        self.peer_features
            .map(|peer| std::array::from_fn(|i| self.local_features[i] & peer[i]))
    }

    fn role_bit(&self) -> u8 {
        match self.role {
            LmpRole::Central => 0,
            LmpRole::Peripheral => 1,
        }
    }

    /// Central-only: builds the initial `LMP_host_connection_req` and moves
    /// this end into `ConnectionRequested`.
    pub fn build_connection_request(&mut self) -> Result<Vec<u8>, SimbleError> {
        if self.role != LmpRole::Central {
            return Err(SimbleError::DeviceError(
                "LMP: only the central initiates a host_connection_req".into(),
            ));
        }
        if self.state != LmpLinkState::Idle {
            return Err(SimbleError::DeviceError(
                "LMP: connection establishment already in progress".into(),
            ));
        }
        self.state = LmpLinkState::ConnectionRequested;
        Ok(LmpHostConnectionReq::new(self.role_bit())
            .as_bytes()
            .to_vec())
    }

    /// Processes one incoming LMP PDU, returning zero or more PDUs to send
    /// back to the peer (a single incoming PDU can trigger more than one
    /// outgoing PDU, e.g. accepting a connection also kicks off this side's
    /// own feature request).
    pub fn receive(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let header = *bytes
            .first()
            .ok_or_else(|| SimbleError::PacketParseError("LMP: empty packet".into()))?;

        match header_opcode(header) {
            opcode::HOST_CONNECTION_REQ => self.on_host_connection_req(bytes),
            opcode::ACCEPTED => self.on_accepted(bytes),
            opcode::NOT_ACCEPTED => self.on_not_accepted(bytes),
            opcode::FEATURES_REQ => self.on_features_req(bytes),
            opcode::FEATURES_RES => self.on_features_res(bytes),
            other => Err(SimbleError::PacketParseError(format!(
                "LMP: unrecognized opcode {other:#04x}"
            ))),
        }
    }

    fn on_host_connection_req(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let (pkt, _) = LmpHostConnectionReq::parse(bytes).ok_or_else(|| {
            SimbleError::PacketParseError("LMP: malformed host_connection_req".into())
        })?;
        if self.role != LmpRole::Peripheral {
            return Err(SimbleError::DeviceError(
                "LMP: host_connection_req received by a non-peripheral link end".into(),
            ));
        }

        let tid = pkt.tid();
        if self.defer_to_host {
            // Say nothing. The owner raises HCI Connection Request to its
            // host and calls accept/reject when the host answers.
            self.pending_tid = tid;
            self.state = LmpLinkState::ConnectionPending;
            return Ok(Vec::new());
        }
        if !self.accept_connections {
            self.state = LmpLinkState::Rejected;
            self.rejected_reason = Some(reject_reason::CONNECTION_REJECTED_LIMITED_RESOURCES);
            let not_accepted = LmpNotAccepted::new(
                tid,
                opcode::HOST_CONNECTION_REQ,
                reject_reason::CONNECTION_REJECTED_LIMITED_RESOURCES,
            );
            return Ok(vec![not_accepted.as_bytes().to_vec()]);
        }

        self.state = LmpLinkState::ConnectionAccepted;
        let accepted = LmpAccepted::new(tid, opcode::HOST_CONNECTION_REQ);
        self.requested_peer_features = true;
        let features_req = LmpFeaturesReq::new(self.role_bit());
        Ok(vec![
            accepted.as_bytes().to_vec(),
            features_req.as_bytes().to_vec(),
        ])
    }

    fn on_accepted(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let (pkt, _) = LmpAccepted::parse(bytes)
            .ok_or_else(|| SimbleError::PacketParseError("LMP: malformed accepted".into()))?;
        if pkt.accepted_opcode == opcode::HOST_CONNECTION_REQ
            && self.state == LmpLinkState::ConnectionRequested
        {
            self.state = LmpLinkState::ConnectionAccepted;
        }
        Ok(Vec::new())
    }

    fn on_not_accepted(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let (pkt, _) = LmpNotAccepted::parse(bytes)
            .ok_or_else(|| SimbleError::PacketParseError("LMP: malformed not_accepted".into()))?;
        self.state = LmpLinkState::Rejected;
        self.rejected_reason = Some(pkt.error_code);
        Ok(Vec::new())
    }

    fn on_features_req(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let (pkt, _) = LmpFeaturesReq::parse(bytes)
            .ok_or_else(|| SimbleError::PacketParseError("LMP: malformed features_req".into()))?;
        if !matches!(
            self.state,
            LmpLinkState::ConnectionAccepted | LmpLinkState::Connected
        ) {
            return Err(SimbleError::DeviceError(
                "LMP: features_req received before connection establishment".into(),
            ));
        }

        let tid = pkt.tid();
        let mut out = vec![
            LmpFeaturesRes::new(tid, self.local_features)
                .as_bytes()
                .to_vec(),
        ];
        if !self.requested_peer_features {
            self.requested_peer_features = true;
            out.push(LmpFeaturesReq::new(self.role_bit()).as_bytes().to_vec());
        }
        Ok(out)
    }

    fn on_features_res(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let (pkt, _) = LmpFeaturesRes::parse(bytes)
            .ok_or_else(|| SimbleError::PacketParseError("LMP: malformed features_res".into()))?;
        self.peer_features = Some(pkt.features);
        if self.state == LmpLinkState::ConnectionAccepted {
            self.state = LmpLinkState::Connected;
        }
        Ok(Vec::new())
    }
}

#[cfg(test)]
#[path = "lmp_tests.rs"]
mod tests;
