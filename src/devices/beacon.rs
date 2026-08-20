// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Proximity and Telemetry BLE Beacon virtual device templates (Eddystone & iBeacon).

use crate::device::VirtualDevice;
use crate::gap::{AdvertisingData, flags};
use crate::types::{Address, AddressType, Uuid};

/// Eddystone 16-bit Service UUID (0xFEAA).
pub const EDDYSTONE_SERVICE_UUID: u16 = 0xFEAA;

/// An Eddystone-UID proximity beacon.
#[derive(Debug, Clone)]
pub struct EddystoneUidBeacon {
    pub device: VirtualDevice,
}

impl EddystoneUidBeacon {
    /// Creates a new Eddystone-UID beacon broadcasting a 10-byte namespace and 6-byte instance.
    pub fn new(
        name: impl Into<String>,
        address: Address,
        namespace: &[u8; 10],
        instance: &[u8; 6],
        tx_power: i8,
    ) -> Self {
        let name_str = name.into();

        // Eddystone-UID frame:
        // Byte 0: Frame Type = 0x00 (UID)
        // Byte 1: Ranging Data (Calibrated Tx power at 0m)
        // Bytes 2-11: 10-byte Namespace ID
        // Bytes 12-17: 6-byte Instance ID
        // Bytes 18-19: Reserved (0x00, 0x00)
        let mut uid_frame = Vec::with_capacity(20);
        uid_frame.push(0x00); // Frame Type: UID
        uid_frame.push(tx_power as u8);
        uid_frame.extend_from_slice(namespace);
        uid_frame.extend_from_slice(instance);
        uid_frame.extend_from_slice(&[0x00, 0x00]);

        let ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
            .with_service_uuid_16(EDDYSTONE_SERVICE_UUID)
            .with_service_data_16(EDDYSTONE_SERVICE_UUID, &uid_frame);

        Self {
            device: create_beacon_device(&name_str, address, ad),
        }
    }
}

/// An Apple-compatible iBeacon proximity broadcaster.
#[derive(Debug, Clone)]
pub struct IBeacon {
    pub device: VirtualDevice,
}

impl IBeacon {
    /// Creates a new iBeacon broadcaster with UUID, Major, Minor, and Measured Power.
    pub fn new(
        name: impl Into<String>,
        address: Address,
        uuid: Uuid,
        major: u16,
        minor: u16,
        measured_power: i8,
    ) -> Self {
        let name_str = name.into();

        // Apple iBeacon Manufacturer Specific Payload:
        // Byte 0: 0x02 (iBeacon SubType)
        // Byte 1: 0x15 (Subtype Length = 21 bytes)
        // Bytes 2-17: 16-byte Proximity UUID (Big Endian)
        // Bytes 18-19: Major (Big Endian)
        // Bytes 20-21: Minor (Big Endian)
        // Byte 22: Measured Power at 1 meter
        let mut mfg_payload = Vec::with_capacity(23);
        mfg_payload.push(0x02);
        mfg_payload.push(0x15);

        let uuid_128 = uuid.to_128_bit_bytes();
        // In iBeacon specification, UUID is transmitted big-endian
        for &b in uuid_128.iter().rev() {
            mfg_payload.push(b);
        }
        mfg_payload.extend_from_slice(&major.to_be_bytes());
        mfg_payload.extend_from_slice(&minor.to_be_bytes());
        mfg_payload.push(measured_power as u8);

        // Apple Company Identifier = 0x004C
        let ad = AdvertisingData::new()
            .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
            .with_manufacturer_data(0x004C, &mfg_payload);

        Self {
            device: create_beacon_device(&name_str, address, ad),
        }
    }
}

#[inline]
fn create_beacon_device(name: &str, address: Address, ad: AdvertisingData) -> VirtualDevice {
    let mut dev = VirtualDevice::new(name, address, AddressType::Random);
    dev.advertising_data = Some(ad);
    dev.is_advertising = true;
    dev
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn test_eddystone_uid_beacon_advertising() {
        let addr = Address::from_be_bytes([0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);
        let ns = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A];
        let instance = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
        let beacon = EddystoneUidBeacon::new("Eddy1", addr, &ns, &instance, -20);

        assert!(beacon.device.is_advertising);
        let ad = beacon.device.advertising_data.unwrap().to_bytes();
        assert!(ad.len() > 20);
    }

    #[test]
    fn test_ibeacon_advertising_payload() {
        let addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33]);
        let uuid = Uuid::from_str("E2C56DB5-DFFB-48D2-B060-D0F5A71096E0").unwrap();
        let ibeacon = IBeacon::new("AppleBeacon", addr, uuid, 100, 1, -59);

        assert!(ibeacon.device.is_advertising);
        let ad = ibeacon.device.advertising_data.unwrap().to_bytes();

        // Check manufacturer data header: Length 0x1A, Type 0xFF, Company ID 0x004C (Apple)
        assert_eq!(&ad[3..7], &[0x1A, 0xFF, 0x4C, 0x00]);
    }
}
