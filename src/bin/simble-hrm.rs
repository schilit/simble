// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Standalone CLI Sample App: BLE Heart Rate Monitor Simulator.

use simble::devices::HeartRateMonitor;
use simble::profiles::GenericAttributeProfileService;
use simble::types::Address;

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║             Simble Virtual Heart Rate Monitor (HRM)          ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let addr = Address::from_be_bytes([0xF0, 0xDE, 0xF1, 0x22, 0x33, 0x44]);
    let mut hrm = HeartRateMonitor::new("Simble-HRM", addr);

    let db_hash = GenericAttributeProfileService::compute_database_hash(&hrm.device.gatt_db);
    println!("• Device Name   : {}", hrm.device.name);
    println!("• Address       : {}", hrm.device.address);
    println!("• GATT DB Hash  : {db_hash:02X?}");
    println!(
        "• Attributes    : {}\n",
        hrm.device.gatt_db.attributes.len()
    );

    println!("Simulating Heart Rate telemetry events:");
    let bpm_sequence = [65, 68, 72, 75, 80, 88, 95, 102, 98, 85, 76, 70];
    for (i, &bpm) in bpm_sequence.iter().enumerate() {
        let pdu = hrm.send_heart_rate(bpm);
        println!(
            "  [{:02}] BPM: {:3} bpm  |  ATT Notification (Handle 0x{:04X}): {:02X?}",
            i + 1,
            bpm,
            hrm.hrs.measurement_val_handle,
            pdu
        );
    }

    println!("\nSimulation complete.");
}
