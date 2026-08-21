// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! L2CAP Signaling and Credit-Based Flow Control packets (Bluetooth Core Vol 3, Part A, Section 4).

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, U16, Unaligned, byteorder::LittleEndian,
};

/// Standard L2CAP Signaling Command Codes.
pub mod signaling_code {
    /// Command reject.
    pub const COMMAND_REJECT: u8 = 0x01;
    /// Connection request.
    pub const CONNECTION_REQUEST: u8 = 0x02;
    /// Connection response.
    pub const CONNECTION_RESPONSE: u8 = 0x03;
    /// Configuration request.
    pub const CONFIGURATION_REQUEST: u8 = 0x04;
    /// Configuration response.
    pub const CONFIGURATION_RESPONSE: u8 = 0x05;
    /// Disconnection request.
    pub const DISCONNECTION_REQUEST: u8 = 0x06;
    /// Disconnection response.
    pub const DISCONNECTION_RESPONSE: u8 = 0x07;
    /// Le credit based connection request.
    pub const LE_CREDIT_BASED_CONNECTION_REQUEST: u8 = 0x14;
    /// Le credit based connection response.
    pub const LE_CREDIT_BASED_CONNECTION_RESPONSE: u8 = 0x15;
    /// Le flow control credit.
    pub const LE_FLOW_CONTROL_CREDIT: u8 = 0x16;
    /// Credit based connection request.
    pub const CREDIT_BASED_CONNECTION_REQUEST: u8 = 0x17;
    /// Credit based connection response.
    pub const CREDIT_BASED_CONNECTION_RESPONSE: u8 = 0x18;
    /// Credit based reconfigure request.
    pub const CREDIT_BASED_RECONFIGURE_REQUEST: u8 = 0x19;
    /// Credit based reconfigure response.
    pub const CREDIT_BASED_RECONFIGURE_RESPONSE: u8 = 0x1A;
}

/// L2CAP Signaling Packet Header.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct L2capSignalingHeader {
    /// Code.
    pub code: u8,
    /// Identifier.
    pub identifier: u8,
    /// Length.
    pub length: U16<LittleEndian>,
}

/// L2CAP Connection Request Result codes (Basic Mode, OpCode 0x02/0x03).
pub mod connection_result {
    /// Successful.
    pub const SUCCESSFUL: u16 = 0x0000;
    /// Pending.
    pub const PENDING: u16 = 0x0001;
    /// Refused psm not supported.
    pub const REFUSED_PSM_NOT_SUPPORTED: u16 = 0x0002;
    /// Refused security block.
    pub const REFUSED_SECURITY_BLOCK: u16 = 0x0003;
    /// Refused no resources available.
    pub const REFUSED_NO_RESOURCES_AVAILABLE: u16 = 0x0004;
    /// Refused invalid source cid.
    pub const REFUSED_INVALID_SOURCE_CID: u16 = 0x0006;
    /// Refused source cid already allocated.
    pub const REFUSED_SOURCE_CID_ALREADY_ALLOCATED: u16 = 0x0007;
}

/// L2CAP Configuration Response Result codes (OpCode 0x05).
pub mod configuration_result {
    /// Success.
    pub const SUCCESS: u16 = 0x0000;
    /// Unacceptable parameters.
    pub const UNACCEPTABLE_PARAMETERS: u16 = 0x0001;
    /// Rejected.
    pub const REJECTED: u16 = 0x0002;
    /// Unknown options.
    pub const UNKNOWN_OPTIONS: u16 = 0x0003;
}

/// L2CAP Configuration Option types (Bluetooth Core Vol 3, Part A, 5).
pub(crate) mod configuration_option {
    pub const MTU: u8 = 0x01;
}

/// Connection Request (OpCode 0x02) — Classic (BR/EDR) Basic Mode channel setup.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct ConnectionRequestHeader {
    /// Psm.
    pub psm: U16<LittleEndian>,
    /// Source cid.
    pub source_cid: U16<LittleEndian>,
}

/// Connection Response (OpCode 0x03) — Classic (BR/EDR) Basic Mode channel setup.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct ConnectionResponseHeader {
    /// Destination cid.
    pub destination_cid: U16<LittleEndian>,
    /// Source cid.
    pub source_cid: U16<LittleEndian>,
    /// Result.
    pub result: U16<LittleEndian>,
    /// Status.
    pub status: U16<LittleEndian>,
}

/// Configuration Request (OpCode 0x04) fixed header; variable-length TLV
/// options follow (see `configuration_option`, `encode_mtu_option`).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct ConfigurationRequestHeader {
    /// Destination cid.
    pub destination_cid: U16<LittleEndian>,
    /// Flags.
    pub flags: U16<LittleEndian>,
}

/// Configuration Response (OpCode 0x05) fixed header; variable-length TLV
/// options follow.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct ConfigurationResponseHeader {
    /// Source cid.
    pub source_cid: U16<LittleEndian>,
    /// Flags.
    pub flags: U16<LittleEndian>,
    /// Result.
    pub result: U16<LittleEndian>,
}

/// Encodes an MTU configuration option TLV (type 0x01, length 2).
pub(crate) fn encode_mtu_option(mtu: u16) -> [u8; 4] {
    let mut buf = [0u8; 4];
    buf[0] = configuration_option::MTU;
    buf[1] = 2;
    buf[2..4].copy_from_slice(&mtu.to_le_bytes());
    buf
}

/// Scans a configuration options TLV list for an MTU option.
pub(crate) fn parse_mtu_option(options: &[u8]) -> Option<u16> {
    let mut i = 0;
    while i + 2 <= options.len() {
        let option_type = options[i];
        let len = options[i + 1] as usize;
        let value_start = i + 2;
        let value_end = value_start.checked_add(len)?;
        if value_end > options.len() {
            return None;
        }
        if option_type == configuration_option::MTU && len == 2 {
            return Some(u16::from_le_bytes([
                options[value_start],
                options[value_start + 1],
            ]));
        }
        i = value_end;
    }
    None
}

/// LE Credit Based Connection Request Header (OpCode 0x14).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCreditBasedConnectionRequestHeader {
    /// Spsm.
    pub spsm: U16<LittleEndian>,
    /// Mtu.
    pub mtu: U16<LittleEndian>,
    /// Mps.
    pub mps: U16<LittleEndian>,
    /// Initial credits.
    pub initial_credits: U16<LittleEndian>,
}

/// LE Credit Based Connection Response Header (OpCode 0x15).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCreditBasedConnectionResponseHeader {
    /// Mtu.
    pub mtu: U16<LittleEndian>,
    /// Mps.
    pub mps: U16<LittleEndian>,
    /// Initial credits.
    pub initial_credits: U16<LittleEndian>,
    /// Result.
    pub result: U16<LittleEndian>,
}

/// LE Flow Control Credit (OpCode 0x16).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeFlowControlCredit {
    /// Cid.
    pub cid: U16<LittleEndian>,
    /// Credits.
    pub credits: U16<LittleEndian>,
}

/// Disconnection Request (OpCode 0x06).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct DisconnectionRequest {
    /// Destination cid.
    pub destination_cid: U16<LittleEndian>,
    /// Source cid.
    pub source_cid: U16<LittleEndian>,
}

/// Disconnection Response (OpCode 0x07).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct DisconnectionResponse {
    /// Destination cid.
    pub destination_cid: U16<LittleEndian>,
    /// Source cid.
    pub source_cid: U16<LittleEndian>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signaling_packet_serialization() {
        let req = LeCreditBasedConnectionRequestHeader {
            spsm: U16::from(0x0025), // SPSM
            mtu: U16::from(512),
            mps: U16::from(251),
            initial_credits: U16::from(10),
        };
        let bytes = req.as_bytes();
        assert_eq!(bytes.len(), 8);

        let parsed = LeCreditBasedConnectionRequestHeader::ref_from_bytes(bytes).unwrap();
        assert_eq!(parsed.spsm.get(), 0x0025);
        assert_eq!(parsed.mtu.get(), 512);
        assert_eq!(parsed.mps.get(), 251);
        assert_eq!(parsed.initial_credits.get(), 10);
    }
}
