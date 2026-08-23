// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! L2CAP (Logical Link Control and Adaptation Protocol) packet definitions,
//! HCI ACL frame headers, and zero-copy parsers.

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned, byteorder::little_endian::U16,
};

/// Fixed Channel Identifiers (CIDs) in Bluetooth LE.
pub mod cid {
    /// Null / invalid identifier.
    pub const NULL: u16 = 0x0000;
    /// L2CAP Signaling channel (BR/EDR).
    pub const BR_SIGNALING: u16 = 0x0001;
    /// Connectionless channel.
    pub const CONNECTIONLESS: u16 = 0x0002;
    /// Attribute Protocol (ATT) fixed channel.
    pub const ATT: u16 = 0x0004;
    /// LE L2CAP Signaling channel.
    pub const LE_SIGNALING: u16 = 0x0005;
    /// Security Manager Protocol (SMP) fixed channel.
    pub const SMP: u16 = 0x0006;
    /// First dynamically allocated CID.
    pub const DYNAMIC_START: u16 = 0x0040;
}

/// Packet Boundary Flags in HCI ACL Data packets.
#[repr(u8)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum AclPacketBoundary {
    /// First non-automatically-flushable packet (PBF = 0b00).
    FirstNonFlushable = 0b00,
    /// Continuing fragment (PBF = 0b01).
    Continuing = 0b01,
    /// First automatically-flushable packet (PBF = 0b10).
    FirstAutoFlushable = 0b10,
    /// Complete L2CAP PDU (PBF = 0b11).
    CompletePdu = 0b11,
}

/// 4-byte HCI ACL Data Packet Header.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct HciAclHeader {
    /// Lower 12 bits: Connection Handle.
    /// Bits 12-13: Packet Boundary Flag (PB).
    /// Bits 14-15: Broadcast Flag (BC).
    pub handle_and_flags: U16,
    /// Length of data payload in this ACL packet fragment.
    pub data_length: U16,
}

impl HciAclHeader {
    /// Creates a new instance.
    pub fn new(handle: u16, pb: AclPacketBoundary, data_len: u16) -> Self {
        let h_flags = (handle & 0x0FFF) | (((pb as u8) as u16 & 0x03) << 12);
        Self {
            handle_and_flags: U16::from_bytes(h_flags.to_le_bytes()),
            data_length: U16::from_bytes(data_len.to_le_bytes()),
        }
    }

    /// Handle.
    pub fn handle(&self) -> u16 {
        self.handle_and_flags.get() & 0x0FFF
    }

    /// Packet boundary.
    pub fn packet_boundary(&self) -> AclPacketBoundary {
        match (self.handle_and_flags.get() >> 12) & 0x03 {
            0b00 => AclPacketBoundary::FirstNonFlushable,
            0b01 => AclPacketBoundary::Continuing,
            0b10 => AclPacketBoundary::FirstAutoFlushable,
            _ => AclPacketBoundary::CompletePdu,
        }
    }

    /// Whether first fragment.
    pub fn is_first_fragment(&self) -> bool {
        matches!(
            self.packet_boundary(),
            AclPacketBoundary::FirstNonFlushable | AclPacketBoundary::FirstAutoFlushable
        )
    }

    /// Parses this value from its byte representation.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header_ref, payload) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected_len = header_ref.data_length.get() as usize;
        if payload.len() < expected_len {
            return None;
        }
        Some((header_ref, &payload[..expected_len]))
    }
}

/// L2CAP Basic Information Frame (B-frame) Header.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct L2capHeader {
    /// Length of payload in bytes (excluding the 4-byte L2CAP header).
    pub length: U16,
    /// Destination Channel Identifier.
    pub cid: U16,
}

impl L2capHeader {
    /// Parses an L2CAP header from a byte slice using zero-copy prefix extraction.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header_ref, payload) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected_len = header_ref.length.get() as usize;
        if payload.len() < expected_len {
            return None;
        }
        Some((header_ref, &payload[..expected_len]))
    }

    /// Creates a new L2CAP header.
    pub const fn new(length: u16, cid: u16) -> Self {
        Self {
            length: U16::from_bytes(length.to_le_bytes()),
            cid: U16::from_bytes(cid.to_le_bytes()),
        }
    }

    /// Serializes an L2CAP packet with the given CID and payload.
    pub fn serialize(cid: u16, payload: &[u8]) -> Vec<u8> {
        let header = Self::new(payload.len() as u16, cid);
        let mut buf = Vec::with_capacity(4 + payload.len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(payload);
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hci_acl_header_parsing() {
        let payload = [0u8; 10];
        let mut packet = Vec::new();
        let acl = HciAclHeader::new(0x0042, AclPacketBoundary::FirstAutoFlushable, 10);
        packet.extend_from_slice(acl.as_bytes());
        packet.extend_from_slice(&payload);

        let (parsed, parsed_payload) = HciAclHeader::parse(&packet).expect("Parse ACL header");
        assert_eq!(parsed.handle(), 0x0042);
        assert_eq!(
            parsed.packet_boundary(),
            AclPacketBoundary::FirstAutoFlushable
        );
        assert!(parsed.is_first_fragment());
        assert_eq!(parsed_payload.len(), 10);
    }

    #[test]
    fn test_l2cap_header_serialize_and_parse() {
        let payload = [0x08, 0x00, 0x01];
        let packet = L2capHeader::serialize(cid::ATT, &payload);
        assert_eq!(packet.len(), 4 + 3);

        let (header, parsed_payload) = L2capHeader::parse(&packet).expect("Valid parse");
        assert_eq!(header.length.get(), 3);
        assert_eq!(header.cid.get(), cid::ATT);
        assert_eq!(parsed_payload, &payload[..]);
    }

    /// Both headers are exactly 4 bytes on the wire with no padding; the ACL
    /// header in particular is written straight after the HCI packet-type byte.
    #[test]
    fn test_wire_layout_has_no_padding() {
        assert_eq!(core::mem::size_of::<HciAclHeader>(), 4);
        assert_eq!(core::mem::align_of::<HciAclHeader>(), 1);
        assert_eq!(core::mem::size_of::<L2capHeader>(), 4);
        assert_eq!(core::mem::align_of::<L2capHeader>(), 1);
    }

    /// Exact wire bytes: handle in the low 12 bits, PB flag in bits 12-13,
    /// both fields little-endian (Core Spec Vol 4, Part E, Section 5.4.2).
    #[test]
    fn test_hci_acl_header_exact_wire_bytes() {
        // Handle 0x0042, first auto-flushable (0b10) -> 0x2042 -> LE 42 20.
        let acl = HciAclHeader::new(0x0042, AclPacketBoundary::FirstAutoFlushable, 10);
        assert_eq!(acl.as_bytes(), &[0x42, 0x20, 0x0A, 0x00]);

        // Continuing fragment (0b01) on the same handle -> 0x1042.
        let acl = HciAclHeader::new(0x0042, AclPacketBoundary::Continuing, 27);
        assert_eq!(acl.as_bytes(), &[0x42, 0x10, 0x1B, 0x00]);

        // The maximum 12-bit handle must not bleed into the flag bits.
        let acl = HciAclHeader::new(0x0EFF, AclPacketBoundary::FirstNonFlushable, 0);
        assert_eq!(acl.as_bytes(), &[0xFF, 0x0E, 0x00, 0x00]);
        assert_eq!(acl.handle(), 0x0EFF);

        // A handle wider than 12 bits is masked rather than corrupting the flags.
        let acl = HciAclHeader::new(0xF042, AclPacketBoundary::CompletePdu, 1);
        assert_eq!(acl.handle(), 0x0042);
        assert_eq!(acl.packet_boundary(), AclPacketBoundary::CompletePdu);
    }

    /// Every packet-boundary encoding round-trips, and only the two "first"
    /// variants report as a first fragment.
    #[test]
    fn test_packet_boundary_round_trips() {
        for (pb, first) in [
            (AclPacketBoundary::FirstNonFlushable, true),
            (AclPacketBoundary::Continuing, false),
            (AclPacketBoundary::FirstAutoFlushable, true),
            (AclPacketBoundary::CompletePdu, false),
        ] {
            let acl = HciAclHeader::new(0x0001, pb, 0);
            assert_eq!(acl.packet_boundary(), pb, "{pb:?} must round-trip");
            assert_eq!(acl.is_first_fragment(), first, "{pb:?} first-fragment");
        }
    }

    /// A full ACL frame carrying an ATT Write Request, as captured on the wire:
    /// ACL header, L2CAP header on the ATT CID, then the ATT PDU.
    #[test]
    fn test_nested_acl_l2cap_att_frame() {
        let att = [0x12, 0x10, 0x00, 0x01, 0x00];
        let l2cap = L2capHeader::serialize(cid::ATT, &att);
        assert_eq!(
            l2cap,
            vec![0x05, 0x00, 0x04, 0x00, 0x12, 0x10, 0x00, 0x01, 0x00]
        );

        let mut frame = Vec::new();
        let acl_header = HciAclHeader::new(
            0x0040,
            AclPacketBoundary::FirstAutoFlushable,
            l2cap.len() as u16,
        );
        frame.extend_from_slice(acl_header.as_bytes());
        frame.extend_from_slice(&l2cap);
        assert_eq!(
            frame,
            vec![
                0x40, 0x20, 0x09, 0x00, 0x05, 0x00, 0x04, 0x00, 0x12, 0x10, 0x00, 0x01, 0x00
            ]
        );

        let (acl, acl_payload) = HciAclHeader::parse(&frame).expect("ACL header");
        assert_eq!(acl.handle(), 0x0040);
        let (l2c, att_pdu) = L2capHeader::parse(acl_payload).expect("L2CAP header");
        assert_eq!(l2c.cid.get(), cid::ATT);
        assert_eq!(att_pdu, &att);
    }

    /// Both parsers must reject a frame shorter than its own length field and
    /// must trim, not return, bytes beyond it.
    #[test]
    fn test_length_field_is_enforced() {
        // data_length says 10 but only 3 payload bytes follow.
        let mut short = HciAclHeader::new(0x0001, AclPacketBoundary::CompletePdu, 10)
            .as_bytes()
            .to_vec();
        short.extend_from_slice(&[0xAA; 3]);
        assert!(HciAclHeader::parse(&short).is_none());

        // Header itself truncated.
        assert!(HciAclHeader::parse(&[0x01, 0x00, 0x04]).is_none());
        assert!(L2capHeader::parse(&[0x03, 0x00, 0x04]).is_none());

        // L2CAP length 3 but only 2 bytes follow.
        assert!(L2capHeader::parse(&[0x03, 0x00, 0x04, 0x00, 0x08, 0x00]).is_none());

        // Trailing bytes past the declared length are trimmed off, so a second
        // frame in the same buffer cannot leak into the first one's payload.
        let (header, payload) =
            L2capHeader::parse(&[0x02, 0x00, 0x04, 0x00, 0x08, 0x00, 0xDE, 0xAD]).expect("trim");
        assert_eq!(header.length.get(), 2);
        assert_eq!(payload, &[0x08, 0x00]);

        // Zero-length payloads are legal and yield an empty slice.
        let (header, payload) = L2capHeader::parse(&[0x00, 0x00, 0x05, 0x00]).expect("empty");
        assert_eq!(header.cid.get(), cid::LE_SIGNALING);
        assert!(payload.is_empty());
    }
}
