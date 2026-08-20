// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Generic Attribute Profile (GATT) Service (0x1801) and Database Hash (0x2B2A) calculation.
//!
//! Implements Bluetooth Core Specification Vol 3, Part G, Section 7:
//! - Service Changed characteristic (0x2A05)
//! - Client Supported Features characteristic (0x2B29)
//! - Database Hash characteristic (0x2B2A)
//! - Server Supported Features characteristic (0x2B3A)

use crate::crypto::aes_cmac;
use crate::gatt::{
    AttributePermissions, CharacteristicProperties, GattDatabase, desc_uuid, service_uuid,
};
use crate::types::Uuid;

/// Standard GATT Profile Characteristic UUIDs.
pub mod gatt_char_uuid {
    pub const GENERIC_ATTRIBUTE_SERVICE: u16 = 0x1801;
    pub const SERVICE_CHANGED: u16 = 0x2A05;
    pub const CLIENT_SUPPORTED_FEATURES: u16 = 0x2B29;
    pub const DATABASE_HASH: u16 = 0x2B2A;
    pub const SERVER_SUPPORTED_FEATURES: u16 = 0x2B3A;
}

/// Generic Attribute Profile Service representation in the GATT database.
#[derive(Debug, Clone)]
pub struct GenericAttributeProfileService {
    pub service_handle: u16,
    pub service_changed_val_handle: Option<u16>,
    pub client_supported_features_val_handle: Option<u16>,
    pub database_hash_val_handle: Option<u16>,
}

impl GenericAttributeProfileService {
    /// Computes the 128-bit AES-CMAC Database Hash over the entire GATT Database
    /// as specified in Bluetooth Core Spec Vol 3, Part G, Section 7.3.1.
    pub fn compute_database_hash(db: &GattDatabase) -> [u8; 16] {
        let mut m = Vec::new();

        for (&handle, attr) in &db.attributes {
            match attr.uuid {
                Uuid::Uuid16(u) => match u {
                    service_uuid::PRIMARY_SERVICE
                    | service_uuid::SECONDARY_SERVICE
                    | service_uuid::INCLUDE
                    | service_uuid::CHARACTERISTIC
                    | 0x2900 => {
                        // Attribute Data = Handle (2B LE) + UUID (2B LE) + Value
                        m.extend_from_slice(&handle.to_le_bytes());
                        m.extend_from_slice(&u.to_le_bytes());
                        m.extend_from_slice(&attr.value);
                    }
                    desc_uuid::CHARACTERISTIC_USER_DESCRIPTION
                    | desc_uuid::CLIENT_CHARACTERISTIC_CONFIGURATION
                    | 0x2903
                    | 0x2904
                    | 0x2905 => {
                        // Attribute Data = Handle (2B LE) + UUID (2B LE)
                        m.extend_from_slice(&handle.to_le_bytes());
                        m.extend_from_slice(&u.to_le_bytes());
                    }
                    _ => {}
                },
                Uuid::Uuid128(u) => {
                    m.extend_from_slice(&handle.to_le_bytes());
                    m.extend_from_slice(&u);
                    m.extend_from_slice(&attr.value);
                }
            }
        }

        // Bluetooth Core Specification: key is 16 zero-bytes
        let key = [0u8; 16];
        aes_cmac(&key, &m)
    }

    /// Registers the Generic Attribute Service (0x1801) in the GATT database.
    pub fn register(
        db: &mut GattDatabase,
        database_hash_enabled: bool,
        service_changed_enabled: bool,
    ) -> Self {
        let service_handle = db.add_service(
            Uuid::from_u16(gatt_char_uuid::GENERIC_ATTRIBUTE_SERVICE),
            true,
        );

        let mut service_changed_val_handle = None;
        if service_changed_enabled {
            let (_, val_h) = db.add_characteristic(
                Uuid::from_u16(gatt_char_uuid::SERVICE_CHANGED),
                CharacteristicProperties(CharacteristicProperties::INDICATE),
                vec![0x00, 0x00, 0xFF, 0xFF], // Start handle 0x0000, End handle 0xFFFF
                AttributePermissions::default(),
            );
            let _ = db.add_cccd();
            service_changed_val_handle = Some(val_h);
        }

        let mut client_supported_features_val_handle = None;
        if database_hash_enabled || service_changed_enabled {
            let (_, val_h) = db.add_characteristic(
                Uuid::from_u16(gatt_char_uuid::CLIENT_SUPPORTED_FEATURES),
                CharacteristicProperties(
                    CharacteristicProperties::READ | CharacteristicProperties::WRITE,
                ),
                vec![0x00],
                AttributePermissions::default(),
            );
            client_supported_features_val_handle = Some(val_h);
        }

        let mut database_hash_val_handle = None;
        if database_hash_enabled {
            // Allocate characteristic with 16-zero placeholder
            let (_, val_h) = db.add_characteristic(
                Uuid::from_u16(gatt_char_uuid::DATABASE_HASH),
                CharacteristicProperties(CharacteristicProperties::READ),
                vec![0u8; 16],
                AttributePermissions::default(),
            );

            // Compute actual hash of database and write into attribute
            let hash = Self::compute_database_hash(db);
            let _ = db.write(val_h, &hash);
            database_hash_val_handle = Some(val_h);
        }

        Self {
            service_handle,
            service_changed_val_handle,
            client_supported_features_val_handle,
            database_hash_val_handle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generic_attribute_service_and_database_hash() {
        let mut db = GattDatabase::new();

        let gatt_svc = GenericAttributeProfileService::register(&mut db, true, true);
        assert_eq!(gatt_svc.service_handle, 1);
        assert!(gatt_svc.database_hash_val_handle.is_some());

        let hash_h = gatt_svc.database_hash_val_handle.unwrap();
        let hash = db.read(hash_h, 0).unwrap();
        assert_eq!(hash.len(), 16);
        assert_ne!(hash, &[0u8; 16]); // Hash must be non-zero
    }
}
