// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A2DP through the real simulated BR/EDR link.
//!
//! Every byte these tests assert on crossed [`simble::controller::sim`]: two
//! `ClassicHost`s, an inquiry, a page, an ACL, two L2CAP channel handshakes,
//! AVDTP signalling and RTP media. Nothing is wired directly together —
//! `test_a_speaker_that_cannot_be_found_is_not_streamed_to` is what proves
//! it, by turning off one scan and watching the whole thing fail.
//!
//! The audio is not invented. The source encodes PCM with
//! `simble::audio::sbc::SbcEncoder` and the sink decodes it with
//! `SbcDecoder`, both of which are verified against bluez's `libsbc` in
//! `tests/sbc_interop_test.rs` — so PCM that comes back out the far end is
//! evidence that what crossed the link was a real SBC stream.

use simble::classic::avdtp::{AvdtpEvent, StreamState, error_code, signal_identifier};
use simble::device::a2dp::{A2dpSink, SourcePhase, sbc_full_capability};
use simble::device::classic_host::scan_enable;
use simble::device::profile_scene::LinkPhase;
use simble::device::speaker_scene::SpeakerScene;

/// Steps to give the scene. Bring-up is dozens of HCI round trips with
/// nothing to show for them; 4000 is far more than it takes and the tests
/// that pass stop early anyway.
const STEPS: usize = 4000;

/// A second of a 440 Hz sine at 44.1 kHz, interleaved stereo — enough to
/// make many whole SBC frames, which is what the packetiser needs to have
/// anything to do.
fn tone(samples: usize) -> Vec<i16> {
    (0..samples)
        .flat_map(|n| {
            let t = n as f64 / 44100.0;
            let v = ((2.0 * std::f64::consts::PI * 440.0 * t).sin() * 12000.0) as i16;
            [v, v]
        })
        .collect()
}

#[test]
fn test_a_source_configures_opens_and_starts_a_stream_on_a_found_speaker() {
    let mut scene = SpeakerScene::new();
    assert!(
        scene.run_until_streaming(STEPS),
        "stream never started: link {:?}, source {:?}, error {:?} / {:?}",
        scene.phase(),
        scene.source().phase(),
        scene.error(),
        scene.source().error(),
    );

    assert_eq!(scene.phase(), LinkPhase::Connected);
    assert_eq!(scene.source().phase(), SourcePhase::Streaming);

    // The *sequence* is the claim, not the endpoint. AVDTP 6.x: nothing may
    // be configured before it is discovered, opened before it is configured,
    // or started before it is opened, and each step here was entered only
    // because the previous one's response arrived.
    let sink_events: Vec<&str> = scene
        .sink()
        .events()
        .iter()
        .map(|event| match event {
            AvdtpEvent::StreamConfigured { .. } => "configured",
            AvdtpEvent::StreamOpened { .. } => "opened",
            AvdtpEvent::StreamStarted { .. } => "started",
            AvdtpEvent::StreamSuspended { .. } => "suspended",
            AvdtpEvent::StreamClosed { .. } => "closed",
            AvdtpEvent::CommandRefused { .. } => "refused",
            _ => "other",
        })
        .collect();
    assert_eq!(
        sink_events,
        vec!["configured", "opened", "started"],
        "the sink saw the wrong sequence"
    );

    // Both ends agree the stream is up, and the media transport channel —
    // the *second* L2CAP channel on PSM 0x0019, which is the whole reason
    // dispatch had to learn about CIDs — is attached.
    assert_eq!(scene.sink().state(), StreamState::Streaming);
    assert!(
        scene.sink().has_media_channel(),
        "STREAMING with no transport channel is silence, not audio"
    );

    // And the codec they settled on is the one the source asked for.
    let configuration = scene.sink().configuration().expect("sink is configured");
    assert_eq!(configuration.sampling_frequency, 0b0010, "44.1 kHz only");
    assert_eq!(configuration.channel_mode, 0b0001, "joint stereo only");
}

#[test]
fn test_media_crosses_the_link_and_decodes_back_to_pcm() {
    let mut scene = SpeakerScene::new();
    assert!(scene.run_until_streaming(STEPS), "stream never started");

    // 4096 stereo sample pairs is exactly 32 SBC frames at 44.1 kHz with 16
    // blocks of 8 subbands: 128 samples per channel per frame.
    const SBC_FRAMES: usize = 32;
    scene.play(&tone(4096));

    // Done when the encoder has consumed all the PCM and every packet the
    // source wrote has been received. Several SBC frames ride in one RTP
    // packet, so the packet count is the thing to compare, not the frame
    // count — that difference is the A2DP payload header's whole job.
    let arrived = scene.run_until(STEPS, |scene| {
        scene.source().pcm_queued() == 0
            && scene.source().packets_sent() > 0
            && scene.sink().frame_count() == scene.source().packets_sent()
    });
    assert!(
        arrived,
        "{} of {} packets arrived, {} PCM samples left unencoded",
        scene.sink().frame_count(),
        scene.source().packets_sent(),
        scene.source().pcm_queued(),
    );

    let frames = scene.sink_mut().take_frames();
    // RTP sequence numbers are consecutive: a gap means the transport
    // channel dropped a packet, which on a lossless simulated link would be
    // a bug in the packetiser, not in the radio.
    for pair in frames.windows(2) {
        assert_eq!(
            pair[1].sequence_number,
            pair[0].sequence_number.wrapping_add(1),
            "media packets arrived out of order or with a gap"
        );
    }

    // The payload is real SBC: the libsbc-verified decoder turns it into
    // PCM. A frame of noise would fail the sync word or the CRC.
    let audio = A2dpSink::decode(&frames);
    assert_eq!(
        audio.frames,
        SBC_FRAMES,
        "the libsbc-verified decoder got {} whole SBC frames out of {} \
         payloads, not {SBC_FRAMES}",
        audio.frames,
        frames.len()
    );
    assert_eq!(
        audio.undecodable_bytes, 0,
        "the payloads did not end on a frame boundary"
    );
    assert_eq!(
        audio.pcm.len(),
        audio.frames * 256,
        "16 blocks x 8 subbands x 2 channels = 256 interleaved samples a frame"
    );
    // The tone is not silence — a decoder handed zeros would also return
    // the right *count*.
    assert!(
        audio.pcm.iter().any(|s| s.abs() > 1000),
        "the decoded audio is silent, so nothing survived the round trip"
    );
}

#[test]
fn test_a_speaker_that_cannot_be_found_is_not_streamed_to() {
    // Page scan only: connectable but not discoverable. The phone's inquiry
    // is a real inquiry over the simulated radio, so it finds nothing and
    // the plan stops in a named phase.
    let mut scene = SpeakerScene::with_speaker_scan_enable(A2dpSink::new(), scan_enable::PAGE_ONLY);
    scene.run_until(STEPS, |scene| scene.phase() == LinkPhase::Failed);

    assert_eq!(scene.phase(), LinkPhase::Failed);
    assert!(
        scene
            .error()
            .is_some_and(|e| e.starts_with("inquiry did not find")),
        "wrong reason: {:?}",
        scene.error()
    );
    assert_eq!(
        scene.source().phase(),
        SourcePhase::Connecting,
        "the profile never got a link to run on"
    );
    assert!(scene.sink().events().is_empty());
}

/// A sink that will only do 16 kHz mono.
fn fussy_sink() -> A2dpSink {
    let mut capability = sbc_full_capability();
    capability.sampling_frequency = 0b1000; // 16 kHz only
    capability.channel_mode = 0b1000; // mono only
    A2dpSink::with_capability(capability)
}

#[test]
fn test_a_source_does_not_configure_a_sink_it_cannot_match() {
    // Get_Capabilities comes back with nothing in common, so the source
    // gives up *before* Set_Configuration — the sink is never asked.
    let mut scene = SpeakerScene::with_sink(fussy_sink());
    scene.run_until(STEPS, |scene| {
        matches!(
            scene.source().phase(),
            SourcePhase::Failed | SourcePhase::Streaming
        )
    });

    assert_eq!(scene.source().phase(), SourcePhase::Failed);
    assert!(
        scene
            .source()
            .error()
            .is_some_and(|e| e.contains("no common SBC operating point")),
        "wrong reason: {:?}",
        scene.source().error()
    );
    assert_eq!(scene.sink().state(), StreamState::Idle);
    assert!(
        scene.sink().rejections().is_empty(),
        "the sink was asked something it should never have been asked"
    );
}

#[test]
fn test_a_refused_set_configuration_leaves_the_sink_idle() {
    // The same fussy sink, but a source that proposes 44.1 kHz joint stereo
    // without negotiating — which is what makes the sink's refusal path
    // reachable at all. Before this, the sink accepted it.
    let mut scene = SpeakerScene::with_sink(fussy_sink());
    scene
        .source_mut()
        .misconfigure_with(sbc_high_quality_configuration_for_test());

    scene.run_until(STEPS, |scene| {
        matches!(
            scene.source().phase(),
            SourcePhase::Failed | SourcePhase::Streaming
        )
    });

    assert_eq!(
        scene.source().phase(),
        SourcePhase::Failed,
        "a sink that cannot do 44.1 kHz joint stereo must not end up streaming"
    );

    // The point of the test: the refusal must leave the sink where it was.
    // A rejection that still mutates state is the bug shape this file exists
    // to catch, and it was exactly the bug that was here.
    assert_eq!(
        scene.sink().state(),
        StreamState::Idle,
        "a refused Set_Configuration moved the endpoint out of IDLE"
    );
    assert_eq!(
        scene.sink().configuration(),
        None,
        "a refused Set_Configuration left a configuration behind"
    );
    assert!(
        !scene.sink().has_media_channel(),
        "a refused Set_Configuration left a media transport attached"
    );

    // And it was refused for the stated reason, not by silence.
    assert_eq!(
        scene.sink().rejections(),
        vec![(
            signal_identifier::SET_CONFIGURATION,
            error_code::INVALID_CAPABILITIES
        )],
        "wrong refusal, or none: sink events were {:?}",
        scene.sink().events()
    );
    // No stream event at all: the endpoint never left IDLE.
    assert!(
        !scene
            .sink()
            .events()
            .iter()
            .any(|e| matches!(e, AvdtpEvent::StreamConfigured { .. })),
        "the sink reported a stream it refused to configure"
    );
}

/// 44.1 kHz joint stereo, 16 blocks, 8 subbands, loudness, bitpool 2..53 —
/// the configuration every phone picks, spelled out here so the test does
/// not depend on a private constant.
fn sbc_high_quality_configuration_for_test() -> simble::classic::a2dp::SbcMediaCodecInformation {
    simble::classic::a2dp::SbcMediaCodecInformation {
        sampling_frequency: 0b0010,
        channel_mode: 0b0001,
        block_length: 0b0001,
        subbands: 0b0001,
        allocation_method: 0b0001,
        minimum_bitpool_value: 2,
        maximum_bitpool_value: 53,
    }
}
