// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Live smoke test against a running netsimd: connects as a named device via
//! the WebSocket HCI endpoint, sends HCI Reset, and waits for the
//! Command Complete event. Requires `netsimd --ws-port 7681` running.

use simble::transport::{HciChannel, NetsimTransport, h4_type};

fn main() {
    let url = "ws://127.0.0.1:7681/v1/websocket/bt?name=simble-smoke";
    let mut transport = NetsimTransport::connect(url).expect("connect to netsimd");
    println!("connected: {url}");

    let channel = HciChannel::new();
    channel
        .send_command(&[0x03, 0x0C, 0x00])
        .expect("queue HCI Reset");

    for _ in 0..50 {
        transport.pump(&channel).expect("pump");
        if let Some(packet) = channel.poll_controller_packet() {
            println!("received H4 packet: {packet:02X?}");
            assert_eq!(packet[0], h4_type::HCI_EVENT);
            assert_eq!(packet[1], 0x0E, "expected Command Complete");
            println!("HCI Reset -> Command Complete: netsim round trip OK");
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("no event received from netsimd");
}
