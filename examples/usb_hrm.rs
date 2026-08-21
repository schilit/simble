// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Real-hardware demo against a physical USB Bluetooth dongle: brings the
//! controller up with raw HCI, advertises as "Simble HRM" with the Heart Rate
//! service UUID, then prints every HCI event so a phone running nRF Connect
//! can scan, connect, and have the whole session visible on the console.
//!
//! REQUIRES HARDWARE — this example cannot run (or be tested) without a USB
//! Bluetooth dongle plugged in. Note that macOS's built-in Broadcom
//! controller is not USB-accessible; a separate dongle is required. Run with
//! no arguments to probe for the first Bluetooth-class device, or pass an
//! explicit `vid:pid` (hex, e.g. `0a12:0001`):
//!
//! ```sh
//! cargo run --example usb_hrm [-- vid:pid]
//! ```

use simble::transport::{HciChannel, UsbTransport, h4_type, usb::parse_vid_pid};

fn cmd(channel: &HciChannel, opcode: [u8; 2], params: &[u8]) {
    let mut c = vec![opcode[0], opcode[1], params.len() as u8];
    c.extend_from_slice(params);
    channel.send_command(&c).expect("queue command");
}

/// Minimal decode of the events the demo produces, so a phone session is
/// legible on the console; everything else is printed raw.
fn describe_event(p: &[u8]) -> String {
    match p[1] {
        0x05 if p.len() >= 7 => format!(
            "Disconnection Complete (handle {:#06x}, reason {:#04x})",
            u16::from_le_bytes([p[4], p[5]]),
            p[6]
        ),
        0x0E if p.len() >= 7 => format!(
            "Command Complete (opcode {:#06x}, status {:#04x})",
            u16::from_le_bytes([p[4], p[5]]),
            p[6]
        ),
        0x0F if p.len() >= 7 => format!(
            "Command Status (opcode {:#06x}, status {:#04x})",
            u16::from_le_bytes([p[5], p[6]]),
            p[3]
        ),
        0x3E => match p[3] {
            // LE Connection Complete / LE Enhanced Connection Complete both
            // carry status, handle, then the peer address at offset 8.
            0x01 | 0x0A if p.len() >= 14 => format!(
                "LE Connection Complete (status {:#04x}, handle {:#06x}, peer {})",
                p[4],
                u16::from_le_bytes([p[5], p[6]]),
                bd_addr(&p[8..14])
            ),
            0x03 => "LE Connection Update Complete".to_string(),
            0x04 => "LE Read Remote Features Complete".to_string(),
            0x05 => "LE Long Term Key Request".to_string(),
            sub => format!("LE Meta Event (subevent {sub:#04x})"),
        },
        0x08 => "Encryption Change".to_string(),
        0x13 => "Number Of Completed Packets".to_string(),
        code => format!("event {code:#04x}"),
    }
}

/// BD_ADDR bytes are transmitted little-endian (Core Spec Vol 4, Part E);
/// print most-significant octet first, the way addresses are written.
fn bd_addr(le_bytes: &[u8]) -> String {
    le_bytes
        .iter()
        .rev()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

fn main() {
    let mut transport = match std::env::args().nth(1) {
        Some(spec) => {
            let (vid, pid) = parse_vid_pid(&spec).expect("vid:pid argument");
            UsbTransport::open(vid, pid)
        }
        None => UsbTransport::open_first(),
    }
    .unwrap_or_else(|e| {
        eprintln!("Failed to open a USB Bluetooth dongle: {e}");
        eprintln!();
        eprintln!("Checklist:");
        eprintln!("  - Is a USB Bluetooth dongle plugged in? (macOS's built-in");
        eprintln!("    controller is PCIe-attached and NOT usable here.)");
        eprintln!("  - macOS: the dongle must not be claimed by the OS Bluetooth stack.");
        eprintln!("  - Linux: usbfs permissions are needed (udev rule, or run with sudo).");
        eprintln!("  - Vendor-specific dongles may need explicit selection: pass vid:pid.");
        std::process::exit(1);
    });
    let channel = HciChannel::new();

    cmd(&channel, [0x03, 0x0C], &[]); // Reset
    // The post-Reset default event mask excludes LE Meta Events (Core Spec
    // Vol 4 Part E 7.3.1, bit 61) - unmask before anything LE can be seen.
    cmd(&channel, [0x01, 0x0C], &[0xFF; 8]); // Set Event Mask
    cmd(&channel, [0x01, 0x20], &[0xFF; 8]); // LE Set Event Mask
    cmd(&channel, [0x09, 0x10], &[]); // Read BD_ADDR

    // LE Set Advertising Parameters: interval 100ms both bounds, ADV_IND,
    // public own address, all channels, no filter.
    cmd(
        &channel,
        [0x06, 0x20],
        &[
            0xA0, 0x00, 0xA0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
            0x00,
        ],
    );
    // LE Set Advertising Data: Flags (LE General Discoverable, no BR/EDR),
    // Complete List of 16-bit Service UUIDs (0x180D Heart Rate), Complete
    // Local Name "Simble HRM".
    let mut ad = vec![0x02, 0x01, 0x06];
    ad.extend_from_slice(&[0x03, 0x03, 0x0D, 0x18]);
    ad.push(1 + b"Simble HRM".len() as u8);
    ad.push(0x09);
    ad.extend_from_slice(b"Simble HRM");
    let mut ad_param = vec![ad.len() as u8];
    ad_param.extend_from_slice(&ad);
    ad_param.resize(32, 0x00);
    cmd(&channel, [0x08, 0x20], &ad_param);
    cmd(&channel, [0x0A, 0x20], &[0x01]); // LE Set Advertising Enable

    println!("Advertising as \"Simble HRM\" - scan with nRF Connect. Ctrl-C to stop.");
    loop {
        transport.pump(&channel).expect("pump");
        while let Some(p) = channel.poll_controller_packet() {
            match p[0] {
                h4_type::HCI_EVENT => {
                    // Read BD_ADDR's Command Complete carries the address.
                    if p[1] == 0x0E && p.len() >= 13 && p[4..6] == [0x09, 0x10] {
                        println!("controller BD_ADDR: {}", bd_addr(&p[7..13]));
                    }
                    println!("{}: {:02X?}", describe_event(&p), p);
                }
                h4_type::HCI_ACL_DATA => println!("ACL data: {p:02X?}"),
                _ => println!("packet: {p:02X?}"),
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
