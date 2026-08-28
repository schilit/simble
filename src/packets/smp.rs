// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SMP (Security Manager Protocol) PDU definitions and zero-copy parsers.

use zerocopy::byteorder::little_endian::U16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

/// SMP Opcode constants.
pub mod opcode {
    /// Pairing request.
    pub const PAIRING_REQUEST: u8 = 0x01;
    /// Pairing response.
    pub const PAIRING_RESPONSE: u8 = 0x02;
    /// Pairing confirm.
    pub const PAIRING_CONFIRM: u8 = 0x03;
    /// Pairing random.
    pub const PAIRING_RANDOM: u8 = 0x04;
    /// Pairing failed.
    pub const PAIRING_FAILED: u8 = 0x05;
    /// Encryption info.
    pub const ENCRYPTION_INFO: u8 = 0x06;
    /// Master identification.
    pub const MASTER_IDENTIFICATION: u8 = 0x07;
    /// Identity info.
    pub const IDENTITY_INFO: u8 = 0x08;
    /// Identity addr info.
    pub const IDENTITY_ADDR_INFO: u8 = 0x09;
    /// Signing info.
    pub const SIGNING_INFO: u8 = 0x0A;
    /// Security request.
    pub const SECURITY_REQUEST: u8 = 0x0B;
    /// Pairing public key.
    pub const PAIRING_PUBLIC_KEY: u8 = 0x0C;
    /// Pairing dhkey check.
    pub const PAIRING_DHKEY_CHECK: u8 = 0x0D;
}

/// IO Capability options for pairing.
pub mod io_capability {
    /// Display only.
    pub const DISPLAY_ONLY: u8 = 0x00;
    /// Display yes no.
    pub const DISPLAY_YES_NO: u8 = 0x01;
    /// Keyboard only.
    pub const KEYBOARD_ONLY: u8 = 0x02;
    /// No input no output.
    pub const NO_INPUT_NO_OUTPUT: u8 = 0x03;
    /// Keyboard display.
    pub const KEYBOARD_DISPLAY: u8 = 0x04;
}

/// SMP Pairing Failed reason codes (Bluetooth Core Spec Vol 3, Part H, Section 3.5.5).
pub mod error_code {
    /// Passkey entry failed.
    pub const PASSKEY_ENTRY_FAILED: u8 = 0x01;
    /// Oob not available.
    pub const OOB_NOT_AVAILABLE: u8 = 0x02;
    /// Authentication requirements.
    pub const AUTHENTICATION_REQUIREMENTS: u8 = 0x03;
    /// Confirm value failed.
    pub const CONFIRM_VALUE_FAILED: u8 = 0x04;
    /// Pairing not supported.
    pub const PAIRING_NOT_SUPPORTED: u8 = 0x05;
    /// Encryption key size.
    pub const ENCRYPTION_KEY_SIZE: u8 = 0x06;
    /// Command not supported.
    pub const COMMAND_NOT_SUPPORTED: u8 = 0x07;
    /// Unspecified reason.
    pub const UNSPECIFIED_REASON: u8 = 0x08;
    /// Repeated attempts.
    pub const REPEATED_ATTEMPTS: u8 = 0x09;
    /// Invalid parameters.
    pub const INVALID_PARAMETERS: u8 = 0x0A;
    /// Dhkey check failed.
    pub const DHKEY_CHECK_FAILED: u8 = 0x0B;
    /// Numeric comparison failed.
    pub const NUMERIC_COMPARISON_FAILED: u8 = 0x0C;
    /// Br edr pairing in progress.
    pub const BR_EDR_PAIRING_IN_PROGRESS: u8 = 0x0D;
    /// Cross transport key derivation not allowed.
    pub const CROSS_TRANSPORT_KEY_DERIVATION_NOT_ALLOWED: u8 = 0x0E;
}

/// Auth Req bit flags (Bluetooth Core Spec Vol 3, Part H, Figure 3.3).
pub mod auth_req {
    /// Bonding.
    pub const BONDING: u8 = 0b0000_0001;
    /// Mitm.
    pub const MITM: u8 = 0b0000_0100;
    /// Sc.
    pub const SC: u8 = 0b0000_1000;
    /// Keypress.
    pub const KEYPRESS: u8 = 0b0001_0000;
    /// Ct2.
    pub const CT2: u8 = 0b0010_0000;
}

/// Key Distribution / Generation bit flags (Bluetooth Core Spec Vol 3, Part H, Section 3.6.1).
pub mod key_distribution {
    /// Enc key.
    pub const ENC_KEY: u8 = 0b0001;
    /// Id key.
    pub const ID_KEY: u8 = 0b0010;
    /// Sign key.
    pub const SIGN_KEY: u8 = 0b0100;
    /// Link key.
    pub const LINK_KEY: u8 = 0b1000;
}

/// SMP Pairing Request / Response PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct SmpPairingPacket {
    /// Opcode.
    pub opcode: u8,
    /// Io capability.
    pub io_capability: u8,
    /// Oob data flag.
    pub oob_data_flag: u8,
    /// Auth req.
    pub auth_req: u8,
    /// Max encryption key size.
    pub max_encryption_key_size: u8,
    /// Initiator key distribution.
    pub initiator_key_distribution: u8,
    /// Responder key distribution.
    pub responder_key_distribution: u8,
}

impl SmpPairingPacket {
    /// Parses this value from its byte representation.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        Ref::<&[u8], Self>::from_prefix(bytes).ok()
    }

    /// New response.
    pub fn new_response(io_cap: u8, auth_req: u8, init_key_dist: u8, resp_key_dist: u8) -> Self {
        Self {
            opcode: opcode::PAIRING_RESPONSE,
            io_capability: io_cap,
            oob_data_flag: 0x00,
            auth_req,
            max_encryption_key_size: 16,
            initiator_key_distribution: init_key_dist,
            responder_key_distribution: resp_key_dist,
        }
    }
}

/// SMP Pairing Failed PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct SmpPairingFailed {
    /// Opcode.
    pub opcode: u8,
    /// Reason.
    pub reason: u8,
}

impl SmpPairingFailed {
    /// Creates a new instance.
    pub fn new(reason: u8) -> Self {
        Self {
            opcode: opcode::PAIRING_FAILED,
            reason,
        }
    }

    /// Parses this value from its byte representation.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (ref_val, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if ref_val.opcode != opcode::PAIRING_FAILED {
            return None;
        }
        Some((ref_val, rest))
    }
}

/// SMP Master Identification PDU (opcode 0x07, Core Spec Vol 3, Part H,
/// Section 3.6.3). Carries the EDIV and Rand that, together with the LTK from
/// the preceding Encryption Information, let the peer restart encryption for
/// this bond later. EDIV is little-endian on the wire — the one field here
/// whose byte order a hand-rolled parser can get wrong, which is exactly why
/// it is a typed [`U16`] rather than two indexed bytes.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct MasterIdentification {
    /// Opcode (0x07).
    pub opcode: u8,
    /// Encrypted Diversifier, little-endian.
    pub ediv: U16,
    /// 8-byte Rand.
    pub rand: [u8; 8],
}

impl MasterIdentification {
    /// Builds a Master Identification PDU.
    pub fn new(ediv: u16, rand: [u8; 8]) -> Self {
        Self {
            opcode: opcode::MASTER_IDENTIFICATION,
            ediv: U16::new(ediv),
            rand,
        }
    }

    /// Parses a Master Identification PDU, rejecting a mismatched opcode.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (ref_val, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if ref_val.opcode != opcode::MASTER_IDENTIFICATION {
            return None;
        }
        Some((ref_val, rest))
    }
}

/// SMP Identity Address Information PDU (opcode 0x09, Core Spec Vol 3, Part H,
/// Section 3.6.5). Distributes the peer's identity address — the key a bond
/// record is filed under, so its two fields (the address type and the six
/// address octets) must land in exactly the right places.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct IdentityAddressInformation {
    /// Opcode (0x09).
    pub opcode: u8,
    /// Address type: 0x00 public, 0x01 static random. Section 3.6.5 defines
    /// no other value; the parser range-checks it.
    pub addr_type: u8,
    /// Six-octet identity address, wire order (least-significant byte first).
    pub address: [u8; 6],
}

impl IdentityAddressInformation {
    /// Builds an Identity Address Information PDU.
    pub fn new(addr_type: u8, address: [u8; 6]) -> Self {
        Self {
            opcode: opcode::IDENTITY_ADDR_INFO,
            addr_type,
            address,
        }
    }

    /// Parses an Identity Address Information PDU, rejecting a mismatched
    /// opcode. The address-type range check stays with the caller, which owns
    /// the bond record the value keys.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (ref_val, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if ref_val.opcode != opcode::IDENTITY_ADDR_INFO {
            return None;
        }
        Some((ref_val, rest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smp_pairing_packet_parsing() {
        let resp =
            SmpPairingPacket::new_response(io_capability::NO_INPUT_NO_OUTPUT, 0x01, 0x07, 0x07);

        let bytes = resp.as_bytes();
        let (parsed, _) = SmpPairingPacket::parse(bytes).expect("Valid SMP parse");
        assert_eq!(parsed.io_capability, io_capability::NO_INPUT_NO_OUTPUT);
    }

    /// SMP pairing PDUs are byte-packed with no length or endianness fields.
    /// `Preq`/`Pres` are fed verbatim into the confirm-value calculation, so a
    /// struct that gained padding would corrupt pairing rather than fail loudly.
    #[test]
    fn test_wire_layout_has_no_padding() {
        assert_eq!(core::mem::size_of::<SmpPairingPacket>(), 7);
        assert_eq!(core::mem::align_of::<SmpPairingPacket>(), 1);
        assert_eq!(core::mem::size_of::<SmpPairingFailed>(), 2);
        assert_eq!(core::mem::align_of::<SmpPairingFailed>(), 1);
    }

    /// Exact on-the-wire bytes, Core Spec Vol 3, Part H, Figure 3.1.
    #[test]
    fn test_exact_wire_bytes_round_trip() {
        // Pairing Request: NoInputNoOutput, no OOB, bonding + Secure Connections,
        // 16-byte max key, ENC|ID|SIGN distributed both ways.
        let wire = [0x01, 0x03, 0x00, 0x09, 0x10, 0x07, 0x07];
        let (req, rest) = SmpPairingPacket::parse(&wire).expect("pairing request");
        assert_eq!(req.opcode, opcode::PAIRING_REQUEST);
        assert_eq!(req.io_capability, io_capability::NO_INPUT_NO_OUTPUT);
        assert_eq!(req.oob_data_flag, 0x00);
        assert_eq!(req.auth_req, auth_req::BONDING | auth_req::SC);
        assert_eq!(req.max_encryption_key_size, 16);
        assert_eq!(req.initiator_key_distribution, 0x07);
        assert_eq!(req.responder_key_distribution, 0x07);
        assert!(rest.is_empty());
        assert_eq!(req.as_bytes(), &wire);

        // The response built from those parameters differs only in opcode.
        let resp = SmpPairingPacket::new_response(
            io_capability::NO_INPUT_NO_OUTPUT,
            auth_req::BONDING | auth_req::SC,
            0x07,
            0x07,
        );
        assert_eq!(resp.as_bytes(), &[0x02, 0x03, 0x00, 0x09, 0x10, 0x07, 0x07]);

        // Pairing Failed: Confirm Value Failed.
        let failed = SmpPairingFailed::new(error_code::CONFIRM_VALUE_FAILED);
        assert_eq!(failed.as_bytes(), &[0x05, 0x04]);
        let (parsed, rest) = SmpPairingFailed::parse(failed.as_bytes()).expect("pairing failed");
        assert_eq!(parsed.reason, error_code::CONFIRM_VALUE_FAILED);
        assert!(rest.is_empty());
    }

    /// Truncated PDUs are rejected, and `SmpPairingFailed` refuses a PDU that
    /// is long enough but carries a different opcode.
    #[test]
    fn test_rejects_truncated_and_mismatched_pdus() {
        assert!(SmpPairingPacket::parse(&[]).is_none());
        assert!(SmpPairingPacket::parse(&[0x01, 0x03, 0x00, 0x09, 0x10, 0x07]).is_none());
        assert!(SmpPairingFailed::parse(&[0x05]).is_none());

        // A Pairing Confirm is not a Pairing Failed.
        assert!(SmpPairingFailed::parse(&[opcode::PAIRING_CONFIRM, 0x04]).is_none());

        // Trailing bytes beyond the fixed struct are returned, not consumed.
        let (_, rest) =
            SmpPairingPacket::parse(&[0x01, 0x03, 0x00, 0x09, 0x10, 0x07, 0x07, 0xAA]).unwrap();
        assert_eq!(rest, &[0xAA]);
    }

    /// `SmpPairingPacket` deliberately does not filter on opcode, because it
    /// backs both Pairing Request (0x01) and Pairing Response (0x02). Callers
    /// dispatch on the opcode byte before calling it. Pinned so that anyone
    /// adding an opcode check here notices the shared use first.
    #[test]
    fn test_pairing_packet_parse_does_not_filter_opcode() {
        let (parsed, _) = SmpPairingPacket::parse(&[0x02, 0x03, 0x00, 0x09, 0x10, 0x07, 0x07])
            .expect("pairing response parses too");
        assert_eq!(parsed.opcode, opcode::PAIRING_RESPONSE);
    }

    /// The key-distribution PDUs are byte-packed like the pairing PDUs, so the
    /// same no-padding guarantee has to hold or the wire read shifts.
    #[test]
    fn test_key_distribution_wire_layout_has_no_padding() {
        assert_eq!(core::mem::size_of::<MasterIdentification>(), 11);
        assert_eq!(core::mem::align_of::<MasterIdentification>(), 1);
        assert_eq!(core::mem::size_of::<IdentityAddressInformation>(), 8);
        assert_eq!(core::mem::align_of::<IdentityAddressInformation>(), 1);
    }

    /// Exact on-the-wire bytes for Master Identification (Section 3.6.3): the
    /// EDIV is little-endian, so 0x1234 is `34 12`.
    #[test]
    fn test_master_identification_wire_bytes() {
        let rand = [1, 2, 3, 4, 5, 6, 7, 8];
        let pdu = MasterIdentification::new(0x1234, rand);
        let wire = [0x07, 0x34, 0x12, 1, 2, 3, 4, 5, 6, 7, 8];
        assert_eq!(pdu.as_bytes(), &wire);

        let (parsed, rest) = MasterIdentification::parse(&wire).expect("master id");
        assert_eq!(parsed.ediv.get(), 0x1234);
        assert_eq!(parsed.rand, rand);
        assert!(rest.is_empty());

        // A different opcode is refused even at the right length.
        assert!(MasterIdentification::parse(&[opcode::ENCRYPTION_INFO; 11]).is_none());
        // Truncated PDUs are refused.
        assert!(MasterIdentification::parse(&wire[..10]).is_none());
    }

    /// Exact on-the-wire bytes for Identity Address Information (Section 3.6.5).
    #[test]
    fn test_identity_address_information_wire_bytes() {
        let addr = [0x06, 0x05, 0x04, 0x03, 0x02, 0x01];
        let pdu = IdentityAddressInformation::new(0x01, addr);
        let wire = [0x09, 0x01, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01];
        assert_eq!(pdu.as_bytes(), &wire);

        let (parsed, rest) = IdentityAddressInformation::parse(&wire).expect("identity addr");
        assert_eq!(parsed.addr_type, 0x01);
        assert_eq!(parsed.address, addr);
        assert!(rest.is_empty());

        assert!(IdentityAddressInformation::parse(&[opcode::IDENTITY_INFO; 8]).is_none());
        assert!(IdentityAddressInformation::parse(&wire[..7]).is_none());
    }
}
