// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SMP (Security Manager Protocol) PDU definitions and zero-copy parsers.

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
