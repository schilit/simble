// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Standalone example: Simulating a BLE HID Keyboard peripheral.

use simble::devices::BleKeyboard;
use simble::types::Address;

fn main() {
    println!("=== Simble Virtual HID Keyboard ===");

    let addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let mut keyboard = BleKeyboard::new("Simble-Keyboard", addr);

    let text = "Hello from Simble!";
    println!("Typing string: \"{text}\"");

    let reports = keyboard.type_text(text);
    println!(
        "Generated {} HID input report notifications.",
        reports.len()
    );
}
