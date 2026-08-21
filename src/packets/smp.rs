// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SMP (Security Manager Protocol) PDU definitions and zero-copy parsers.

use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

/// SMP Opcode constants.
pub mod opcode {
    pub const PAIRING_REQUEST: u8 = 0x01;
    pub const PAIRING_RESPONSE: u8 = 0x02;
    pub const PAIRING_CONFIRM: u8 = 0x03;
    pub const PAIRING_RANDOM: u8 = 0x04;
    pub const PAIRING_FAILED: u8 = 0x05;
    pub const ENCRYPTION_INFO: u8 = 0x06;
    pub const MASTER_IDENTIFICATION: u8 = 0x07;
    pub const IDENTITY_INFO: u8 = 0x08;
    pub const IDENTITY_ADDR_INFO: u8 = 0x09;
    pub const SIGNING_INFO: u8 = 0x0A;
    pub const SECURITY_REQUEST: u8 = 0x0B;
    pub const PAIRING_PUBLIC_KEY: u8 = 0x0C;
    pub const PAIRING_DHKEY_CHECK: u8 = 0x0D;
}

/// IO Capability options for pairing.
pub mod io_capability {
    pub const DISPLAY_ONLY: u8 = 0x00;
    pub const DISPLAY_YES_NO: u8 = 0x01;
    pub const KEYBOARD_ONLY: u8 = 0x02;
    pub const NO_INPUT_NO_OUTPUT: u8 = 0x03;
    pub const KEYBOARD_DISPLAY: u8 = 0x04;
}

/// SMP Pairing Failed reason codes (Bluetooth Core Spec Vol 3, Part H, Section 3.5.5).
pub mod error_code {
    pub const PASSKEY_ENTRY_FAILED: u8 = 0x01;
    pub const OOB_NOT_AVAILABLE: u8 = 0x02;
    pub const AUTHENTICATION_REQUIREMENTS: u8 = 0x03;
    pub const CONFIRM_VALUE_FAILED: u8 = 0x04;
    pub const PAIRING_NOT_SUPPORTED: u8 = 0x05;
    pub const ENCRYPTION_KEY_SIZE: u8 = 0x06;
    pub const COMMAND_NOT_SUPPORTED: u8 = 0x07;
    pub const UNSPECIFIED_REASON: u8 = 0x08;
    pub const REPEATED_ATTEMPTS: u8 = 0x09;
    pub const INVALID_PARAMETERS: u8 = 0x0A;
    pub const DHKEY_CHECK_FAILED: u8 = 0x0B;
    pub const NUMERIC_COMPARISON_FAILED: u8 = 0x0C;
    pub const BR_EDR_PAIRING_IN_PROGRESS: u8 = 0x0D;
    pub const CROSS_TRANSPORT_KEY_DERIVATION_NOT_ALLOWED: u8 = 0x0E;
}

/// Auth Req bit flags (Bluetooth Core Spec Vol 3, Part H, Figure 3.3).
pub mod auth_req {
    pub const BONDING: u8 = 0b0000_0001;
    pub const MITM: u8 = 0b0000_0100;
    pub const SC: u8 = 0b0000_1000;
    pub const KEYPRESS: u8 = 0b0001_0000;
    pub const CT2: u8 = 0b0010_0000;
}

/// Key Distribution / Generation bit flags (Bluetooth Core Spec Vol 3, Part H, Section 3.6.1).
pub mod key_distribution {
    pub const ENC_KEY: u8 = 0b0001;
    pub const ID_KEY: u8 = 0b0010;
    pub const SIGN_KEY: u8 = 0b0100;
    pub const LINK_KEY: u8 = 0b1000;
}

/// SMP Pairing Request / Response PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct SmpPairingPacket {
    pub opcode: u8,
    pub io_capability: u8,
    pub oob_data_flag: u8,
    pub auth_req: u8,
    pub max_encryption_key_size: u8,
    pub initiator_key_distribution: u8,
    pub responder_key_distribution: u8,
}

impl SmpPairingPacket {
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        Ref::<&[u8], Self>::from_prefix(bytes).ok()
    }

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
    pub opcode: u8,
    pub reason: u8,
}

impl SmpPairingFailed {
    pub fn new(reason: u8) -> Self {
        Self {
            opcode: opcode::PAIRING_FAILED,
            reason,
        }
    }

    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (ref_val, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        if ref_val.opcode != opcode::PAIRING_FAILED {
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
}
