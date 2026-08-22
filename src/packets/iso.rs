// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! HCI Isochronous (ISO) data packets — the LE Audio media plane (Core Spec
//! Vol 4, Part E, Section 5.4.5). An ISO packet carries one *SDU* (a codec
//! frame's worth of audio) from a host to its controller, and back up on the
//! receiving side.
//!
//! Simble models the SDU transport — framing, sequence numbers, routing over
//! the simulated radio — but **not** the LC3 codec or CIS establishment:
//! SDU payloads are opaque bytes, so a scene can carry PCM today and LC3
//! frames once a codec exists, without changing this layer.

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned, byteorder::little_endian::U16,
};

/// PB_Flag values in an ISO Data packet header (Vol 4, Part E, 5.4.5).
pub mod iso_boundary {
    /// First fragment of a fragmented SDU.
    pub const FIRST_FRAGMENT: u8 = 0b00;
    /// A continuation fragment.
    pub const CONTINUATION: u8 = 0b01;
    /// A complete, unfragmented SDU (what this simulation emits).
    pub const COMPLETE_SDU: u8 = 0b10;
    /// The last fragment of a fragmented SDU.
    pub const LAST_FRAGMENT: u8 = 0b11;
}

/// The 4-byte HCI ISO Data packet header that follows the H4 type byte.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct HciIsoHeader {
    /// Bits 0-11: Connection_Handle. Bits 12-13: PB_Flag. Bit 14: TS_Flag.
    pub handle_and_flags: U16,
    /// Length of everything after this header (load header + SDU).
    pub data_total_length: U16,
}

impl HciIsoHeader {
    /// Builds a header for a complete (unfragmented) SDU carrying a
    /// timestamp-free load header.
    pub fn new(handle: u16, data_total_length: u16) -> Self {
        let flags = (handle & 0x0FFF) | ((iso_boundary::COMPLETE_SDU as u16) << 12);
        Self {
            handle_and_flags: U16::new(flags),
            data_total_length: U16::new(data_total_length),
        }
    }

    /// The connection handle this SDU belongs to.
    pub fn handle(&self) -> u16 {
        self.handle_and_flags.get() & 0x0FFF
    }

    /// The packet-boundary flag (see [`iso_boundary`]).
    pub fn boundary(&self) -> u8 {
        ((self.handle_and_flags.get() >> 12) & 0x03) as u8
    }

    /// Whether a timestamp precedes the load header.
    pub fn has_timestamp(&self) -> bool {
        (self.handle_and_flags.get() >> 14) & 0x01 == 1
    }
}

/// The ISO Data Load header present on a first/complete SDU without a
/// timestamp: a packet sequence number and the SDU's length + status.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct IsoDataLoadHeader {
    /// Increments once per SDU, so a sink can detect loss or reordering.
    pub packet_sequence_number: U16,
    /// Bits 0-11: ISO_SDU_Length. Bits 12-13: Packet_Status_Flag.
    pub sdu_length_and_status: U16,
}

impl IsoDataLoadHeader {
    /// Builds a load header for a valid SDU of `sdu_length` bytes.
    pub fn new(sequence_number: u16, sdu_length: u16) -> Self {
        Self {
            packet_sequence_number: U16::new(sequence_number),
            sdu_length_and_status: U16::new(sdu_length & 0x0FFF),
        }
    }

    /// The SDU's length in bytes.
    pub fn sdu_length(&self) -> u16 {
        self.sdu_length_and_status.get() & 0x0FFF
    }

    /// Packet status: 0 valid, 1 possibly invalid, 2 lost.
    pub fn status(&self) -> u8 {
        ((self.sdu_length_and_status.get() >> 12) & 0x03) as u8
    }
}

/// Builds a complete H4 ISO data packet carrying `sdu`.
pub fn build_iso_packet(handle: u16, sequence_number: u16, sdu: &[u8]) -> Vec<u8> {
    let load = IsoDataLoadHeader::new(sequence_number, sdu.len() as u16);
    let header = HciIsoHeader::new(handle, (4 + sdu.len()) as u16);
    let mut packet = Vec::with_capacity(9 + sdu.len());
    packet.push(crate::transport::h4_type::HCI_ISO_DATA);
    packet.extend_from_slice(header.as_bytes());
    packet.extend_from_slice(load.as_bytes());
    packet.extend_from_slice(sdu);
    packet
}

/// One received SDU: its connection handle, sequence number, and payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsoSdu {
    /// The connection the SDU arrived on.
    pub handle: u16,
    /// The sender's packet sequence number.
    pub sequence_number: u16,
    /// The SDU payload (a codec frame; opaque here).
    pub payload: Vec<u8>,
}

/// Parses a complete H4 ISO data packet. `None` if it isn't one, if it is
/// truncated, or if it is a fragment (this simulation sends complete SDUs).
pub fn parse_iso_packet(packet: &[u8]) -> Option<IsoSdu> {
    let (&h4, rest) = packet.split_first()?;
    if h4 != crate::transport::h4_type::HCI_ISO_DATA {
        return None;
    }
    let (header, rest) = HciIsoHeader::ref_from_prefix(rest).ok()?;
    if header.boundary() != iso_boundary::COMPLETE_SDU
        && header.boundary() != iso_boundary::FIRST_FRAGMENT
    {
        return None;
    }
    // A timestamped load header adds 4 leading bytes this build doesn't emit.
    let rest = if header.has_timestamp() {
        rest.get(4..)?
    } else {
        rest
    };
    let (load, payload) = IsoDataLoadHeader::ref_from_prefix(rest).ok()?;
    let length = usize::from(load.sdu_length());
    if payload.len() < length {
        return None;
    }
    Some(IsoSdu {
        handle: header.handle(),
        sequence_number: load.packet_sequence_number.get(),
        payload: payload[..length].to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_iso_packet_round_trip() {
        let sdu = vec![0x11, 0x22, 0x33, 0x44];
        let packet = build_iso_packet(0x0042, 7, &sdu);
        assert_eq!(packet[0], crate::transport::h4_type::HCI_ISO_DATA);

        let parsed = parse_iso_packet(&packet).expect("round trips");
        assert_eq!(parsed.handle, 0x0042);
        assert_eq!(parsed.sequence_number, 7);
        assert_eq!(parsed.payload, sdu);
    }

    #[test]
    fn test_header_flags_are_encoded() {
        let packet = build_iso_packet(0x0FFF, 0, &[0xAA]);
        let (_, rest) = packet.split_first().unwrap();
        let (header, _) = HciIsoHeader::ref_from_prefix(rest).unwrap();
        assert_eq!(header.handle(), 0x0FFF);
        assert_eq!(header.boundary(), iso_boundary::COMPLETE_SDU);
        assert!(!header.has_timestamp());
        // data_total_length covers the load header plus the SDU.
        assert_eq!(header.data_total_length.get(), 5);
    }

    #[test]
    fn test_truncated_and_foreign_packets_are_rejected() {
        assert!(parse_iso_packet(&[0x02, 0x40, 0x00]).is_none(), "ACL data");
        let mut short = build_iso_packet(1, 1, &[1, 2, 3, 4]);
        short.truncate(short.len() - 2);
        assert!(parse_iso_packet(&short).is_none(), "truncated SDU");
    }
}
