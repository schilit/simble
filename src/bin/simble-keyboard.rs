// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Standalone CLI Sample App: BLE HID Keyboard Simulator.

use simble::devices::BleKeyboard;
use simble::types::Address;

fn main() {
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                Simble Virtual BLE HID Keyboard               ║");
    println!("╚══════════════════════════════════════════════════════════════╝\n");

    let addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let mut keyboard = BleKeyboard::new("Simble-Keyboard", addr);

    println!("• Device Name   : {}", keyboard.device.name);
    println!("• Address       : {}", keyboard.device.address);
    println!("• Battery Level : 100%");
    println!("• Protocol Mode : Report Mode (0x01)\n");

    let message = "Hello from Rust Simble!";
    println!("Simulating typing keystrokes for: \"{message}\"");

    let packets = keyboard.type_text(message);
    println!("• Total HID Reports Emitted: {}", packets.len());
    for (i, pdu) in packets.iter().take(6).enumerate() {
        println!("  Report [{:02}]: {:02X?}", i + 1, pdu);
    }
    if packets.len() > 6 {
        println!(
            "  ... and {} more HID notification packets.",
            packets.len() - 6
        );
    }

    println!("\nSimulation complete.");
}
