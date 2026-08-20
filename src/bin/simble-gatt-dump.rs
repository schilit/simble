// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Standalone CLI Sample App: Dumps GATT database structures of virtual devices.

use simble::devices::{BleKeyboard, BleMouse, EddystoneUidBeacon, HeartRateMonitor, IBeacon};
use simble::profiles::GenericAttributeProfileService;
use simble::types::{Address, Uuid};

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║             Simble GATT Database Inspector & Hash Dump       ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let addr = Address::from_be_bytes([0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC]);

    let devices: Vec<(&str, simble::gatt::GattDatabase)> = vec![
        (
            "Heart Rate Monitor",
            HeartRateMonitor::new("HRM", addr).device.gatt_db,
        ),
        ("BLE Keyboard", BleKeyboard::new("KBD", addr).device.gatt_db),
        ("BLE Mouse", BleMouse::new("Mouse", addr).device.gatt_db),
        (
            "iBeacon",
            IBeacon::new("iBeacon", addr, Uuid::Uuid128([0u8; 16]), 1, 1, -59)
                .device
                .gatt_db,
        ),
        (
            "Eddystone Beacon",
            EddystoneUidBeacon::new("Eddystone", addr, &[0u8; 10], &[0u8; 6], -20)
                .device
                .gatt_db,
        ),
    ];

    for (name, db) in devices {
        let hash = GenericAttributeProfileService::compute_database_hash(&db);
        println!("┌─────────────────────────────────────────────────────────────┐");
        println!("│ Device: {:<20}  Hash: {:02X?} │", name, &hash[..8]);
        println!("├────────┬────────────────────────────────────────────┬────────┤");
        println!("│ Handle │ UUID                                       │ Length │");
        println!("├────────┼────────────────────────────────────────────┼────────┤");
        for attr in db.attributes.values() {
            println!(
                "│ 0x{:04X} │ {:<42} │ {:4} B │",
                attr.handle,
                attr.uuid.to_string(),
                attr.value.len()
            );
        }
        println!("└────────┴────────────────────────────────────────────┴────────┘\n");
    }
}
