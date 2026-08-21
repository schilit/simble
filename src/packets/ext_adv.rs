// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Extended and Periodic Advertising HCI packets (Core Spec 5.0, Vol 4,
//! Part E, Sections 7.8.52-7.8.69 command group / Section 7.7.65 LE Meta Event
//! subevents), plus host-side advertising-set state tracking.
//!
//! Mirrors the Direction Finding packet layer (`src/df/packets.rs`) in style:
//! zero-copy `#[repr(C)]` structs, `Option`-returning `parse`, `serialize`
//! helpers for variable-length trailers. Over-the-air behavior lives in the
//! netsim controller; [`AdvertisingSets`] tracks only what the host must
//! remember (parameters, fragment reassembly, enable state).

use crate::packets::hci::{HciCommand, OpCode};
use std::collections::BTreeMap;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned,
    byteorder::{LittleEndian, U16},
};

/// Extended/Periodic Advertising OpCodes (OGF 0x08, LE Controller Commands).
///
/// OCFs per Bluetooth Core Spec Vol 4, Part E, Sections 7.8.52-7.8.69.
pub mod ext_adv_opcode {
    use super::OpCode;

    /// 7.8.52 LE Set Advertising Set Random Address.
    pub const LE_SET_ADVERTISING_SET_RANDOM_ADDRESS: OpCode = OpCode::from_bytes([0x35, 0x20]);
    /// 7.8.53 LE Set Extended Advertising Parameters.
    pub const LE_SET_EXTENDED_ADVERTISING_PARAMETERS: OpCode = OpCode::from_bytes([0x36, 0x20]);
    /// 7.8.54 LE Set Extended Advertising Data.
    pub const LE_SET_EXTENDED_ADVERTISING_DATA: OpCode = OpCode::from_bytes([0x37, 0x20]);
    /// 7.8.55 LE Set Extended Scan Response Data.
    pub const LE_SET_EXTENDED_SCAN_RESPONSE_DATA: OpCode = OpCode::from_bytes([0x38, 0x20]);
    /// 7.8.56 LE Set Extended Advertising Enable.
    pub const LE_SET_EXTENDED_ADVERTISING_ENABLE: OpCode = OpCode::from_bytes([0x39, 0x20]);
    /// 7.8.57 LE Read Maximum Advertising Data Length.
    pub const LE_READ_MAXIMUM_ADVERTISING_DATA_LENGTH: OpCode = OpCode::from_bytes([0x3A, 0x20]);
    /// 7.8.59 LE Remove Advertising Set.
    pub const LE_REMOVE_ADVERTISING_SET: OpCode = OpCode::from_bytes([0x3C, 0x20]);
    /// 7.8.60 LE Clear Advertising Sets.
    pub const LE_CLEAR_ADVERTISING_SETS: OpCode = OpCode::from_bytes([0x3D, 0x20]);
    /// 7.8.61 LE Set Periodic Advertising Parameters.
    pub const LE_SET_PERIODIC_ADVERTISING_PARAMETERS: OpCode = OpCode::from_bytes([0x3E, 0x20]);
    /// 7.8.62 LE Set Periodic Advertising Data.
    pub const LE_SET_PERIODIC_ADVERTISING_DATA: OpCode = OpCode::from_bytes([0x3F, 0x20]);
    /// 7.8.63 LE Set Periodic Advertising Enable.
    pub const LE_SET_PERIODIC_ADVERTISING_ENABLE: OpCode = OpCode::from_bytes([0x40, 0x20]);
    /// 7.8.64 LE Set Extended Scan Parameters.
    pub const LE_SET_EXTENDED_SCAN_PARAMETERS: OpCode = OpCode::from_bytes([0x41, 0x20]);
    /// 7.8.65 LE Set Extended Scan Enable.
    pub const LE_SET_EXTENDED_SCAN_ENABLE: OpCode = OpCode::from_bytes([0x42, 0x20]);
    /// 7.8.67 LE Periodic Advertising Create Sync.
    pub const LE_PERIODIC_ADVERTISING_CREATE_SYNC: OpCode = OpCode::from_bytes([0x44, 0x20]);
    /// 7.8.68 LE Periodic Advertising Create Sync Cancel.
    pub const LE_PERIODIC_ADVERTISING_CREATE_SYNC_CANCEL: OpCode = OpCode::from_bytes([0x45, 0x20]);
    /// 7.8.69 LE Periodic Advertising Terminate Sync.
    pub const LE_PERIODIC_ADVERTISING_TERMINATE_SYNC: OpCode = OpCode::from_bytes([0x46, 0x20]);
}

/// Extended/Periodic Advertising LE Meta Subevent Codes (Event Code 0x3E).
///
/// Per Bluetooth Core Spec Vol 4, Part E, Sections 7.7.65.13-7.7.65.19.
pub mod ext_adv_subevent_code {
    /// 7.7.65.13 LE Extended Advertising Report.
    pub const LE_EXTENDED_ADVERTISING_REPORT: u8 = 0x0D;
    /// 7.7.65.14 LE Periodic Advertising Sync Established.
    pub const LE_PERIODIC_ADVERTISING_SYNC_ESTABLISHED: u8 = 0x0E;
    /// 7.7.65.15 LE Periodic Advertising Report.
    pub const LE_PERIODIC_ADVERTISING_REPORT: u8 = 0x0F;
    /// 7.7.65.16 LE Periodic Advertising Sync Lost.
    pub const LE_PERIODIC_ADVERTISING_SYNC_LOST: u8 = 0x10;
    /// 7.7.65.18 LE Advertising Set Terminated.
    pub const LE_ADVERTISING_SET_TERMINATED: u8 = 0x12;
    /// 7.7.65.19 LE Scan Request Received.
    pub const LE_SCAN_REQUEST_RECEIVED: u8 = 0x13;
}

/// Advertising_Event_Properties bits (7.8.53).
pub mod adv_event_properties {
    /// Advertise as connectable.
    pub const CONNECTABLE: u16 = 1 << 0;
    /// Advertise as scannable.
    pub const SCANNABLE: u16 = 1 << 1;
    /// Use directed advertising.
    pub const DIRECTED: u16 = 1 << 2;
    /// High-duty-cycle directed connectable advertising.
    pub const HIGH_DUTY_CYCLE_DIRECTED_CONNECTABLE: u16 = 1 << 3;
    /// Use legacy advertising PDUs.
    pub const USE_LEGACY_PDUS: u16 = 1 << 4;
    /// Omit the advertiser's address (anonymous advertising).
    pub const ANONYMOUS: u16 = 1 << 5;
    /// Include the TX power level in the advertisement.
    pub const INCLUDE_TX_POWER: u16 = 1 << 6;
}

/// Operation field for the Set Extended Advertising / Scan Response /
/// Periodic Advertising Data commands (7.8.54).
pub mod data_operation {
    /// An intermediate fragment of fragmented data.
    pub const INTERMEDIATE_FRAGMENT: u8 = 0x00;
    /// The first fragment of fragmented data.
    pub const FIRST_FRAGMENT: u8 = 0x01;
    /// The last fragment of fragmented data.
    pub const LAST_FRAGMENT: u8 = 0x02;
    /// The complete data in a single operation.
    pub const COMPLETE: u8 = 0x03;
    /// Keep the current data, refreshing only the Advertising DID.
    pub const UNCHANGED: u8 = 0x04;
}

/// Advertising PHY values (7.8.53: Primary/Secondary_Advertising_PHY).
pub mod adv_phy {
    /// LE 1M PHY.
    pub const LE_1M: u8 = 0x01;
    /// LE 2M PHY.
    pub const LE_2M: u8 = 0x02;
    /// LE Coded PHY.
    pub const LE_CODED: u8 = 0x03;
}

/// Event_Type bits of the LE Extended Advertising Report (7.7.65.13).
pub mod ext_adv_report_event_type {
    /// Report is for a connectable advertisement.
    /// Advertise as connectable.
    pub const CONNECTABLE: u16 = 1 << 0;
    /// Report is for a scannable advertisement.
    /// Advertise as scannable.
    pub const SCANNABLE: u16 = 1 << 1;
    /// Report is for a directed advertisement.
    /// Use directed advertising.
    pub const DIRECTED: u16 = 1 << 2;
    /// Report is a scan response.
    pub const SCAN_RESPONSE: u16 = 1 << 3;
    /// Report is from a legacy advertising PDU.
    pub const LEGACY_PDU: u16 = 1 << 4;
    /// Bits 5-6: 0 = complete, 1 = incomplete (more to come), 2 = truncated.
    pub const DATA_STATUS_MASK: u16 = 0b11 << 5;
    /// Data status bits 5-6 = incomplete (more to come).
    pub const DATA_STATUS_INCOMPLETE: u16 = 0b01 << 5;
    /// Data status bits 5-6 = truncated (no more data will be sent).
    pub const DATA_STATUS_TRUNCATED: u16 = 0b10 << 5;
}

/// 3-octet little-endian unsigned integer.
///
/// The extended advertising interval fields are 3 octets (7.8.53:
/// 0x000020-0xFFFFFF, units of 0.625 ms) — wider than any primitive zerocopy
/// byteorder type, hence this dedicated wrapper.
#[repr(transparent)]
#[derive(
    FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy, PartialEq, Eq,
)]
pub struct U24([u8; 3]);

impl U24 {
    /// Builds a 3-octet integer from the low 3 bytes of `value`.
    pub const fn new(value: u32) -> Self {
        let b = value.to_le_bytes();
        Self([b[0], b[1], b[2]])
    }

    /// Returns the value as a `u32`.
    pub fn get(&self) -> u32 {
        u32::from_le_bytes([self.0[0], self.0[1], self.0[2], 0])
    }
}

/// LE Set Advertising Set Random Address Command (7.8.52).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetAdvertisingSetRandomAddress {
    pub(crate) advertising_handle: u8,
    pub(crate) random_address: [u8; 6],
}

impl HciCommand for LeSetAdvertisingSetRandomAddress {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_ADVERTISING_SET_RANDOM_ADDRESS;
}

/// LE Set Extended Advertising Parameters Command (7.8.53).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetExtendedAdvertisingParameters {
    /// Identifier of the advertising set.
    pub advertising_handle: u8,
    /// Advertising event properties bitfield (see `adv_event_properties`).
    pub advertising_event_properties: U16<LittleEndian>,
    /// Minimum primary advertising interval (units of 0.625 ms).
    pub primary_advertising_interval_min: U24, // Units of 0.625 ms.
    /// Maximum primary advertising interval (units of 0.625 ms).
    pub primary_advertising_interval_max: U24, // Units of 0.625 ms.
    /// Primary advertising channel map bitmask (bitmask: bit0 ch37, bit1 ch38, bit2 ch39).
    pub primary_advertising_channel_map: u8, // Bitmask: bit0 ch37, bit1 ch38, bit2 ch39.
    /// Own device address type.
    pub own_address_type: u8,
    /// Peer device address type.
    pub peer_address_type: u8,
    /// Peer device address.
    pub peer_address: [u8; 6],
    /// Advertising filter policy.
    pub advertising_filter_policy: u8,
    /// Requested advertising TX power in dBm (0x7F = no preference).
    pub advertising_tx_power: i8, // dBm; 0x7F = no preference.
    /// Primary advertising PHY.
    pub primary_advertising_phy: u8,
    /// Maximum advertising events skipped on the secondary channel.
    pub secondary_advertising_max_skip: u8,
    /// Secondary advertising PHY.
    pub secondary_advertising_phy: u8,
    /// Advertising Set ID (SID).
    pub advertising_sid: u8,
    /// Enable scan-request notifications.
    pub scan_request_notification_enable: u8,
}

impl HciCommand for LeSetExtendedAdvertisingParameters {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_PARAMETERS;
}

/// LE Set Extended Advertising Parameters Command Complete return parameters
/// (7.8.53).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetExtendedAdvertisingParametersResponse {
    pub(crate) status: u8,
    pub(crate) selected_tx_power: i8, // dBm.
}

/// LE Set Extended Advertising Data Command Header (7.8.54).
///
/// Followed by `advertising_data_length` octets of advertising data.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetExtendedAdvertisingDataHeader {
    /// Identifier of the advertising set.
    pub advertising_handle: u8,
    /// Data operation (see `data_operation`).
    pub operation: u8, // See `data_operation`.
    /// Controller fragmentation preference.
    pub fragment_preference: u8,
    /// Length in octets of the advertising data that follows.
    pub advertising_data_length: u8,
}

impl HciCommand for LeSetExtendedAdvertisingDataHeader {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_DATA;
}

impl LeSetExtendedAdvertisingDataHeader {
    /// Parses the fixed header and returns it with the trailing data bytes.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.advertising_data_length as usize;
        if rest.len() < expected {
            return None;
        }
        Some((header, &rest[..expected]))
    }

    /// Serializes the command: fixed header followed by the data fragment.
    pub fn serialize(
        advertising_handle: u8,
        operation: u8,
        fragment_preference: u8,
        data: &[u8],
    ) -> Vec<u8> {
        let header = Self {
            advertising_handle,
            operation,
            fragment_preference,
            advertising_data_length: data.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + data.len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(data);
        buf
    }
}

/// LE Set Extended Scan Response Data Command Header (7.8.55).
///
/// Followed by `scan_response_data_length` octets of scan response data.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetExtendedScanResponseDataHeader {
    pub(crate) advertising_handle: u8,
    pub(crate) operation: u8, // See `data_operation`.
    pub(crate) fragment_preference: u8,
    pub(crate) scan_response_data_length: u8,
}

impl HciCommand for LeSetExtendedScanResponseDataHeader {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_EXTENDED_SCAN_RESPONSE_DATA;
}

impl LeSetExtendedScanResponseDataHeader {
    /// Parses the fixed header and returns it with the trailing data bytes.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.scan_response_data_length as usize;
        if rest.len() < expected {
            return None;
        }
        Some((header, &rest[..expected]))
    }

    /// Serializes the command: fixed header followed by the data fragment.
    pub fn serialize(
        advertising_handle: u8,
        operation: u8,
        fragment_preference: u8,
        data: &[u8],
    ) -> Vec<u8> {
        let header = Self {
            advertising_handle,
            operation,
            fragment_preference,
            scan_response_data_length: data.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + data.len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(data);
        buf
    }
}

/// Per-set entry of the LE Set Extended Advertising Enable command (7.8.56).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct AdvertisingEnableEntry {
    /// Identifier of the advertising set.
    pub advertising_handle: u8,
    /// Advertising duration (units of 10 ms; 0 = no limit).
    pub duration: U16<LittleEndian>, // Units of 10 ms; 0 = no limit.
    /// Maximum number of extended advertising events (0 = no limit).
    pub max_extended_advertising_events: u8, // 0 = no limit.
}

/// LE Set Extended Advertising Enable Command Header (7.8.56).
///
/// Followed by `num_sets` [`AdvertisingEnableEntry`] records.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetExtendedAdvertisingEnableHeader {
    /// Enable (nonzero) or disable (zero).
    pub enable: u8,
    /// Number of advertising-set entries that follow.
    pub num_sets: u8,
}

impl HciCommand for LeSetExtendedAdvertisingEnableHeader {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_ENABLE;
}

impl LeSetExtendedAdvertisingEnableHeader {
    /// Parses the fixed header and the trailing per-set enable entries.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[AdvertisingEnableEntry])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.num_sets as usize * size_of::<AdvertisingEnableEntry>();
        if rest.len() < expected {
            return None;
        }
        let entries = <[AdvertisingEnableEntry]>::ref_from_bytes(&rest[..expected]).ok()?;
        Some((header, entries))
    }

    /// Serializes the command: fixed header followed by the per-set entries.
    pub fn serialize(enable: bool, entries: &[AdvertisingEnableEntry]) -> Vec<u8> {
        let header = Self {
            enable: enable.into(),
            num_sets: entries.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + entries.as_bytes().len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(entries.as_bytes());
        buf
    }
}

/// LE Read Maximum Advertising Data Length Command (7.8.57). No parameters.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Default, Clone, Copy)]
pub struct LeReadMaximumAdvertisingDataLength {}

impl HciCommand for LeReadMaximumAdvertisingDataLength {
    const OP_CODE: OpCode = ext_adv_opcode::LE_READ_MAXIMUM_ADVERTISING_DATA_LENGTH;
}

/// LE Read Maximum Advertising Data Length Command Complete return parameters
/// (7.8.57).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeReadMaximumAdvertisingDataLengthResponse {
    pub(crate) status: u8,
    pub(crate) max_advertising_data_length: U16<LittleEndian>, // 0x001F-0x0672.
}

/// LE Remove Advertising Set Command (7.8.59).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeRemoveAdvertisingSet {
    pub(crate) advertising_handle: u8,
}

impl HciCommand for LeRemoveAdvertisingSet {
    const OP_CODE: OpCode = ext_adv_opcode::LE_REMOVE_ADVERTISING_SET;
}

/// LE Clear Advertising Sets Command (7.8.60). No parameters.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Default, Clone, Copy)]
pub struct LeClearAdvertisingSets {}

impl HciCommand for LeClearAdvertisingSets {
    const OP_CODE: OpCode = ext_adv_opcode::LE_CLEAR_ADVERTISING_SETS;
}

/// LE Set Periodic Advertising Parameters Command (7.8.61).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetPeriodicAdvertisingParameters {
    /// Identifier of the advertising set.
    pub advertising_handle: u8,
    /// Minimum periodic advertising interval (units of 1.25 ms).
    pub periodic_advertising_interval_min: U16<LittleEndian>, // Units of 1.25 ms.
    /// Maximum periodic advertising interval (units of 1.25 ms).
    pub periodic_advertising_interval_max: U16<LittleEndian>, // Units of 1.25 ms.
    /// Periodic advertising properties bitfield (bit 6: include TX power).
    pub periodic_advertising_properties: U16<LittleEndian>, // Bit 6: include TX power.
}

impl HciCommand for LeSetPeriodicAdvertisingParameters {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_PARAMETERS;
}

/// LE Set Periodic Advertising Data Command Header (7.8.62).
///
/// Followed by `advertising_data_length` octets of periodic advertising data.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetPeriodicAdvertisingDataHeader {
    /// Identifier of the advertising set.
    pub advertising_handle: u8,
    /// Data operation (see `data_operation`).
    pub operation: u8, // See `data_operation` (0x04 Unchanged added in 5.2).
    /// Length in octets of the advertising data that follows.
    pub advertising_data_length: u8,
}

impl HciCommand for LeSetPeriodicAdvertisingDataHeader {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_DATA;
}

impl LeSetPeriodicAdvertisingDataHeader {
    /// Parses the fixed header and the trailing per-report data.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.advertising_data_length as usize;
        if rest.len() < expected {
            return None;
        }
        Some((header, &rest[..expected]))
    }

    /// Serializes the command: fixed header followed by the data fragment.
    pub fn serialize(advertising_handle: u8, operation: u8, data: &[u8]) -> Vec<u8> {
        let header = Self {
            advertising_handle,
            operation,
            advertising_data_length: data.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + data.len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(data);
        buf
    }
}

/// LE Set Periodic Advertising Enable Command (7.8.63).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetPeriodicAdvertisingEnable {
    pub(crate) enable: u8, // Bit 0: enable; bit 1: include ADI (5.4).
    pub(crate) advertising_handle: u8,
}

impl HciCommand for LeSetPeriodicAdvertisingEnable {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_PERIODIC_ADVERTISING_ENABLE;
}

/// Per-PHY entry of the LE Set Extended Scan Parameters command (7.8.64).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct ScanPhyParameters {
    /// Scan type (0 = passive, 1 = active).
    pub scan_type: u8, // 0 = passive, 1 = active.
    /// Scan interval (units of 0.625 ms).
    pub scan_interval: U16<LittleEndian>, // Units of 0.625 ms.
    /// Scan window (units of 0.625 ms).
    pub scan_window: U16<LittleEndian>, // Units of 0.625 ms.
}

/// LE Set Extended Scan Parameters Command Header (7.8.64).
///
/// Followed by one [`ScanPhyParameters`] entry per bit set in
/// `scanning_phys` (bit 0: LE 1M, bit 2: LE Coded).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetExtendedScanParametersHeader {
    /// Own device address type.
    pub own_address_type: u8,
    /// Scanning filter policy.
    pub scanning_filter_policy: u8,
    /// Bitmask of PHYs to scan on.
    pub scanning_phys: u8,
}

impl HciCommand for LeSetExtendedScanParametersHeader {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_EXTENDED_SCAN_PARAMETERS;
}

impl LeSetExtendedScanParametersHeader {
    /// Parses the fixed header and the trailing per-PHY scan parameters.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[ScanPhyParameters])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.scanning_phys.count_ones() as usize * size_of::<ScanPhyParameters>();
        if rest.len() < expected {
            return None;
        }
        let entries = <[ScanPhyParameters]>::ref_from_bytes(&rest[..expected]).ok()?;
        Some((header, entries))
    }

    /// Serializes the command: fixed header followed by one entry per PHY bit.
    /// `entries.len()` must equal the number of bits set in `scanning_phys`.
    pub fn serialize(
        own_address_type: u8,
        scanning_filter_policy: u8,
        scanning_phys: u8,
        entries: &[ScanPhyParameters],
    ) -> Vec<u8> {
        debug_assert_eq!(scanning_phys.count_ones() as usize, entries.len());
        let header = Self {
            own_address_type,
            scanning_filter_policy,
            scanning_phys,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + entries.as_bytes().len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(entries.as_bytes());
        buf
    }
}

/// LE Set Extended Scan Enable Command (7.8.65).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeSetExtendedScanEnable {
    pub(crate) enable: u8,
    pub(crate) filter_duplicates: u8,
    pub(crate) duration: U16<LittleEndian>, // Units of 10 ms; 0 = continuous.
    pub(crate) period: U16<LittleEndian>,   // Units of 1.28 s; 0 = continuous.
}

impl HciCommand for LeSetExtendedScanEnable {
    const OP_CODE: OpCode = ext_adv_opcode::LE_SET_EXTENDED_SCAN_ENABLE;
}

/// LE Periodic Advertising Create Sync Command (7.8.67).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LePeriodicAdvertisingCreateSync {
    /// Create-sync options bitfield (bit 0: use Periodic Advertiser List).
    pub options: u8, // Bit 0: use Periodic Advertiser List.
    /// Advertising Set ID (SID).
    pub advertising_sid: u8,
    /// Advertiser address type.
    pub advertiser_address_type: u8,
    /// Advertiser device address.
    pub advertiser_address: [u8; 6],
    /// Number of periodic advertising events that may be skipped.
    pub skip: U16<LittleEndian>,
    /// Synchronization timeout (units of 10 ms).
    pub sync_timeout: U16<LittleEndian>, // Units of 10 ms.
    /// CTE type constraint for synchronization.
    pub sync_cte_type: u8,
}

impl HciCommand for LePeriodicAdvertisingCreateSync {
    const OP_CODE: OpCode = ext_adv_opcode::LE_PERIODIC_ADVERTISING_CREATE_SYNC;
}

/// LE Periodic Advertising Create Sync Cancel Command (7.8.68). No parameters.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Default, Clone, Copy)]
pub struct LePeriodicAdvertisingCreateSyncCancel {}

impl HciCommand for LePeriodicAdvertisingCreateSyncCancel {
    const OP_CODE: OpCode = ext_adv_opcode::LE_PERIODIC_ADVERTISING_CREATE_SYNC_CANCEL;
}

/// LE Periodic Advertising Terminate Sync Command (7.8.69).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LePeriodicAdvertisingTerminateSync {
    pub(crate) sync_handle: U16<LittleEndian>,
}

impl HciCommand for LePeriodicAdvertisingTerminateSync {
    const OP_CODE: OpCode = ext_adv_opcode::LE_PERIODIC_ADVERTISING_TERMINATE_SYNC;
}

/// Fixed portion of one report inside an LE Extended Advertising Report event
/// (7.7.65.13). Followed by `data_length` octets of advertising data.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct ExtendedAdvertisingReportHeader {
    /// Event type bitfield (see `ext_adv_report_event_type`).
    pub event_type: U16<LittleEndian>, // See `ext_adv_report_event_type`.
    /// Advertiser address type.
    pub address_type: u8,
    /// Advertiser device address.
    pub address: [u8; 6],
    /// Primary advertising PHY.
    pub primary_phy: u8,
    /// Secondary advertising PHY (0 = no packets on secondary channel).
    pub secondary_phy: u8, // 0 = no packets on secondary channel.
    /// Advertising Set ID (SID).
    pub advertising_sid: u8, // 0xFF = no ADI field provided.
    /// TX power in dBm (dBm; 0x7F = not available).
    pub tx_power: i8, // dBm; 0x7F = not available.
    /// Received signal strength in dBm (dBm; 0x7F = not available).
    pub rssi: i8, // dBm; 0x7F = not available.
    /// Periodic advertising interval (0 = none).
    pub periodic_advertising_interval: U16<LittleEndian>, // Units of 1.25 ms; 0 = none.
    /// Target (directed) address type.
    pub direct_address_type: u8,
    /// Target (directed) device address.
    pub direct_address: [u8; 6],
    /// Length in octets of the data that follows.
    pub data_length: u8,
}

/// LE Extended Advertising Report Event (Subevent 0x0D).
///
/// One event carries `num_reports` back-to-back variable-length reports, so
/// parsing yields a list rather than a single header + trailer pair.
pub struct LeExtendedAdvertisingReportEvent;

impl LeExtendedAdvertisingReportEvent {
    /// Parses the subevent parameters (after the subevent code octet) into
    /// per-report (header, data) pairs. `None` on truncation or a report
    /// count of zero (7.7.65.13 requires 1..=0x0A reports).
    #[allow(clippy::type_complexity)]
    pub fn parse(
        bytes: &[u8],
    ) -> Option<Vec<(Ref<&[u8], ExtendedAdvertisingReportHeader>, &[u8])>> {
        let (&num_reports, mut rest) = bytes.split_first()?;
        if num_reports == 0 {
            return None;
        }
        let mut reports = Vec::with_capacity(num_reports as usize);
        for _ in 0..num_reports {
            let (header, tail) =
                Ref::<&[u8], ExtendedAdvertisingReportHeader>::from_prefix(rest).ok()?;
            let data_len = header.data_length as usize;
            if tail.len() < data_len {
                return None;
            }
            reports.push((header, &tail[..data_len]));
            rest = &tail[data_len..];
        }
        Some(reports)
    }

    /// Serializes the subevent parameters: report count followed by each
    /// report's fixed header and advertising data.
    pub fn serialize(reports: &[(ExtendedAdvertisingReportHeader, &[u8])]) -> Vec<u8> {
        let mut buf = vec![reports.len() as u8];
        for (header, data) in reports {
            debug_assert_eq!(header.data_length as usize, data.len());
            buf.extend_from_slice(header.as_bytes());
            buf.extend_from_slice(data);
        }
        buf
    }
}

/// LE Periodic Advertising Sync Established Event (Subevent 0x0E, 7.7.65.14).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LePeriodicAdvertisingSyncEstablishedEvent {
    /// HCI status code.
    pub status: u8,
    /// Sync handle identifying the periodic advertising train.
    pub sync_handle: U16<LittleEndian>,
    /// Advertising Set ID (SID).
    pub advertising_sid: u8,
    /// Advertiser address type.
    pub advertiser_address_type: u8,
    /// Advertiser device address.
    pub advertiser_address: [u8; 6],
    /// Advertiser PHY.
    pub advertiser_phy: u8,
    /// Periodic advertising interval (0 = none).
    pub periodic_advertising_interval: U16<LittleEndian>, // Units of 1.25 ms.
    /// Advertiser clock accuracy.
    pub advertiser_clock_accuracy: u8,
}

/// LE Periodic Advertising Report Event Header (Subevent 0x0F, 7.7.65.15).
///
/// Followed by `data_length` octets of periodic advertising data.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LePeriodicAdvertisingReportEventHeader {
    /// Sync handle identifying the periodic advertising train.
    pub sync_handle: U16<LittleEndian>,
    /// TX power in dBm (dBm; 0x7F = not available).
    pub tx_power: i8, // dBm; 0x7F = not available.
    /// Received signal strength in dBm (dBm; 0x7F = not available).
    pub rssi: i8, // dBm; 0x7F = not available.
    /// Constant Tone Extension type (0xFF = no CTE).
    pub cte_type: u8, // 0xFF = no CTE.
    /// Data completeness status (0 = complete, 1 = more to come, 2 = truncated).
    pub data_status: u8, // 0 = complete, 1 = more to come, 2 = truncated.
    /// Length in octets of the data that follows.
    pub data_length: u8,
}

impl LePeriodicAdvertisingReportEventHeader {
    /// Parses the fixed header and returns it with the trailing data bytes.
    pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        let (header, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
        let expected = header.data_length as usize;
        if rest.len() < expected {
            return None;
        }
        Some((header, &rest[..expected]))
    }

    /// Serializes the subevent parameters: fixed header followed by the data.
    pub fn serialize(
        sync_handle: u16,
        tx_power: i8,
        rssi: i8,
        cte_type: u8,
        data_status: u8,
        data: &[u8],
    ) -> Vec<u8> {
        let header = Self {
            sync_handle: U16::from_bytes(sync_handle.to_le_bytes()),
            tx_power,
            rssi,
            cte_type,
            data_status,
            data_length: data.len() as u8,
        };
        let mut buf = Vec::with_capacity(size_of::<Self>() + data.len());
        buf.extend_from_slice(header.as_bytes());
        buf.extend_from_slice(data);
        buf
    }
}

/// LE Periodic Advertising Sync Lost Event (Subevent 0x10, 7.7.65.16).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LePeriodicAdvertisingSyncLostEvent {
    /// Sync handle identifying the periodic advertising train.
    pub sync_handle: U16<LittleEndian>,
}

/// LE Advertising Set Terminated Event (Subevent 0x12, 7.7.65.18).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeAdvertisingSetTerminatedEvent {
    /// HCI status code.
    pub status: u8,
    /// Identifier of the advertising set.
    pub advertising_handle: u8,
    /// Connection handle (valid only if a connection was created).
    pub connection_handle: U16<LittleEndian>, // Valid only if a connection was created.
    /// Number of completed extended advertising events.
    pub num_completed_extended_advertising_events: u8,
}

/// LE Scan Request Received Event (Subevent 0x13, 7.7.65.19).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeScanRequestReceivedEvent {
    /// Identifier of the advertising set.
    pub advertising_handle: u8,
    /// Scanner address type.
    pub scanner_address_type: u8,
    /// Scanner device address.
    pub scanner_address: [u8; 6],
}

/// Maximum total advertising data per set (7.8.57 upper bound, 0x0672).
pub(crate) const MAX_ADVERTISING_DATA_LENGTH: usize = 1650;

/// Errors from applying advertising-set commands, mirroring the HCI status
/// codes a controller would return (Vol 1, Part F).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdvSetError {
    /// 0x42 Unknown Advertising Identifier.
    UnknownAdvertisingIdentifier,
    /// 0x12 Invalid HCI Command Parameters.
    InvalidParameters,
    /// 0x0C Command Disallowed (e.g. reconfiguring an enabled set).
    CommandDisallowed,
    /// 0x07 Memory Capacity Exceeded (data beyond the maximum length).
    MemoryCapacityExceeded,
}

/// Host-side view of one advertising set: parameters plus data assembled
/// across fragmented Set-Data operations.
#[derive(Debug, Clone, Default)]
pub struct AdvertisingSet {
    /// Advertising set handle.
    pub handle: u8,
    /// Random device address assigned to the set, if any.
    pub random_address: Option<[u8; 6]>,
    /// Extended advertising parameters, once set.
    pub params: Option<LeSetExtendedAdvertisingParameters>,
    /// Assembled advertising data.
    pub advertising_data: Vec<u8>,
    /// Assembled scan response data.
    pub scan_response_data: Vec<u8>,
    /// Periodic advertising parameters, once set.
    pub periodic_params: Option<LeSetPeriodicAdvertisingParameters>,
    /// Assembled periodic advertising data.
    pub periodic_data: Vec<u8>,
    /// Whether extended advertising is currently enabled.
    pub enabled: bool,
    /// Whether periodic advertising is currently enabled.
    pub periodic_enabled: bool,
    /// Advertising duration.
    pub duration: u16,
    /// Maximum number of extended advertising events (0 = no limit).
    pub max_extended_advertising_events: u8,
    // In-progress fragment reassembly buffers (First..Last not yet committed).
    adv_fragments: Option<Vec<u8>>,
    scan_rsp_fragments: Option<Vec<u8>>,
    periodic_fragments: Option<Vec<u8>>,
}

/// Applies one Set-Data operation (7.8.54 Operation field) to a committed
/// buffer and its in-progress reassembly buffer. Data only becomes visible in
/// `committed` once a Complete or Last fragment arrives, matching when a
/// controller would start using it.
fn apply_data_operation(
    committed: &mut Vec<u8>,
    pending: &mut Option<Vec<u8>>,
    operation: u8,
    data: &[u8],
    max_len: usize,
) -> Result<(), AdvSetError> {
    match operation {
        data_operation::COMPLETE => {
            if data.len() > max_len {
                return Err(AdvSetError::MemoryCapacityExceeded);
            }
            *pending = None;
            *committed = data.to_vec();
            Ok(())
        }
        data_operation::UNCHANGED => {
            // Unchanged is a DID-refresh only: no data may accompany it and
            // there must be existing data to refresh (7.8.54).
            if !data.is_empty() || committed.is_empty() {
                return Err(AdvSetError::InvalidParameters);
            }
            Ok(())
        }
        data_operation::FIRST_FRAGMENT => {
            if data.len() > max_len {
                return Err(AdvSetError::MemoryCapacityExceeded);
            }
            *pending = Some(data.to_vec());
            Ok(())
        }
        data_operation::INTERMEDIATE_FRAGMENT | data_operation::LAST_FRAGMENT => {
            let Some(buffer) = pending.as_mut() else {
                // An intermediate/last fragment without a first has no
                // sequence to extend (7.8.54).
                return Err(AdvSetError::InvalidParameters);
            };
            if buffer.len() + data.len() > max_len {
                *pending = None;
                return Err(AdvSetError::MemoryCapacityExceeded);
            }
            buffer.extend_from_slice(data);
            if operation == data_operation::LAST_FRAGMENT {
                *committed = pending.take().unwrap();
            }
            Ok(())
        }
        _ => Err(AdvSetError::InvalidParameters),
    }
}

/// Host-side tracker applying the extended/periodic advertising command
/// surface to a table of [`AdvertisingSet`]s (the role `cs/procedures.rs`
/// plays for Channel Sounding).
#[derive(Debug, Clone)]
pub struct AdvertisingSets {
    sets: BTreeMap<u8, AdvertisingSet>,
    /// Maximum total advertising data length per set, in octets.
    pub max_data_length: usize,
}

impl Default for AdvertisingSets {
    fn default() -> Self {
        Self::new()
    }
}

impl AdvertisingSets {
    /// Creates an empty advertising-set table with the default max data length.
    pub fn new() -> Self {
        Self {
            sets: BTreeMap::new(),
            max_data_length: MAX_ADVERTISING_DATA_LENGTH,
        }
    }

    /// Returns the advertising set with the given handle, if present.
    pub fn get(&self, handle: u8) -> Option<&AdvertisingSet> {
        self.sets.get(&handle)
    }

    /// Number of advertising sets currently tracked.
    pub fn len(&self) -> usize {
        self.sets.len()
    }

    /// Whether no advertising sets are tracked.
    pub fn is_empty(&self) -> bool {
        self.sets.is_empty()
    }

    fn get_mut(&mut self, handle: u8) -> Result<&mut AdvertisingSet, AdvSetError> {
        self.sets
            .get_mut(&handle)
            .ok_or(AdvSetError::UnknownAdvertisingIdentifier)
    }

    /// Applies LE Set Extended Advertising Parameters (7.8.53), creating the
    /// set on first use of its handle.
    pub fn set_parameters(
        &mut self,
        params: &LeSetExtendedAdvertisingParameters,
    ) -> Result<(), AdvSetError> {
        if params.advertising_handle > 0xEF {
            return Err(AdvSetError::InvalidParameters);
        }
        let set = self
            .sets
            .entry(params.advertising_handle)
            .or_insert_with(|| AdvertisingSet {
                handle: params.advertising_handle,
                ..AdvertisingSet::default()
            });
        // 7.8.53: parameters may not change while the set is advertising.
        if set.enabled {
            return Err(AdvSetError::CommandDisallowed);
        }
        set.params = Some(*params);
        Ok(())
    }

    /// Applies LE Set Advertising Set Random Address (7.8.52). The set must
    /// already exist and, per the spec, must not be connectable-and-enabled.
    pub fn set_random_address(&mut self, handle: u8, address: [u8; 6]) -> Result<(), AdvSetError> {
        let set = self.get_mut(handle)?;
        let connectable = set.params.is_some_and(|p| {
            p.advertising_event_properties.get() & adv_event_properties::CONNECTABLE != 0
        });
        if set.enabled && connectable {
            return Err(AdvSetError::CommandDisallowed);
        }
        set.random_address = Some(address);
        Ok(())
    }

    /// Applies LE Set Extended Advertising Data (7.8.54) with fragment
    /// reassembly per the Operation field.
    pub fn set_advertising_data(
        &mut self,
        handle: u8,
        operation: u8,
        data: &[u8],
    ) -> Result<(), AdvSetError> {
        let max_len = self.max_data_length;
        let set = self.get_mut(handle)?;
        // 7.8.54: while advertising is enabled only a single Complete (or
        // Unchanged) operation may replace the data.
        if set.enabled
            && operation != data_operation::COMPLETE
            && operation != data_operation::UNCHANGED
        {
            return Err(AdvSetError::CommandDisallowed);
        }
        let AdvertisingSet {
            advertising_data,
            adv_fragments,
            ..
        } = set;
        apply_data_operation(advertising_data, adv_fragments, operation, data, max_len)
    }

    /// Applies LE Set Extended Scan Response Data (7.8.55) with fragment
    /// reassembly per the Operation field.
    pub fn set_scan_response_data(
        &mut self,
        handle: u8,
        operation: u8,
        data: &[u8],
    ) -> Result<(), AdvSetError> {
        let max_len = self.max_data_length;
        let set = self.get_mut(handle)?;
        if set.enabled && operation != data_operation::COMPLETE {
            return Err(AdvSetError::CommandDisallowed);
        }
        let AdvertisingSet {
            scan_response_data,
            scan_rsp_fragments,
            ..
        } = set;
        apply_data_operation(
            scan_response_data,
            scan_rsp_fragments,
            operation,
            data,
            max_len,
        )
    }

    /// Applies LE Set Extended Advertising Enable (7.8.56). Disabling with an
    /// empty entry list disables every set.
    pub fn set_enable(
        &mut self,
        enable: bool,
        entries: &[AdvertisingEnableEntry],
    ) -> Result<(), AdvSetError> {
        if !enable && entries.is_empty() {
            for set in self.sets.values_mut() {
                set.enabled = false;
            }
            return Ok(());
        }
        // 7.8.56: enabling requires at least one set and no duplicate handles.
        if entries.is_empty() {
            return Err(AdvSetError::InvalidParameters);
        }
        for (i, entry) in entries.iter().enumerate() {
            if entries[..i]
                .iter()
                .any(|e| e.advertising_handle == entry.advertising_handle)
            {
                return Err(AdvSetError::InvalidParameters);
            }
        }
        // Validate every handle before mutating so a bad entry leaves the
        // whole command without effect, as a controller status failure would.
        for entry in entries {
            let set = self
                .sets
                .get(&entry.advertising_handle)
                .ok_or(AdvSetError::UnknownAdvertisingIdentifier)?;
            if enable && (set.params.is_none() || set.adv_fragments.is_some()) {
                // Enabling mid-reassembly or before parameters exist has no
                // defined data to advertise (7.8.56).
                return Err(AdvSetError::CommandDisallowed);
            }
        }
        for entry in entries {
            let set = self.sets.get_mut(&entry.advertising_handle).unwrap();
            set.enabled = enable;
            set.duration = entry.duration.get();
            set.max_extended_advertising_events = entry.max_extended_advertising_events;
        }
        Ok(())
    }

    /// Applies LE Set Periodic Advertising Parameters (7.8.61).
    pub fn set_periodic_parameters(
        &mut self,
        params: &LeSetPeriodicAdvertisingParameters,
    ) -> Result<(), AdvSetError> {
        let set = self.get_mut(params.advertising_handle)?;
        // 7.8.61: parameters may not change while periodic advertising runs.
        if set.periodic_enabled {
            return Err(AdvSetError::CommandDisallowed);
        }
        set.periodic_params = Some(*params);
        Ok(())
    }

    /// Applies LE Set Periodic Advertising Data (7.8.62) with fragment
    /// reassembly per the Operation field.
    pub fn set_periodic_data(
        &mut self,
        handle: u8,
        operation: u8,
        data: &[u8],
    ) -> Result<(), AdvSetError> {
        let max_len = self.max_data_length;
        let set = self.get_mut(handle)?;
        // 7.8.62: periodic advertising must be configured before data.
        if set.periodic_params.is_none() {
            return Err(AdvSetError::CommandDisallowed);
        }
        if set.periodic_enabled
            && operation != data_operation::COMPLETE
            && operation != data_operation::UNCHANGED
        {
            return Err(AdvSetError::CommandDisallowed);
        }
        let AdvertisingSet {
            periodic_data,
            periodic_fragments,
            ..
        } = set;
        apply_data_operation(periodic_data, periodic_fragments, operation, data, max_len)
    }

    /// Applies LE Set Periodic Advertising Enable (7.8.63).
    pub fn set_periodic_enable(&mut self, enable: bool, handle: u8) -> Result<(), AdvSetError> {
        let set = self.get_mut(handle)?;
        if enable && (set.periodic_params.is_none() || set.periodic_fragments.is_some()) {
            return Err(AdvSetError::CommandDisallowed);
        }
        set.periodic_enabled = enable;
        Ok(())
    }

    /// Applies LE Remove Advertising Set (7.8.59).
    pub fn remove(&mut self, handle: u8) -> Result<(), AdvSetError> {
        let set = self.get_mut(handle)?;
        // 7.8.59: an advertising set may not be removed while in use.
        if set.enabled || set.periodic_enabled {
            return Err(AdvSetError::CommandDisallowed);
        }
        self.sets.remove(&handle);
        Ok(())
    }

    /// Applies LE Clear Advertising Sets (7.8.60).
    pub fn clear(&mut self) -> Result<(), AdvSetError> {
        // 7.8.60: disallowed while any set is in use.
        if self.sets.values().any(|s| s.enabled || s.periodic_enabled) {
            return Err(AdvSetError::CommandDisallowed);
        }
        self.sets.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ext_adv_command_opcodes() {
        assert_eq!(LeSetAdvertisingSetRandomAddress::OP_CODE.get(), 0x2035);
        assert_eq!(LeSetExtendedAdvertisingParameters::OP_CODE.get(), 0x2036);
        assert_eq!(LeSetExtendedAdvertisingDataHeader::OP_CODE.get(), 0x2037);
        assert_eq!(LeSetExtendedScanResponseDataHeader::OP_CODE.get(), 0x2038);
        assert_eq!(LeSetExtendedAdvertisingEnableHeader::OP_CODE.get(), 0x2039);
        assert_eq!(LeReadMaximumAdvertisingDataLength::OP_CODE.get(), 0x203A);
        assert_eq!(LeRemoveAdvertisingSet::OP_CODE.get(), 0x203C);
        assert_eq!(LeClearAdvertisingSets::OP_CODE.get(), 0x203D);
        assert_eq!(LeSetPeriodicAdvertisingParameters::OP_CODE.get(), 0x203E);
        assert_eq!(LeSetPeriodicAdvertisingDataHeader::OP_CODE.get(), 0x203F);
        assert_eq!(LeSetPeriodicAdvertisingEnable::OP_CODE.get(), 0x2040);
        assert_eq!(LeSetExtendedScanParametersHeader::OP_CODE.get(), 0x2041);
        assert_eq!(LeSetExtendedScanEnable::OP_CODE.get(), 0x2042);
        assert_eq!(LePeriodicAdvertisingCreateSync::OP_CODE.get(), 0x2044);
        assert_eq!(LePeriodicAdvertisingCreateSyncCancel::OP_CODE.get(), 0x2045);
        assert_eq!(LePeriodicAdvertisingTerminateSync::OP_CODE.get(), 0x2046);
    }

    #[test]
    fn test_extended_advertising_parameters_layout() {
        // 7.8.53 total parameter length is 25 octets.
        assert_eq!(size_of::<LeSetExtendedAdvertisingParameters>(), 25);
    }

    #[test]
    fn test_u24_roundtrip() {
        let interval = U24::new(0x0000_A020);
        assert_eq!(interval.get(), 0x0000_A020);
        assert_eq!(U24::new(0xFF_FF_FF).get(), 0xFF_FF_FF);
    }

    #[test]
    fn test_enable_command_roundtrip() {
        let entries = [AdvertisingEnableEntry {
            advertising_handle: 1,
            duration: U16::from_bytes(500u16.to_le_bytes()),
            max_extended_advertising_events: 10,
        }];
        let bytes = LeSetExtendedAdvertisingEnableHeader::serialize(true, &entries);
        let (header, parsed) = LeSetExtendedAdvertisingEnableHeader::parse(&bytes).expect("parse");
        assert_eq!(header.enable, 1);
        assert_eq!(header.num_sets, 1);
        assert_eq!(parsed[0].duration.get(), 500);
    }

    fn default_params(handle: u8) -> LeSetExtendedAdvertisingParameters {
        LeSetExtendedAdvertisingParameters {
            advertising_handle: handle,
            advertising_event_properties: U16::from_bytes(0u16.to_le_bytes()),
            primary_advertising_interval_min: U24::new(0x20),
            primary_advertising_interval_max: U24::new(0x40),
            primary_advertising_channel_map: 0x07,
            own_address_type: 0,
            peer_address_type: 0,
            peer_address: [0; 6],
            advertising_filter_policy: 0,
            advertising_tx_power: 0x7F,
            primary_advertising_phy: adv_phy::LE_1M,
            secondary_advertising_max_skip: 0,
            secondary_advertising_phy: adv_phy::LE_2M,
            advertising_sid: 0,
            scan_request_notification_enable: 0,
        }
    }

    #[test]
    fn test_fragmented_reassembly() {
        let mut sets = AdvertisingSets::new();
        sets.set_parameters(&default_params(0)).unwrap();

        sets.set_advertising_data(0, data_operation::FIRST_FRAGMENT, &[1, 2])
            .unwrap();
        // Data must stay invisible until the last fragment commits.
        assert!(sets.get(0).unwrap().advertising_data.is_empty());
        sets.set_advertising_data(0, data_operation::INTERMEDIATE_FRAGMENT, &[3, 4])
            .unwrap();
        sets.set_advertising_data(0, data_operation::LAST_FRAGMENT, &[5])
            .unwrap();
        assert_eq!(sets.get(0).unwrap().advertising_data, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn test_intermediate_without_first_rejected() {
        let mut sets = AdvertisingSets::new();
        sets.set_parameters(&default_params(3)).unwrap();
        assert_eq!(
            sets.set_advertising_data(3, data_operation::INTERMEDIATE_FRAGMENT, &[1]),
            Err(AdvSetError::InvalidParameters)
        );
    }
}
