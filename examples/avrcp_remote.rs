// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Live **AVRCP** against a running netsimd, in either role.
//!
//! AVRCP is 4 300 lines of protocol that, until now, no foreign stack had
//! ever spoken to: `tests/avrcp_test.rs` drives two simble `Protocol`s
//! back-to-back, and two simble endpoints agree with each other by
//! construction. This example is the half that puts it on a real link, so
//! `tests/interop/avrcp_peer.py` can point Bumble's AVRCP at it.
//!
//! ```text
//! netsimd --logtostderr --no-shutdown --ws-port 7681
//! cargo build --example avrcp_remote
//! .venv/bin/python tests/interop/avrcp_peer.py
//! ```
//!
//! ## The two roles
//!
//! `AVRCP_ROLE=target` (the default) is a **phone**: discoverable,
//! connectable, publishing an AVRCP Target SDP record, holding a media
//! player. It initiates nothing — every byte it sends is an answer — which is
//! what makes it the honest thing to point a foreign controller at.
//!
//! `AVRCP_ROLE=controller` is a **head unit**: it inquires, pages the peer,
//! opens PSM 0x0017 and drives the peer's player. The facts it asserts are
//! the peer's answers.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `AVRCP_ROLE` | `target` (default) or `controller` |
//! | `AVRCP_PEER` | controller role: the BD_ADDR to page |
//! | `AVRCP_EXPECT_KEYS` | target role: AV/C operation IDs that must arrive, e.g. `44,46,4B` |
//! | `AVRCP_EXPECT_VOLUME` | controller role: the volume the peer must accept, 0-127 |
//! | `AVRCP_TITLE` | target role: the track title to serve |
//! | `AVRCP_ARTIST` | target role: the artist to serve |
//! | `AVRCP_TIMEOUT_SECS` | how long to wait (default 45) |
//! | `SIMBLE_NAME`, `SIMBLE_ADDR` | this device's identity on the air |
//! | `SIMBLE_BTSNOOP` | write a btsnoop capture here |
//!
//! The run ends by printing the AVCTP SDUs the **peer** sent, as hex. Those
//! are the octets `tests/avrcp_foreign_bytes_test.rs` pins, so `cargo test`
//! re-checks the parse with no netsim in sight.

use std::str::FromStr;
use std::time::{Duration, Instant};

use simble::classic::avc::{ResponseCode, operation_id};
use simble::classic::avrcp::{
    AVRCP_PID, AvrcpEvent, ControllerServiceRecord, TargetServiceRecord, controller_features,
    event_id, play_status, target_features,
};
use simble::classic::sdp::SdpServer;
use simble::device::avrcp::{AvrcpController, AvrcpTarget, Track};
use simble::device::classic_host::scan_enable;
use simble::device::{ClassicHost, SdpHandler};
use simble::transport::{HciChannel, HciTransport, LiveTransport};
use simble::types::Address;

/// SDP record handle for whichever AVRCP record this run publishes.
const SERVICE_RECORD_HANDLE: u32 = 0x0001_000E;

/// Class of Device 0x5A020C: Phone / Smartphone — the target role.
const PHONE_CLASS_OF_DEVICE: [u8; 3] = [0x0C, 0x02, 0x5A];
/// Class of Device 0x200420: Audio/Video, Hands-free — the controller role.
const HEAD_UNIT_CLASS_OF_DEVICE: [u8; 3] = [0x20, 0x04, 0x20];

fn env(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Parses a comma-separated list of hex AV/C operation IDs.
fn parse_keys(text: &str) -> Vec<u8> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| u8::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .collect()
}

fn key_name(operation: u8) -> &'static str {
    match operation {
        operation_id::PLAY => "PLAY",
        operation_id::PAUSE => "PAUSE",
        operation_id::STOP => "STOP",
        operation_id::FORWARD => "FORWARD",
        operation_id::BACKWARD => "BACKWARD",
        operation_id::VOLUME_UP => "VOLUME_UP",
        operation_id::VOLUME_DOWN => "VOLUME_DOWN",
        _ => "other",
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect()
}

/// The AVCTP SDUs that arrived from the peer, in order, deduplicated.
///
/// These are the only bytes in the run nothing in this repo produced, so they
/// are the ones worth keeping. An AVCTP single packet is
/// `header(1) | PID(2) | AV/C frame`, and the PID is AVRCP's 0x110E — a
/// filter tight enough that nothing else on the link can be mistaken for one.
#[derive(Default)]
struct Captured {
    inbound: Vec<Vec<u8>>,
}

impl Captured {
    fn observe(&mut self, packet: &[u8]) {
        // H4 ACL: type(1) + ACL header(4) + L2CAP header(4) + SDU. A
        // continuation fragment (PB bits 0b01) has no L2CAP header.
        let [0x02, rest @ ..] = packet else { return };
        if rest.len() <= 8 || (rest[1] >> 4) & 0x03 == 0x01 {
            return;
        }
        let sdu = &rest[8..];
        let Some(pid) = sdu.get(1..3) else { return };
        if u16::from_be_bytes([pid[0], pid[1]]) != AVRCP_PID {
            return;
        }
        if !self.inbound.iter().any(|seen| seen == sdu) {
            self.inbound.push(sdu.to_vec());
        }
    }

    fn report(&self) {
        println!("\n--- captured foreign AVCTP SDUs ---");
        if self.inbound.is_empty() {
            println!("(none — nothing arrived on PSM 0x0017 with PID 0x110E)");
            return;
        }
        for sdu in &self.inbound {
            let direction = if sdu[0] & 0x02 == 0 {
                "command "
            } else {
                "response"
            };
            println!("{direction} {}", hex(sdu));
        }
    }
}

/// One line per AVRCP event, so a live run reads as the conversation it was.
fn describe(event: &AvrcpEvent) -> String {
    match event {
        AvrcpEvent::KeyEvent {
            operation_id,
            pressed,
            ..
        } => format!(
            "key {} ({operation_id:#04x}) {}",
            key_name(*operation_id),
            if *pressed { "pressed" } else { "released" }
        ),
        AvrcpEvent::NotificationRegistered { event_id } => {
            format!("peer registered for notification {event_id:#04x}")
        }
        AvrcpEvent::VolumeSet { volume } => format!("peer set volume {volume}"),
        AvrcpEvent::PassThroughResponse {
            response,
            operation_id,
            pressed,
        } => format!(
            "peer answered {response:?} to {} {}",
            key_name(*operation_id),
            if *pressed { "press" } else { "release" }
        ),
        AvrcpEvent::SupportedEventsReceived(ids) => {
            format!("peer supports notification events {ids:02x?}")
        }
        AvrcpEvent::PlayStatusReceived {
            song_length,
            song_position,
            play_status,
        } => format!("peer play status {play_status:#04x}, {song_position} of {song_length} ms"),
        AvrcpEvent::ElementAttributesReceived(attributes) => format!(
            "peer track metadata: {:?}",
            attributes
                .iter()
                .map(|a| (a.attribute_id, a.value.as_str()))
                .collect::<Vec<_>>()
        ),
        AvrcpEvent::VolumeAccepted { volume } => format!("peer accepted volume {volume}"),
        AvrcpEvent::CommandRejected {
            pdu_id,
            status_code,
        } => {
            format!("peer REJECTED PDU {pdu_id:#04x} with status {status_code:#04x}")
        }
        AvrcpEvent::CommandNotImplemented { pdu_id } => {
            format!("peer does not implement PDU {pdu_id:#04x}")
        }
        AvrcpEvent::NotificationReceived { event, interim } => format!(
            "notification {event:?} ({})",
            if *interim { "interim" } else { "changed" }
        ),
        other => format!("{other:?}"),
    }
}

fn main() {
    let role = env("AVRCP_ROLE", "target");
    let timeout = Duration::from_secs(env_u64("AVRCP_TIMEOUT_SECS", 45));
    let name = env(
        "SIMBLE_NAME",
        if role == "controller" {
            "simble-head-unit"
        } else {
            "simble-player"
        },
    );
    let address = env("SIMBLE_ADDR", "F0:DE:C0:00:0A:1C");
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
    println!("connected over {backend} as {name} ({local}), role {role}");

    let outcome = match role.as_str() {
        "target" => run_target(&mut transport, &name, timeout),
        "controller" => run_controller(&mut transport, &name, timeout),
        other => Err(format!("unknown AVRCP_ROLE: {other}")),
    };

    match outcome {
        Ok(summary) => println!("PASS: {summary}"),
        Err(reason) => {
            eprintln!("FAIL: {reason}");
            std::process::exit(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Target
// ---------------------------------------------------------------------------

fn run_target(
    transport: &mut LiveTransport,
    name: &str,
    timeout: Duration,
) -> Result<String, String> {
    let wanted_keys = parse_keys(&env("AVRCP_EXPECT_KEYS", "44,46"));
    let title = env("AVRCP_TITLE", "Careful With That Axe");
    let artist = env("AVRCP_ARTIST", "Simble Ensemble");

    let mut host = ClassicHost::new(name, PHONE_CLASS_OF_DEVICE);
    let mut sdp = SdpHandler::new(SdpServer::new());
    sdp.server_mut().service_records.insert(
        SERVICE_RECORD_HANDLE,
        TargetServiceRecord::new(
            SERVICE_RECORD_HANDLE,
            target_features::CATEGORY_1 | target_features::CATEGORY_2,
        )
        .to_service_attributes(),
    );
    host.register_handler(Box::new(sdp))
        .map_err(|e| e.to_string())?;

    let mut target = AvrcpTarget::new();
    target.set_playlist(vec![
        Track::new(&title, &artist, "Unreachable Profiles", 213_000),
        Track::new(
            "Continuation State",
            "The Fragmented",
            "Unreachable Profiles",
            187_000,
        ),
    ]);
    target.set_playback_status(play_status::PLAYING);
    host.register_handler(Box::new(target))
        .map_err(|e| e.to_string())?;

    println!(
        "serving \"{title}\" by {artist}; waiting for a controller to press {:02x?}",
        wanted_keys
    );

    let channel = HciChannel::new();
    bring_up(&host, &channel, scan_enable::INQUIRY_AND_PAGE);

    let mut captured = Captured::default();
    let mut reported = 0usize;
    // Once the keys have arrived there is still traffic owed to the peer —
    // the response to the last key, and the CHANGED notification the key
    // caused. Exiting the instant the condition is met takes the L2CAP
    // channel down with those still queued, and the peer sees a hang it did
    // nothing to deserve.
    let grace = Duration::from_millis(env_u64("AVRCP_GRACE_MS", 2500));
    let mut satisfied_at: Option<Instant> = None;
    let deadline = Instant::now() + timeout;
    loop {
        if Instant::now() > deadline {
            captured.report();
            let seen = host
                .handler::<AvrcpTarget>()
                .map(AvrcpTarget::key_presses)
                .unwrap_or_default();
            return Err(format!(
                "timed out after {timeout:?}; keys received {seen:02x?}, wanted {wanted_keys:02x?}"
            ));
        }
        pump(transport, &channel, &mut host, &mut captured)?;

        let Some(target) = host.handler::<AvrcpTarget>() else {
            return Err("the AVRCP target handler vanished".into());
        };
        while reported < target.events().len() {
            println!("avrcp: {}", describe(&target.events()[reported]));
            reported += 1;
        }
        let seen = target.key_presses();
        if !wanted_keys.is_empty() && wanted_keys.iter().all(|key| seen.contains(key)) {
            let started = *satisfied_at.get_or_insert_with(Instant::now);
            if started.elapsed() >= grace {
                let status = target.playback_status();
                captured.report();
                return Ok(format!(
                    "a foreign AVRCP controller pressed {seen:02x?}; the player ended \
                     in playback status {status:#04x}"
                ));
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------
// Controller
// ---------------------------------------------------------------------------

fn run_controller(
    transport: &mut LiveTransport,
    name: &str,
    timeout: Duration,
) -> Result<String, String> {
    let peer = env("AVRCP_PEER", "");
    let peer =
        Address::from_str(&peer).map_err(|_| format!("AVRCP_PEER is not a BD_ADDR: {peer:?}"))?;
    let want_volume = env_u64("AVRCP_EXPECT_VOLUME", 0x40) as u8;

    let mut host = ClassicHost::new(name, HEAD_UNIT_CLASS_OF_DEVICE);
    let mut sdp = SdpHandler::new(SdpServer::new());
    sdp.server_mut().service_records.insert(
        SERVICE_RECORD_HANDLE,
        ControllerServiceRecord::new(
            SERVICE_RECORD_HANDLE,
            controller_features::CATEGORY_1 | controller_features::CATEGORY_2,
        )
        .to_service_attributes(),
    );
    host.register_handler(Box::new(sdp))
        .map_err(|e| e.to_string())?;
    host.register_handler(Box::new(AvrcpController::new()))
        .map_err(|e| e.to_string())?;

    let channel = HciChannel::new();
    bring_up(&host, &channel, scan_enable::NONE);

    let mut captured = Captured::default();
    let mut reported = 0usize;
    let mut driven = false;
    // Bring-up is queued, not acknowledged. Paging before the controller has
    // worked through Reset and Write Scan Enable is a page against a
    // half-configured controller, and rootcanal answers it with a status the
    // plan then has to unpick.
    let page_at = Instant::now() + Duration::from_millis(env_u64("AVRCP_PAGE_DELAY_MS", 1500));
    let mut paged = false;
    let deadline = Instant::now() + timeout;

    println!("paging {peer}; will ask it to set volume {want_volume}");
    loop {
        if Instant::now() > deadline {
            captured.report();
            return Err(format!("timed out after {timeout:?} driving {peer}"));
        }
        pump(transport, &channel, &mut host, &mut captured)?;

        if !paged && Instant::now() >= page_at {
            for packet in host.create_connection(peer) {
                let _ = channel.inject_host_packet(packet);
            }
            paged = true;
        }

        let connected = host
            .handler::<AvrcpController>()
            .is_some_and(AvrcpController::is_connected);
        if connected && !driven {
            driven = true;
            println!("avctp: control channel open");
            let controller = host
                .handler_mut::<AvrcpController>()
                .ok_or("the AVRCP controller handler vanished")?;
            // Everything asked for here is answered by the *peer*: which
            // notification events it supports, and whether it takes a volume.
            controller.query_supported_events();
            controller.set_volume(want_volume);
            controller.monitor(event_id::VOLUME_CHANGED);
            controller.tap(operation_id::PLAY);
        }

        let Some(controller) = host.handler::<AvrcpController>() else {
            return Err("the AVRCP controller handler vanished".into());
        };
        while reported < controller.events().len() {
            println!("avrcp: {}", describe(&controller.events()[reported]));
            reported += 1;
        }
        let accepted = controller
            .events()
            .iter()
            .any(|event| matches!(event, AvrcpEvent::VolumeAccepted { volume } if *volume == want_volume));
        let answered_keys = controller.events().iter().any(|event| {
            matches!(
                event,
                AvrcpEvent::PassThroughResponse {
                    response: ResponseCode::Accepted | ResponseCode::NotImplemented,
                    ..
                }
            )
        });
        if accepted && answered_keys {
            let events = controller.remote().supported_events.clone();
            captured.report();
            return Ok(format!(
                "a foreign AVRCP target accepted volume {want_volume}, answered a \
                 PASS THROUGH and advertised notification events {events:02x?}"
            ));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---------------------------------------------------------------------------
// Plumbing
// ---------------------------------------------------------------------------

fn bring_up(host: &ClassicHost, channel: &HciChannel, scan: u8) {
    for packet in host.start_commands() {
        let _ = channel.inject_host_packet(packet);
    }
    for packet in host.set_scan_enable(scan) {
        let _ = channel.inject_host_packet(packet);
    }
}

fn pump(
    transport: &mut LiveTransport,
    channel: &HciChannel,
    host: &mut ClassicHost,
    captured: &mut Captured,
) -> Result<(), String> {
    transport
        .pump(channel)
        .map_err(|e| format!("transport: {e}"))?;
    while let Some(packet) = channel.poll_controller_packet() {
        captured.observe(&packet);
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
    Ok(())
}
