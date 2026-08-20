// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Device Information Service (DIS) standard profile (UUID 0x180A).

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use crate::types::Uuid;

/// Standard characteristic UUIDs for Device Information Service.
pub mod characteristic_uuid {
    pub const SYSTEM_ID: u16 = 0x2A23;
    pub const MODEL_NUMBER: u16 = 0x2A24;
    pub const SERIAL_NUMBER: u16 = 0x2A25;
    pub const FIRMWARE_REVISION: u16 = 0x2A26;
    pub const HARDWARE_REVISION: u16 = 0x2A27;
    pub const SOFTWARE_REVISION: u16 = 0x2A28;
    pub const MANUFACTURER_NAME: u16 = 0x2A29;
    pub const IEEE_REGULATORY_CERT: u16 = 0x2A2A;
    pub const PNP_ID: u16 = 0x2A50;
}

/// Device Information Service configuration.
#[derive(Debug, Clone, Default)]
pub struct DeviceInformationService {
    pub manufacturer_name: Option<String>,
    pub model_number: Option<String>,
    pub serial_number: Option<String>,
    pub hardware_revision: Option<String>,
    pub firmware_revision: Option<String>,
    pub software_revision: Option<String>,
}

impl DeviceInformationService {
    /// Creates a new builder for Device Information Service.
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_manufacturer_name(mut self, name: impl Into<String>) -> Self {
        self.manufacturer_name = Some(name.into());
        self
    }

    pub fn with_model_number(mut self, model: impl Into<String>) -> Self {
        self.model_number = Some(model.into());
        self
    }

    pub fn with_serial_number(mut self, serial: impl Into<String>) -> Self {
        self.serial_number = Some(serial.into());
        self
    }

    pub fn with_hardware_revision(mut self, rev: impl Into<String>) -> Self {
        self.hardware_revision = Some(rev.into());
        self
    }

    pub fn with_firmware_revision(mut self, rev: impl Into<String>) -> Self {
        self.firmware_revision = Some(rev.into());
        self
    }

    pub fn with_software_revision(mut self, rev: impl Into<String>) -> Self {
        self.software_revision = Some(rev.into());
        self
    }

    /// Registers this Device Information Service in the given GATT database.
    pub fn register(&self, gatt_db: &mut GattDatabase) -> u16 {
        let service_handle = gatt_db.add_service(Uuid::from_u16(0x180A), true);

        let props = CharacteristicProperties(CharacteristicProperties::READ);
        let perms = AttributePermissions::default();

        if let Some(mfg) = &self.manufacturer_name {
            gatt_db.add_characteristic(
                Uuid::from_u16(characteristic_uuid::MANUFACTURER_NAME),
                props,
                mfg.as_bytes().to_vec(),
                perms,
            );
        }

        if let Some(model) = &self.model_number {
            gatt_db.add_characteristic(
                Uuid::from_u16(characteristic_uuid::MODEL_NUMBER),
                props,
                model.as_bytes().to_vec(),
                perms,
            );
        }

        if let Some(serial) = &self.serial_number {
            gatt_db.add_characteristic(
                Uuid::from_u16(characteristic_uuid::SERIAL_NUMBER),
                props,
                serial.as_bytes().to_vec(),
                perms,
            );
        }

        if let Some(hw) = &self.hardware_revision {
            gatt_db.add_characteristic(
                Uuid::from_u16(characteristic_uuid::HARDWARE_REVISION),
                props,
                hw.as_bytes().to_vec(),
                perms,
            );
        }

        if let Some(fw) = &self.firmware_revision {
            gatt_db.add_characteristic(
                Uuid::from_u16(characteristic_uuid::FIRMWARE_REVISION),
                props,
                fw.as_bytes().to_vec(),
                perms,
            );
        }

        if let Some(sw) = &self.software_revision {
            gatt_db.add_characteristic(
                Uuid::from_u16(characteristic_uuid::SOFTWARE_REVISION),
                props,
                sw.as_bytes().to_vec(),
                perms,
            );
        }

        service_handle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_device_information_service_registration() {
        let mut db = GattDatabase::new();
        let dis = DeviceInformationService::new()
            .with_manufacturer_name("Google LLC")
            .with_model_number("Pixel 10")
            .with_firmware_revision("1.0.0");

        let svc_handle = dis.register(&mut db);
        assert_eq!(svc_handle, 0x0001);

        // Verify manufacturer name characteristic
        let results = db.read_by_type(
            0x0001,
            0xFFFF,
            Uuid::from_u16(characteristic_uuid::MANUFACTURER_NAME),
        );
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, b"Google LLC");
    }
}
