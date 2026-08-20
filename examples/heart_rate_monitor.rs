// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Standalone example: Simulating a BLE Heart Rate Monitor peripheral.

use simble::devices::HeartRateMonitor;
use simble::types::Address;

fn main() {
    println!("=== Simble Virtual Heart Rate Monitor ===");

    let addr = Address::from_be_bytes([0xF0, 0x11, 0x22, 0x33, 0x44, 0x55]);
    let mut hrm = HeartRateMonitor::new("Simble-HRM", addr);

    println!("Device Address: {}", hrm.device.address);

    // Simulate heart rate progression
    let heart_rates = [68, 72, 75, 80, 85, 92, 88, 79];
    for bpm in heart_rates {
        let pdu = hrm.send_heart_rate(bpm);
        println!("BPM: {bpm} | Emitted ATT Notification PDU: {pdu:02X?}");
    }
}
