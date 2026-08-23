// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Hands-Free Profile (HFP) tests, ported from Bumble's `hfp_test.py`.
//!
//! Bumble's `test_sco_setup` drives `HCI_Enhanced_Setup_Synchronous_
//! Connection_Command` against a controller. Simble now has one — see
//! `src/controller/sim.rs` for the HCI side and `src/device/car_kit.rs` for
//! the same procedure end to end over a simulated link — so what is tested
//! here instead is the *profile's* half of it: the audio-connection state
//! machine, which is the part `hfp.rs` owns. Every other scenario from
//! `hfp_test.py` (24 tests total) is ported below.

use simble::classic::hfp::{
    AgConfiguration, AgIndicator, AgIndicatorState, AgProtocol, AudioCodec, AudioConnectionState,
    CallHoldOperation, CallInfo, CallInfoDirection, CallInfoMode, CallInfoMultiParty,
    CallInfoStatus, CallLineIdentification, HfConfiguration, HfIndicator, HfProtocol, HfpEvent,
    ProfileVersion, VoiceRecognitionState, ag_feature, ag_sdp_feature, find_ag_sdp_record,
    find_hf_sdp_record, hf_feature, hf_sdp_feature, make_ag_sdp_records, make_hf_sdp_records,
    parse_call_infos, parse_network_operator,
};
use simble::classic::sdp::{SdpClient, SdpServer};

fn default_hf_configuration() -> HfConfiguration {
    HfConfiguration {
        supported_hf_features: hf_feature::CODEC_NEGOTIATION
            | hf_feature::ESCO_S4_SETTINGS_SUPPORTED
            | hf_feature::HF_INDICATORS
            | hf_feature::ENHANCED_CALL_STATUS
            | hf_feature::THREE_WAY_CALLING
            | hf_feature::CLI_PRESENTATION_CAPABILITY,
        supported_hf_indicators: vec![HfIndicator::EnhancedSafety, HfIndicator::BatteryLevel],
        supported_audio_codecs: vec![AudioCodec::Cvsd, AudioCodec::Msbc],
    }
}

fn default_hf_sdp_features() -> u16 {
    hf_sdp_feature::WIDE_BAND_SPEECH
        | hf_sdp_feature::THREE_WAY_CALLING
        | hf_sdp_feature::CLI_PRESENTATION_CAPABILITY
}

fn default_ag_configuration() -> AgConfiguration {
    AgConfiguration {
        supported_ag_features: ag_feature::HF_INDICATORS
            | ag_feature::IN_BAND_RING_TONE_CAPABILITY
            | ag_feature::REJECT_CALL
            | ag_feature::CODEC_NEGOTIATION
            | ag_feature::ESCO_S4_SETTINGS_SUPPORTED
            | ag_feature::ENHANCED_CALL_STATUS
            | ag_feature::THREE_WAY_CALLING,
        supported_ag_indicators: vec![
            AgIndicatorState::call(),
            AgIndicatorState::service(),
            AgIndicatorState::callsetup(),
            AgIndicatorState::callsetup(),
            AgIndicatorState::signal(),
            AgIndicatorState::roam(),
            AgIndicatorState::battchg(),
        ],
        supported_hf_indicators: vec![HfIndicator::EnhancedSafety, HfIndicator::BatteryLevel],
        supported_ag_call_hold_operations: vec![
            CallHoldOperation::AddHeldCall,
            CallHoldOperation::HoldAllActiveCalls,
            CallHoldOperation::HoldAllCallsExcept,
            CallHoldOperation::ReleaseAllActiveCalls,
            CallHoldOperation::ReleaseAllHeldCalls,
            CallHoldOperation::ReleaseSpecificCall,
            CallHoldOperation::ConnectTwoCalls,
        ],
        supported_audio_codecs: vec![AudioCodec::Cvsd, AudioCodec::Msbc],
    }
}

fn default_ag_sdp_features() -> u16 {
    ag_sdp_feature::WIDE_BAND_SPEECH
        | ag_sdp_feature::IN_BAND_RING_TONE_CAPABILITY
        | ag_sdp_feature::THREE_WAY_CALLING
}

/// Drives the Service Level Connection procedure to completion by
/// alternately feeding each side's output into the other, the same
/// `drive_*` pattern used in `rfcomm.rs`'s own tests.
fn drive_slc(hf: &mut HfProtocol, ag: &mut AgProtocol) {
    let mut to_ag = vec![hf.start_slc()];
    for _ in 0..20 {
        let mut to_hf = Vec::new();
        for line in to_ag.drain(..) {
            let (out, _) = ag.receive(&line);
            to_hf.extend(out);
        }
        let mut next_to_ag = Vec::new();
        let mut done = false;
        for line in to_hf {
            let (out, events) = hf.receive(&line);
            next_to_ag.extend(out);
            if events.contains(&HfpEvent::SlcComplete) {
                done = true;
            }
        }
        to_ag = next_to_ag;
        if done && to_ag.is_empty() {
            return;
        }
    }
    panic!("SLC did not complete within the round-trip budget");
}

fn make_hfp_connections(
    hf_config: HfConfiguration,
    ag_config: AgConfiguration,
) -> (HfProtocol, AgProtocol) {
    let mut hf = HfProtocol::new(hf_config);
    let mut ag = AgProtocol::new(ag_config);
    drive_slc(&mut hf, &mut ag);
    (hf, ag)
}

fn default_hfp_connections() -> (HfProtocol, AgProtocol) {
    make_hfp_connections(default_hf_configuration(), default_ag_configuration())
}

#[test]
fn test_slc_with_minimal_features() {
    let (hf, ag) = make_hfp_connections(
        HfConfiguration {
            supported_hf_features: 0,
            supported_hf_indicators: Vec::new(),
            supported_audio_codecs: Vec::new(),
        },
        AgConfiguration {
            supported_ag_features: 0,
            supported_ag_indicators: vec![AgIndicatorState::call()],
            supported_hf_indicators: Vec::new(),
            supported_ag_call_hold_operations: Vec::new(),
            supported_audio_codecs: Vec::new(),
        },
    );

    assert_eq!(hf.supported_ag_features, ag.supported_ag_features);
    assert_eq!(hf.supported_hf_features, ag.supported_hf_features);
    assert_eq!(
        hf.supported_ag_call_hold_operations,
        ag.supported_ag_call_hold_operations
    );
    for (a, b) in hf.ag_indicators.iter().zip(ag.ag_indicators.iter()) {
        assert_eq!(a.indicator, b.indicator);
        assert_eq!(a.current_status, b.current_status);
    }
}

#[test]
fn test_slc() {
    let (hf, ag) = default_hfp_connections();

    assert_eq!(hf.supported_ag_features, ag.supported_ag_features);
    assert_eq!(hf.supported_hf_features, ag.supported_hf_features);
    assert_eq!(
        hf.supported_ag_call_hold_operations,
        ag.supported_ag_call_hold_operations
    );
    for (a, b) in hf.ag_indicators.iter().zip(ag.ag_indicators.iter()) {
        assert_eq!(a.indicator, b.indicator);
        assert_eq!(a.current_status, b.current_status);
    }
}

#[test]
fn test_ag_indicator() {
    let (mut hf, mut ag) = default_hfp_connections();

    let ciev = ag.update_ag_indicator(AgIndicator::Call, 1).unwrap();
    let (_, events) = hf.receive(&ciev);

    let updated = events
        .into_iter()
        .find_map(|e| match e {
            HfpEvent::AgIndicatorUpdated(state) => Some(state),
            _ => None,
        })
        .expect("ag_indicator event");
    assert_eq!(updated.current_status, 1);
    assert_eq!(updated.indicator, AgIndicator::Call);
}

#[test]
fn test_hf_indicator() {
    let (mut hf, mut ag) = default_hfp_connections();

    let cmd = hf.set_hf_indicator(HfIndicator::BatteryLevel, 100);
    let (_, events) = ag.receive(&cmd);

    let updated = events
        .into_iter()
        .find_map(|e| match e {
            HfpEvent::HfIndicatorUpdated(state) => Some(state),
            _ => None,
        })
        .expect("hf_indicator event");
    assert_eq!(updated.current_status, 100);
}

#[test]
fn test_codec_negotiation() {
    let (mut hf, mut ag) = default_hfp_connections();

    let bcs = ag.negotiate_codec(AudioCodec::Msbc);
    let (out, hf_events) = hf.receive(&bcs);
    let hf_codec = hf_events
        .into_iter()
        .find_map(|e| match e {
            HfpEvent::CodecNegotiated(codec) => Some(codec),
            _ => None,
        })
        .expect("hf codec_negotiation event");

    let (_, ag_events) = ag.receive(&out[0]);
    let ag_codec = ag_events
        .into_iter()
        .find_map(|e| match e {
            HfpEvent::CodecNegotiated(codec) => Some(codec),
            _ => None,
        })
        .expect("ag codec_negotiation event");

    assert_eq!(hf_codec, ag_codec);
    assert_eq!(hf_codec, AudioCodec::Msbc);
}

#[test]
fn test_dial() {
    let (mut hf, mut ag) = default_hfp_connections();
    const NUMBER: &str = "ATD123456789";

    let cmd = hf.dial(NUMBER);
    let (_, events) = ag.receive(&cmd);

    let dialed = events
        .into_iter()
        .find_map(|e| match e {
            HfpEvent::Dial(number) => Some(number),
            _ => None,
        })
        .expect("dial event");
    assert_eq!(dialed, NUMBER);
}

#[test]
fn test_answer() {
    let (mut hf, mut ag) = default_hfp_connections();
    let cmd = hf.answer_incoming_call();
    let (_, events) = ag.receive(&cmd);
    assert!(events.contains(&HfpEvent::Answer));
}

#[test]
fn test_reject_incoming_call() {
    let (mut hf, mut ag) = default_hfp_connections();
    let cmd = hf.reject_incoming_call();
    let (_, events) = ag.receive(&cmd);
    assert!(events.contains(&HfpEvent::HangUp));
}

#[test]
fn test_terminate_call() {
    let (mut hf, mut ag) = default_hfp_connections();
    let cmd = hf.terminate_call();
    let (_, events) = ag.receive(&cmd);
    assert!(events.contains(&HfpEvent::HangUp));
}

#[test]
fn test_query_calls_without_calls() {
    let (mut hf, mut ag) = default_hfp_connections();
    let cmd = hf.query_current_calls();
    let (out, _) = ag.receive(&cmd);
    let mut responses = None;
    for line in out {
        let (_, events) = hf.receive(&line);
        for event in events {
            if let HfpEvent::CommandCompleted {
                ok, responses: r, ..
            } = event
            {
                assert!(ok);
                responses = Some(r);
            }
        }
    }
    assert_eq!(
        parse_call_infos(&responses.expect("command completed")),
        Vec::new()
    );
}

#[test]
fn test_query_calls_with_calls() {
    let (mut hf, mut ag) = default_hfp_connections();
    ag.calls.push(CallInfo {
        index: 1,
        direction: CallInfoDirection::MobileOriginated,
        status: CallInfoStatus::Active,
        mode: CallInfoMode::Voice,
        multi_party: CallInfoMultiParty::NotInConference,
        number: Some("123456789".to_string()),
        kind: None,
    });

    let cmd = hf.query_current_calls();
    let (out, _) = ag.receive(&cmd);
    let mut responses = None;
    for line in out {
        let (_, events) = hf.receive(&line);
        for event in events {
            if let HfpEvent::CommandCompleted {
                ok, responses: r, ..
            } = event
            {
                assert!(ok);
                responses = Some(r);
            }
        }
    }
    assert_eq!(
        parse_call_infos(&responses.expect("command completed")),
        ag.calls
    );
}

#[test]
fn test_hold_call_without_call_index() {
    for operation in [
        CallHoldOperation::ReleaseAllHeldCalls,
        CallHoldOperation::ReleaseAllActiveCalls,
        CallHoldOperation::HoldAllActiveCalls,
        CallHoldOperation::AddHeldCall,
        CallHoldOperation::ConnectTwoCalls,
    ] {
        let (mut hf, mut ag) = default_hfp_connections();
        let cmd = hf.hold_call(operation, None);
        let (_, events) = ag.receive(&cmd);
        assert!(
            events.contains(&HfpEvent::CallHold {
                operation,
                call_index: None
            }),
            "operation {operation:?} did not produce a matching call_hold event"
        );
    }
}

#[test]
fn test_hold_call_with_call_index() {
    for operation in [
        CallHoldOperation::ReleaseSpecificCall,
        CallHoldOperation::HoldAllCallsExcept,
    ] {
        let (mut hf, mut ag) = default_hfp_connections();
        ag.calls.push(CallInfo {
            index: 1,
            direction: CallInfoDirection::MobileOriginated,
            status: CallInfoStatus::Active,
            mode: CallInfoMode::Voice,
            multi_party: CallInfoMultiParty::NotInConference,
            number: Some("123456789".to_string()),
            kind: None,
        });

        let cmd = hf.hold_call(operation, Some(1));
        let (_, events) = ag.receive(&cmd);
        assert!(
            events.contains(&HfpEvent::CallHold {
                operation,
                call_index: Some(1)
            }),
            "operation {operation:?} did not produce a matching call_hold event"
        );
    }
}

#[test]
fn test_ring() {
    let (mut hf, ag) = default_hfp_connections();
    let ring = ag.send_ring();
    let (_, events) = hf.receive(&ring);
    assert!(events.contains(&HfpEvent::Ring));
}

#[test]
fn test_speaker_volume() {
    let (mut hf, mut ag) = default_hfp_connections();
    let cmd = ag.set_speaker_volume(10);
    let (_, events) = hf.receive(&cmd);
    assert!(events.contains(&HfpEvent::SpeakerVolume(10)));
}

#[test]
fn test_microphone_volume() {
    let (mut hf, mut ag) = default_hfp_connections();
    let cmd = ag.set_microphone_volume(10);
    let (_, events) = hf.receive(&cmd);
    assert!(events.contains(&HfpEvent::MicrophoneVolume(10)));
}

#[test]
fn test_cli_notification() {
    let (mut hf, mut ag) = default_hfp_connections();
    // Bumble's version of this test pre-quotes the number and the name,
    // because Bumble's `to_clip_string` does not quote string parameters
    // itself. Simble does (TS 27.007 7.6 makes them string-typed, and a real
    // HF parses them with a quoted-string reader), so the caller passes
    // plain values and gets plain values back.
    let cli = CallLineIdentification {
        number: "123456789".to_string(),
        kind: 129,
        subaddr: None,
        satype: None,
        alpha: Some("Bumble".to_string()),
        cli_validity: None,
    };
    let cmd = ag.send_cli_notification(&cli);
    assert_eq!(
        String::from_utf8_lossy(&cmd),
        "\r\n+CLIP: \"123456789\",129,,,\"Bumble\"\r\n"
    );
    let (_, events) = hf.receive(&cmd);

    let received = events
        .into_iter()
        .find_map(|e| match e {
            HfpEvent::CliNotification(cli) => Some(cli),
            _ => None,
        })
        .expect("cli_notification event");
    assert_eq!(
        received,
        CallLineIdentification {
            number: "123456789".to_string(),
            kind: 129,
            subaddr: Some(String::new()),
            satype: None,
            alpha: Some("Bumble".to_string()),
            cli_validity: None,
        }
    );
}

#[test]
fn test_a_bare_caller_id_is_quoted_the_way_a_real_hands_free_expects_to_read_it() {
    let mut ag = AgProtocol::new(default_ag_configuration());
    let cli = CallLineIdentification {
        number: "+15551234".to_string(),
        kind: 129,
        subaddr: None,
        satype: None,
        alpha: None,
        cli_validity: None,
    };
    // Zephyr's AG emits exactly this shape, and its HF reads the number
    // with at_get_string(); trailing empty optional fields are not sent.
    assert_eq!(
        String::from_utf8_lossy(&ag.send_cli_notification(&cli)),
        "\r\n+CLIP: \"+15551234\",129\r\n"
    );
}

#[test]
fn test_the_operator_name_can_only_be_read_after_its_format_is_selected() {
    let (mut hf, mut ag) = default_hfp_connections();
    ag.network_operator = "Simble Mobile".into();

    // HFP v1.9 4.7 orders these: AT+COPS=3,0 first, then the read. A bare
    // read is an ordering error, and with extended errors off that is a
    // plain ERROR.
    let cmd = hf.query_network_operator();
    let (out, _) = ag.receive(&cmd);
    assert_eq!(String::from_utf8_lossy(&out[0]), "\r\nERROR\r\n");

    let cmd = hf.select_operator_format();
    let (out, _) = ag.receive(&cmd);
    assert_eq!(String::from_utf8_lossy(&out[0]), "\r\nOK\r\n");

    let cmd = hf.query_network_operator();
    let (out, _) = ag.receive(&cmd);
    assert_eq!(
        String::from_utf8_lossy(&out[0]),
        "\r\n+COPS: 0,0,\"Simble Mobile\"\r\n"
    );
}

#[test]
fn test_the_hands_free_reads_the_operator_name_out_of_the_response() {
    let (mut hf, mut ag) = default_hfp_connections();
    ag.network_operator = "Simble Mobile".into();
    let cmd = hf.select_operator_format();
    let (out, _) = ag.receive(&cmd);
    for line in out {
        hf.receive(&line);
    }

    let cmd = hf.query_network_operator();
    let (out, _) = ag.receive(&cmd);
    let mut operator = None;
    for line in out {
        for event in hf.receive(&line).1 {
            if let HfpEvent::CommandCompleted { responses, .. } = event {
                operator = parse_network_operator(&responses);
            }
        }
    }
    assert_eq!(operator.as_deref(), Some("Simble Mobile"));
}

#[test]
fn test_selecting_any_format_but_the_long_alphanumeric_one_is_refused() {
    let (mut hf, mut ag) = default_hfp_connections();
    // Mode 0 would ask the AG to actually select a network, which an HF is
    // not allowed to do.
    let cmd = hf.send_command("AT+COPS=0,0");
    let (out, _) = ag.receive(&cmd);
    assert_eq!(String::from_utf8_lossy(&out[0]), "\r\nERROR\r\n");
}

#[test]
fn test_voice_recognition_from_hf() {
    let (mut hf, mut ag) = default_hfp_connections();
    let cmd = hf.set_voice_recognition(VoiceRecognitionState::Enable);
    let (_, events) = ag.receive(&cmd);
    assert!(events.contains(&HfpEvent::VoiceRecognition(VoiceRecognitionState::Enable)));
}

#[test]
fn test_voice_recognition_from_ag() {
    let (mut hf, ag) = default_hfp_connections();
    let cmd = ag.send_response("+BVRA: 1");
    let (_, events) = hf.receive(&cmd);
    assert!(events.contains(&HfpEvent::VoiceRecognition(VoiceRecognitionState::Enable)));
}

#[test]
fn test_hf_sdp_record() {
    let mut sdp_server = SdpServer::new();
    sdp_server.service_records.insert(
        1,
        make_hf_sdp_records(1, 2, &default_hf_configuration(), ProfileVersion::V1_8),
    );
    let mut sdp_client = SdpClient::new();
    let found =
        find_hf_sdp_record(&mut sdp_client, |req| sdp_server.handle_request(req, 1024)).unwrap();
    assert_eq!(
        found,
        Some((2, ProfileVersion::V1_8, default_hf_sdp_features()))
    );
}

#[test]
fn test_ag_sdp_record() {
    let mut sdp_server = SdpServer::new();
    sdp_server.service_records.insert(
        1,
        make_ag_sdp_records(1, 2, &default_ag_configuration(), ProfileVersion::V1_8),
    );
    let mut sdp_client = SdpClient::new();
    let found =
        find_ag_sdp_record(&mut sdp_client, |req| sdp_server.handle_request(req, 1024)).unwrap();
    assert_eq!(
        found,
        Some((2, ProfileVersion::V1_8, default_ag_sdp_features()))
    );
}

#[test]
fn test_hf_batched_response() {
    let mut hf = HfProtocol::new(default_hf_configuration());
    hf.send_command("AT+BIND=?");
    let (_, events) = hf.receive(b"\r\n+BIND: (1,2)\r\n\r\nOK\r\n");
    assert!(matches!(
        events.as_slice(),
        [HfpEvent::CommandCompleted { ok: true, .. }]
    ));
}

#[test]
fn test_ag_batched_commands() {
    let mut ag = AgProtocol::new(default_ag_configuration());
    let (_, events) = ag.receive(b"ATA\rAT+CHUP\r");
    assert!(events.contains(&HfpEvent::Answer));
    assert!(events.contains(&HfpEvent::HangUp));
}

// ---------------------------------------------------------------------------
// The audio connection
// ---------------------------------------------------------------------------

#[test]
fn test_the_codec_connection_procedure_precedes_the_synchronous_link() {
    // HFP v1.9 4.11.3: `+BCS`, then the HF's `AT+BCS`, and only then does
    // the AG open a synchronous connection. An AG that opened the link
    // first would be opening it for a codec the HF has not agreed to.
    let (mut hf, mut ag) = default_hfp_connections();
    assert_eq!(ag.audio_state(), AudioConnectionState::Disconnected);

    let (outgoing, events) = ag.start_audio_connection();
    assert_eq!(ag.audio_state(), AudioConnectionState::Negotiating);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, HfpEvent::AudioConnectionRequested(_))),
        "the link must not be asked for before the codec is settled"
    );
    assert_eq!(outgoing.len(), 1);
    assert_eq!(
        outgoing[0], b"\r\n+BCS: 2\r\n",
        "mSBC, the wider of the two"
    );

    // The HF confirms, and only now is the transport asked for a link.
    let (to_ag, _) = hf.receive(&outgoing[0]);
    assert_eq!(to_ag[0], b"AT+BCS=2\r");
    let (_, events) = ag.receive(&to_ag[0]);
    assert!(
        events.contains(&HfpEvent::AudioConnectionRequested(AudioCodec::Msbc)),
        "{events:?}"
    );
    assert_eq!(ag.audio_state(), AudioConnectionState::Connecting);
    assert_eq!(hf.audio_state(), AudioConnectionState::Connecting);

    // "Connecting" is not "connected". Only the transport can say that.
    ag.on_audio_connected();
    hf.on_audio_connected();
    assert_eq!(ag.audio_state(), AudioConnectionState::Connected);
    assert_eq!(hf.audio_state(), AudioConnectionState::Connected);
}

#[test]
fn test_without_codec_negotiation_the_link_is_asked_for_straight_away() {
    // No `+BCS` exists to send: CVSD is the only codec HFP guarantees, and
    // an AG that waited for a confirmation nobody will send waits forever.
    let (_, mut ag) = make_hfp_connections(
        HfConfiguration {
            supported_hf_features: 0,
            supported_hf_indicators: Vec::new(),
            supported_audio_codecs: Vec::new(),
        },
        AgConfiguration {
            supported_ag_features: 0,
            supported_ag_indicators: vec![AgIndicatorState::call()],
            supported_hf_indicators: Vec::new(),
            supported_ag_call_hold_operations: Vec::new(),
            supported_audio_codecs: Vec::new(),
        },
    );

    let (outgoing, events) = ag.start_audio_connection();
    assert!(outgoing.is_empty(), "there is nothing to negotiate");
    assert!(events.contains(&HfpEvent::AudioConnectionRequested(AudioCodec::Cvsd)));
    assert_eq!(ag.audio_state(), AudioConnectionState::Connecting);
}

#[test]
fn test_the_ag_picks_only_from_what_the_hf_said_it_can_decode() {
    // The bug this catches: `AT+BAC` used to overwrite the AG's *own* codec
    // list, so there was one list where there should be two and no
    // intersection to take. Nothing noticed while the choice was never
    // acted on.
    let (_, ag) = make_hfp_connections(
        HfConfiguration {
            // A narrowband-only HF.
            supported_audio_codecs: vec![AudioCodec::Cvsd],
            ..default_hf_configuration()
        },
        default_ag_configuration(),
    );
    assert_eq!(
        ag.supported_audio_codecs,
        vec![AudioCodec::Cvsd, AudioCodec::Msbc],
        "the AG's own list survives the HF's AT+BAC"
    );
    assert_eq!(ag.hf_audio_codecs, vec![AudioCodec::Cvsd]);

    let mut ag = ag;
    let (outgoing, _) = ag.start_audio_connection();
    assert_eq!(
        outgoing[0], b"\r\n+BCS: 1\r\n",
        "CVSD: offering mSBC to an HF that cannot decode it is how a call \
         comes up silent"
    );
}

#[test]
fn test_at_bcc_makes_the_ag_open_the_link_the_hf_may_not_open_itself() {
    // HFP gives establishing the audio connection to the AG alone, so the
    // HF's only move is to ask. `OK` answers the command; the `+BCS` that
    // follows is unsolicited and must come after it.
    let (mut hf, mut ag) = default_hfp_connections();
    let request = hf.setup_audio_connection();
    let (outgoing, events) = ag.receive(&request);

    assert!(events.contains(&HfpEvent::CodecConnectionRequested));
    assert!(events.contains(&HfpEvent::AudioConnectionState(
        AudioConnectionState::Negotiating
    )));
    assert_eq!(outgoing[0], b"\r\nOK\r\n", "the command is answered first");
    assert_eq!(outgoing[1], b"\r\n+BCS: 2\r\n");
}

#[test]
fn test_a_second_trigger_does_not_start_a_second_procedure() {
    // A call that rings and is then answered triggers audio twice. The
    // second must be a no-op, not a `+BCS` racing the first exchange.
    let (_, mut ag) = default_hfp_connections();
    let (first, _) = ag.start_audio_connection();
    assert_eq!(first.len(), 1);
    let (second, events) = ag.start_audio_connection();
    assert!(second.is_empty(), "{second:?}");
    assert!(events.is_empty(), "{events:?}");
}

#[test]
fn test_the_codec_decides_the_voice_setting_and_packet_types() {
    // The seam between the profile and HCI: two numbers, and getting either
    // wrong makes a controller build the wrong kind of link.
    assert_eq!(AudioCodec::Cvsd.voice_setting(), 0x0060);
    assert!(!AudioCodec::Cvsd.requires_esco());
    assert_eq!(AudioCodec::Cvsd.esco_packet_type(), 0x0007, "HV1|HV2|HV3");

    for wideband in [AudioCodec::Msbc, AudioCodec::Lc3Swb] {
        assert_eq!(
            wideband.voice_setting(),
            0x0063,
            "{wideband:?} is encoded by the host, so the controller must be \
             told to pass it through untouched"
        );
        assert!(wideband.requires_esco());
        assert_eq!(wideband.esco_packet_type(), 0x0008, "EV3");
    }
}

#[test]
fn test_losing_the_audio_leaves_the_service_level_connection_alone() {
    let (mut hf, mut ag) = default_hfp_connections();
    let (outgoing, _) = ag.start_audio_connection();
    let (to_ag, _) = hf.receive(&outgoing[0]);
    ag.receive(&to_ag[0]);
    ag.on_audio_connected();
    hf.on_audio_connected();

    let events = ag.on_audio_disconnected();
    assert!(events.contains(&HfpEvent::AudioConnectionState(
        AudioConnectionState::Disconnected
    )));
    hf.on_audio_disconnected();
    assert_eq!(ag.audio_state(), AudioConnectionState::Disconnected);

    // And the SLC is untouched: a `+CIEV` still crosses, and the audio can
    // be brought back without redoing any of it.
    let line = ag
        .update_ag_indicator(AgIndicator::Call, 1)
        .expect("the AG still has its indicators");
    let (_, events) = hf.receive(&line);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, HfpEvent::AgIndicatorUpdated(_))),
        "{events:?}"
    );
    let (again, _) = ag.start_audio_connection();
    assert_eq!(again.len(), 1, "a second call costs one codec exchange");
}
