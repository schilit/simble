// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Lists the USB Bluetooth dongles plugged into this machine, with every
//! selector that names each one. A caller cannot choose without a list, and
//! two dongles of the same model share a `vid:pid` — so this prints the
//! index, the `bus/address`, and the `bus.port` path for each.
//!
//! REQUIRES HARDWARE only in the sense that it prints nothing useful without
//! it; with no dongle plugged in it says so and exits 0.
//!
//! ```sh
//! cargo run --example usb_list
//! ```

use simble::transport::usb::list_bluetooth_dongles;

fn main() {
    let dongles = match list_bluetooth_dongles() {
        Ok(d) => d,
        Err(e) => {
            eprintln!("USB enumeration failed: {e}");
            std::process::exit(1);
        }
    };
    if dongles.is_empty() {
        println!("No Bluetooth-class USB dongle found (class E0/01/01).");
        println!("macOS's built-in controller is PCIe-attached and never appears here.");
        return;
    }
    println!(
        "{} Bluetooth-class USB dongle(s); pass any of these to \
         run_on(\"usb\", device: …) or SIMBLE_USB_A / SIMBLE_USB_B:",
        dongles.len()
    );
    for d in &dongles {
        println!("  {}", d.describe());
        println!(
            "      #{}            index — stable this session, not across re-plugs",
            d.index
        );
        println!(
            "      {:04x}:{:04x}     vid:pid — only works when it is the only one",
            d.vendor_id, d.product_id
        );
        println!(
            "      {}          bus/address — precise now, new address after a re-plug",
            d.address_selector()
        );
        println!(
            "      {}          bus.port — the socket, stable across re-plugs",
            d.port_selector()
        );
        match &d.serial_number {
            Some(s) => println!("      serial {s}"),
            None => println!("      (no serial number — cannot be selected by one)"),
        }
    }
}
