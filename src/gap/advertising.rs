// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! GAP (Generic Access Profile) advertising data structures and builders.

use serde::{Deserialize, Serialize};

/// Standard GAP Advertising Data (AD) Types.
pub mod ad_type {
    /// Flags AD type (0x01).
    pub const FLAGS: u8 = 0x01;
    /// Incomplete list of 16-bit Service UUIDs (0x02).
    pub const INCOMPLETE_16BIT_UUIDS: u8 = 0x02;
    /// Complete list of 16-bit Service UUIDs (0x03).
    pub const COMPLETE_16BIT_UUIDS: u8 = 0x03;
    /// Incomplete list of 128-bit Service UUIDs (0x06).
    pub const INCOMPLETE_128BIT_UUIDS: u8 = 0x06;
    /// Complete list of 128-bit Service UUIDs (0x07).
    pub const COMPLETE_128BIT_UUIDS: u8 = 0x07;
    /// Shortened Local Name (0x08).
    pub const SHORTENED_LOCAL_NAME: u8 = 0x08;
    /// Complete Local Name (0x09).
    pub const COMPLETE_LOCAL_NAME: u8 = 0x09;
    /// Tx Power Level (0x0A).
    pub const TX_POWER_LEVEL: u8 = 0x0A;
    /// Service Data - 16-bit UUID (0x16).
    pub const SERVICE_DATA_16BIT: u8 = 0x16;
    /// Appearance (0x19).
    pub const APPEARANCE: u8 = 0x19;
    /// Manufacturer Specific Data (0xFF).
    pub const MANUFACTURER_SPECIFIC_DATA: u8 = 0xFF;
}

/// Common LE Advertising Flags.
pub mod flags {
    /// LE Limited Discoverable Mode.
    pub const LE_LIMITED_DISCOVERABLE: u8 = 0x01;
    /// LE General Discoverable Mode.
    pub const LE_GENERAL_DISCOVERABLE: u8 = 0x02;
    /// BR/EDR Not Supported (LE-only device).
    pub const BR_EDR_NOT_SUPPORTED: u8 = 0x04;
    /// Simultaneous LE and BR/EDR to same device (Controller).
    pub const SIMULTANEOUS_LE_BR_CONTROLLER: u8 = 0x08;
    /// Simultaneous LE and BR/EDR to same device (Host).
    pub const SIMULTANEOUS_LE_BR_HOST: u8 = 0x10;
}

/// Builder for constructing LE Advertising and Scan Response data payloads.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdvertisingData {
    /// Optional LE discovery flags octet.
    pub flags: Option<u8>,
    /// Optional complete local device name.
    pub complete_name: Option<String>,
    /// List of advertised 16-bit Service UUIDs.
    pub service_uuids_16: Vec<u16>,
    /// List of (16-bit UUID, data) Service Data entries.
    pub service_data_16: Vec<(u16, Vec<u8>)>,
    /// Optional manufacturer-specific data (company ID prefixed).
    pub manufacturer_data: Option<Vec<u8>>,
}

impl AdvertisingData {
    /// Creates an empty advertising-data builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the standard LE discovery flags.
    pub fn with_flags(mut self, flags: u8) -> Self {
        self.flags = Some(flags);
        self
    }

    /// Sets the complete device local name.
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.complete_name = Some(name.into());
        self
    }

    /// Adds a 16-bit Service UUID to the advertising payload.
    pub fn with_service_uuid_16(mut self, uuid: u16) -> Self {
        self.service_uuids_16.push(uuid);
        self
    }

    /// Adds 16-bit Service Data (e.g. Eddystone beacon payloads).
    pub fn with_service_data_16(mut self, uuid: u16, data: &[u8]) -> Self {
        self.service_data_16.push((uuid, data.to_vec()));
        self
    }

    /// Sets manufacturer-specific data.
    pub fn with_manufacturer_data(mut self, company_id: u16, data: &[u8]) -> Self {
        let mut buf = Vec::with_capacity(2 + data.len());
        buf.extend_from_slice(&company_id.to_le_bytes());
        buf.extend_from_slice(data);
        self.manufacturer_data = Some(buf);
        self
    }

    /// Encodes the advertising structure into raw Bluetooth GAP byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        // Flags
        if let Some(f) = self.flags {
            bytes.push(2); // Length: 1 (type) + 1 (data)
            bytes.push(ad_type::FLAGS);
            bytes.push(f);
        }

        // Complete Local Name
        if let Some(ref name) = self.complete_name {
            let name_bytes = name.as_bytes();
            bytes.push((1 + name_bytes.len()) as u8);
            bytes.push(ad_type::COMPLETE_LOCAL_NAME);
            bytes.extend_from_slice(name_bytes);
        }

        // 16-bit Service UUIDs
        if !self.service_uuids_16.is_empty() {
            let len = 1 + (self.service_uuids_16.len() * 2);
            bytes.push(len as u8);
            bytes.push(ad_type::COMPLETE_16BIT_UUIDS);
            for uuid in &self.service_uuids_16 {
                bytes.extend_from_slice(&uuid.to_le_bytes());
            }
        }

        // 16-bit Service Data
        for (uuid, data) in &self.service_data_16 {
            let len = 1 + 2 + data.len();
            bytes.push(len as u8);
            bytes.push(ad_type::SERVICE_DATA_16BIT);
            bytes.extend_from_slice(&uuid.to_le_bytes());
            bytes.extend_from_slice(data);
        }

        // Manufacturer Specific Data
        if let Some(ref mfg) = self.manufacturer_data {
            bytes.push((1 + mfg.len()) as u8);
            bytes.push(ad_type::MANUFACTURER_SPECIFIC_DATA);
            bytes.extend_from_slice(mfg);
        }

        bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_advertising_data_builder() {
        let ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
            .with_name("Simble-HRM")
            .with_service_uuid_16(0x180D)
            .with_service_data_16(0xFEAA, &[0x00, 0x10]);

        let bytes = ad.to_bytes();

        // Verify Flags element: [0x02, 0x01, 0x06]
        assert_eq!(&bytes[0..3], &[0x02, 0x01, 0x06]);

        // Verify Name element: [0x0B, 0x09, 'S', 'i', 'm', 'b', 'l', 'e', '-', 'H', 'R', 'M']
        assert_eq!(bytes[3], 11);
        assert_eq!(bytes[4], 0x09);
        assert_eq!(&bytes[5..15], b"Simble-HRM");

        // Verify Service UUID element: [0x03, 0x03, 0x0D, 0x18]
        assert_eq!(&bytes[15..19], &[0x03, 0x03, 0x0D, 0x18]);

        // Verify Service Data element: [0x05, 0x16, 0xAA, 0xFE, 0x00, 0x10]
        assert_eq!(&bytes[19..25], &[0x05, 0x16, 0xAA, 0xFE, 0x00, 0x10]);
    }
}
