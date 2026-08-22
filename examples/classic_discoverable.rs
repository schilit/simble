// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Live BR/EDR peripheral against a running netsimd: brings a [`ClassicHost`]
//! up as discoverable + connectable, then pumps HCI so a real stack (the
//! Android emulator sharing the netsim ether) can inquire, page, connect,
//! open an L2CAP channel and query SDP.
//!
//! Run `netsimd --logtostderr --no-shutdown --ws-port 7681`, start an
//! emulator, then `cargo run --example classic_discoverable` and scan for
//! Bluetooth devices on the emulator.

use simble::classic::sdp::SdpServer;
use simble::device::{ClassicHost, SdpHandler, classic_host::spp_service_record};
use simble::transport::{HciChannel, NetsimTransport};

fn main() {
    let url = "ws://127.0.0.1:7681/v1/websocket/bt?name=simble-classic&address=F0:DE:C0:00:0C:01";
    let mut transport = match NetsimTransport::connect(url) {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("connected to netsim as simble-classic");

    // Class of Device 0x240404: rendering + audio service, Audio/Video major
    // class, wearable-headset minor — what Android renders as a headset.
    let mut host = ClassicHost::new("Simble Classic", [0x04, 0x04, 0x24]);
    let mut sdp = SdpHandler::new(SdpServer::new());
    // Advertise SPP on RFCOMM channel 3 so a peer's service discovery finds
    // something real rather than an empty server.
    sdp.server_mut()
        .service_records
        .insert(0x00010001, spp_service_record(0x00010001, 3, "Simble SPP"));
    host.register_handler(Box::new(sdp)).expect("SDP registers");

    let channel = HciChannel::new();
    for packet in host.start_commands() {
        channel.inject_host_packet(packet).expect("queue bring-up");
    }
    println!("bring-up queued: discoverable + connectable, SDP registered");

    let mut events = 0usize;
    loop {
        if let Err(e) = transport.pump(&channel) {
            eprintln!("transport: {e}");
            return;
        }
        while let Some(packet) = channel.poll_controller_packet() {
            events += 1;
            // Print what the controller says, so a failed bring-up (an
            // unsupported command, a refused scan enable) is visible rather
            // than silent.
            if packet.len() >= 2 && packet[0] == 0x04 {
                match packet[1] {
                    0x0E if packet.len() >= 7 => println!(
                        "  cmd complete: opcode {:#06x} status {:#04x}",
                        u16::from_le_bytes([packet[4], packet[5]]),
                        packet[6]
                    ),
                    0x0F if packet.len() >= 7 => println!(
                        "  cmd status: opcode {:#06x} status {:#04x}",
                        u16::from_le_bytes([packet[5], packet[6]]),
                        packet[3]
                    ),
                    code => println!("  event {code:#04x}"),
                }
            }
            match host.handle_packet(&packet) {
                Ok(outgoing) => {
                    for out in outgoing {
                        let _ = channel.inject_host_packet(out);
                    }
                }
                Err(e) => eprintln!("host: {e}"),
            }
            if let Some((handle, peer)) = host.connection() {
                println!("connected: handle {handle:#06x} peer {peer}");
            }
            if host.has_open_channel() {
                println!("an L2CAP channel is open (SDP reachable)");
            }
        }
        if events > 0 && events.is_multiple_of(50) {
            println!("{events} HCI packets handled");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
