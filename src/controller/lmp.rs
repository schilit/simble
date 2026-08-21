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
    /// flip to `false` to exercise the rejection path.
    pub accept_connections: bool,
    requested_peer_features: bool,
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
            requested_peer_features: false,
        }
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
mod tests {
    use super::*;

    fn drive_to_connected(central: &mut LmpLink, peripheral: &mut LmpLink) {
        let mut to_peripheral = vec![central.build_connection_request().unwrap()];
        let mut to_central: Vec<Vec<u8>> = Vec::new();

        for _ in 0..8 {
            if to_peripheral.is_empty() && to_central.is_empty() {
                break;
            }
            let mut next_to_central = Vec::new();
            for pkt in to_peripheral.drain(..) {
                next_to_central.extend(peripheral.receive(&pkt).unwrap());
            }
            let mut next_to_peripheral = Vec::new();
            for pkt in to_central.drain(..) {
                next_to_peripheral.extend(central.receive(&pkt).unwrap());
            }
            to_central = next_to_central;
            to_peripheral = next_to_peripheral;
        }
    }

    #[test]
    fn test_connection_establishment_and_feature_exchange() {
        let central_features = [0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let peripheral_features = [0x0F, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

        let mut central = LmpLink::new(LmpRole::Central, central_features);
        let mut peripheral = LmpLink::new(LmpRole::Peripheral, peripheral_features);

        drive_to_connected(&mut central, &mut peripheral);

        assert!(central.is_connected());
        assert!(peripheral.is_connected());
        assert_eq!(central.peer_features, Some(peripheral_features));
        assert_eq!(peripheral.peer_features, Some(central_features));
    }

    #[test]
    fn test_negotiated_features_is_intersection() {
        let central_features = [0b1111_0000, 0, 0, 0, 0, 0, 0, 0];
        let peripheral_features = [0b1100_1100, 0, 0, 0, 0, 0, 0, 0];

        let mut central = LmpLink::new(LmpRole::Central, central_features);
        let mut peripheral = LmpLink::new(LmpRole::Peripheral, peripheral_features);
        drive_to_connected(&mut central, &mut peripheral);

        let expected = [0b1100_0000, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(central.negotiated_features(), Some(expected));
        assert_eq!(peripheral.negotiated_features(), Some(expected));
    }

    #[test]
    fn test_connection_rejected() {
        let mut central = LmpLink::new(LmpRole::Central, [0; 8]);
        let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0; 8]);
        peripheral.accept_connections = false;

        let req = central.build_connection_request().unwrap();
        let responses = peripheral.receive(&req).unwrap();
        assert_eq!(responses.len(), 1);

        let outcome = central.receive(&responses[0]).unwrap();
        assert!(outcome.is_empty());
        assert_eq!(central.state, LmpLinkState::Rejected);
        assert_eq!(peripheral.state, LmpLinkState::Rejected);
        assert!(!central.is_connected());
    }

    #[test]
    fn test_malformed_packet_rejected() {
        let mut link = LmpLink::new(LmpRole::Peripheral, [0; 8]);
        assert!(link.receive(&[]).is_err());

        // opcode 0 is unassigned.
        assert!(link.receive(&[0x00]).is_err());
    }

    #[test]
    fn test_features_req_before_connection_established_is_rejected() {
        let mut link = LmpLink::new(LmpRole::Peripheral, [0; 8]);
        let req = LmpFeaturesReq::new(0);
        assert!(link.receive(req.as_bytes()).is_err());
    }

    #[test]
    fn test_only_central_builds_connection_request() {
        let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0; 8]);
        assert!(peripheral.build_connection_request().is_err());
    }
}
