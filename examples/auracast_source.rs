// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! An Auracast broadcast source against a live netsimd: advertises a Broadcast
//! Audio Announcement, publishes the BASE on a periodic advertising train,
//! creates a BIG, and streams LC3 frames on each BIS.
//!
//! ```bash
//! netsimd --logtostderr --no-shutdown --ws-port 7681 &
//! cargo run --example auracast_source --features lc3
//! # then, from a Bumble checkout's venv:
//! .venv/bin/python -m bumble.apps.auracast receive tcp-client:127.0.0.1:6402 \
//!     --output stdout
//! ```
//!
//! Without the `lc3` feature the SDUs carry a recognizable byte pattern
//! instead of audio — enough to prove the transport, not enough for a foreign
//! receiver to decode. Build with `--features lc3` for the interop run.

use simble::device::{BigBroadcaster, BroadcastConfig, BroadcastState};
use simble::transport::{HciChannel, HciTransport, LiveTransport};
use std::str::FromStr;
use std::time::{Duration, Instant};

/// One 10 ms frame of a tone, so a listener can tell real audio from silence
/// and a decoder error from a dead stream.
#[cfg(feature = "lc3")]
fn tone(sample_index: usize, count: usize, hz: f32) -> Vec<i16> {
    (0..count)
        .map(|i| {
            let t = (sample_index + i) as f32 / 48_000.0;
            let envelope = 0.4 * (1.0 - (t * 2.0).fract());
            ((t * hz * std::f32::consts::TAU).sin() * envelope * i16::MAX as f32) as i16
        })
        .collect()
}

fn main() {
    // A controller spec, not just a URL: a `ws://…` is still passed through
    // verbatim, so every existing invocation is unchanged, and `tcp:HOST:PORT`
    // now reaches a standalone rootcanal as well.
    let url = std::env::args().nth(1).unwrap_or_else(|| {
        "ws://127.0.0.1:7681/v1/websocket/bt?name=simble-auracast-src&address=CC:1E:57:00:0B:01"
            .to_string()
    });
    let broadcast_id = std::env::args()
        .nth(2)
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0x00AB_CDEF);
    let seconds: u64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let address = simble::types::Address::from_str("CC:1E:57:00:0B:01")
        .unwrap_or(simble::types::Address::ANY);
    let mut transport = match LiveTransport::open(&url, "simble-auracast-src", address) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("connect: {e}");
            std::process::exit(1);
        }
    };
    let channel = HciChannel::new();

    // Reset, then unmask everything: the post-Reset default event mask
    // excludes LE Meta Events, so LE Create BIG Complete would never arrive.
    for (opcode, params) in [
        ([0x03u8, 0x0C], &[][..]),
        ([0x01, 0x0C], &[0xFF; 8]),
        ([0x01, 0x20], &[0xFF; 8]),
    ] {
        let mut c = vec![opcode[0], opcode[1], params.len() as u8];
        c.extend_from_slice(params);
        channel.send_command(&c).expect("queue");
    }

    let config = BroadcastConfig {
        broadcast_id,
        broadcast_name: "Simble Auracast".to_string(),
        ..Default::default()
    };
    let sdu_bytes = config.max_sdu as usize;
    let sdu_interval = Duration::from_micros(config.sdu_interval_us as u64);
    let num_bis = config.num_bis;
    println!(
        "broadcast_id 0x{:06X}, {} BIS, {} byte SDUs every {} µs",
        config.broadcast_id, num_bis, config.max_sdu, config.sdu_interval_us
    );
    let mut broadcaster = BigBroadcaster::new(config);

    let mut pending = broadcaster.start();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut next_sdu = Instant::now();
    #[cfg(feature = "lc3")]
    let mut sample_index = 0usize;
    let mut sent = 0u64;
    let mut last_state = BroadcastState::Idle;

    #[cfg(feature = "lc3")]
    let mut encoder = simble::audio::lc3::Lc3Encode::new(48_000, 10_000).expect("48 kHz / 10 ms");
    #[cfg(not(feature = "lc3"))]
    println!(
        "WARNING: built without --features lc3. The SDUs carry a byte pattern, \
         not audio: a receiver will sync and count packets but decode silence."
    );

    while Instant::now() < deadline {
        for packet in pending.drain(..) {
            channel.inject_host_packet(packet).expect("queue");
        }
        transport.pump(&channel).expect("pump");
        while let Some(packet) = channel.poll_controller_packet() {
            pending.extend(broadcaster.on_packet(&packet));
        }

        if broadcaster.state() != last_state {
            println!("state: {:?}", broadcaster.state());
            last_state = broadcaster.state();
            if let BroadcastState::Failed(status) = last_state {
                eprintln!("setup failed with HCI status 0x{status:02X}");
                std::process::exit(1);
            }
            if last_state == BroadcastState::Streaming {
                println!("BIS handles: {:04X?}", broadcaster.bis_handles());
            }
        }

        if broadcaster.is_streaming() {
            while next_sdu <= Instant::now() {
                for bis in 1..=num_bis {
                    // One BIS per channel: the two get different notes so a
                    // listener can hear that both streams are live.
                    let hz = if bis == 1 { 440.0 } else { 554.37 };
                    #[cfg(feature = "lc3")]
                    let payload = {
                        let samples = tone(sample_index, encoder.samples_per_frame(), hz);
                        encoder.encode(&samples, sdu_bytes).expect("encode")
                    };
                    #[cfg(not(feature = "lc3"))]
                    let payload = {
                        let _ = hz;
                        let mut p = vec![bis; sdu_bytes];
                        p[..2].copy_from_slice(&(sent as u16).to_le_bytes());
                        p
                    };
                    if let Some(iso) = broadcaster.send_sdu(bis, &payload) {
                        channel.inject_host_packet(iso).expect("queue");
                    }
                }
                #[cfg(feature = "lc3")]
                {
                    sample_index += 480;
                }
                sent += 1;
                next_sdu += sdu_interval;
                if sent.is_multiple_of(100) {
                    println!("sent {sent} SDUs per BIS");
                }
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    channel
        .inject_host_packet(broadcaster.terminate())
        .expect("queue");
    transport.pump(&channel).expect("pump");
    println!("done: {sent} SDUs per BIS across {num_bis} BIS");
}
