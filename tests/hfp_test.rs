// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Hands-Free Profile (HFP) tests, ported from Bumble's `hfp_test.py`.
//!
//! `test_sco_setup` is not ported: it exercises `HCI_Enhanced_Setup_
//! Synchronous_Connection_Command`/eSCO connection establishment end to
//! end, which requires a simulated SCO/eSCO audio transport that Simble
//! does not have (HFP here is signaling-only, matching the scope call in
//! the module doc comment of `src/classic/hfp.rs`). Every other scenario
//! from `hfp_test.py` (24 tests total) is ported below.

use simble::classic::hfp::{
    AgConfiguration, AgIndicator, AgIndicatorState, AgProtocol, AudioCodec, CallHoldOperation,
    CallInfo, CallInfoDirection, CallInfoMode, CallInfoMultiParty, CallInfoStatus,
    CallLineIdentification, HfConfiguration, HfIndicator, HfProtocol, HfpEvent, ProfileVersion,
    VoiceRecognitionState, ag_feature, ag_sdp_feature, find_ag_sdp_record, find_hf_sdp_record,
    hf_feature, hf_sdp_feature, make_ag_sdp_records, make_hf_sdp_records, parse_call_infos,
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
    let cli = CallLineIdentification {
        number: "\"123456789\"".to_string(),
        kind: 129,
        subaddr: None,
        satype: None,
        alpha: Some("\"Bumble\"".to_string()),
        cli_validity: None,
    };
    let cmd = ag.send_cli_notification(&cli);
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
