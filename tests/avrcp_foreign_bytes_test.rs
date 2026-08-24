// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! **Captured foreign bytes** from the AVRCP interop run.
//!
//! `tests/interop/avrcp_peer.py` points Bumble's AVRCP at simble's, in both
//! roles, over netsim's rootcanal. That run is the only thing that proves the
//! AV/C frames, the AVCTP headers and the AVRCP PDU layer are right — simble's
//! controller and simble's target agree with each other by construction — but
//! it needs a live `netsimd` and a Python environment, so it is not run by
//! `cargo test` and a regression can sit in the tree for weeks.
//!
//! This file closes that hole the way `docs/test-strategy.md` asks for: the
//! exact octets **Bumble put on the wire** during a passing run, pinned as
//! consts and fed through the same code paths the live run uses. Nothing here
//! was constructed by simble, so no self-consistent mistake can satisfy it.
//!
//! Provenance: netsimd (Android emulator canary), Bumble 0.0.233, one run of
//! `tests/interop/avrcp_peer.py` with both phases passing. rootcanal hands out
//! a fresh BD_ADDR per session, so nothing here depends on an address.
//!
//! ## What the run configured, on each side
//!
//! Phase 1 ran `examples/avrcp_remote.rs` with `AVRCP_ROLE=target`, serving
//! the track named below at 213 000 ms, playing. Phase 2 ran it with
//! `AVRCP_ROLE=controller` against a `bumble.avrcp.Protocol` holding a
//! `Delegate([VOLUME_CHANGED, PLAYBACK_STATUS_CHANGED])`, asking for volume
//! 0x53. Every assertion below is one of those facts arriving from — or
//! surviving a round trip through — a foreign stack.

use simble::classic::avc::{ResponseCode, operation_id};
use simble::classic::avctp::AVCTP_PSM;
use simble::classic::avrcp::{
    AVRCP_PID, AvrcpEvent, Event, MediaAttribute, Protocol, Response, character_set_id, event_id,
    media_attribute_id, pdu_id, play_status,
};
use simble::device::avrcp::{AvrcpController, AvrcpTarget, Track};
use simble::device::{HandlerChannel, ProtocolHandler};

// ---------------------------------------------------------------------------
// What each side was configured to be.
// ---------------------------------------------------------------------------

/// `AVRCP_TITLE` on the simble side in phase 1. Bumble parsed this string out
/// of simble's GetElementAttributes response and the script asserted on it.
const TRACK_TITLE: &str = "Careful With That Axe";
/// `AVRCP_ARTIST`, likewise.
const TRACK_ARTIST: &str = "Simble Ensemble";
/// The track length simble served, and the number Bumble's
/// `SongAndPlayStatus.song_length` came back as.
const TRACK_LENGTH_MS: u32 = 213_000;

/// `AVRCP_EXPECT_VOLUME` in phase 2 — the volume simble asked Bumble to set.
/// Deliberately not 0 (Bumble's `Delegate.volume` starts there), not 0x7F,
/// and not simble's own default of 0x3F.
const VOLUME: u8 = 0x53;

/// The L2CAP MTU rootcanal's channel negotiated in the run.
const MTU: u16 = 672;

// ---------------------------------------------------------------------------
// Bumble's *commands* — phase 1, its controller driving simble's target.
// ---------------------------------------------------------------------------

/// `GetCapabilities(EVENTS_SUPPORTED)`.
///
/// `00` AVCTP header: transaction label 0, SINGLE, command. `110E` the AVRCP
/// PID. `01` AV/C ctype STATUS. `48` subunit PANEL (0x09 << 3), id 0. `00`
/// opcode VENDOR DEPENDENT. `001958` the Bluetooth SIG company ID. Then the
/// AVRCP PDU: `10` GetCapabilities, `00` SINGLE packet, `0001` one parameter
/// octet, `03` capability EVENTS_SUPPORTED.
const BUMBLE_GET_CAPABILITIES: &str = "00110E0148000019581000000103";

/// `GetPlayStatus` — PDU `30`, no parameters.
const BUMBLE_GET_PLAY_STATUS: &str = "10110E01480000195830000000";

/// `GetElementAttributes(identifier=0, attributes=[])`. Nine parameter
/// octets: eight of element identifier, then a **zero** attribute count,
/// which AVRCP 6.6.1 defines as "send me everything". Bumble's own
/// `get_element_attributes(0, [])`.
const BUMBLE_GET_ELEMENT_ATTRIBUTES: &str = "20110E01480000195820000009000000000000000000";

/// `RegisterNotification(PLAYBACK_STATUS_CHANGED, interval 0)`. Note the
/// ctype: `03` is NOTIFY, not CONTROL or STATUS — a target that only accepted
/// CONTROL here would answer REJECTED(INVALID_COMMAND) and the whole
/// notification mechanism would be dead against every real controller.
const BUMBLE_REGISTER_NOTIFICATION: &str = "30110E034800001958310000050100000000";

/// PASS THROUGH PLAY, pressed. `00` ctype CONTROL, `48` PANEL, `7C` opcode
/// PASS THROUGH, `44` operation PLAY with the state flag clear, `00` no
/// operation data.
const BUMBLE_PLAY_PRESSED: &str = "40110E00487C4400";
/// PASS THROUGH PLAY, released — the same operation ID with bit 7 set.
const BUMBLE_PLAY_RELEASED: &str = "50110E00487CC400";
/// PASS THROUGH PAUSE, pressed.
const BUMBLE_PAUSE_PRESSED: &str = "60110E00487C4600";
/// PASS THROUGH PAUSE, released.
const BUMBLE_PAUSE_RELEASED: &str = "70110E00487CC600";

// ---------------------------------------------------------------------------
// Bumble's *responses* — phase 2, simble's controller driving its target.
// ---------------------------------------------------------------------------

/// `GetCapabilities` response. `02` label 0, SINGLE, **response**. `0C` AV/C
/// IMPLEMENTED / STABLE. Parameters `03 02 0D 01`: capability EVENTS_SUPPORTED,
/// two of them, VOLUME_CHANGED and PLAYBACK_STATUS_CHANGED — the list
/// `bumble.avrcp.Delegate` was constructed with, in the order Bumble stored
/// it, which is not the order simble would have written.
const BUMBLE_CAPABILITIES_RESPONSE: &str = "02110E0C48000019581000000403020D01";

/// `SetAbsoluteVolume` response. `09` AV/C ACCEPTED, and the effective volume
/// Bumble applied echoed back: `53`.
const BUMBLE_VOLUME_RESPONSE: &str = "12110E0948000019585000000153";

/// `RegisterNotification` INTERIM. `0F` is INTERIM, not IMPLEMENTED/STABLE:
/// AVRCP 6.7.2 answers a registration with a snapshot now and a CHANGED
/// later, and a controller that treated INTERIM as the final answer would
/// free the transaction label and drop the CHANGED when it came.
const BUMBLE_NOTIFICATION_INTERIM: &str = "22110E0F4800001958310000020D53";

/// PASS THROUGH PLAY press, ACCEPTED (`09`).
const BUMBLE_PLAY_PRESS_RESPONSE: &str = "32110E09487C4400";
/// PASS THROUGH PLAY release, ACCEPTED.
const BUMBLE_PLAY_RELEASE_RESPONSE: &str = "42110E09487CC400";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
        .collect()
}

fn channel(cid: u16) -> HandlerChannel {
    HandlerChannel {
        psm: AVCTP_PSM,
        cid,
        peer_mtu: MTU,
    }
}

/// The target `examples/avrcp_remote.rs` served in phase 1, with its control
/// channel open — the same handler, configured the same way.
fn simble_target() -> AvrcpTarget {
    let mut target = AvrcpTarget::new();
    target.set_playlist(vec![
        Track::new(
            TRACK_TITLE,
            TRACK_ARTIST,
            "Unreachable Profiles",
            TRACK_LENGTH_MS,
        ),
        Track::new(
            "Continuation State",
            "The Fragmented",
            "Unreachable Profiles",
            187_000,
        ),
    ]);
    target.set_playback_status(play_status::PLAYING);
    target.on_channel_open(channel(0x0040));
    // Whatever the playlist queued for a peer that did not exist yet.
    let _ = target.poll_channel_output(channel(0x0040));
    target
}

/// The AVRCP PDU sitting inside one AVCTP response SDU: header(1) + PID(2) +
/// AV/C ctype/subunit/opcode(3) + company ID(3), then `pdu_id`, packet type,
/// a 16-bit length and the parameters.
fn response_parameters(sdu: &[u8]) -> (u8, ResponseCode, Vec<u8>) {
    let response = match sdu[3] & 0x0F {
        0x08 => ResponseCode::NotImplemented,
        0x09 => ResponseCode::Accepted,
        0x0A => ResponseCode::Rejected,
        0x0C => ResponseCode::ImplementedOrStable,
        0x0D => ResponseCode::Changed,
        0x0F => ResponseCode::Interim,
        other => panic!("unexpected AV/C response code {other:#04x}"),
    };
    let length = usize::from(u16::from_be_bytes([sdu[11], sdu[12]]));
    (sdu[9], response, sdu[13..13 + length].to_vec())
}

// ---------------------------------------------------------------------------
// Framing facts, before any state machine gets involved
// ---------------------------------------------------------------------------

/// Every captured SDU is AVRCP's PID with the C/R bit set the way its
/// direction requires. If this is wrong, nothing below means anything.
#[test]
fn test_every_captured_sdu_is_an_avrcp_message_of_the_right_direction() {
    for hex in [
        BUMBLE_GET_CAPABILITIES,
        BUMBLE_GET_PLAY_STATUS,
        BUMBLE_GET_ELEMENT_ATTRIBUTES,
        BUMBLE_REGISTER_NOTIFICATION,
        BUMBLE_PLAY_PRESSED,
        BUMBLE_PLAY_RELEASED,
        BUMBLE_PAUSE_PRESSED,
        BUMBLE_PAUSE_RELEASED,
    ] {
        let sdu = bytes(hex);
        assert_eq!(u16::from_be_bytes([sdu[1], sdu[2]]), AVRCP_PID, "{hex}");
        assert_eq!(sdu[0] & 0x0C, 0, "{hex}: not a SINGLE packet");
        assert_eq!(sdu[0] & 0x02, 0, "{hex}: C/R says response, not command");
    }
    for hex in [
        BUMBLE_CAPABILITIES_RESPONSE,
        BUMBLE_VOLUME_RESPONSE,
        BUMBLE_NOTIFICATION_INTERIM,
        BUMBLE_PLAY_PRESS_RESPONSE,
        BUMBLE_PLAY_RELEASE_RESPONSE,
    ] {
        let sdu = bytes(hex);
        assert_eq!(u16::from_be_bytes([sdu[1], sdu[2]]), AVRCP_PID, "{hex}");
        assert_eq!(sdu[0] & 0x02, 0x02, "{hex}: C/R says command, not response");
        assert_eq!(sdu[0] & 0x01, 0, "{hex}: IPID set on a real response");
    }
}

// ---------------------------------------------------------------------------
// Phase 1: Bumble's controller drove simble's target
// ---------------------------------------------------------------------------

#[test]
fn test_bumbles_get_capabilities_reads_the_targets_event_list() {
    let mut target = simble_target();
    let out = target.on_channel_data(channel(0x0040), &bytes(BUMBLE_GET_CAPABILITIES));
    assert_eq!(out.len(), 1, "one command, one response");

    let (pdu, code, parameters) = response_parameters(&out[0]);
    assert_eq!(pdu, pdu_id::GET_CAPABILITIES);
    assert_eq!(code, ResponseCode::ImplementedOrStable);
    // Bumble's script asserted PLAYBACK_STATUS_CHANGED was in the list it
    // parsed out of exactly these bytes.
    assert_eq!(parameters[0], 0x03, "capability ID EVENTS_SUPPORTED");
    let events = &parameters[2..];
    assert!(events.contains(&event_id::PLAYBACK_STATUS_CHANGED));
    assert!(events.contains(&event_id::TRACK_CHANGED));
    assert_eq!(usize::from(parameters[1]), events.len());
    // The transaction label came back on the label Bumble used.
    assert_eq!(out[0][0] >> 4, 0);
}

#[test]
fn test_bumbles_get_play_status_reads_the_track_length_it_reported() {
    let mut target = simble_target();
    let out = target.on_channel_data(channel(0x0040), &bytes(BUMBLE_GET_PLAY_STATUS));
    let (pdu, code, parameters) = response_parameters(&out[0]);
    assert_eq!(pdu, pdu_id::GET_PLAY_STATUS);
    assert_eq!(code, ResponseCode::ImplementedOrStable);

    let parsed = Response::parse(pdu, &parameters).expect("a GetPlayStatus response");
    assert_eq!(
        parsed,
        Response::GetPlayStatus {
            song_length: TRACK_LENGTH_MS,
            song_position: 0,
            play_status: play_status::PLAYING,
        },
        "Bumble's SongAndPlayStatus read 213000 / PLAYING out of these octets"
    );
}

#[test]
fn test_bumbles_get_element_attributes_asks_for_everything_and_gets_the_track() {
    let mut target = simble_target();
    let out = target.on_channel_data(channel(0x0040), &bytes(BUMBLE_GET_ELEMENT_ATTRIBUTES));
    let (pdu, code, parameters) = response_parameters(&out[0]);
    assert_eq!(pdu, pdu_id::GET_ELEMENT_ATTRIBUTES);
    assert_eq!(code, ResponseCode::ImplementedOrStable);

    let Some(Response::GetElementAttributes { attributes }) = Response::parse(pdu, &parameters)
    else {
        panic!("not a GetElementAttributes response");
    };
    assert!(attributes.contains(&MediaAttribute {
        attribute_id: media_attribute_id::TITLE,
        character_set_id: character_set_id::UTF_8,
        value: TRACK_TITLE.into(),
    }));
    assert!(attributes.contains(&MediaAttribute {
        attribute_id: media_attribute_id::ARTIST_NAME,
        character_set_id: character_set_id::UTF_8,
        value: TRACK_ARTIST.into(),
    }));
    // The whole answer fitted one AV/C frame, which is why the live run could
    // use Bumble as an oracle at all: its controller never sends
    // RequestContinuingResponse.
    assert!(
        parameters.len() <= 512 - 13,
        "the captured exchange was not a fragmented one"
    );
}

#[test]
fn test_bumbles_notify_ctype_registration_is_answered_with_an_interim_snapshot() {
    let mut target = simble_target();
    let out = target.on_channel_data(channel(0x0040), &bytes(BUMBLE_REGISTER_NOTIFICATION));
    let (pdu, code, parameters) = response_parameters(&out[0]);
    assert_eq!(pdu, pdu_id::REGISTER_NOTIFICATION);
    assert_eq!(
        code,
        ResponseCode::Interim,
        "AVRCP 6.7.2: a registration is answered INTERIM, then CHANGED"
    );
    assert_eq!(
        Response::parse(pdu, &parameters),
        Some(Response::RegisterNotification {
            event: Event::PlaybackStatusChanged {
                play_status: play_status::PLAYING
            }
        }),
        "Bumble's monitor yielded PLAYING from this snapshot"
    );
}

/// The whole of phase 1's key sequence, in order, against one target — the
/// run Bumble made, replayed. The CHANGED notification at the end is the fact
/// the live script asserted last.
#[test]
fn test_bumbles_key_sequence_pauses_the_player_and_draws_a_changed_notification() {
    let mut target = simble_target();
    let cid = channel(0x0040);

    // Register first, exactly as the run did — otherwise there is no listener
    // for the CHANGED to go to and the notification is silently dropped.
    let _ = target.on_channel_data(cid, &bytes(BUMBLE_REGISTER_NOTIFICATION));

    for (hex, expected, pressed) in [
        (BUMBLE_PLAY_PRESSED, operation_id::PLAY, true),
        (BUMBLE_PLAY_RELEASED, operation_id::PLAY, false),
        (BUMBLE_PAUSE_PRESSED, operation_id::PAUSE, true),
        (BUMBLE_PAUSE_RELEASED, operation_id::PAUSE, false),
    ] {
        let out = target.on_channel_data(cid, &bytes(hex));
        assert_eq!(out.len(), 1, "{hex}: a key draws exactly one response");
        // AV/C ACCEPTED, echoing the operation and the state flag back.
        assert_eq!(out[0][3] & 0x0F, 0x09, "{hex}: not ACCEPTED");
        assert_eq!(out[0][5], 0x7C, "{hex}: not a PASS THROUGH response");
        assert_eq!(
            out[0][6],
            expected | if pressed { 0x00 } else { 0x80 },
            "{hex}: the operation or state flag was not echoed"
        );
    }

    assert_eq!(
        target.key_presses(),
        vec![operation_id::PLAY, operation_id::PAUSE],
        "the operation IDs Bumble sent, and nothing simble made up"
    );
    assert_eq!(
        target.playback_status(),
        play_status::PAUSED,
        "Bumble's script read PAUSED off the CHANGED notification"
    );
    // ...and that CHANGED is queued for the peer rather than merely implied.
    let pending = target.poll_channel_output(cid);
    let changed = pending
        .iter()
        .find(|sdu| sdu.get(9) == Some(&pdu_id::REGISTER_NOTIFICATION))
        .expect("a CHANGED notification for the registration");
    assert_eq!(changed[3] & 0x0F, 0x0D, "AV/C CHANGED");
    let (_, _, parameters) = response_parameters(changed);
    assert_eq!(
        Response::parse(pdu_id::REGISTER_NOTIFICATION, &parameters),
        Some(Response::RegisterNotification {
            event: Event::PlaybackStatusChanged {
                play_status: play_status::PAUSED
            }
        })
    );
}

// ---------------------------------------------------------------------------
// Phase 2: simble's controller drove Bumble's target
// ---------------------------------------------------------------------------

/// The controller `examples/avrcp_remote.rs` ran in phase 2, with the same
/// commands in the same order — which is what makes the captured responses'
/// transaction labels line up.
fn simble_controller() -> AvrcpController {
    let mut controller = AvrcpController::new();
    // Drain the channel request the constructor queued.
    let _ = controller.poll_channel_requests();
    controller.on_channel_open(channel(0x0041));
    controller.query_supported_events(); // label 0
    controller.set_volume(VOLUME); // label 1
    controller.monitor(event_id::VOLUME_CHANGED); // label 2
    controller.tap(operation_id::PLAY); // labels 3 and 4
    let _ = controller.poll_channel_output(channel(0x0041));
    controller
}

#[test]
fn test_bumbles_capabilities_response_is_read_as_the_events_its_delegate_holds() {
    let mut controller = simble_controller();
    let out = controller.on_channel_data(channel(0x0041), &bytes(BUMBLE_CAPABILITIES_RESPONSE));
    assert!(out.is_empty(), "a complete response needs no continuation");
    assert_eq!(
        controller.remote().supported_events,
        vec![event_id::VOLUME_CHANGED, event_id::PLAYBACK_STATUS_CHANGED],
        "in Bumble's order, not simble's"
    );
}

#[test]
fn test_bumbles_volume_response_carries_back_the_volume_its_delegate_applied() {
    let mut controller = simble_controller();
    controller.on_channel_data(channel(0x0041), &bytes(BUMBLE_VOLUME_RESPONSE));
    let accepted = controller
        .events()
        .iter()
        .any(|event| matches!(event, AvrcpEvent::VolumeAccepted { volume } if *volume == VOLUME));
    assert!(
        accepted,
        "the live run's `delegate.volume` became {VOLUME}; these are the bytes that said so"
    );
}

#[test]
fn test_bumbles_interim_leaves_the_transaction_open_for_the_changed() {
    let mut controller = simble_controller();
    controller.on_channel_data(channel(0x0041), &bytes(BUMBLE_NOTIFICATION_INTERIM));
    let interim = controller.events().iter().any(|event| {
        matches!(
            event,
            AvrcpEvent::NotificationReceived {
                event: Event::VolumeChanged { volume },
                interim: true,
            } if *volume == VOLUME
        )
    });
    assert!(interim, "the INTERIM snapshot was not recognised as one");

    // The label must still be in flight. Feeding the same label a CHANGED —
    // which is what Bumble sends when its volume next moves — has to be
    // accepted, and a controller that freed the label on INTERIM would drop
    // it on the floor with no error anywhere.
    let mut changed = bytes(BUMBLE_NOTIFICATION_INTERIM);
    changed[3] = 0x0D; // AV/C CHANGED
    changed[14] = 0x20; // a new volume
    controller.on_channel_data(channel(0x0041), &changed);
    let fired = controller.events().iter().any(|event| {
        matches!(
            event,
            AvrcpEvent::NotificationReceived {
                event: Event::VolumeChanged { volume: 0x20 },
                interim: false,
            }
        )
    });
    assert!(fired, "the CHANGED that follows an INTERIM was dropped");
}

#[test]
fn test_bumbles_pass_through_responses_are_matched_to_the_keys_that_caused_them() {
    let mut controller = simble_controller();
    controller.on_channel_data(channel(0x0041), &bytes(BUMBLE_PLAY_PRESS_RESPONSE));
    controller.on_channel_data(channel(0x0041), &bytes(BUMBLE_PLAY_RELEASE_RESPONSE));

    let answers: Vec<(ResponseCode, u8, bool)> = controller
        .events()
        .iter()
        .filter_map(|event| match event {
            AvrcpEvent::PassThroughResponse {
                response,
                operation_id,
                pressed,
            } => Some((*response, *operation_id, *pressed)),
            _ => None,
        })
        .collect();
    // AV/C carries the operation in the response, but simble matches on the
    // transaction label — so this also says the labels lined up.
    assert_eq!(
        answers,
        vec![
            (ResponseCode::Accepted, operation_id::PLAY, true),
            (ResponseCode::Accepted, operation_id::PLAY, false),
        ]
    );
}

/// A `Protocol` on its own, fed the same response with no matching command in
/// flight, must record nothing. The oracle above is only meaningful if an
/// unsolicited response cannot manufacture an event.
#[test]
fn test_a_response_with_no_command_behind_it_is_ignored() {
    let mut controller = Protocol::new(MTU);
    let (out, events) = controller.receive(&bytes(BUMBLE_VOLUME_RESPONSE));
    assert!(out.is_empty());
    assert!(
        events.is_empty(),
        "a response to nothing produced an event: {events:?}"
    );
}
