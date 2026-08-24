// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! An Auracast broadcast sink against a live netsimd: scans for a Broadcast
//! Audio Announcement, syncs to the source's periodic advertising, reads the
//! BASE and BIGInfo off it, joins the BIG, and counts the SDUs that arrive
//! (decoding them when built with `--features lc3`).
//!
//! ```bash
//! netsimd --logtostderr --no-shutdown --ws-port 7681 &
//! # a foreign source:
//! .venv/bin/python -m bumble.apps.auracast transmit tcp-client:127.0.0.1:6402 \
//!     --broadcast-id 12345678
//! cargo run --example auracast_sink --features lc3
//! ```
//!
//! Exit status is the verdict: 0 if SDUs arrived, 1 otherwise.

use simble::device::{BigReceiver, ReceiverConfig, ReceiverState};
use simble::transport::{HciChannel, HciTransport, LiveTransport};
use std::str::FromStr;
use std::time::{Duration, Instant};

fn main() {
    // A controller spec, not just a URL: a `ws://…` is still passed through
    // verbatim, so every existing invocation is unchanged, and `tcp:HOST:PORT`
    // now reaches a standalone rootcanal as well.
    let url = std::env::args().nth(1).unwrap_or_else(|| {
        "ws://127.0.0.1:7681/v1/websocket/bt?name=simble-auracast-sink&address=CC:1E:57:00:0B:02"
            .to_string()
    });
    let broadcast_id = std::env::args()
        .nth(2)
        .and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok());
    let seconds: u64 = std::env::args()
        .nth(3)
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);

    let address = simble::types::Address::from_str("CC:1E:57:00:0B:02")
        .unwrap_or(simble::types::Address::ANY);
    let mut transport = match LiveTransport::open(&url, "simble-auracast-sink", address) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("connect: {e}");
            std::process::exit(1);
        }
    };
    let channel = HciChannel::new();
    for (opcode, params) in [
        ([0x03u8, 0x0C], &[][..]),
        ([0x01, 0x0C], &[0xFF; 8]),
        ([0x01, 0x20], &[0xFF; 8]),
    ] {
        let mut c = vec![opcode[0], opcode[1], params.len() as u8];
        c.extend_from_slice(params);
        channel.send_command(&c).expect("queue");
    }

    let mut receiver = BigReceiver::new(ReceiverConfig {
        broadcast_id,
        ..Default::default()
    });
    match broadcast_id {
        Some(id) => println!("looking for broadcast_id 0x{id:06X}"),
        None => println!("looking for any Auracast broadcast"),
    }

    let mut pending = receiver.start();
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut last_state = ReceiverState::Idle;
    let mut reported = 0u64;
    // One decoder per BIS: LC3 carries state between frames, so feeding two
    // streams through one decoder corrupts both. Created only once the BASE
    // says what to decode.
    #[cfg(feature = "lc3")]
    let mut decoders: std::collections::HashMap<u16, simble::audio::lc3::Lc3Stream> =
        std::collections::HashMap::new();
    #[cfg(feature = "lc3")]
    let mut codec_config: Option<(u32, u32)> = None;
    #[cfg(feature = "lc3")]
    let mut decoded_frames = 0u64;
    #[cfg(feature = "lc3")]
    let mut decode_errors = 0u64;

    while Instant::now() < deadline {
        for packet in pending.drain(..) {
            channel.inject_host_packet(packet).expect("queue");
        }
        transport.pump(&channel).expect("pump");
        while let Some(packet) = channel.poll_controller_packet() {
            pending.extend(receiver.on_packet(&packet));
        }

        if receiver.state() != last_state {
            println!("state: {:?}", receiver.state());
            last_state = receiver.state();
            match last_state {
                ReceiverState::SyncingToPeriodicAdvertising => {
                    let found = receiver.found().expect("a source was matched");
                    println!(
                        "  source {:02X?} sid {} broadcast_id 0x{:06X}",
                        found.address, found.advertising_sid, found.broadcast_id
                    );
                }
                ReceiverState::SyncingToBig => {
                    let base = receiver.base().expect("the BASE was parsed");
                    let subgroup = &base.subgroups[0];
                    println!(
                        "  BASE: presentation delay {} µs, codec {:02X?}, config {:?}",
                        base.presentation_delay,
                        subgroup.codec_id,
                        subgroup.codec_specific_configuration
                    );
                    println!(
                        "  BIS indices {:?}",
                        subgroup.bis.iter().map(|b| b.index).collect::<Vec<_>>()
                    );
                    if let Some(info) = receiver.big_info() {
                        println!(
                            "  BIGInfo: {} BIS, max_sdu {}, sdu_interval {} µs, encryption {}",
                            info.num_bis,
                            info.max_sdu.get(),
                            info.sdu_interval.get(),
                            info.encryption
                        );
                    }
                    #[cfg(feature = "lc3")]
                    {
                        let config = &subgroup.codec_specific_configuration;
                        if let (Some(frequency), Some(duration)) =
                            (config.sampling_frequency, config.frame_duration)
                        {
                            codec_config = Some((frequency.hz(), duration.us()));
                        }
                    }
                }
                ReceiverState::Receiving => {
                    println!("  BIS handles {:04X?}", receiver.bis_handles());
                    // Stop scanning: rootcanal delivers every advertisement in
                    // the simulation otherwise, and none of them matter now.
                    pending.push(receiver.stop_scanning());
                }
                ReceiverState::Failed(status) => {
                    eprintln!("sync failed with HCI status 0x{status:02X}");
                    std::process::exit(1);
                }
                _ => {}
            }
        }

        while let Some(_sdu) = receiver.poll_sdu() {
            #[cfg(feature = "lc3")]
            if let Some((rate, duration)) = codec_config {
                let decoder = decoders.entry(_sdu.handle).or_insert_with(|| {
                    simble::audio::lc3::Lc3Stream::new(rate, duration).expect("codec config")
                });
                match decoder.decode(&_sdu.payload) {
                    Ok(_) => decoded_frames += 1,
                    Err(_) => decode_errors += 1,
                }
            }
        }
        if receiver.sdu_count() >= reported + 100 {
            reported = receiver.sdu_count();
            println!("received {reported} SDUs");
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    println!("total SDUs received: {}", receiver.sdu_count());
    #[cfg(feature = "lc3")]
    println!("LC3 frames decoded: {decoded_frames} ({decode_errors} errors)");
    if receiver.sdu_count() == 0 {
        eprintln!("no audio arrived — state {:?}", receiver.state());
        std::process::exit(1);
    }
}
