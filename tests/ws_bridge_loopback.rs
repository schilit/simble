// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! End-to-end WebSocket interop between SimBLE's own client and server halves:
//! `NetsimTransport` (the browser/netsim client) talking to `WsServerConn`
//! (the `usb-ble-ws` bridge's server end) over a real loopback TCP socket.
//!
//! The bridge normally puts a physical dongle behind the server; here a tiny
//! fake controller stands in — it answers each host command with a Command
//! Complete event — so the full path (RFC 6455 handshake, masked client
//! frames, unmasked server frames, H4 both ways) is exercised without hardware.

use simble::transport::{HciChannel, NetsimTransport, WsServerConn};
use std::net::TcpListener;
use std::thread;
use std::time::{Duration, Instant};

#[test]
fn client_and_server_bridge_hci_over_websocket() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();

    // Server: accept one client, then act as a fake controller — every host
    // command gets a Command Complete event echoing its opcode.
    let server = thread::spawn(move || {
        let (stream, _peer) = listener.accept().expect("accept");
        let (mut ws, query) = WsServerConn::accept(stream).expect("ws handshake");
        assert!(
            query.contains("name=test"),
            "query string round-trips: {query:?}"
        );

        let channel = HciChannel::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            // A closed client surfaces as an error; that's the clean exit.
            if ws.pump(&channel).is_err() {
                break;
            }
            if let Some(host_pkt) = channel.poll_host_packet() {
                // host_pkt is H4: [0x01, opcode_lo, opcode_hi, plen, ...].
                let cmd_complete = vec![0x04, 0x0E, 0x04, 0x01, host_pkt[1], host_pkt[2], 0x00];
                channel.receive_from_controller(cmd_complete).unwrap();
            }
            thread::sleep(Duration::from_millis(2));
        }
    });

    // Client: connect, send HCI Reset (opcode 0x0C03), await the event.
    let url = format!("ws://{addr}/v1/websocket/bt?name=test&address=AA:BB:CC:DD:EE:FF");
    let mut client = NetsimTransport::connect(&url).expect("client connect");
    let channel = HciChannel::new();
    channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut received = None;
    while Instant::now() < deadline && received.is_none() {
        client.pump(&channel).expect("client pump");
        received = channel.poll_controller_packet();
        thread::sleep(Duration::from_millis(2));
    }

    let event = received.expect("client should receive the Command Complete event");
    // HCI event, Command Complete (0x0E), echoing the Reset opcode 0x0C03.
    assert_eq!(event, vec![0x04, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00]);

    drop(client); // lets the server's pump see the close and exit
    server.join().unwrap();
}
