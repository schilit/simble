// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Tests Generic Attribute Profile Service (0x1801), Database Hash (0x2B2A)
//! computation & verification, and Service Changed (0x2A05) indications.

use simble::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use simble::profiles::GenericAttributeProfileService;
use simble::types::Uuid;

#[test]
fn test_database_hash_computation_and_consistency() {
    let mut db = GattDatabase::new();

    // 1. Add Generic Access Service (0x1800) with Device Name and Appearance
    db.add_service(Uuid::from_u16(0x1800), true);
    db.add_characteristic(
        Uuid::from_u16(0x2A00), // Device Name
        CharacteristicProperties(CharacteristicProperties::READ | CharacteristicProperties::WRITE),
        b"SimbleDevice".to_vec(),
        AttributePermissions::default(),
    );
    db.add_characteristic(
        Uuid::from_u16(0x2A01), // Appearance
        CharacteristicProperties(CharacteristicProperties::READ),
        vec![0x00, 0x00],
        AttributePermissions::default(),
    );

    // 2. Register Generic Attribute Service (0x1801)
    let gatt_svc = GenericAttributeProfileService::register(&mut db, true, true);
    assert!(gatt_svc.database_hash_val_handle.is_some());

    // 3. Compute hash and verify it is a valid 128-bit non-zero AES-CMAC
    let hash1 = GenericAttributeProfileService::compute_database_hash(&db);
    assert_eq!(hash1.len(), 16);
    assert_ne!(hash1, [0u8; 16]);

    // 4. Adding a new service must change the computed database hash
    db.add_service(Uuid::from_u16(0x180F), true); // Battery Service
    let hash2 = GenericAttributeProfileService::compute_database_hash(&db);
    assert_ne!(hash1, hash2);

    // 5. Hash must be deterministic (same db produces same hash)
    let hash3 = GenericAttributeProfileService::compute_database_hash(&db);
    assert_eq!(hash2, hash3);
}

#[test]
fn test_service_changed_characteristic_and_features() {
    let mut db = GattDatabase::new();

    let gatt_svc = GenericAttributeProfileService::register(&mut db, true, true);

    // Verify Service Changed characteristic (0x2A05)
    let sc_handle = gatt_svc
        .service_changed_val_handle
        .expect("Service Changed handle");
    let sc_val = db.read(sc_handle, 0).expect("Read Service Changed");
    assert_eq!(sc_val, &[0x00, 0x00, 0xFF, 0xFF]); // Full range affected

    // Verify Client Supported Features (0x2B29)
    let csf_handle = gatt_svc
        .client_supported_features_val_handle
        .expect("CSF handle");
    assert_eq!(db.read(csf_handle, 0).unwrap(), &[0x00]);

    // Write Client Supported Features (e.g. Robust Caching = 0x01)
    db.write(csf_handle, &[0x01]).expect("Write CSF");
    assert_eq!(db.read(csf_handle, 0).unwrap(), &[0x01]);
}
