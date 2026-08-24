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
//! no arguments to take the first Bluetooth-class device, or name one in any
//! of [`UsbSelector`]'s forms — `cargo run --example usb_list` prints every
//! name each plugged-in dongle answers to:
//!
//! ```sh
//! cargo run --example usb_hrm                 # the first one found
//! cargo run --example usb_hrm -- "#1"         # by index
//! cargo run --example usb_hrm -- 0a12:0001    # by vid:pid, if unique
//! cargo run --example usb_hrm -- 02/4         # by bus/address
//! cargo run --example usb_hrm -- 02.3.4       # by the socket it is in
//! ```

use simble::device::host::{EVENT_MASK_ALL, LE_EVENT_MASK_CORE_4_0};
use simble::transport::usb::UsbSelector;
use simble::transport::{HciChannel, UsbTransport, h4_type};

/// Builds one HCI command packet.
///
/// The packet is *queued*, not sent. A real controller grants the host a
/// command budget and silently discards anything past it — this dongle
/// answered `Reset` and dropped the six commands written behind it. Honouring
/// that budget is the transport's job now
/// ([`CommandCredits`](simble::transport::CommandCredits)), so this example
/// queues everything at once and lets `pump` release it.
fn cmd(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
    let mut c = vec![opcode[0], opcode[1], params.len() as u8];
    c.extend_from_slice(params);
    c
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
    let selector = match std::env::args().nth(1) {
        Some(spec) => UsbSelector::parse(&spec).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(2);
        }),
        None => UsbSelector::First,
    };
    let mut transport = UsbTransport::open_selected(&selector).unwrap_or_else(|e| {
        eprintln!("Failed to open a USB Bluetooth dongle: {e}");
        eprintln!();
        eprintln!("Checklist:");
        eprintln!("  - Is a USB Bluetooth dongle plugged in? (macOS's built-in");
        eprintln!("    controller is PCIe-attached and NOT usable here.)");
        eprintln!("  - macOS: the dongle must not be claimed by the OS Bluetooth stack.");
        eprintln!("  - Linux: usbfs permissions are needed (udev rule, or run with sudo).");
        eprintln!("  - With two dongles of one model plugged in, a vid:pid names both and");
        eprintln!("    is refused rather than guessed at. `cargo run --example usb_list`");
        eprintln!("    prints a name that reaches exactly one of them.");
        std::process::exit(1);
    });
    let channel = HciChannel::new();

    // The advertising data payload: Flags (LE General Discoverable, no
    // BR/EDR), Complete List of 16-bit Service UUIDs (0x180D Heart Rate),
    // Complete Local Name "Simble HRM".
    let mut ad = vec![0x02, 0x01, 0x06];
    ad.extend_from_slice(&[0x03, 0x03, 0x0D, 0x18]);
    ad.push(1 + b"Simble HRM".len() as u8);
    ad.push(0x09);
    ad.extend_from_slice(b"Simble HRM");
    // LE Set Advertising Data's parameter is fixed-length: a length byte then
    // 31 bytes of data, zero-padded.
    let mut ad_param = vec![ad.len() as u8];
    ad_param.extend_from_slice(&ad);
    ad_param.resize(32, 0x00);

    let pending: Vec<Vec<u8>> = vec![
        cmd([0x03, 0x0C], &[]), // Reset
        // The post-Reset default event mask excludes LE Meta Events (Core
        // Spec Vol 4 Part E 7.3.1, bit 61) - unmask before anything LE can be
        // seen.
        //
        // Both masks come from the library and neither is 0xFF x8: bits 62-63
        // of Event_Mask are reserved, and a 4.0 controller's LE_Event_Mask
        // stops at bit 4. Setting a bit the controller does not define gets
        // the whole command rejected with 0x12, Invalid HCI Command
        // Parameters - which nothing downstream sees, so the mask simply
        // never applies and no LE Meta Event ever arrives. See
        // `host::EVENT_MASK_ALL` and `host::LE_EVENT_MASK_CORE_4_0`.
        cmd([0x01, 0x0C], &EVENT_MASK_ALL), // Set Event Mask
        cmd([0x01, 0x20], &LE_EVENT_MASK_CORE_4_0), // LE Set Event Mask
        cmd([0x09, 0x10], &[]),             // Read BD_ADDR
        // LE Set Advertising Parameters: interval 100ms both bounds, ADV_IND,
        // public own address, all channels, no filter.
        cmd(
            [0x06, 0x20],
            &[
                0xA0, 0x00, 0xA0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
                0x00,
            ],
        ),
        cmd([0x08, 0x20], &ad_param), // LE Set Advertising Data
        cmd([0x0A, 0x20], &[0x01]),   // LE Set Advertising Enable
    ];

    // All seven go into the channel at once and the transport paces them.
    // Writing all seven to the dongle instead would have it answer the first
    // and discard the other six, with no error and nothing in any log.
    for command in pending {
        channel.send_command(&command).expect("queue command");
    }

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
