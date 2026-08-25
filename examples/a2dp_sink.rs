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
use simble::device::classic_host::{authentication_requirements, io_capability, scan_enable};
use simble::device::{ClassicHost, SdpHandler};
use simble::transport::{HciChannel, HciTransport, LiveTransport};
use simble::types::Address;

/// SDP record handle for the Audio Sink record this speaker publishes.
const SINK_SERVICE_RECORD_HANDLE: u32 = 0x0001_000B;

/// A2DP Audio Sink service class (Assigned Numbers). The number a phone
/// looks for to decide this device can receive audio.
const AUDIO_SINK_SERVICE_UUID: u16 = 0x110B;

/// Class of Device 0x240414: Audio/Video major class, Loudspeaker minor
/// class, Audio + Rendering service bits.
///
/// The minor class is the part a phone acts on before any SDP query, and it
/// decides the *category*, not just the offer: a Wearable Headset minor
/// class (0x240404) pairs and streams identically but Android files the
/// device under headphones, and a thing named `simble-speaker` then does not
/// appear in "Speakers and displays" — measured on a Pixel, which is the
/// sort of thing only a real phone can settle. `SIMBLE_COD` overrides it.
const SPEAKER_CLASS_OF_DEVICE: [u8; 3] = [0x14, 0x04, 0x24];

/// Parses `0x240404` (or `240404`) into the three octets a Class of Device
/// occupies on the wire, least-significant first.
fn parse_class_of_device(text: &str) -> Option<[u8; 3]> {
    let cleaned = text
        .trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X");
    let value = u32::from_str_radix(cleaned, 16).ok()?;
    (value <= 0xFF_FFFF).then_some([value as u8, (value >> 8) as u8, (value >> 16) as u8])
}

fn env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn main() {
    let want_frames = env_usize("A2DP_EXPECT_FRAMES", 20);
    // Long enough that a person can pick the speaker out of a phone's
    // pairing list, tap through a prompt and start a track. The interop
    // script exits on success, so a generous ceiling costs it nothing.
    let timeout = Duration::from_secs(env_usize("A2DP_TIMEOUT_SECS", 180) as u64);
    let class_of_device = std::env::var("SIMBLE_COD")
        .ok()
        .and_then(|text| parse_class_of_device(&text))
        .unwrap_or(SPEAKER_CLASS_OF_DEVICE);
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
    // On a dongle the *controller* owns the identity: `local` was never sent
    // anywhere, and the address the phone will show is the silicon's. Saying
    // the wrong one here sends a person hunting for a device that is not in
    // the list under that name.
    let on_air = match &mut transport {
        LiveTransport::Usb(usb) => match usb.read_bd_addr() {
            Ok(real) => real,
            Err(e) => {
                eprintln!("could not read the dongle's BD_ADDR ({e})");
                local
            }
        },
        _ => local,
    };
    println!(
        "connected over {backend} as {name:?} at {on_air}, class {:#08x}",
        u32::from_le_bytes([
            class_of_device[0],
            class_of_device[1],
            class_of_device[2],
            0
        ])
    );
    println!("discoverable and connectable — pair with {name:?} from the phone, then play audio");

    let mut host = ClassicHost::new(&name, class_of_device);
    // A speaker has no display and no keypad. Claiming otherwise is what
    // escalates SSP from Just Works to Numeric Comparison and makes a phone
    // ask a person to compare six digits with a box that cannot show them —
    // `ClassicHost` defaults to DisplayYesNo, which is right for a phone and
    // wrong for this.
    host.set_io_capability(
        io_capability::NO_INPUT_NO_OUTPUT,
        authentication_requirements::GENERAL_BONDING,
    );
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
    // Publish the name and the Audio Sink service class in the inquiry
    // response itself. A phone decides what to *offer* from the Class of
    // Device and this list, both readable before it has connected to
    // anything — without it we are a nameless row that has to be paged
    // before it can say what it is.
    for packet in host.set_extended_inquiry_response(&name, &[AUDIO_SINK_SERVICE_UUID]) {
        channel.inject_host_packet(packet).expect("queue EIR");
    }

    let mut decoded_frames = 0usize;
    let mut reported = Vec::new();
    let mut failure: Option<String> = None;
    let deadline = Instant::now() + timeout;
    // Each milestone announced once. A live run spends most of its time
    // waiting for a person, and silence between "waiting for a source" and
    // the first AVDTP event cannot be told from a dead radio.
    let mut said_connected = false;
    let mut said_paired = false;
    let mut said_encrypted = false;
    // A2DP over an unencrypted link is a link a phone will not stream on.
    // See the `asked_for_authentication` use below for why this side has to
    // ask rather than wait.
    let mut asked_for_authentication = false;
    let mut asked_for_encryption = false;
    let mut saw_authentication_complete = false;

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
            // Authentication Complete (Vol 4, Part E, 7.7.6) is the event
            // that says the link key has actually been used to authenticate
            // this link, as opposed to merely existing. `LinkSecurity`
            // reports both through one `authenticated` flag, so the two are
            // told apart here rather than there.
            if packet.len() > 2 && packet[0] == 0x04 && packet[1] == 0x06 {
                saw_authentication_complete = true;
            }
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

        if !said_connected && let Some((handle, peer)) = host.connection() {
            println!("link: the phone paged us; ACL up with {peer} on handle {handle:#06x}");
            said_connected = true;
        }
        let security = host.security();
        if !said_paired {
            if let Some(status) = security.pairing_status.filter(|s| *s != 0x00) {
                failure = Some(format!(
                    "the phone's pairing failed: Simple Pairing Complete status {status:#04x}"
                ));
                break;
            }
            if security.authenticated {
                if let Some(capability) = security.peer_io_capability {
                    println!("link: the phone's IO capability is {capability:#04x}");
                }
                if let Some((_, peer)) = host.connection()
                    && let Some(key) = host.link_key(peer)
                {
                    println!(
                        "link: bonded, link key type {:#04x} ({})",
                        key.key_type,
                        if key.is_authenticated() {
                            "authenticated"
                        } else {
                            "unauthenticated"
                        }
                    );
                }
                said_paired = true;
            }
        }
        // Having a link key is not having an encrypted link, and a phone
        // will not open an A2DP media channel on an unencrypted one. Which
        // side starts encryption is not fixed: normally the initiator does,
        // but a Pixel that has just re-bonded after asking us for a key we
        // did not have never gets round to it, and both sides then wait —
        // the phone showing "connecting" until it times out. A real headset
        // does not wait, and neither does this: once bonded, ask.
        //
        // Authentication first, because Set Connection Encryption is only
        // valid on a link that has been authenticated. Asking for it uses
        // the key the pairing just produced, which arrives back here as a
        // Link Key Request that `ClassicHost` answers from its store.
        if said_paired && !asked_for_authentication {
            asked_for_authentication = true;
            println!("link: asking for authentication so the link can be encrypted");
            for packet in host.authenticate() {
                let _ = channel.inject_host_packet(packet);
            }
        }
        if saw_authentication_complete && !asked_for_encryption {
            asked_for_encryption = true;
            println!("link: authenticated; asking for encryption");
            for packet in host.encrypt(true) {
                let _ = channel.inject_host_packet(packet);
            }
        }
        if !said_encrypted && security.encrypted {
            println!("link: encrypted");
            said_encrypted = true;
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
