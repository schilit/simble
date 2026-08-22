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
use simble::device::{ClassicHost, RfcommHandler, SdpHandler, classic_host::spp_service_record};
use simble::transport::{HciChannel, NetsimTransport};

/// The RFCOMM server channel the SDP record advertises. Both must agree, or
/// a peer opens a DLC the device refuses.
const SPP_CHANNEL: u8 = 3;

fn main() {
    // Name and address are overridable so a second instance can join the
    // same ether: two devices at one address silently corrupt discovery.
    // netsim reads the address least-significant byte first, which is why
    // the default looks reversed relative to what it advertises.
    let name = std::env::var("SIMBLE_NAME").unwrap_or_else(|_| "simble-classic".to_string());
    let address = std::env::var("SIMBLE_ADDR").unwrap_or_else(|_| "F0:DE:C0:00:0C:01".to_string());
    let url =
        format!("ws://127.0.0.1:7681/v1/websocket/bt?name={name}&address={address}");
    let url = url.as_str();
    let mut transport = match NetsimTransport::connect(url) {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("connected to netsim as {name}");

    // Class of Device 0x240404: rendering + audio service, Audio/Video major
    // class, wearable-headset minor — what Android renders as a headset.
    let mut host = ClassicHost::new("Simble Classic", [0x04, 0x04, 0x24]);
    let mut sdp = SdpHandler::new(SdpServer::new());
    // Advertise SPP on RFCOMM channel 3 so a peer's service discovery finds
    // something real rather than an empty server.
    sdp.server_mut().service_records.insert(
        0x00010001,
        spp_service_record(0x00010001, SPP_CHANNEL, "Simble SPP"),
    );
    host.register_handler(Box::new(sdp)).expect("SDP registers");

    // RFCOMM on the advertised channel, so the SDP record is a promise the
    // device can keep: a terminal app that connects gets an echoing port.
    let (rfcomm, port) = RfcommHandler::echoing(SPP_CHANNEL);
    host.register_handler(Box::new(rfcomm))
        .expect("RFCOMM registers");

    // A greeting queued now, long before a peer exists, is the device-driven
    // half of the port: nothing prompts it, so it can only reach the peer if
    // writes survive until a DLC opens and the host drains them unasked.
    if let Ok(greeting) = std::env::var("SIMBLE_SPP_GREETING")
        && let Ok(mut port) = port.lock()
    {
        port.write(greeting.into_bytes());
    }

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
        // Report what a peer sent over the serial port. The port echoes, so
        // the reply has already been queued; this just makes it visible.
        if let Ok(mut port) = port.lock() {
            for payload in port.take_received() {
                println!(
                    "SPP received {} bytes: {:?}",
                    payload.len(),
                    String::from_utf8_lossy(&payload)
                );
            }
        }
        // Flush anything the port queued while we were not handling a packet.
        for out in host.poll() {
            let _ = channel.inject_host_packet(out);
        }
        if events > 0 && events.is_multiple_of(50) {
            println!("{events} HCI packets handled");
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}
