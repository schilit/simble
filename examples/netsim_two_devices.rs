// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Live two-device demo against a running netsimd: "adv" advertises, "scan"
//! scans, and the advertisement crosses netsim's simulated PHY between two
//! independently connected Simble devices. Requires
//! `netsimd --no-shutdown --ws-port 7681` running.

use simble::transport::{HciChannel, NetsimTransport, h4_type};

fn cmd(channel: &HciChannel, opcode: [u8; 2], params: &[u8]) {
    let mut c = vec![opcode[0], opcode[1], params.len() as u8];
    c.extend_from_slice(params);
    channel.send_command(&c).expect("queue command");
}

fn drain(t: &mut NetsimTransport<std::net::TcpStream>, ch: &HciChannel) -> Vec<Vec<u8>> {
    t.pump(ch).expect("pump");
    let mut out = Vec::new();
    while let Some(p) = ch.poll_controller_packet() {
        out.push(p);
    }
    out
}

fn main() {
    let mut adv_t = NetsimTransport::connect(
        "ws://127.0.0.1:7681/v1/websocket/bt?name=simble-adv&address=11:22:33:44:55:01",
    )
    .expect("connect adv");
    let mut scan_t = NetsimTransport::connect(
        "ws://127.0.0.1:7681/v1/websocket/bt?name=simble-scan&address=11:22:33:44:55:02",
    )
    .expect("connect scan");
    let adv = HciChannel::new();
    let scan = HciChannel::new();

    cmd(&adv, [0x03, 0x0C], &[]); // Reset
    cmd(&scan, [0x03, 0x0C], &[]);
    // The post-Reset default event mask excludes LE Meta Events (Core Spec
    // Vol 4 Part E 7.3.1, bit 61) - unmask on both controllers first.
    for ch in [&adv, &scan] {
        cmd(ch, [0x01, 0x0C], &[0xFF; 8]); // Set Event Mask
        cmd(ch, [0x01, 0x20], &[0xFF; 8]); // LE Set Event Mask
    }

    // Advertiser: params (ADV_IND, public), data ("SimbleAdv" complete name), enable.
    cmd(
        &adv,
        [0x06, 0x20],
        &[
            0x00, 0x08, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x07,
            0x00,
        ],
    );
    let mut ad = vec![0x0A, 0x09]; // len 10, Complete Local Name
    ad.extend_from_slice(b"SimbleAdv");
    let mut ad_param = vec![(ad.len()) as u8];
    ad_param.extend_from_slice(&ad);
    ad_param.resize(32, 0x00);
    cmd(&adv, [0x08, 0x20], &ad_param);
    cmd(&adv, [0x0A, 0x20], &[0x01]);

    // Scanner: passive scan, enable with no dedup.
    cmd(
        &scan,
        [0x0B, 0x20],
        &[0x00, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00],
    );
    cmd(&scan, [0x0C, 0x20], &[0x01, 0x00]);

    for _ in 0..100 {
        drain(&mut adv_t, &adv);
        for p in drain(&mut scan_t, &scan) {
            // LE Meta Event (0x3E), subevent LE Advertising Report (0x02)
            if p[0] == h4_type::HCI_EVENT && p[1] == 0x3E && p[3] == 0x02 {
                let name_visible = p.windows(9).any(|w| w == b"SimbleAdv");
                println!("scanner saw advertising report: {p:02X?}");
                println!("contains advertiser name: {name_visible}");
                if name_visible {
                    println!(
                        "TWO-DEVICE ROUND TRIP OK: simble-scan sees simble-adv through netsim"
                    );
                    return;
                }
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("scanner never saw the advertiser");
}
