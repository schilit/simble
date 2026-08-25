// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Live A2DP **source** against a *real Bluetooth speaker*, over a real USB
//! dongle: the phone's half of the sequence, with consumer kit on the other
//! end and no simulator anywhere in the path.
//!
//! [`examples/a2dp_sink`](a2dp_sink) is the mirror of this and answers a
//! foreign source over netsim. Both of those still have software on both
//! ends. This one does not: the peer is a speaker somebody bought, whose
//! firmware nobody here can read, and whose refusals are therefore evidence
//! rather than a modelling choice. Every rung it climbs is a fact about
//! simble's bytes that a simulator cannot establish, because a simulator
//! shares simble's assumptions.
//!
//! # The ladder
//!
//! Each stage is reported as it is reached, and the run's summary names the
//! highest one — a stall at AVDTP with the speaker's real capability bytes
//! in hand is a better outcome than a green run that proves nothing.
//!
//! 1. **Inquiry** — find the speaker. It must be in pairing mode.
//! 2. **Pairing** — SSP against a peer that has never met simble.
//! 3. **SDP** — read the Audio Sink record's AVDTP L2CAP PSM.
//! 4. **AVDTP** — discover its endpoints and negotiate an SBC configuration
//!    it will accept, intersecting with what it offers rather than assuming.
//! 5. **Stream** — open the media transport channel, encode PCM to SBC, send
//!    RTP-framed media, and make a noise.
//!
//! # Running it
//!
//! ```sh
//! cargo run --example usb_list                # which dongle is which
//! cargo run --example a2dp_source -- 00:11:22:33:44:55
//! cargo run --example a2dp_source             # inquire and take the first speaker
//! ```
//!
//! **Use a dual-mode dongle.** A2DP is Classic, and a controller built
//! without BR/EDR support (a Zephyr `hci_usb` nRF52840, say) cannot inquire
//! at all — the run fails at rung 1 for a reason that is not the speaker's.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `SIMBLE_HCI` | which controller (default `usb` — the only dongle present) |
//! | `SIMBLE_TIMEOUT` | seconds before a stalled stage is a failure (default 60) |
//! | `SIMBLE_STREAM_SECS` | how long to play for once streaming (default 10) |
//! | `SIMBLE_INQUIRY_SECS` | inquiry duration, in 1.28 s units (default 8) |
//! | `SIMBLE_NO_PAIR` | `1` to skip pairing, for a speaker already bonded |
//! | `SIMBLE_IO_CAPABILITY` | `noinputnooutput` (default), `displayyesno`, … |
//!
//! The default IO capability is No Input No Output, which is what makes SSP
//! pick Just Works: nobody is standing at this end to confirm a six-digit
//! number, and a speaker has no keypad either.

use std::str::FromStr;
use std::time::{Duration, Instant};

use simble::device::a2dp_source_runner::{A2dpSourceRunner, SAMPLE_RATE, SourceRung};
use simble::device::classic_host::{inquiry_mode, io_capability, scan_enable};
use simble::transport::{HciChannel, HciTransport, LiveTransport};
use simble::types::Address;

struct Note(f32, u32);

/// A short, unmistakably deliberate phrase — a major arpeggio up, an octave
/// above, then down. Chosen over a single steady tone because a steady tone
/// is what a broken stream *also* sounds like when a buffer repeats, while a
/// melody that plays in order and in tune says the frames arrived in order
/// and decoded correctly. The pitches are A4 and the major triad above it.
const MELODY: &[Note] = &[
    Note(440.00, 260), // A4
    Note(554.37, 260), // C#5
    Note(659.25, 260), // E5
    Note(880.00, 420), // A5
    Note(659.25, 260), // E5
    Note(554.37, 260), // C#5
    Note(440.00, 520), // A4
    Note(0.0, 360),    // rest, so a loop is audibly a loop
];

/// Renders [`MELODY`] to interleaved stereo PCM at [`SAMPLE_RATE`].
///
/// Each note gets a short linear fade in and out. Without it every note
/// boundary is a step discontinuity, which SBC faithfully encodes as a click
/// — and a run full of clicks is indistinguishable from a run with real
/// framing bugs, which is exactly the confusion this example exists to avoid.
fn render_melody() -> Vec<i16> {
    let mut pcm = Vec::new();
    for Note(frequency, milliseconds) in MELODY {
        let samples = (SAMPLE_RATE as u64 * *milliseconds as u64 / 1000) as usize;
        // 5 ms of fade at each end, or a quarter of a very short note.
        let fade = (SAMPLE_RATE as usize / 200).min(samples / 4).max(1);
        for n in 0..samples {
            let amplitude = if *frequency == 0.0 {
                0.0
            } else {
                let envelope = if n < fade {
                    n as f32 / fade as f32
                } else if n + fade >= samples {
                    (samples - n) as f32 / fade as f32
                } else {
                    1.0
                };
                // 0.35 of full scale: loud enough to hear across a room,
                // quiet enough that nothing clips once SBC has rounded it.
                let phase = std::f32::consts::TAU * *frequency * n as f32 / SAMPLE_RATE as f32;
                phase.sin() * envelope * 0.35
            };
            let sample = (amplitude * i16::MAX as f32) as i16;
            pcm.push(sample);
            pcm.push(sample);
        }
    }
    pcm
}
fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decodes an SBC codec-specific information element into the words a
/// person reads, so the report says "44.1 kHz joint stereo" and not only
/// four bytes. The bit layouts are A2DP spec §4.3.2.
fn describe_sbc(data: &[u8]) -> String {
    let Some(&[b0, b1, min, max]) = data.get(..4) else {
        return format!("(not a 4-byte SBC element: {})", hex(data));
    };
    let mut parts = Vec::new();
    let mut flags = |value: u8, names: [(u8, &str); 4]| {
        let set: Vec<&str> = names
            .iter()
            .filter(|(bit, _)| value & bit != 0)
            .map(|(_, name)| *name)
            .collect();
        parts.push(if set.is_empty() {
            "none".to_string()
        } else {
            set.join("/")
        });
    };
    flags(
        b0 >> 4,
        [(0x8, "16k"), (0x4, "32k"), (0x2, "44.1k"), (0x1, "48k")],
    );
    flags(
        b0 & 0x0F,
        [
            (0x8, "mono"),
            (0x4, "dual"),
            (0x2, "stereo"),
            (0x1, "joint"),
        ],
    );
    flags(
        b1 >> 4,
        [(0x8, "blk4"), (0x4, "blk8"), (0x2, "blk12"), (0x1, "blk16")],
    );
    let subbands = match (b1 >> 2) & 0x03 {
        0x2 => "4sb",
        0x1 => "8sb",
        0x3 => "4sb/8sb",
        _ => "no subbands",
    };
    let allocation = match b1 & 0x03 {
        0x2 => "SNR",
        0x1 => "loudness",
        0x3 => "SNR/loudness",
        _ => "no allocation",
    };
    format!(
        "freq {}, channels {}, blocks {}, {subbands}, {allocation}, bitpool {min}..{max}",
        parts[0], parts[1], parts[2],
    )
}

/// Names an AVDTP service category (AVDTP §8.21).
fn service_category_name(category: u8) -> &'static str {
    match category {
        0x01 => "Media Transport",
        0x02 => "Reporting",
        0x03 => "Recovery",
        0x04 => "Content Protection",
        0x05 => "Header Compression",
        0x06 => "Multiplexing",
        0x07 => "Media Codec",
        0x08 => "Delay Reporting",
        _ => "unknown",
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn parse_io_capability(text: &str) -> Result<u8, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "displayonly" => Ok(io_capability::DISPLAY_ONLY),
        "displayyesno" => Ok(io_capability::DISPLAY_YES_NO),
        "keyboardonly" => Ok(io_capability::KEYBOARD_ONLY),
        "none" | "noinputnooutput" => Ok(io_capability::NO_INPUT_NO_OUTPUT),
        other => Err(format!("unknown SIMBLE_IO_CAPABILITY: {other}")),
    }
}

// ---------------------------------------------------------------------------
// The run

fn main() {
    let mut args = std::env::args().skip(1);
    let target = match args.next() {
        Some(text) => match Address::from_str(&text) {
            Ok(address) => Some(address),
            Err(e) => {
                eprintln!("{e}");
                eprintln!(
                    "usage: a2dp_source [<speaker-address>]\n\
                     (omit the address to inquire and take the first Audio/Video device)"
                );
                std::process::exit(2);
            }
        },
        None => None,
    };
    let io_capability = match std::env::var("SIMBLE_IO_CAPABILITY") {
        Ok(text) => match parse_io_capability(&text) {
            Ok(capability) => capability,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        // Just Works: nobody is standing here to compare a number.
        Err(_) => io_capability::NO_INPUT_NO_OUTPUT,
    };
    let timeout = Duration::from_secs(env_u64("SIMBLE_TIMEOUT", 60));
    let stream_secs = env_u64("SIMBLE_STREAM_SECS", 10);
    let inquiry_length = env_u64("SIMBLE_INQUIRY_SECS", 8).clamp(1, 48) as u8;
    let pair = !env_flag("SIMBLE_NO_PAIR");

    // A dongle by default: this example's entire reason to exist is the peer
    // being real, and neither simulated controller has a speaker on it.
    let spec = std::env::var("SIMBLE_HCI").unwrap_or_else(|_| "usb".to_string());
    let mut transport = match LiveTransport::open(&spec, "simble-a2dp-source", Address::ANY) {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("cannot reach a controller ({spec}): {e}");
            eprintln!("try `cargo run --example usb_list` to see what is plugged in");
            std::process::exit(1);
        }
    };
    println!("connected to {} via {spec}", transport.describe());
    match target {
        Some(address) => println!("target: {address}"),
        None => println!("target: the first Audio/Video device the inquiry finds"),
    }

    // The whole climb lives in the library now (the wasm WebA2dpSource walks
    // the same one); this example is its transport, its clock, its melody
    // and its report.
    let started = Instant::now();
    let now_ms = || started.elapsed().as_secs_f64() * 1000.0;
    let mut run = A2dpSourceRunner::new(target, io_capability, pair);
    let melody = render_melody();
    let channel = HciChannel::new();
    for packet in run
        .host()
        .start_commands()
        .into_iter()
        // A source is neither discoverable nor connectable: it does the
        // finding. `start_commands` ends by enabling both scans, so this
        // must follow it.
        .chain(run.host().set_scan_enable(scan_enable::NONE))
        // Extended results carry the peer's name in the EIR, which saves a
        // Remote Name Request — and a speaker's name is how a person
        // confirms the run found the right box.
        .chain(run.host().set_inquiry_mode(inquiry_mode::WITH_EXTENDED))
    {
        channel.inject_host_packet(packet).expect("queue bring-up");
    }

    let mut failure: Option<String> = None;
    let deadline = Instant::now() + timeout;

    while run.rung() != SourceRung::Done && failure.is_none() {
        if let Err(e) = transport.pump(&channel) {
            failure = Some(format!("transport: {e}"));
            break;
        }
        while let Some(packet) = channel.poll_controller_packet() {
            match run.handle_packet(&packet) {
                Ok(outgoing) => {
                    for out in outgoing {
                        let _ = channel.inject_host_packet(out);
                    }
                }
                Err(e) => eprintln!("host: {e}"),
            }
        }
        match run.step(now_ms(), inquiry_length) {
            Ok(packets) => {
                for packet in packets {
                    let _ = channel.inject_host_packet(packet);
                }
            }
            Err(e) => failure = Some(e),
        }
        for line in run.take_log() {
            println!("  {line}");
        }
        // The melody loops for as long as the run streams: keep half a
        // second queued and let the runner meter it out at real time.
        if run.pending_samples() < SAMPLE_RATE as usize {
            run.queue_pcm(&melody);
        }
        run.feed(now_ms());
        // Profiles speak unprompted: the SDP query, every AVDTP signalling
        // PDU and every media packet leave this way rather than from `step`.
        for packet in run.poll() {
            let _ = channel.inject_host_packet(packet);
        }
        if let Some(ms) = run.streaming_ms(now_ms())
            && ms >= stream_secs as f64 * 1000.0
        {
            run.finish();
        }
        if Instant::now() > deadline && failure.is_none() {
            failure = Some(format!(
                "timed out after {timeout:?} at stage: {}",
                run.rung().label()
            ));
        }
        // 5 ms rather than the 20 ms an SPP example can afford: an SBC frame
        // at 44.1 kHz is about 3 ms of audio, so a slower loop cannot keep a
        // stream fed.
        std::thread::sleep(Duration::from_millis(5));
    }
    for line in run.take_log() {
        println!("  {line}");
    }

    // --- the verdict ------------------------------------------------------
    println!();
    println!("--- how far up the ladder ---");
    println!("highest stage reached: {}", run.highest().label());
    let packets_sent = run.packets_sent();
    if let Some(psm) = run.avdtp_psm() {
        println!("the speaker's AVDTP PSM: {psm:#06x}");
    }
    if let Some(parameters) = run.negotiated() {
        println!("negotiated SBC: {parameters}");
    }
    if !run.capabilities().is_empty() {
        println!("the speaker's AVDTP capabilities, as bytes:");
        for (seid, category, data) in run.capabilities() {
            println!(
                "  SEID {seid} category {category:#04x} ({}): {}",
                service_category_name(*category),
                hex(data)
            );
            // Category 0x07 is Media Codec: media type, codec type, then the
            // codec element. Codec type 0x00 is SBC.
            if *category == 0x07 && data.get(1) == Some(&0x00) {
                println!("    SBC: {}", describe_sbc(&data[2..]));
            }
        }
    }
    println!("RTP media packets written: {packets_sent}");
    if run.highest() >= SourceRung::Streaming {
        println!(
            "\nThe stream reached STREAMING and {packets_sent} media packets went out. \
             Whether sound came out of the speaker is a question only a person in the \
             room can answer — this program cannot hear."
        );
    }
    match failure {
        Some(e) => {
            println!("\nFAIL at {}: {e}", run.rung().label());
            std::process::exit(1);
        }
        None => {
            println!("\nok");
        }
    }
}
