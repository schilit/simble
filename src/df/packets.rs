// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Direction Finding HCI packets (Bluetooth Core Spec Vol 4, Part E, Section
//! 7.8 command group / Section 7.7.65 LE Meta Event subevents): Constant Tone
//! Extension (CTE) parameter configuration commands and IQ sample report
//! events. Mirrors the Channel Sounding packet layer (`src/packets/hci.rs`)
//! in style: zero-copy `#[repr(C)]` structs, `Option`-returning `parse`.

use crate::packets::hci::{HciCommand, OpCode};
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned,
    byteorder::{I16, LittleEndian, U16},
};

/// Direction Finding OpCodes (OGF 0x08, LE Controller Commands).
///
/// OCFs per Bluetooth Core Spec Vol 4, Part E, Section 7.8.80-7.8.87
/// (Direction Finding feature, introduced in Core Spec 5.1). Values taken
/// from the spec's HCI command assigned-number table; cross-checked against
/// the OCF ordering used by other open HCI stacks (e.g. Zephyr's `hci.h`).
pub mod df_opcode {
    use super::OpCode;

    /// 7.8.80 LE Set Connectionless CTE Transmit Parameters.
    pub const LE_SET_CONNECTIONLESS_CTE_TRANSMIT_PARAMETERS: OpCode =
        OpCode::from_bytes([0x51, 0x20]);
    /// 7.8.81 LE Set Connectionless CTE Transmit Enable.
    pub const LE_SET_CONNECTIONLESS_CTE_TRANSMIT_ENABLE: OpCode = OpCode::from_bytes([0x52, 0x20]);
    /// 7.8.82 LE Set Connectionless IQ Sampling Enable.
    pub const LE_SET_CONNECTIONLESS_IQ_SAMPLING_ENABLE: OpCode = OpCode::from_bytes([0x53, 0x20]);
    /// 7.8.83 LE Set Connection CTE Receive Parameters.
    pub const LE_SET_CONNECTION_CTE_RECEIVE_PARAMETERS: OpCode = OpCode::from_bytes([0x54, 0x20]);
    /// 7.8.84 LE Set Connection CTE Transmit Parameters.
    pub const LE_SET_CONNECTION_CTE_TRANSMIT_PARAMETERS: OpCode = OpCode::from_bytes([0x55, 0x20]);
    /// 7.8.85 LE Connection CTE Request Enable.
    pub const LE_CONNECTION_CTE_REQUEST_ENABLE: OpCode = OpCode::from_bytes([0x56, 0x20]);
    /// 7.8.86 LE Connection CTE Response Enable.
    pub const LE_CONNECTION_CTE_RESPONSE_ENABLE: OpCode = OpCode::from_bytes([0x57, 0x20]);
    /// 7.8.87 LE Read Antenna Information.
    pub const LE_READ_ANTENNA_INFORMATION: OpCode = OpCode::from_bytes([0x58, 0x20]);
}

/// Direction Finding LE Meta Subevent Codes (Event Code 0x3E).
///
/// Per Bluetooth Core Spec Vol 4, Part E, Section 7.7.65.20-22, numbered
/// immediately after LE Channel Selection Algorithm (0x14).
pub mod df_subevent_code {
    /// 7.7.65.20 LE Connectionless IQ Report.
    pub const LE_CONNECTIONLESS_IQ_REPORT: u8 = 0x15;
    /// 7.7.65.21 LE Connection IQ Report.
    pub const LE_CONNECTION_IQ_REPORT: u8 = 0x16;
    /// 7.7.65.22 LE CTE Request Failed.
    pub const LE_CTE_REQUEST_FAILED: u8 = 0x17;
}

/// LE Set Connectionless CTE Transmit Parameters Command Header (7.8.80).
///
/// Followed by `switching_pattern_length` Antenna_ID octets.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetConnectionlessCteTransmitParametersHeader {
    /// Advertising set the CTE parameters apply to.
    pub advertising_handle: u8,
    /// CTE length in units of 8 microseconds.
    pub cte_length: u8, // Units of 8us.
    /// CTE type (AoA, AoD 1us, or AoD 2us).
    pub cte_type: u8,
    /// Number of CTEs to transmit per periodic advertising interval.
    pub cte_count: u8,
    /// Number of Antenna_ID octets that follow this header.
    pub switching_pattern_length: u8,
}

impl HciCommand for LeSetConnectionlessCteTransmitParametersHeader {
    const OP_CODE: OpCode = df_opcode::LE_SET_CONNECTIONLESS_CTE_TRANSMIT_PARAMETERS;
}

impl LeSetConnectionlessCteTransmitParametersHeader {
    /// Parses the fixed header and returns the trailing antenna-ID octets.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.switching_pattern_length as usize;
        if rest.len() < expected {
            return None;
        }
        Some((header, &rest[..expected]))
    }
}

/// LE Set Connectionless CTE Transmit Enable Command (7.8.81).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetConnectionlessCteTransmitEnable {
    /// Advertising set to enable or disable CTE transmission for.
    pub advertising_handle: u8,
    /// 0 to disable, 1 to enable CTE transmission.
    pub cte_enable: u8,
}

impl HciCommand for LeSetConnectionlessCteTransmitEnable {
    const OP_CODE: OpCode = df_opcode::LE_SET_CONNECTIONLESS_CTE_TRANSMIT_ENABLE;
}

/// LE Set Connectionless IQ Sampling Enable Command (7.8.82).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetConnectionlessIqSamplingEnable {
    /// Periodic advertising sync handle to sample.
    pub sync_handle: U16<LittleEndian>,
    /// 0 to disable, 1 to enable IQ sampling.
    pub iq_sampling_enable: u8,
    /// Switching/sampling slot duration (1us or 2us).
    pub slot_durations: u8,
    /// Maximum number of CTEs to sample per periodic event (0 = all).
    pub max_sampled_ctes: u8,
}

impl HciCommand for LeSetConnectionlessIqSamplingEnable {
    const OP_CODE: OpCode = df_opcode::LE_SET_CONNECTIONLESS_IQ_SAMPLING_ENABLE;
}

/// LE Set Connection CTE Receive Parameters Command Header (7.8.83).
///
/// Followed by `switching_pattern_length` Antenna_ID octets.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetConnectionCteReceiveParametersHeader {
    /// Connection to configure CTE reception for.
    pub connection_handle: U16<LittleEndian>,
    /// 0 to disable, 1 to enable IQ sampling on received CTEs.
    pub sampling_enable: u8,
    /// Switching/sampling slot duration (1us or 2us).
    pub slot_durations: u8,
    /// Number of Antenna_ID octets that follow this header.
    pub switching_pattern_length: u8,
}

impl HciCommand for LeSetConnectionCteReceiveParametersHeader {
    const OP_CODE: OpCode = df_opcode::LE_SET_CONNECTION_CTE_RECEIVE_PARAMETERS;
}

impl LeSetConnectionCteReceiveParametersHeader {
    /// Parses the fixed header and returns the trailing antenna-ID octets.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.switching_pattern_length as usize;
        if rest.len() < expected {
            return None;
        }
        Some((header, &rest[..expected]))
    }

    /// Serializes the command: fixed header followed by the antenna switching pattern.
    pub fn serialize(
        connection_handle: u16,
        sampling_enable: u8,
        slot_durations: u8,
        antenna_ids: &[u8],
    ) -> Vec<u8> {
        let header = Self {
            connection_handle: U16::from_bytes(connection_handle.to_le_bytes()),
            sampling_enable,
            slot_durations,
            switching_pattern_length: antenna_ids.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + antenna_ids.len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(antenna_ids);
        buf
    }
}

/// LE Set Connection CTE Transmit Parameters Command Header (7.8.84).
///
/// Followed by `switching_pattern_length` Antenna_ID octets.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetConnectionCteTransmitParametersHeader {
    /// Connection to configure CTE transmission for.
    pub connection_handle: U16<LittleEndian>,
    /// Allowed CTE types bitmask: bit0 AoA, bit1 AoD-1us, bit2 AoD-2us.
    pub cte_types: u8, // Bitmask: bit0 AoA, bit1 AoD-1us, bit2 AoD-2us.
    /// Number of Antenna_ID octets that follow this header.
    pub switching_pattern_length: u8,
}

impl HciCommand for LeSetConnectionCteTransmitParametersHeader {
    const OP_CODE: OpCode = df_opcode::LE_SET_CONNECTION_CTE_TRANSMIT_PARAMETERS;
}

impl LeSetConnectionCteTransmitParametersHeader {
    /// Parses the fixed header and returns the trailing antenna-ID octets.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.switching_pattern_length as usize;
        if rest.len() < expected {
            return None;
        }
        Some((header, &rest[..expected]))
    }
}

/// LE Connection CTE Request Enable Command (7.8.85).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeConnectionCteRequestEnable {
    /// Connection on which to request CTEs from the peer.
    pub connection_handle: U16<LittleEndian>,
    /// 0 to disable, 1 to enable periodic CTE requests.
    pub enable: u8,
    /// Interval (in connection events) between CTE requests.
    pub cte_request_interval: U16<LittleEndian>,
    /// Requested CTE length in units of 8 microseconds.
    pub requested_cte_length: u8,
    /// Requested CTE type (AoA, AoD 1us, or AoD 2us).
    pub requested_cte_type: u8,
}

impl HciCommand for LeConnectionCteRequestEnable {
    const OP_CODE: OpCode = df_opcode::LE_CONNECTION_CTE_REQUEST_ENABLE;
}

/// LE Connection CTE Response Enable Command (7.8.86).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeConnectionCteResponseEnable {
    /// Connection on which to respond to CTE requests.
    pub connection_handle: U16<LittleEndian>,
    /// 0 to disable, 1 to enable CTE responses.
    pub enable: u8,
}

impl HciCommand for LeConnectionCteResponseEnable {
    const OP_CODE: OpCode = df_opcode::LE_CONNECTION_CTE_RESPONSE_ENABLE;
}

/// LE Read Antenna Information Command (7.8.87). No parameters.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Default, Clone, Copy)]
pub struct LeReadAntennaInformation {}

impl HciCommand for LeReadAntennaInformation {
    const OP_CODE: OpCode = df_opcode::LE_READ_ANTENNA_INFORMATION;
}

/// LE Read Antenna Information Command Complete return parameters (7.8.87).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeReadAntennaInformationResponse {
    /// HCI status code.
    pub status: u8,
    /// Bitmask of supported switching and sampling rates.
    pub supported_switching_sampling_rates: u8,
    /// Number of antennae available to the controller.
    pub num_antennae: u8,
    /// Maximum supported antenna switching-pattern length.
    pub max_switching_pattern_length: u8,
    /// Maximum supported CTE length in units of 8 microseconds.
    pub max_cte_length: u8,
}

/// LE Connectionless IQ Report Event Header (Subevent 0x15).
///
/// Followed by `sample_count` (I, Q) sample pairs, each a signed octet.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeConnectionlessIqReportEventHeader {
    /// Periodic advertising sync handle the report belongs to.
    pub sync_handle: U16<LittleEndian>,
    /// RF channel index the CTE was received on.
    pub channel_index: u8,
    /// Received signal strength in units of 0.1 dBm.
    pub rssi: I16<LittleEndian>, // Units: 0.1 dBm.
    /// Antenna ID the RSSI was measured on.
    pub rssi_antenna_id: u8,
    /// CTE type (AoA, AoD 1us, or AoD 2us).
    pub cte_type: u8,
    /// Switching/sampling slot duration (1us or 2us).
    pub slot_durations: u8,
    /// Packet status (CRC/insufficient-resources indication).
    pub packet_status: u8,
    /// Periodic advertising event counter.
    pub periodic_event_counter: U16<LittleEndian>,
    /// Number of (I, Q) sample pairs that follow this header.
    pub sample_count: u8,
}

impl LeConnectionlessIqReportEventHeader {
    /// Parses the fixed header and returns the trailing (I, Q) sample octets.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[i8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.sample_count as usize * 2;
        if rest.len() < expected {
            return None;
        }
        let iq = <[i8]>::ref_from_bytes(&rest[..expected]).ok()?;
        Some((header, iq))
    }
}

/// LE Connection IQ Report Event Header (Subevent 0x16).
///
/// Followed by `sample_count` (I, Q) sample pairs, each a signed octet.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeConnectionIqReportEventHeader {
    /// Connection the report belongs to.
    pub connection_handle: U16<LittleEndian>,
    /// Receiver PHY the CTE was received on.
    pub rx_phy: u8,
    /// Data channel index the CTE was received on.
    pub data_channel_index: u8,
    /// Received signal strength in units of 0.1 dBm.
    pub rssi: I16<LittleEndian>, // Units: 0.1 dBm.
    /// Antenna ID the RSSI was measured on.
    pub rssi_antenna_id: u8,
    /// CTE type (AoA, AoD 1us, or AoD 2us).
    pub cte_type: u8,
    /// Switching/sampling slot duration (1us or 2us).
    pub slot_durations: u8,
    /// Packet status (CRC/insufficient-resources indication).
    pub packet_status: u8,
    /// Connection event counter.
    pub connection_event_counter: U16<LittleEndian>,
    /// Number of (I, Q) sample pairs that follow this header.
    pub sample_count: u8,
}

impl LeConnectionIqReportEventHeader {
    /// Parses the fixed header and returns the trailing (I, Q) sample octets.
    ///
    /// `rest.len() == 2 * sample_count`, ordered I0, Q0, I1, Q1, ...
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[i8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.sample_count as usize * 2;
        if rest.len() < expected {
            return None;
        }
        let iq = <[i8]>::ref_from_bytes(&rest[..expected]).ok()?;
        Some((header, iq))
    }
}

/// LE CTE Request Failed Event (Subevent 0x17).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCteRequestFailedEvent {
    /// HCI status code explaining the failure.
    pub status: u8,
    /// Connection on which the CTE request failed.
    pub connection_handle: U16<LittleEndian>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_df_command_opcodes() {
        assert_eq!(
            LeSetConnectionCteReceiveParametersHeader::OP_CODE.get(),
            0x2054
        );
        assert_eq!(LeConnectionCteRequestEnable::OP_CODE.get(), 0x2056);
        assert_eq!(LeReadAntennaInformation::OP_CODE.get(), 0x2058);
    }

    #[test]
    fn test_connection_cte_receive_parameters_roundtrip() {
        let bytes =
            LeSetConnectionCteReceiveParametersHeader::serialize(0x0042, 1, 1, &[0, 1, 2, 3]);
        let (header, antenna_ids) =
            LeSetConnectionCteReceiveParametersHeader::parse(&bytes).expect("parse");
        assert_eq!(header.connection_handle.get(), 0x0042);
        assert_eq!(header.switching_pattern_length, 4);
        assert_eq!(antenna_ids, &[0, 1, 2, 3]);
    }
}
