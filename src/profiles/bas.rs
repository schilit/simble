// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Battery Service (BAS) standard profile (UUID 0x180F).

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use crate::types::Uuid;

/// Battery Service manager.
#[derive(Debug, Clone)]
pub struct BatteryService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Attribute handle of the Level Decl.
    pub level_decl_handle: u16,
    /// Attribute handle of the Level Val.
    pub level_val_handle: u16,
    /// Attribute handle of the Cccd.
    pub cccd_handle: u16,
    /// Battery Level.
    pub battery_level: u8,
}

impl BatteryService {
    /// Battery Service UUID (0x180F).
    pub const SERVICE_UUID: Uuid = Uuid::Uuid16(0x180F);
    /// Battery Level Characteristic UUID (0x2A19).
    pub const BATTERY_LEVEL_UUID: Uuid = Uuid::Uuid16(0x2A19);

    /// Registers a new Battery Service with an initial percentage level in the GATT database.
    pub fn register(gatt_db: &mut GattDatabase, initial_level: u8) -> Self {
        let service_handle = gatt_db.add_service(Self::SERVICE_UUID, true);

        let props = CharacteristicProperties(
            CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
        );
        let perms = AttributePermissions::default();

        let (level_decl_handle, level_val_handle) = gatt_db.add_characteristic(
            Self::BATTERY_LEVEL_UUID,
            props,
            vec![initial_level.min(100)],
            perms,
        );

        let cccd_handle = gatt_db.add_cccd();

        Self {
            service_handle,
            level_decl_handle,
            level_val_handle,
            cccd_handle,
            battery_level: initial_level.min(100),
        }
    }

    /// Updates the battery percentage in the GATT database.
    pub fn set_level(&mut self, gatt_db: &mut GattDatabase, level: u8) -> Result<(), u8> {
        let clamped = level.min(100);
        gatt_db.write(self.level_val_handle, &[clamped])?;
        self.battery_level = clamped;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_battery_service_lifecycle() {
        let mut db = GattDatabase::new();
        let mut bas = BatteryService::register(&mut db, 85);

        assert_eq!(bas.battery_level, 85);

        // Read value from GATT DB
        let val = db.read(bas.level_val_handle, 0).unwrap();
        assert_eq!(val, &[85]);

        // Update level to 90%
        bas.set_level(&mut db, 90).unwrap();
        assert_eq!(bas.battery_level, 90);
        let val = db.read(bas.level_val_handle, 0).unwrap();
        assert_eq!(val, &[90]);
    }
}
