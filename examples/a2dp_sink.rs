// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Live A2DP **sink** against a running netsimd: a discoverable Bluetooth
//! speaker that a foreign stack can find, page, configure and stream to.
//!
//! Two simble endpoints agree with each other by construction. This one
//! answers a *foreign* source — `tests/interop/a2dp_peer.py` runs Bumble's
//! A2DP source against it — so every byte asserted here is a byte a stack
//! that has never seen simble's code produced: Bumble's AVDTP Discover, its
//! Get_All_Capabilities, its Set_Configuration naming an SBC operating point
//! it chose, its Open, its second L2CAP channel on PSM 0x0019, its Start,
//! and its RTP/SBC media.
//!
//! The verdict is this program's exit status, and the checks are stated as
//! environment variables so the Python side decides what counts:
//!
//! * `A2DP_EXPECT_FRAMES` — how many whole SBC frames must decode (default 20).
//! * `A2DP_TIMEOUT_SECS` — how long to wait (default 30).
//! * `SIMBLE_NAME`, `SIMBLE_ADDR` — this device's identity on the air.
//! * `SIMBLE_BTSNOOP` — write a btsnoop capture here.
//!
//! ```bash
//! netsimd --logtostderr --no-shutdown --ws-port 7681
//! cargo build --example a2dp_sink
//! .venv/bin/python tests/interop/a2dp_peer.py
//! ```
//!
//! Decoding is the point of the frame count: the SBC decoder is verified
//! against bluez's `libsbc` in both directions, so a frame that decodes is
//! evidence Bumble's encoder and simble's decoder agree on the bitstream —
//! not merely that some bytes arrived.

use std::str::FromStr;
use std::time::{Duration, Instant};

use simble::classic::a2dp::make_audio_sink_service_sdp_records;
use simble::classic::avdtp::AvdtpEvent;
use simble::classic::sdp::SdpServer;
use simble::device::a2dp::A2dpSink;
use simble::device::classic_host::scan_enable;
use simble::device::{ClassicHost, SdpHandler};
use simble::transport::{HciChannel, HciTransport, LiveTransport};
use simble::types::Address;

/// SDP record handle for the Audio Sink record this speaker publishes.
const SINK_SERVICE_RECORD_HANDLE: u32 = 0x0001_000B;

/// Class of Device 0x240414: Audio/Video major class, Loudspeaker minor
/// class, Audio + Rendering service bits — what a phone renders as a
/// speaker, and what makes this device recognisably the *rendering* side.
const SPEAKER_CLASS_OF_DEVICE: [u8; 3] = [0x14, 0x04, 0x24];

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let want_frames = env_usize("A2DP_EXPECT_FRAMES", 20);
    let timeout = Duration::from_secs(env_usize("A2DP_TIMEOUT_SECS", 30) as u64);
    let name = std::env::var("SIMBLE_NAME").unwrap_or_else(|_| "simble-speaker".to_string());
    let address = std::env::var("SIMBLE_ADDR").unwrap_or_else(|_| "F0:DE:C0:00:0C:0B".to_string());
    let local = Address::from_str(&address).unwrap_or(Address::ANY);

    // netsim unless `$SIMBLE_HCI` says otherwise, so every existing
    // invocation is unchanged; `tcp:HOST:PORT` reaches a Bumble-hosted
    // controller instead, which is how this runs in CI with no Android SDK.
    let mut transport = match LiveTransport::open_from_env(&name, local) {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    if let Ok(path) = std::env::var("SIMBLE_BTSNOOP")
        && let Ok(file) = std::fs::File::create(&path)
        && transport.set_trace(file)
    {
        println!("btsnoop capture: {path}");
    }
    let backend = transport.describe();
    println!("connected over {backend} as {name} ({local}); waiting for a source");

    let mut host = ClassicHost::new(&name, SPEAKER_CLASS_OF_DEVICE);
    let mut sdp = SdpHandler::new(SdpServer::new());
    sdp.server_mut().service_records.insert(
        SINK_SERVICE_RECORD_HANDLE,
        make_audio_sink_service_sdp_records(SINK_SERVICE_RECORD_HANDLE, None),
    );
    host.register_handler(Box::new(sdp)).expect("SDP registers");
    host.register_handler(Box::new(A2dpSink::new()))
        .expect("AVDTP registers");

    let channel = HciChannel::new();
    for packet in host.start_commands() {
        channel.inject_host_packet(packet).expect("queue bring-up");
    }
    for packet in host.set_scan_enable(scan_enable::INQUIRY_AND_PAGE) {
        channel
            .inject_host_packet(packet)
            .expect("queue scan enable");
    }

    let mut decoded_frames = 0usize;
    let mut reported = Vec::new();
    let mut failure: Option<String> = None;
    let deadline = Instant::now() + timeout;

    while decoded_frames < want_frames && failure.is_none() {
        if Instant::now() > deadline {
            failure = Some(format!(
                "timed out after {timeout:?} with {decoded_frames} of {want_frames} SBC frames"
            ));
            break;
        }
        if let Err(e) = transport.pump(&channel) {
            failure = Some(format!("transport: {e}"));
            break;
        }
        while let Some(packet) = channel.poll_controller_packet() {
            match host.handle_packet(&packet) {
                Ok(outgoing) => {
                    for out in outgoing {
                        let _ = channel.inject_host_packet(out);
                    }
                }
                Err(e) => eprintln!("host: {e}"),
            }
        }
        for packet in host.poll() {
            let _ = channel.inject_host_packet(packet);
        }

        let Some(sink) = host.handler_mut::<A2dpSink>() else {
            failure = Some("the sink handler vanished".to_string());
            break;
        };
        // Report each AVDTP step once, in order — the sequence a foreign
        // source drove, which is the thing worth seeing in a live run.
        while reported.len() < sink.events().len() {
            let event = sink.events()[reported.len()].clone();
            println!("avdtp: {}", describe(&event));
            reported.push(event);
        }
        let frames = sink.take_frames();
        if !frames.is_empty() {
            let audio = A2dpSink::decode(&frames);
            if audio.undecodable_bytes > 0 {
                failure = Some(format!(
                    "{} bytes of the peer's media did not begin a whole SBC frame",
                    audio.undecodable_bytes
                ));
                break;
            }
            if audio.frames == 0 {
                failure = Some(
                    "media arrived but the libsbc-verified decoder read no frames from it".into(),
                );
                break;
            }
            decoded_frames += audio.frames;
            println!(
                "sbc: {} frames from {} payloads ({} PCM samples); {decoded_frames} total",
                audio.frames,
                frames.len(),
                audio.pcm.len()
            );
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    if let Some(reason) = failure {
        eprintln!("FAIL: {reason}");
        std::process::exit(1);
    }
    println!("PASS: {decoded_frames} SBC frames from a foreign A2DP source decoded");
}

/// One line per AVDTP step, so a live run reads as the sequence it was.
fn describe(event: &AvdtpEvent) -> String {
    match event {
        AvdtpEvent::StreamConfigured { seid } => format!("SEID {seid} configured"),
        AvdtpEvent::StreamOpened { seid } => format!("SEID {seid} open"),
        AvdtpEvent::StreamStarted { seid } => format!("SEID {seid} streaming"),
        AvdtpEvent::StreamSuspended { seid } => format!("SEID {seid} suspended"),
        AvdtpEvent::StreamClosed { seid } => format!("SEID {seid} closed"),
        AvdtpEvent::StreamAborted { seid } => format!("SEID {seid} aborted"),
        AvdtpEvent::StreamReconfigured { seid } => format!("SEID {seid} reconfigured"),
        AvdtpEvent::DelayReport { seid, delay } => format!("SEID {seid} delay {delay} ms"),
        AvdtpEvent::CommandRefused {
            signal_identifier,
            error_code,
        } => format!("refused signal {signal_identifier:#04x} with error {error_code:#04x}"),
        other => format!("{other:?}"),
    }
}
