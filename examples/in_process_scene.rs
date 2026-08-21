// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Several Simble devices in ONE process, talking to each other with **no
//! netsim, no Rootcanal, and no radio** — through the in-process
//! [`Link`](simble::controller::sim::Link) (tier 1 of Simble's controller
//! ladder). A scanner discovers three advertisers, then connects to one and
//! exchanges a byte of ATT-shaped data. The same code runs unchanged in a
//! browser page (wasm), so a single page can host a whole scene.

use simble::controller::sim::Link;
use simble::transport::HciChannel;
use simble::types::Address;

/// Queue an HCI command (opcode + parameters) on a device's host channel.
fn cmd(ch: &HciChannel, opcode: [u8; 2], params: &[u8]) {
    let mut c = vec![opcode[0], opcode[1], params.len() as u8];
    c.extend_from_slice(params);
    ch.send_command(&c).unwrap();
}

/// Build advertising data with Flags + Complete Local Name, and enable it.
fn advertise_as(ch: &HciChannel, name: &str) {
    let mut data = vec![0x02, 0x01, 0x06]; // Flags: LE General Discoverable
    data.push(name.len() as u8 + 1);
    data.push(0x09); // Complete Local Name
    data.extend_from_slice(name.as_bytes());
    let mut params = vec![data.len() as u8];
    params.extend_from_slice(&data);
    cmd(ch, [0x08, 0x20], &params); // LE Set Advertising Data
    cmd(ch, [0x0A, 0x20], &[0x01]); // LE Set Advertising Enable
}

/// Pull the Complete Local Name out of an LE Advertising Report event.
fn name_from_report(pkt: &[u8]) -> Option<String> {
    // 04 3E len | 02 num evt addr_type | addr(6) | data_len data… rssi
    let data_len = *pkt.get(13)? as usize;
    let data = pkt.get(14..14 + data_len)?;
    let mut i = 0;
    while i + 1 < data.len() {
        let len = data[i] as usize;
        if len == 0 || i + 1 + len > data.len() {
            break;
        }
        if data[i + 1] == 0x09 {
            return Some(String::from_utf8_lossy(&data[i + 2..i + 1 + len]).into_owned());
        }
        i += 1 + len;
    }
    None
}

fn main() {
    let mut link = Link::new();

    // One scanner and three advertisers — all in this process.
    let scanner = link.add_device("AA:BB:CC:00:00:FF".parse::<Address>().unwrap());
    let thermo = link.add_device("AA:BB:CC:00:00:01".parse::<Address>().unwrap());
    let hr = link.add_device("AA:BB:CC:00:00:02".parse::<Address>().unwrap());
    let bulb = link.add_device("AA:BB:CC:00:00:03".parse::<Address>().unwrap());

    advertise_as(&thermo, "Thermometer");
    advertise_as(&hr, "Heart Rate");
    advertise_as(&bulb, "Light Bulb");
    cmd(&scanner, [0x0C, 0x20], &[0x01, 0x00]); // LE Set Scan Enable

    link.tick(); // route advertising across the shared medium

    println!("Scene has {} devices. Scanner sees:", link.device_count());
    while let Some(pkt) = scanner.poll_controller_packet() {
        if let Some(name) = name_from_report(&pkt) {
            let addr = &pkt[7..13]; // little-endian on the wire
            println!(
                "  {:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}  {name}",
                addr[5], addr[4], addr[3], addr[2], addr[1], addr[0]
            );
        }
    }

    // Connect the scanner to the heart-rate monitor and send it a byte.
    let mut connect = vec![0x10, 0x00, 0x10, 0x00, 0x00, 0x00]; // scan params + filter + addr type
    let mut peer = hr_address_le();
    connect.append(&mut peer);
    cmd(&scanner, [0x0D, 0x20], &connect); // LE Create Connection
    link.tick();

    // Drain the scanner's events and pick out the LE Connection Complete (the
    // queue may also hold advertising reports from the still-on-air devices).
    let mut handle = None;
    while let Some(p) = scanner.poll_controller_packet() {
        if p.len() >= 7 && p[0] == 0x04 && p[1] == 0x3E && p[3] == 0x01 {
            handle = Some(u16::from_le_bytes([p[5], p[6]]));
        }
    }
    match handle {
        Some(h) => {
            println!("\nConnected to Heart Rate on handle 0x{h:04X}. Sending a byte…");
            // One-byte ACL payload: handle+flags(2), length(2), payload(1).
            let acl = [h as u8, (h >> 8) as u8, 0x01, 0x00, 0x42];
            scanner.send_acl_data(&acl).unwrap();
            link.tick();
            if hr.poll_controller_packet().is_some() {
                println!("Heart Rate received it. All in one process, no netsim.");
            }
        }
        None => println!("\n(no connection — advertiser was not on air)"),
    }
}

/// The heart-rate monitor's address in little-endian wire order.
fn hr_address_le() -> Vec<u8> {
    let mut b = "AA:BB:CC:00:00:02"
        .parse::<Address>()
        .unwrap()
        .to_be_bytes();
    b.reverse();
    b.to_vec()
}
