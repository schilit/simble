// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! GATT (Generic Attribute Profile) Server Database and handle management.

use crate::att::error_code;
use crate::types::Uuid;
use std::collections::BTreeMap;

/// Standard GATT UUIDs for declarations and descriptors.
pub mod service_uuid {
    pub const PRIMARY_SERVICE: u16 = 0x2800;
    pub const SECONDARY_SERVICE: u16 = 0x2801;
    pub const INCLUDE: u16 = 0x2802;
    pub const CHARACTERISTIC: u16 = 0x2803;
}

pub mod desc_uuid {
    pub const CLIENT_CHARACTERISTIC_CONFIGURATION: u16 = 0x2902;
    pub const CHARACTERISTIC_USER_DESCRIPTION: u16 = 0x2901;
}

/// Bitmask for characteristic properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CharacteristicProperties(pub u8);

impl CharacteristicProperties {
    pub const BROADCAST: u8 = 0x01;
    pub const READ: u8 = 0x02;
    pub const WRITE_WITHOUT_RESPONSE: u8 = 0x04;
    pub const WRITE: u8 = 0x08;
    pub const NOTIFY: u8 = 0x10;
    pub const INDICATE: u8 = 0x20;
    pub const AUTHENTICATED_SIGNED_WRITES: u8 = 0x40;
    pub const EXTENDED_PROPERTIES: u8 = 0x80;
}

/// Attribute permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttributePermissions {
    pub read: bool,
    pub write: bool,
    pub read_auth: bool,
    pub write_auth: bool,
    pub encrypt_read: bool,
    pub encrypt_write: bool,
}

impl AttributePermissions {
    /// Read-only attribute permissions without authentication/encryption.
    pub const fn read_only() -> Self {
        Self {
            read: true,
            write: false,
            read_auth: false,
            write_auth: false,
            encrypt_read: false,
            encrypt_write: false,
        }
    }

    /// Write-only attribute permissions without authentication/encryption.
    pub const fn write_only() -> Self {
        Self {
            read: false,
            write: true,
            read_auth: false,
            write_auth: false,
            encrypt_read: false,
            encrypt_write: false,
        }
    }
}

impl Default for AttributePermissions {
    fn default() -> Self {
        Self {
            read: true,
            write: true,
            read_auth: false,
            write_auth: false,
            encrypt_read: false,
            encrypt_write: false,
        }
    }
}

/// A GATT attribute stored in the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    pub handle: u16,
    pub uuid: Uuid,
    pub value: Vec<u8>,
    pub permissions: AttributePermissions,
}

/// The local GATT Database representing all registered services, characteristics,
/// and descriptors on the device.
#[derive(Debug, Clone, Default)]
pub struct GattDatabase {
    pub attributes: BTreeMap<u16, Attribute>,
    next_handle: u16,
}

impl GattDatabase {
    /// Creates a new, empty GATT database.
    pub fn new() -> Self {
        Self {
            attributes: BTreeMap::new(),
            next_handle: 0x0001,
        }
    }

    /// Allocates the next available attribute handle.
    fn allocate_handle(&mut self) -> u16 {
        let h = self.next_handle;
        self.next_handle = self.next_handle.checked_add(1).expect("Handle overflow");
        h
    }

    /// Adds a Primary or Secondary Service declaration.
    pub fn add_service(&mut self, uuid: Uuid, primary: bool) -> u16 {
        let handle = self.allocate_handle();
        let decl_uuid = if primary {
            Uuid::from_u16(service_uuid::PRIMARY_SERVICE)
        } else {
            Uuid::from_u16(service_uuid::SECONDARY_SERVICE)
        };

        let val = match uuid {
            Uuid::Uuid16(u) => u.to_le_bytes().to_vec(),
            Uuid::Uuid128(u) => u.to_vec(),
        };

        self.attributes.insert(
            handle,
            Attribute {
                handle,
                uuid: decl_uuid,
                value: val,
                permissions: AttributePermissions::read_only(),
            },
        );

        handle
    }

    /// Adds a Characteristic declaration followed by its Value attribute.
    pub fn add_characteristic(
        &mut self,
        uuid: Uuid,
        properties: CharacteristicProperties,
        initial_value: Vec<u8>,
        permissions: AttributePermissions,
    ) -> (u16, u16) {
        let decl_handle = self.allocate_handle();
        let val_handle = self.allocate_handle();

        // 1. Characteristic Declaration Attribute (0x2803)
        // Value = [properties (1B), value_handle (2B), uuid (2B or 16B)]
        let mut decl_val = Vec::with_capacity(3 + 16);
        decl_val.push(properties.0);
        decl_val.extend_from_slice(&val_handle.to_le_bytes());
        match uuid {
            Uuid::Uuid16(u) => decl_val.extend_from_slice(&u.to_le_bytes()),
            Uuid::Uuid128(u) => decl_val.extend_from_slice(&u),
        }

        self.attributes.insert(
            decl_handle,
            Attribute {
                handle: decl_handle,
                uuid: Uuid::from_u16(service_uuid::CHARACTERISTIC),
                value: decl_val,
                permissions: AttributePermissions::read_only(),
            },
        );

        // 2. Characteristic Value Attribute
        self.attributes.insert(
            val_handle,
            Attribute {
                handle: val_handle,
                uuid,
                value: initial_value,
                permissions,
            },
        );

        (decl_handle, val_handle)
    }

    /// Adds a Client Characteristic Configuration Descriptor (CCCD, 0x2902).
    pub fn add_cccd(&mut self) -> u16 {
        let handle = self.allocate_handle();
        self.attributes.insert(
            handle,
            Attribute {
                handle,
                uuid: Uuid::from_u16(desc_uuid::CLIENT_CHARACTERISTIC_CONFIGURATION),
                value: vec![0x00, 0x00],
                permissions: AttributePermissions::default(),
            },
        );
        handle
    }

    /// Adds a custom descriptor.
    pub fn add_descriptor(
        &mut self,
        uuid: Uuid,
        value: Vec<u8>,
        permissions: AttributePermissions,
    ) -> u16 {
        self.add_attribute(uuid, permissions, value)
    }

    /// Adds a raw attribute directly with custom permissions and value.
    pub fn add_attribute(
        &mut self,
        uuid: Uuid,
        permissions: AttributePermissions,
        value: Vec<u8>,
    ) -> u16 {
        let handle = self.allocate_handle();
        self.attributes.insert(
            handle,
            Attribute {
                handle,
                uuid,
                value,
                permissions,
            },
        );
        handle
    }

    /// Reads an attribute value starting at `offset`.
    pub fn read(&self, handle: u16, offset: usize) -> Result<&[u8], u8> {
        let attr = self
            .attributes
            .get(&handle)
            .ok_or(error_code::INVALID_HANDLE)?;
        if !attr.permissions.read {
            return Err(error_code::READ_NOT_PERMITTED);
        }
        if offset > attr.value.len() {
            return Err(error_code::INVALID_OFFSET);
        }
        Ok(&attr.value[offset..])
    }

    fn check_write_permitted(&mut self, handle: u16) -> Result<&mut Attribute, u8> {
        let attr = self
            .attributes
            .get_mut(&handle)
            .ok_or(error_code::INVALID_HANDLE)?;
        if !attr.permissions.write {
            return Err(error_code::WRITE_NOT_PERMITTED);
        }
        Ok(attr)
    }

    /// Writes a value to an attribute.
    pub fn write(&mut self, handle: u16, value: &[u8]) -> Result<(), u8> {
        let attr = self.check_write_permitted(handle)?;
        attr.value = value.to_vec();
        Ok(())
    }

    /// Sets an attribute value directly from the host simulation without checking client permissions.
    pub fn set_value(&mut self, handle: u16, value: &[u8]) -> Result<(), u8> {
        let attr = self
            .attributes
            .get_mut(&handle)
            .ok_or(error_code::INVALID_HANDLE)?;
        attr.value = value.to_vec();
        Ok(())
    }

    /// Writes a value to an attribute starting at a specific byte offset.
    pub fn write_offset(&mut self, handle: u16, offset: usize, value: &[u8]) -> Result<(), u8> {
        let attr = self.check_write_permitted(handle)?;
        if attr.value.len() < offset + value.len() {
            attr.value.resize(offset + value.len(), 0);
        }
        attr.value[offset..offset + value.len()].copy_from_slice(value);
        Ok(())
    }

    /// Finds attributes within handle range `[start..=end]` matching `uuid`.
    pub fn read_by_type(&self, start: u16, end: u16, uuid: Uuid) -> Vec<(u16, &[u8])> {
        self.attributes
            .range(start..=end)
            .filter(|(_, attr)| attr.uuid == uuid && attr.permissions.read)
            .map(|(&h, attr)| (h, attr.value.as_slice()))
            .collect()
    }

    /// Finds information (Handle + UUID) within handle range `[start..=end]`.
    pub fn find_information(&self, start: u16, end: u16) -> Vec<(u16, Uuid)> {
        self.attributes
            .range(start..=end)
            .map(|(&h, attr)| (h, attr.uuid))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gatt_database_service_and_characteristic_creation() {
        let mut db = GattDatabase::new();

        let svc_handle = db.add_service(Uuid::from_u16(0x180D), true); // Heart Rate Service
        assert_eq!(svc_handle, 0x0001);

        let (decl_h, val_h) = db.add_characteristic(
            Uuid::from_u16(0x2A37), // Heart Rate Measurement
            CharacteristicProperties(CharacteristicProperties::NOTIFY),
            vec![0x00, 0x50],
            AttributePermissions::default(),
        );
        assert_eq!(decl_h, 0x0002);
        assert_eq!(val_h, 0x0003);

        let cccd_h = db.add_cccd();
        assert_eq!(cccd_h, 0x0004);

        // Read initial value
        let val = db.read(val_h, 0).expect("Read valid");
        assert_eq!(val, &[0x00, 0x50]);

        // Write new value
        db.write(val_h, &[0x00, 0x55]).expect("Write valid");
        assert_eq!(db.read(val_h, 0).unwrap(), &[0x00, 0x55]);
    }
}
