// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! AVRCP as a **connectable** profile: two BR/EDR devices on one simulated
//! link, with the remote control crossing a real AVCTP channel.
//!
//! Everything here goes through [`simble::controller::sim`]. Nothing is wired
//! directly together, so a key press has to survive inquiry, the Remote Name
//! Request, the page, an L2CAP connect-and-configure on PSM 0x0017, the AVCTP
//! header, an AV/C PASS THROUGH frame, and the whole path back. Turning off
//! the acceptor's inquiry scan is the switch that proves it: the plan fails in
//! `Inquiring` rather than connecting anyway.
//!
//! `tests/avrcp_test.rs` already drives two `avrcp::Protocol`s back to back.
//! What it cannot show is any of the above — and until this file, nothing
//! could, because nothing could host the profile at all.

use simble::classic::avc::{ResponseCode, operation_id};
use simble::classic::avrcp::{AvrcpEvent, event_id, media_attribute_id, play_status};
use simble::device::a2dp::SourcePhase;
use simble::device::classic_host::scan_enable;
use simble::device::media_scene::{MediaPlayerScene, RemoteControlScene};
use simble::device::{ClassicLinkPhase, Track};

/// How many scene ticks any of these plans is allowed. Generous, and bounded:
/// a plan that needs more has stalled, and a test that ran until it passed
/// would say nothing about how.
const STEPS: usize = 400;

/// Interleaved stereo PCM — a quiet ramp, enough samples for several SBC
/// frames at 44.1 kHz joint stereo.
fn pcm(samples: usize) -> Vec<i16> {
    (0..samples * 2)
        .map(|i| ((i as i32 * 71) % 8000 - 4000) as i16)
        .collect()
}

// ---------------------------------------------------------------------------
// AVRCP on its own
// ---------------------------------------------------------------------------

#[test]
fn test_a_head_unit_finds_a_phone_and_opens_an_avctp_channel() {
    let mut scene = RemoteControlScene::new();
    assert!(
        scene.run_until_connected(STEPS),
        "the control channel never opened: phase {:?}, error {:?}",
        scene.phase(),
        scene.error()
    );
    assert_eq!(scene.phase(), ClassicLinkPhase::Connected);
    assert_eq!(scene.error(), None);
    // The link the profile rode in on is a real ACL at both ends.
    assert!(scene.remote_host().connection().is_some());
    assert!(scene.player_host().connection().is_some());
    // ...and the channel is on the AVCTP control PSM, not some other one.
    assert!(
        scene
            .remote_host()
            .channel_is_open(simble::classic::avctp::AVCTP_PSM)
    );
}

/// The inquiry is not decoration. Same scene, same handlers, one bit
/// different on the phone, and the whole thing fails to connect.
#[test]
fn test_a_phone_that_is_not_discoverable_is_never_reached() {
    let mut scene = RemoteControlScene::with_phone_scan_enable(Vec::new(), scan_enable::NONE);
    assert!(
        !scene.run_until_connected(STEPS),
        "the control channel opened to a phone that answers no inquiry"
    );
    assert_eq!(scene.phase(), ClassicLinkPhase::Failed);
    assert!(
        scene.error().is_some_and(|e| e.contains("inquiry")),
        "expected the plan to say the inquiry found nothing, got {:?}",
        scene.error()
    );
    assert!(!scene.controller().is_connected());
    assert!(!scene.target().is_connected());
}

/// The other half of the same claim: when it *does* connect, the address and
/// the name came off the air rather than out of the scene's constructor.
#[test]
fn test_the_head_unit_finds_the_phone_by_inquiry_and_resolves_its_name() {
    let mut scene = RemoteControlScene::new();
    assert!(scene.run_until_connected(STEPS));
    assert!(
        scene
            .remote_host()
            .discovered()
            .iter()
            .any(|device| device.address == simble::device::media_scene::PLAYER_ADDRESS),
        "the head unit connected to something it never found in the inquiry"
    );
    assert_eq!(
        scene
            .remote_host()
            .name_of(simble::device::media_scene::PLAYER_ADDRESS),
        Some("Simble Phone"),
        "the name came from a Remote Name Request, not from the scene"
    );
}

#[test]
fn test_pause_crosses_the_link_and_moves_the_players_state() {
    let mut scene = RemoteControlScene::new();
    assert!(scene.run_until_connected(STEPS));

    // The phone is playing.
    scene.target_mut().set_playback_status(play_status::PLAYING);
    scene.run_until(STEPS, |_| false);
    assert!(scene.target().is_playing());

    scene.controller_mut().pause();
    let paused = scene.run_until(STEPS, |scene| {
        scene.target().playback_status() == play_status::PAUSED
    });
    assert!(paused, "the phone never saw the PAUSE");

    // The key arrived as a key, not as a state poke: the target logged the
    // AV/C operation ID the controller pressed.
    assert_eq!(scene.target().key_presses(), vec![operation_id::PAUSE]);

    // And the controller was told the phone accepted it. The response has to
    // travel back, so this is a state to run to, not one to read off.
    let accepted = scene.run_until(STEPS, |scene| {
        scene.controller().events().iter().any(|event| {
            matches!(
                event,
                AvrcpEvent::PassThroughResponse {
                    response: ResponseCode::Accepted,
                    operation_id: operation_id::PAUSE,
                    pressed: true,
                }
            )
        })
    });
    assert!(accepted, "no ACCEPTED came back for the PAUSE");
}

#[test]
fn test_a_player_with_no_transport_controls_refuses_the_key() {
    let mut scene = RemoteControlScene::new();
    scene
        .target_mut()
        .set_key_event_response(ResponseCode::NotImplemented);
    assert!(scene.run_until_connected(STEPS));

    scene.controller_mut().play();
    let refused = scene.run_until(STEPS, |scene| {
        scene.controller().events().iter().any(|event| {
            matches!(
                event,
                AvrcpEvent::PassThroughResponse {
                    response: ResponseCode::NotImplemented,
                    ..
                }
            )
        })
    });
    assert!(
        refused,
        "a refusal has to reach the controller as a refusal"
    );
    // The refusal is not a lie: the player did not start.
    assert!(!scene.target().is_playing());
}

#[test]
fn test_the_controller_reads_the_track_the_phone_is_playing() {
    let mut scene = RemoteControlScene::new();
    assert!(scene.run_until_connected(STEPS));

    scene.controller_mut().query_metadata(&[]);
    let read = scene.run_until(STEPS, |scene| scene.controller().remote().title().is_some());
    assert!(read, "GetElementAttributes never answered");

    assert_eq!(
        scene.controller().remote().title(),
        Some("Careful With That Axe")
    );
    assert_eq!(
        scene.controller().remote().artist(),
        Some("Simble Ensemble")
    );
    assert_eq!(
        scene
            .controller()
            .remote()
            .attribute(media_attribute_id::ALBUM_NAME),
        Some("Unreachable Profiles")
    );
}

#[test]
fn test_next_track_changes_what_the_phone_reports() {
    let mut scene = RemoteControlScene::new();
    assert!(scene.run_until_connected(STEPS));

    scene.controller_mut().next_track();
    let advanced = scene.run_until(STEPS, |scene| {
        scene.target().track().map(|t| t.title.as_str()) == Some("Continuation State")
    });
    assert!(advanced, "FORWARD did not advance the playlist");

    // Read it back over the wire rather than trusting the target's own field.
    scene.controller_mut().query_metadata(&[]);
    let read = scene.run_until(STEPS, |scene| {
        scene.controller().remote().title() == Some("Continuation State")
    });
    assert!(read);
    assert_eq!(scene.controller().remote().artist(), Some("The Fragmented"));
}

#[test]
fn test_play_status_comes_back_with_the_track_length() {
    let mut scene = RemoteControlScene::new();
    assert!(scene.run_until_connected(STEPS));
    scene.target_mut().set_playback_status(play_status::PLAYING);

    scene.controller_mut().query_play_status();
    let answered = scene.run_until(STEPS, |scene| {
        scene.controller().remote().song_length.is_some()
    });
    assert!(answered, "GetPlayStatus never answered");
    assert_eq!(scene.controller().remote().song_length, Some(213_000));
    assert_eq!(
        scene.controller().remote().playback_status,
        Some(play_status::PLAYING)
    );
}

#[test]
fn test_a_notification_registration_is_answered_and_then_fires() {
    let mut scene = RemoteControlScene::new();
    assert!(scene.run_until_connected(STEPS));

    scene
        .controller_mut()
        .monitor(event_id::PLAYBACK_STATUS_CHANGED);
    let registered = scene.run_until(STEPS, |scene| {
        scene.target().events().iter().any(|event| {
            matches!(
                event,
                AvrcpEvent::NotificationRegistered {
                    event_id: event_id::PLAYBACK_STATUS_CHANGED
                }
            )
        })
    });
    assert!(registered, "the target never saw the registration");
    // The INTERIM snapshot tells the controller where the player is *now*,
    // before anything has changed. It has to come back across the link.
    let snapshot = scene.run_until(STEPS, |scene| {
        scene.controller().remote().playback_status.is_some()
    });
    assert!(snapshot, "the INTERIM snapshot never arrived");
    assert_eq!(
        scene.controller().remote().playback_status,
        Some(play_status::STOPPED)
    );

    // Now the phone starts playing from its own screen — no key involved.
    scene.target_mut().set_playback_status(play_status::PLAYING);
    let fired = scene.run_until(STEPS, |scene| {
        scene.controller().remote().playback_status == Some(play_status::PLAYING)
    });
    assert!(fired, "the CHANGED notification never arrived");

    // A CHANGED spends the registration. `monitor` re-arms it, so a *second*
    // change must arrive too — the thing that silently stops working if it
    // does not.
    scene.target_mut().set_playback_status(play_status::PAUSED);
    let fired_again = scene.run_until(STEPS, |scene| {
        scene.controller().remote().playback_status == Some(play_status::PAUSED)
    });
    assert!(
        fired_again,
        "the registration was not re-armed after the first CHANGED"
    );
}

#[test]
fn test_metadata_too_big_for_one_frame_still_arrives_over_a_real_channel() {
    // The continuation path, driven through L2CAP rather than back-to-back:
    // a title long enough that the response has to be fragmented and pulled
    // across with RequestContinuingResponse.
    let long_title = "The Interminable Ballad of the Unread Continuation State ".repeat(12);
    let mut scene = RemoteControlScene::with_player(vec![Track::new(
        &long_title,
        "Simble Ensemble",
        "Unreachable Profiles",
        400_000,
    )]);
    assert!(scene.run_until_connected(STEPS));

    scene.controller_mut().query_metadata(&[]);
    let read = scene.run_until(STEPS, |scene| scene.controller().remote().title().is_some());
    assert!(read, "the fragmented metadata never arrived");
    assert_eq!(scene.controller().remote().title(), Some(&*long_title));
    assert!(
        scene.controller().remote().title().unwrap().len() > 512,
        "the title has to exceed one AV/C frame or this proves nothing"
    );
}

// ---------------------------------------------------------------------------
// A2DP and AVRCP on the same link
// ---------------------------------------------------------------------------

#[test]
fn test_audio_and_transport_controls_share_one_link() {
    let mut scene = MediaPlayerScene::new();
    assert!(
        scene.run_until_ready(STEPS),
        "audio and control never both came up: phase {:?}, source {:?}, error {:?}",
        scene.phase(),
        scene.source().phase(),
        scene.error()
    );
    assert_eq!(scene.source().phase(), SourcePhase::Streaming);
    assert!(scene.sink().has_media_channel());
    assert!(scene.remote().is_connected());
    // Three L2CAP channels on one ACL: AVDTP signalling, AVDTP media
    // transport, and AVCTP control.
    assert!(
        scene
            .phone_host()
            .channel_is_open(simble::classic::avdtp::AVDTP_PSM)
    );
    assert!(
        scene
            .phone_host()
            .channel_is_open(simble::classic::avctp::AVCTP_PSM)
    );
}

#[test]
fn test_a_pause_from_the_speaker_stops_the_audio() {
    let mut scene = MediaPlayerScene::new();
    assert!(scene.run_until_ready(STEPS));

    // Stream for a while, and check that audio is genuinely flowing first —
    // otherwise "it stopped" is trivially true.
    for _ in 0..60 {
        assert!(scene.play(&pcm(128)), "the player should be playing");
        scene.tick();
    }
    let before = scene.sink_mut().take_frames();
    assert!(
        !before.is_empty(),
        "no SBC reached the speaker, so there was nothing to stop"
    );
    let audio = simble::device::A2dpSink::decode(&before);
    assert!(
        audio.frames > 0,
        "the frames that arrived did not decode, so they were not audio"
    );

    // Press PAUSE on the speaker.
    scene.remote_mut().pause();
    let paused = scene.run_until(STEPS, |scene| !scene.player().is_playing());
    assert!(paused, "the phone never saw the speaker's PAUSE");
    assert_eq!(scene.player().playback_status(), play_status::PAUSED);
    assert_eq!(scene.player().key_presses(), vec![operation_id::PAUSE]);

    // Keep offering audio. The player is paused, so it refuses to take it,
    // and nothing more reaches the speaker.
    let _ = scene.sink_mut().take_frames();
    for _ in 0..60 {
        assert!(
            !scene.play(&pcm(128)),
            "a paused player must not accept samples"
        );
        scene.tick();
    }
    assert!(
        scene.sink_mut().take_frames().is_empty(),
        "the speaker was still receiving audio after it asked for a PAUSE"
    );

    // PLAY starts it again — a pause that could not be undone would pass the
    // assertion above for the wrong reason.
    scene.remote_mut().play();
    let resumed = scene.run_until(STEPS, |scene| scene.player().is_playing());
    assert!(resumed, "PLAY did not restart the player");
    for _ in 0..60 {
        assert!(scene.play(&pcm(128)));
        scene.tick();
    }
    assert!(
        !scene.sink_mut().take_frames().is_empty(),
        "audio did not resume after PLAY"
    );
}

#[test]
fn test_the_speaker_can_read_the_phones_track_while_streaming() {
    let mut scene = MediaPlayerScene::new();
    assert!(scene.run_until_ready(STEPS));

    for _ in 0..40 {
        scene.play(&pcm(128));
        scene.tick();
    }
    scene.remote_mut().query_metadata(&[]);
    let read = scene.run_until(STEPS, |scene| scene.remote().remote().title().is_some());
    assert!(read, "metadata did not answer while audio was flowing");
    assert_eq!(
        scene.remote().remote().title(),
        Some("Careful With That Axe")
    );
    // And the audio kept moving across the other channel throughout.
    assert!(
        scene.sink().frame_count() > 0 || {
            for _ in 0..20 {
                scene.play(&pcm(128));
                scene.tick();
            }
            scene.sink().frame_count() > 0
        }
    );
}
