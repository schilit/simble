// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! L2CAP Signaling and Credit-Based Flow Control packets (Bluetooth Core Vol 3, Part A, Section 4).

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, U16, Unaligned, byteorder::LittleEndian,
};

/// Standard L2CAP Signaling Command Codes.
pub mod signaling_code {
    pub const COMMAND_REJECT: u8 = 0x01;
    pub const CONNECTION_REQUEST: u8 = 0x02;
    pub const CONNECTION_RESPONSE: u8 = 0x03;
    pub const CONFIGURATION_REQUEST: u8 = 0x04;
    pub const CONFIGURATION_RESPONSE: u8 = 0x05;
    pub const DISCONNECTION_REQUEST: u8 = 0x06;
    pub const DISCONNECTION_RESPONSE: u8 = 0x07;
    pub const LE_CREDIT_BASED_CONNECTION_REQUEST: u8 = 0x14;
    pub const LE_CREDIT_BASED_CONNECTION_RESPONSE: u8 = 0x15;
    pub const LE_FLOW_CONTROL_CREDIT: u8 = 0x16;
    pub const CREDIT_BASED_CONNECTION_REQUEST: u8 = 0x17;
    pub const CREDIT_BASED_CONNECTION_RESPONSE: u8 = 0x18;
    pub const CREDIT_BASED_RECONFIGURE_REQUEST: u8 = 0x19;
    pub const CREDIT_BASED_RECONFIGURE_RESPONSE: u8 = 0x1A;
}

/// L2CAP Signaling Packet Header.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct L2capSignalingHeader {
    pub code: u8,
    pub identifier: u8,
    pub length: U16<LittleEndian>,
}

/// LE Credit Based Connection Request Header (OpCode 0x14).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCreditBasedConnectionRequestHeader {
    pub spsm: U16<LittleEndian>,
    pub mtu: U16<LittleEndian>,
    pub mps: U16<LittleEndian>,
    pub initial_credits: U16<LittleEndian>,
}

/// LE Credit Based Connection Response Header (OpCode 0x15).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCreditBasedConnectionResponseHeader {
    pub mtu: U16<LittleEndian>,
    pub mps: U16<LittleEndian>,
    pub initial_credits: U16<LittleEndian>,
    pub result: U16<LittleEndian>,
}

/// LE Flow Control Credit (OpCode 0x16).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeFlowControlCredit {
    pub cid: U16<LittleEndian>,
    pub credits: U16<LittleEndian>,
}

/// Disconnection Request (OpCode 0x06).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct DisconnectionRequest {
    pub destination_cid: U16<LittleEndian>,
    pub source_cid: U16<LittleEndian>,
}

/// Disconnection Response (OpCode 0x07).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct DisconnectionResponse {
    pub destination_cid: U16<LittleEndian>,
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
