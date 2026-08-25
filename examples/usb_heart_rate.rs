// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A real Heart Rate peripheral on a real USB dongle, for a real phone.
//!
//! REQUIRES HARDWARE — a USB Bluetooth dongle. macOS's built-in controller is
//! PCIe-attached and cannot be claimed.
//!
//! The difference from [`usb_hrm`](../usb_hrm.rs): that example drives raw HCI
//! and advertises a Heart Rate *UUID* with nothing behind it, so a phone
//! connects and finds an empty device. This one hosts the catalog's `hrm`
//! script through [`UsbScene`], so there is an actual GATT database — Heart
//! Rate Service 0x180D, a Heart Rate Measurement characteristic with a CCCD,
//! and a `tick` that changes the value. Subscribe in nRF Connect and the
//! number moves.
//!
//! ```sh
//! cargo run --example usb_heart_rate            # first dongle
//! cargo run --example usb_heart_rate 02.3.4     # a specific one
//! ```
//!
//! `cargo run --example usb_list` prints every selector each dongle answers to.

use simble::devices::catalog::EXAMPLES;
use simble::transport::usb::UsbScene;
use simble::transport::usb::UsbSelector;
use simble::types::Address;

/// Script-clock seconds per tick. The catalog's `hrm` recomputes its value
/// from `t`, so this is what makes the reading move rather than sit still.
const TICK_SECONDS: f64 = 0.25;

fn main() {
    let selector = match std::env::args().nth(1) {
        Some(spec) => UsbSelector::parse(&spec).unwrap_or_else(|e| {
            eprintln!("simble: {e}");
            std::process::exit(2);
        }),
        None => UsbSelector::First,
    };

    // The catalog entry, not a copy of it: the same script the MCP `example`
    // tool serves and the web pages run, so a bug here is a bug everywhere.
    let script = EXAMPLES
        .iter()
        .find(|e| e.name == "hrm")
        .expect("the catalog ships an `hrm` peripheral")
        .script;

    let mut scene = UsbScene::new(selector);
    // The dongle advertises its own public address whatever we pass here —
    // a public address lives in ROM. This names the device inside the scene;
    // the phone will see the controller's address.
    let address = Address::from_be_bytes([0xC0, 0xFF, 0xEE, 0x00, 0x00, 0x01]);
    if let Err(e) = scene.add_peripheral(address, script) {
        eprintln!("simble: {e}");
        eprintln!();
        eprintln!("Checklist:");
        eprintln!("  - Is a USB Bluetooth dongle plugged in?");
        eprintln!("  - Two of the same model? Name one: `cargo run --example usb_list`");
        eprintln!("  - Linux: usbfs permissions (udev rule, or run with sudo).");
        std::process::exit(1);
    }

    println!("Heart Rate peripheral live on {}.", scene.selector());
    println!("Scan with nRF Connect, connect, and subscribe to 0x2A37 — the value moves.");
    println!("Ctrl-C to stop.");

    // `tick` advances the clock *by* this much; it is not an absolute time.
    loop {
        scene.tick(TICK_SECONDS);
        scene.pump();
        std::thread::sleep(std::time::Duration::from_secs_f64(TICK_SECONDS));
    }
}
