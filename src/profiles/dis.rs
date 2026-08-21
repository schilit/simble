// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Device Information Service (DIS, UUID 0x180A).
//!
//! Provides manufacturer, model, serial, hardware, firmware, and software revision strings.

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use crate::types::Uuid;

/// DIS Characteristic UUIDs.
pub(crate) mod characteristic_uuid {
    /// System Id characteristic UUID.
    pub(crate) const SYSTEM_ID: u16 = 0x2A23;
    /// Model Number characteristic UUID.
    pub(crate) const MODEL_NUMBER: u16 = 0x2A24;
    /// Serial Number characteristic UUID.
    pub(crate) const SERIAL_NUMBER: u16 = 0x2A25;
    /// Firmware Revision characteristic UUID.
    pub(crate) const FIRMWARE_REVISION: u16 = 0x2A26;
    /// Hardware Revision characteristic UUID.
    pub(crate) const HARDWARE_REVISION: u16 = 0x2A27;
    /// Software Revision characteristic UUID.
    pub(crate) const SOFTWARE_REVISION: u16 = 0x2A28;
    /// Manufacturer Name characteristic UUID.
    pub(crate) const MANUFACTURER_NAME: u16 = 0x2A29;
    /// Ieee Regulatory characteristic UUID.
    pub(crate) const IEEE_REGULATORY: u16 = 0x2A2A;
    /// Pnp Id characteristic UUID.
    pub(crate) const PNP_ID: u16 = 0x2A50;
}

macro_rules! impl_dis_field {
    ($fn_name:ident, $field:ident) => {
        pub fn $fn_name(mut self, val: impl Into<String>) -> Self {
            self.$field = Some(val.into());
            self
        }
    };
}

/// Builder and manager for Device Information Service (0x180A).
#[derive(Debug, Clone, Default)]
pub struct DeviceInformationService {
    /// Manufacturer Name.
    pub manufacturer_name: Option<String>,
    /// Model Number.
    pub model_number: Option<String>,
    /// Serial Number.
    pub serial_number: Option<String>,
    /// Hardware Revision.
    pub hardware_revision: Option<String>,
    /// Firmware Revision.
    pub firmware_revision: Option<String>,
    /// Software Revision.
    pub software_revision: Option<String>,
}

impl DeviceInformationService {
    /// Creates a new builder for Device Information Service.
    pub fn new() -> Self {
        Self::default()
    }

    impl_dis_field!(with_manufacturer_name, manufacturer_name);
    impl_dis_field!(with_model_number, model_number);
    impl_dis_field!(with_serial_number, serial_number);
    impl_dis_field!(with_hardware_revision, hardware_revision);
    impl_dis_field!(with_firmware_revision, firmware_revision);
    impl_dis_field!(with_software_revision, software_revision);

    /// Registers this Device Information Service in the given GATT database.
    pub fn register(&self, gatt_db: &mut GattDatabase) -> u16 {
        let service_handle = gatt_db.add_service(Uuid::from_u16(0x180A), true);

        let props = CharacteristicProperties(CharacteristicProperties::READ);
        let perms = AttributePermissions::default();

        let entries: &[(u16, &Option<String>)] = &[
            (
                characteristic_uuid::MANUFACTURER_NAME,
                &self.manufacturer_name,
            ),
            (characteristic_uuid::MODEL_NUMBER, &self.model_number),
            (characteristic_uuid::SERIAL_NUMBER, &self.serial_number),
            (
                characteristic_uuid::HARDWARE_REVISION,
                &self.hardware_revision,
            ),
            (
                characteristic_uuid::FIRMWARE_REVISION,
                &self.firmware_revision,
            ),
            (
                characteristic_uuid::SOFTWARE_REVISION,
                &self.software_revision,
            ),
        ];

        for &(uuid_16, value_opt) in entries {
            if let Some(val) = value_opt {
                gatt_db.add_characteristic(
                    Uuid::from_u16(uuid_16),
                    props,
                    val.as_bytes().to_vec(),
                    perms,
                );
            }
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
            .with_manufacturer_name("Google Inc.")
            .with_model_number("Pixel-Sim-1")
            .with_firmware_revision("2.0.0");

        let handle = dis.register(&mut db);
        assert_eq!(handle, 1);
        assert_eq!(db.attributes.len(), 7); // 1 Service + 3 * (1 Decl + 1 Val) = 7
    }
}
