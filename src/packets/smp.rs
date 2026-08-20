// Copyright 2026 The Android Open Source Project
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
