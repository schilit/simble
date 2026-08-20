// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Heart Rate Service (HRS) standard profile (UUID 0x180D).

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use crate::types::Uuid;

/// Body Sensor Location standard values.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodySensorLocation {
    Other = 0,
    Chest = 1,
    Wrist = 2,
    Finger = 3,
    Hand = 4,
    EarLobe = 5,
    Foot = 6,
}

/// Heart Rate Service manager.
#[derive(Debug, Clone)]
pub struct HeartRateService {
    pub service_handle: u16,
    pub measurement_decl_handle: u16,
    pub measurement_val_handle: u16,
    pub measurement_cccd_handle: u16,
    pub location_handle: Option<u16>,
}

impl HeartRateService {
    /// Heart Rate Service UUID (0x180D).
    pub const SERVICE_UUID: Uuid = Uuid::Uuid16(0x180D);
    /// Heart Rate Measurement Characteristic UUID (0x2A37).
    pub const MEASUREMENT_UUID: Uuid = Uuid::Uuid16(0x2A37);
    /// Body Sensor Location Characteristic UUID (0x2A38).
    pub const BODY_SENSOR_LOCATION_UUID: Uuid = Uuid::Uuid16(0x2A38);

    /// Registers a Heart Rate Service in the GATT database.
    pub fn register(
        gatt_db: &mut GattDatabase,
        sensor_location: Option<BodySensorLocation>,
    ) -> Self {
        let service_handle = gatt_db.add_service(Self::SERVICE_UUID, true);

        // Heart Rate Measurement (Notify only)
        let (measurement_decl_handle, measurement_val_handle) = gatt_db.add_characteristic(
            Self::MEASUREMENT_UUID,
            CharacteristicProperties(CharacteristicProperties::NOTIFY),
            vec![0x00, 0x00], // Flags=0 (8-bit bpm), BPM=0
            AttributePermissions::default(),
        );

        let measurement_cccd_handle = gatt_db.add_cccd();

        // Optional Body Sensor Location (Read only)
        let location_handle = sensor_location.map(|loc| {
            let (_, val_h) = gatt_db.add_characteristic(
                Self::BODY_SENSOR_LOCATION_UUID,
                CharacteristicProperties(CharacteristicProperties::READ),
                vec![loc as u8],
                AttributePermissions::default(),
            );
            val_h
        });

        Self {
            service_handle,
            measurement_decl_handle,
            measurement_val_handle,
            measurement_cccd_handle,
            location_handle,
        }
    }

    /// Encodes a standard 8-bit Heart Rate Measurement packet.
    pub fn encode_measurement_8bit(bpm: u8) -> Vec<u8> {
        vec![0x00, bpm]
    }

    /// Encodes a standard 16-bit Heart Rate Measurement packet.
    pub fn encode_measurement_16bit(bpm: u16) -> Vec<u8> {
        let mut buf = vec![0x01]; // Bit 0 = 1 for 16-bit BPM
        buf.extend_from_slice(&bpm.to_le_bytes());
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_heart_rate_service_registration_and_encoding() {
        let mut db = GattDatabase::new();
        let hrs = HeartRateService::register(&mut db, Some(BodySensorLocation::Chest));

        let m8 = HeartRateService::encode_measurement_8bit(72);
        assert_eq!(m8, &[0x00, 72]);

        let m16 = HeartRateService::encode_measurement_16bit(180);
        assert_eq!(m16, &[0x01, 180, 0]);

        // Verify sensor location
        let loc = db.read(hrs.location_handle.unwrap(), 0).unwrap();
        assert_eq!(loc, &[BodySensorLocation::Chest as u8]);
    }
}
