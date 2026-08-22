// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Ranging Service (RAS, UUID 0x185B) per Bluetooth SIG Channel Sounding Profile.
//!
//! Exposes ranging features, real-time distance measurements, and procedure controls
//! to central applications (e.g. Android Distance Measurement API).

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// Bluetooth SIG Ranging Service UUIDs.
pub mod ras_uuid {
    use crate::types::Uuid;

    /// Ranging Service UUID.
    pub const RANGING_SERVICE: Uuid = Uuid::Uuid16(0x185B);
    /// Ranging Features characteristic UUID.
    pub const RANGING_FEATURES: Uuid = Uuid::Uuid16(0x2B6E);
    /// Ranging Realtime Data characteristic UUID.
    pub const RANGING_REALTIME_DATA: Uuid = Uuid::Uuid16(0x2B70);
    /// Ranging On Demand Data characteristic UUID.
    pub const RANGING_ON_DEMAND_DATA: Uuid = Uuid::Uuid16(0x2B71);
    /// Ranging Control Point characteristic UUID.
    pub const RANGING_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2B72);
}

/// Ranging Service container holding GATT attribute handles.
#[derive(Debug, Clone)]
pub struct RangingService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Attribute handle of the Features.
    pub features_handle: u16,
    /// Value attribute handle of the Features characteristic.
    pub features_value_handle: u16,
    /// Attribute handle of the Realtime Data.
    pub realtime_data_handle: u16,
    /// Value attribute handle of the Realtime Data characteristic.
    pub realtime_data_value_handle: u16,
    /// Attribute handle of the Control Point.
    pub control_point_handle: u16,
    /// Value attribute handle of the Control Point characteristic.
    pub control_point_value_handle: u16,
}

impl RangingService {
    /// Registers the Ranging Service (0x185B) into a GATT database.
    pub fn register(db: &mut GattDatabase) -> Self {
        let service_handle = db.add_service(ras_uuid::RANGING_SERVICE, true);

        // 1. Ranging Features (0x2B6E) - Read only
        // Feature bits: bit 0: Real-Time Ranging Data, bit 1: On-Demand Ranging Data
        let (features_handle, features_value_handle) = db.add_characteristic(
            ras_uuid::RANGING_FEATURES,
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![0x03, 0x00, 0x00, 0x00], // Features: Real-Time + On-Demand supported
            AttributePermissions::default(),
        );

        // 2. Real-Time Ranging Data (0x2B70) - Notify
        let (realtime_data_handle, realtime_data_value_handle) = db
            .add_characteristic_with_cccd(
                ras_uuid::RANGING_REALTIME_DATA,
                CharacteristicProperties(CharacteristicProperties::NOTIFY),
                vec![0x00; 8],
                AttributePermissions::default(),
            );

        // 3. Ranging Control Point (0x2B72) - Write | Indicate
        let (control_point_handle, control_point_value_handle) = db.add_characteristic_with_cccd(
            ras_uuid::RANGING_CONTROL_POINT,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::INDICATE,
            ),
            vec![],
            AttributePermissions::write_only(),
        );

        Self {
            service_handle,
            features_handle,
            features_value_handle,
            realtime_data_handle,
            realtime_data_value_handle,
            control_point_handle,
            control_point_value_handle,
        }
    }

    /// Encodes a real-time ranging measurement (distance in meters and confidence) into GATT payload.
    pub fn encode_ranging_data(distance_meters: f32, confidence: f32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&distance_meters.to_le_bytes());
        buf.extend_from_slice(&confidence.to_le_bytes());
        buf
    }

    /// Updates the real-time ranging data in the GATT database and creates a notification packet.
    pub fn update_ranging_data(
        &self,
        db: &mut GattDatabase,
        distance_meters: f32,
        confidence: f32,
    ) -> Result<Vec<u8>, u8> {
        let payload = Self::encode_ranging_data(distance_meters, confidence);
        db.set_value(self.realtime_data_value_handle, &payload)?;
        Ok(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ranging_service_registration_and_update() {
        let mut db = GattDatabase::new();
        let ras = RangingService::register(&mut db);

        assert_eq!(ras.service_handle, 1);
        assert_eq!(ras.features_handle, 2);
        assert_eq!(ras.features_value_handle, 3);
        assert_eq!(ras.realtime_data_handle, 4);
        assert_eq!(ras.realtime_data_value_handle, 5);

        // Read Features
        let features = db.read(ras.features_value_handle, 0).unwrap();
        assert_eq!(features, &[0x03, 0x00, 0x00, 0x00]);

        // Update Ranging Data (e.g. 2.45 meters, confidence 0.95)
        let payload = ras.update_ranging_data(&mut db, 2.45, 0.95).unwrap();
        assert_eq!(payload.len(), 8);

        let read_back = db.read(ras.realtime_data_value_handle, 0).unwrap();
        assert_eq!(read_back, &payload);
    }
}
