// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Port of Bumble's avdtp_test.py test suite (signaling message round trips
//! and message-assembler robustness; `test_rtp` is not ported because RTP
//! media transport is out of Simble's scope), plus new fragmentation,
//! negotiation-flow, and stream-state-machine tests with no Bumble
//! equivalent.

use simble::classic::a2dp::{self, codec_type, sbc};
use simble::classic::avdtp::{
    self, AvdtpEvent, MediaCodecCapabilities, MediaType, Message, MessageAssembler, Protocol,
    SepInfo, ServiceCapability, StreamEndPointType, StreamState, error_code, service_category,
    write_message,
};
use simble::l2cap::classic::ClassicChannelManager;

const MTU: u16 = 672;

fn sbc_codec_capabilities(info: a2dp::SbcMediaCodecInformation) -> MediaCodecCapabilities {
    MediaCodecCapabilities {
        media_type: MediaType::Audio,
        media_codec_type: codec_type::SBC,
        media_codec_information: info.to_bytes().to_vec(),
    }
}

fn source_codec_capabilities() -> MediaCodecCapabilities {
    sbc_codec_capabilities(a2dp::SbcMediaCodecInformation {
        sampling_frequency: sbc::sampling_frequency::SF_44100,
        channel_mode: sbc::channel_mode::JOINT_STEREO,
        block_length: sbc::block_length::BL_16,
        subbands: sbc::subbands::S_8,
        allocation_method: sbc::allocation_method::LOUDNESS,
        minimum_bitpool_value: 2,
        maximum_bitpool_value: 53,
    })
}

fn sink_codec_capabilities() -> MediaCodecCapabilities {
    sbc_codec_capabilities(a2dp::SbcMediaCodecInformation {
        sampling_frequency: sbc::sampling_frequency::SF_16000
            | sbc::sampling_frequency::SF_32000
            | sbc::sampling_frequency::SF_44100
            | sbc::sampling_frequency::SF_48000,
        channel_mode: sbc::channel_mode::MONO
            | sbc::channel_mode::DUAL_CHANNEL
            | sbc::channel_mode::STEREO
            | sbc::channel_mode::JOINT_STEREO,
        block_length: sbc::block_length::BL_4
            | sbc::block_length::BL_8
            | sbc::block_length::BL_12
            | sbc::block_length::BL_16,
        subbands: sbc::subbands::S_4 | sbc::subbands::S_8,
        allocation_method: sbc::allocation_method::SNR | sbc::allocation_method::LOUDNESS,
        minimum_bitpool_value: 2,
        maximum_bitpool_value: 53,
    })
}

/// Sends `pdus` from `a` to `b`, then ping-pongs responses until both sides
/// go quiet, returning the events each side observed.
fn exchange(
    a: &mut Protocol,
    b: &mut Protocol,
    pdus: Vec<Vec<u8>>,
) -> (Vec<AvdtpEvent>, Vec<AvdtpEvent>) {
    let mut a_events = Vec::new();
    let mut b_events = Vec::new();
    let mut to_b = pdus;
    let mut to_a: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let mut next_to_a = Vec::new();
        for pdu in to_b.drain(..) {
            let (out, events) = b.receive(&pdu);
            next_to_a.extend(out);
            b_events.extend(events);
        }
        let mut next_to_b = Vec::new();
        for pdu in to_a.drain(..) {
            let (out, events) = a.receive(&pdu);
            next_to_b.extend(out);
            a_events.extend(events);
        }
        to_a = next_to_a;
        to_b = next_to_b;
        if to_a.is_empty() && to_b.is_empty() {
            break;
        }
    }
    (a_events, b_events)
}

/// Decodes a single (unfragmented) signaling PDU into its message.
fn decode_single(pdu: &[u8]) -> (u8, Message) {
    let mut assembler = MessageAssembler::new();
    let assembled = assembler.on_pdu(pdu).expect("complete message");
    let message = Message::parse(
        assembled.signal_identifier,
        assembled.message_type,
        &assembled.payload,
    )
    .expect("parsable message");
    (assembled.transaction_label, message)
}

fn all_messages() -> Vec<Message> {
    vec![
        Message::DiscoverCommand,
        Message::DiscoverResponse(vec![SepInfo {
            seid: 1,
            in_use: true,
            media_type: MediaType::Audio,
            tsep: StreamEndPointType::Sink,
        }]),
        Message::GetCapabilitiesCommand { acp_seid: 1 },
        Message::GetCapabilitiesResponse(vec![
            ServiceCapability::media_transport(),
            MediaCodecCapabilities {
                media_type: MediaType::Audio,
                media_codec_type: codec_type::SBC,
                media_codec_information: vec![0x21, 0x15, 0x02, 0xfa],
            }
            .to_capability(),
            ServiceCapability::delay_reporting(),
        ]),
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::GET_CAPABILITIES,
            error_code: error_code::BAD_ACP_SEID,
        },
        Message::GetAllCapabilitiesCommand { acp_seid: 1 },
        Message::GetAllCapabilitiesResponse(vec![ServiceCapability::media_transport()]),
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::GET_ALL_CAPABILITIES,
            error_code: error_code::BAD_ACP_SEID,
        },
        Message::SetConfigurationCommand {
            acp_seid: 1,
            int_seid: 2,
            capabilities: vec![ServiceCapability::media_transport()],
        },
        Message::SetConfigurationResponse,
        Message::SetConfigurationReject {
            service_category: service_category::MEDIA_TRANSPORT,
            error_code: error_code::UNSUPPORTED_CONFIGURATION,
        },
        Message::GetConfigurationCommand { acp_seid: 1 },
        Message::GetConfigurationResponse(vec![ServiceCapability::media_transport()]),
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::GET_CONFIGURATION,
            error_code: error_code::BAD_ACP_SEID,
        },
        Message::ReconfigureCommand {
            acp_seid: 1,
            capabilities: vec![ServiceCapability::media_transport()],
        },
        Message::ReconfigureResponse,
        Message::ReconfigureReject {
            service_category: service_category::MEDIA_TRANSPORT,
            error_code: error_code::UNSUPPORTED_CONFIGURATION,
        },
        Message::OpenCommand { acp_seid: 1 },
        Message::OpenResponse,
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::OPEN,
            error_code: error_code::BAD_ACP_SEID,
        },
        Message::StartCommand {
            acp_seids: vec![1, 2],
        },
        Message::StartResponse,
        Message::StartReject {
            acp_seid: 1,
            error_code: error_code::BAD_STATE,
        },
        Message::CloseCommand { acp_seid: 1 },
        Message::CloseResponse,
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::CLOSE,
            error_code: error_code::BAD_ACP_SEID,
        },
        Message::SuspendCommand {
            acp_seids: vec![1, 2],
        },
        Message::SuspendResponse,
        Message::SuspendReject {
            acp_seid: 1,
            error_code: error_code::BAD_STATE,
        },
        Message::AbortCommand { acp_seid: 1 },
        Message::AbortResponse,
        Message::SecurityControlCommand {
            acp_seid: 1,
            data: b"foo".to_vec(),
        },
        Message::SecurityControlResponse,
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::SECURITY_CONTROL,
            error_code: error_code::BAD_ACP_SEID,
        },
        Message::GeneralReject {
            signal_identifier: 0,
        },
        Message::DelayReportCommand {
            acp_seid: 1,
            delay: 100,
        },
        Message::DelayReportResponse,
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::DELAYREPORT,
            error_code: error_code::BAD_ACP_SEID,
        },
    ]
}

#[test]
fn test_messages() {
    for message in all_messages() {
        let payload = message.to_payload();
        let parsed = Message::parse(
            message.signal_identifier(),
            message.message_type(),
            &payload,
        )
        .expect("parsable message");
        assert_eq!(parsed, message);
        assert_eq!(parsed.to_payload(), payload);
    }
}

#[test]
fn test_messages_survive_signaling_round_trip() {
    for message in all_messages() {
        let pdus = write_message(5, &message, MTU);
        assert_eq!(pdus.len(), 1);
        let (label, parsed) = decode_single(&pdus[0]);
        assert_eq!(label, 5);
        assert_eq!(parsed, message);
    }
}

#[test]
fn test_message_assembler_truncated_pdu() {
    // Truncated PDUs from a remote peer must be dropped without panicking
    // (and without ever completing a message).
    for pdu in [
        &b""[..],      // empty PDU
        &[0x00][..],   // 1-byte SINGLE_PACKET, missing the signal byte
        &[0x04][..],   // 1-byte START_PACKET, missing the signal byte
        &[0x44, 0x10], // 2-byte START_PACKET, missing the packet count
    ] {
        let mut assembler = MessageAssembler::new();
        assert_eq!(assembler.on_pdu(pdu), None);
    }
}

#[test]
fn test_message_fragmentation_round_trip() {
    let message = Message::GetCapabilitiesResponse(vec![
        ServiceCapability::media_transport(),
        ServiceCapability {
            service_category: service_category::CONTENT_PROTECTION,
            data: (0..30).collect(),
        },
    ]);
    let pdus = write_message(3, &message, 8);
    assert!(pdus.len() > 1);
    // Every fragment must fit in the MTU, and the start packet's count
    // field must equal the number of fragments.
    for pdu in &pdus {
        assert!(pdu.len() <= 8);
    }
    assert_eq!(pdus[0][2] as usize, pdus.len());

    let mut assembler = MessageAssembler::new();
    let mut assembled = None;
    for pdu in &pdus {
        assembled = assembler.on_pdu(pdu);
    }
    let assembled = assembled.expect("message completed on the last fragment");
    assert_eq!(assembled.transaction_label, 3);
    let parsed = Message::parse(
        assembled.signal_identifier,
        assembled.message_type,
        &assembled.payload,
    )
    .expect("parsable message");
    assert_eq!(parsed, message);
}

/// Signaling analog of Bumble's `test_source_sink_1` (a2dp_test.py) without
/// the RTP media path: full discover / get capabilities / configure / open /
/// start / suspend / close flow between two endpoints.
#[test]
fn test_full_streaming_negotiation() {
    let mut int = Protocol::new(MTU);
    let mut acp = Protocol::new(MTU);
    let source_seid = int.add_source(source_codec_capabilities(), false);
    let sink_seid = acp.add_sink(sink_codec_capabilities());

    // Discover.
    let pdus = int.discover().unwrap();
    let (int_events, _) = exchange(&mut int, &mut acp, pdus);
    let AvdtpEvent::EndpointsDiscovered(seps) = &int_events[0] else {
        panic!("expected EndpointsDiscovered, got {int_events:?}");
    };
    assert_eq!(
        seps,
        &vec![SepInfo {
            seid: sink_seid,
            in_use: false,
            media_type: MediaType::Audio,
            tsep: StreamEndPointType::Sink,
        }]
    );

    // Get capabilities (AVDTP 1.3 issues Get_All_Capabilities).
    let pdus = int.get_capabilities(sink_seid).unwrap();
    let (int_events, _) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::CapabilitiesReceived {
            seid: sink_seid,
            capabilities: vec![
                ServiceCapability::media_transport(),
                sink_codec_capabilities().to_capability(),
            ],
        }]
    );
    let remote_sink = int
        .find_remote_sink_by_codec(MediaType::Audio, codec_type::SBC, 0, 0)
        .expect("remote SBC sink found");
    assert_eq!(remote_sink.seid, sink_seid);

    // Set configuration.
    let configuration = vec![
        ServiceCapability::media_transport(),
        source_codec_capabilities().to_capability(),
    ];
    let pdus = int
        .set_configuration(sink_seid, source_seid, configuration.clone())
        .unwrap();
    let (int_events, acp_events) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::StreamConfigured { seid: source_seid }]
    );
    assert_eq!(
        acp_events,
        vec![AvdtpEvent::StreamConfigured { seid: sink_seid }]
    );
    let sink = acp.get_local_endpoint_by_seid(sink_seid).unwrap();
    assert_eq!(sink.state, StreamState::Configured);
    assert_eq!(sink.configuration, configuration);
    assert_eq!(sink.remote_seid, Some(source_seid));

    // Open.
    let pdus = int.open(sink_seid).unwrap();
    let (int_events, acp_events) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::StreamOpened { seid: source_seid }]
    );
    assert_eq!(
        acp_events,
        vec![AvdtpEvent::StreamOpened { seid: sink_seid }]
    );

    // Start.
    let pdus = int.start(&[sink_seid]).unwrap();
    let (int_events, acp_events) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::StreamStarted { seid: source_seid }]
    );
    assert_eq!(
        acp_events,
        vec![AvdtpEvent::StreamStarted { seid: sink_seid }]
    );
    let source = int.get_local_endpoint_by_seid(source_seid).unwrap();
    assert_eq!(source.state, StreamState::Streaming);
    assert!(source.in_use());
    let sink = acp.get_local_endpoint_by_seid(sink_seid).unwrap();
    assert_eq!(sink.state, StreamState::Streaming);
    assert!(sink.in_use());

    // A discover during streaming reports the sink as in use.
    let pdus = int.discover().unwrap();
    let (int_events, _) = exchange(&mut int, &mut acp, pdus);
    let AvdtpEvent::EndpointsDiscovered(seps) = &int_events[0] else {
        panic!("expected EndpointsDiscovered, got {int_events:?}");
    };
    assert!(seps[0].in_use);

    // Suspend back to OPEN.
    let pdus = int.suspend(&[sink_seid]).unwrap();
    let (int_events, acp_events) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::StreamSuspended { seid: source_seid }]
    );
    assert_eq!(
        acp_events,
        vec![AvdtpEvent::StreamSuspended { seid: sink_seid }]
    );
    assert_eq!(
        acp.get_local_endpoint_by_seid(sink_seid).unwrap().state,
        StreamState::Open
    );

    // Close back to IDLE.
    let pdus = int.close(sink_seid).unwrap();
    let (int_events, acp_events) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::StreamClosed { seid: source_seid }]
    );
    assert_eq!(
        acp_events,
        vec![AvdtpEvent::StreamClosed { seid: sink_seid }]
    );
    let source = int.get_local_endpoint_by_seid(source_seid).unwrap();
    assert_eq!(source.state, StreamState::Idle);
    assert!(!source.in_use());
    let sink = acp.get_local_endpoint_by_seid(sink_seid).unwrap();
    assert_eq!(sink.state, StreamState::Idle);
    assert!(!sink.in_use());
}

#[test]
fn test_get_capabilities_rejected_for_unknown_seid() {
    let mut int = Protocol::new(MTU);
    let mut acp = Protocol::new(MTU);
    let pdus = int.get_capabilities(9).unwrap();
    let (int_events, _) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::CommandRejected {
            signal_identifier: avdtp::signal_identifier::GET_ALL_CAPABILITIES,
            error_code: error_code::BAD_ACP_SEID,
        }]
    );
}

#[test]
fn test_set_configuration_rejected_when_sep_in_use() {
    let mut acp = Protocol::new(MTU);
    let sink_seid = acp.add_sink(sink_codec_capabilities());

    let configure = Message::SetConfigurationCommand {
        acp_seid: sink_seid,
        int_seid: 1,
        capabilities: vec![ServiceCapability::media_transport()],
    };
    let (out, _) = acp.receive(&write_message(0, &configure, MTU)[0]);
    assert_eq!(decode_single(&out[0]).1, Message::SetConfigurationResponse);

    // A second configuration while CONFIGURED must be rejected: the SEP is
    // already part of a stream.
    let (out, events) = acp.receive(&write_message(1, &configure, MTU)[0]);
    assert!(events.is_empty());
    assert_eq!(
        decode_single(&out[0]).1,
        Message::SetConfigurationReject {
            service_category: 0,
            error_code: error_code::SEP_IN_USE,
        }
    );
}

#[test]
fn test_open_rejected_before_configuration() {
    let mut acp = Protocol::new(MTU);
    let sink_seid = acp.add_sink(sink_codec_capabilities());

    let open = Message::OpenCommand {
        acp_seid: sink_seid,
    };
    let (out, events) = acp.receive(&write_message(0, &open, MTU)[0]);
    assert!(events.is_empty());
    assert_eq!(
        decode_single(&out[0]).1,
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::OPEN,
            error_code: error_code::BAD_STATE,
        }
    );
    assert_eq!(
        acp.get_local_endpoint_by_seid(sink_seid).unwrap().state,
        StreamState::Idle
    );
}

#[test]
fn test_start_rejected_before_open() {
    let mut acp = Protocol::new(MTU);
    let sink_seid = acp.add_sink(sink_codec_capabilities());

    let configure = Message::SetConfigurationCommand {
        acp_seid: sink_seid,
        int_seid: 1,
        capabilities: vec![ServiceCapability::media_transport()],
    };
    acp.receive(&write_message(0, &configure, MTU)[0]);

    // CONFIGURED but not OPEN: Start must be rejected with the failing SEID.
    let start = Message::StartCommand {
        acp_seids: vec![sink_seid],
    };
    let (out, events) = acp.receive(&write_message(1, &start, MTU)[0]);
    assert!(events.is_empty());
    assert_eq!(
        decode_single(&out[0]).1,
        Message::StartReject {
            acp_seid: sink_seid,
            error_code: error_code::BAD_STATE,
        }
    );
    assert_eq!(
        acp.get_local_endpoint_by_seid(sink_seid).unwrap().state,
        StreamState::Configured
    );
}

#[test]
fn test_bad_seid_rejected_with_bad_acp_seid() {
    let mut acp = Protocol::new(MTU);
    acp.add_sink(sink_codec_capabilities());

    let get_capabilities = Message::GetCapabilitiesCommand { acp_seid: 7 };
    let (out, _) = acp.receive(&write_message(0, &get_capabilities, MTU)[0]);
    assert_eq!(
        decode_single(&out[0]).1,
        Message::Reject {
            signal_identifier: avdtp::signal_identifier::GET_CAPABILITIES,
            error_code: error_code::BAD_ACP_SEID,
        }
    );
}

#[test]
fn test_unknown_signal_identifier_draws_general_reject() {
    let mut acp = Protocol::new(MTU);
    // Signal identifier 0x0E is beyond DELAYREPORT (0x0D): single-packet
    // command header followed by the unknown signal byte.
    let pdu = vec![0x50, 0x0E];
    let (out, events) = acp.receive(&pdu);
    assert!(events.is_empty());
    assert_eq!(
        decode_single(&out[0]),
        (
            5,
            Message::GeneralReject {
                signal_identifier: 0x0E,
            }
        )
    );
}

#[test]
fn test_abort_returns_stream_to_idle() {
    let mut int = Protocol::new(MTU);
    let mut acp = Protocol::new(MTU);
    let source_seid = int.add_source(source_codec_capabilities(), false);
    let sink_seid = acp.add_sink(sink_codec_capabilities());

    let pdus = int
        .set_configuration(
            sink_seid,
            source_seid,
            vec![
                ServiceCapability::media_transport(),
                source_codec_capabilities().to_capability(),
            ],
        )
        .unwrap();
    exchange(&mut int, &mut acp, pdus);
    let pdus = int.open(sink_seid).unwrap();
    exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        acp.get_local_endpoint_by_seid(sink_seid).unwrap().state,
        StreamState::Open
    );

    let pdus = int.abort(sink_seid).unwrap();
    let (int_events, acp_events) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::StreamAborted { seid: source_seid }]
    );
    assert_eq!(
        acp_events,
        vec![AvdtpEvent::StreamAborted { seid: sink_seid }]
    );
    assert_eq!(
        int.get_local_endpoint_by_seid(source_seid).unwrap().state,
        StreamState::Idle
    );
    assert_eq!(
        acp.get_local_endpoint_by_seid(sink_seid).unwrap().state,
        StreamState::Idle
    );
}

#[test]
fn test_initiator_rejects_invalid_local_transitions() {
    let mut int = Protocol::new(MTU);
    let source_seid = int.add_source(source_codec_capabilities(), false);

    // No stream configured with remote SEID 1 yet.
    assert!(int.open(1).is_err());
    assert!(int.start(&[1]).is_err());
    assert!(int.close(1).is_err());

    // Configure locally via a peer, then verify start-before-open fails.
    let mut acp = Protocol::new(MTU);
    let sink_seid = acp.add_sink(sink_codec_capabilities());
    let pdus = int
        .set_configuration(
            sink_seid,
            source_seid,
            vec![ServiceCapability::media_transport()],
        )
        .unwrap();
    exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int.get_local_endpoint_by_seid(source_seid).unwrap().state,
        StreamState::Configured
    );
    assert!(int.start(&[sink_seid]).is_err());
    assert!(int.suspend(&[sink_seid]).is_err());
    // And a second set_configuration while not IDLE fails locally.
    assert!(
        int.set_configuration(sink_seid, source_seid, Vec::new())
            .is_err()
    );
}

#[test]
fn test_delay_report_and_security_control_events() {
    let mut acp = Protocol::new(MTU);
    let sink_seid = acp.add_sink(sink_codec_capabilities());

    let delay_report = Message::DelayReportCommand {
        acp_seid: sink_seid,
        delay: 1500,
    };
    let (out, events) = acp.receive(&write_message(0, &delay_report, MTU)[0]);
    assert_eq!(
        events,
        vec![AvdtpEvent::DelayReport {
            seid: sink_seid,
            delay: 1500,
        }]
    );
    assert_eq!(decode_single(&out[0]).1, Message::DelayReportResponse);

    let security_control = Message::SecurityControlCommand {
        acp_seid: sink_seid,
        data: b"cp".to_vec(),
    };
    let (out, events) = acp.receive(&write_message(1, &security_control, MTU)[0]);
    assert_eq!(
        events,
        vec![AvdtpEvent::SecurityControl {
            seid: sink_seid,
            data: b"cp".to_vec(),
        }]
    );
    assert_eq!(decode_single(&out[0]).1, Message::SecurityControlResponse);
}

#[test]
fn test_reconfigure_updates_codec_configuration() {
    let mut int = Protocol::new(MTU);
    let mut acp = Protocol::new(MTU);
    let source_seid = int.add_source(source_codec_capabilities(), false);
    let sink_seid = acp.add_sink(sink_codec_capabilities());

    let pdus = int
        .set_configuration(
            sink_seid,
            source_seid,
            vec![
                ServiceCapability::media_transport(),
                source_codec_capabilities().to_capability(),
            ],
        )
        .unwrap();
    exchange(&mut int, &mut acp, pdus);

    // Reconfigure is only legal while OPEN.
    assert!(int.reconfigure(sink_seid, Vec::new()).is_err());

    let pdus = int.open(sink_seid).unwrap();
    exchange(&mut int, &mut acp, pdus);

    let new_codec = sbc_codec_capabilities(a2dp::SbcMediaCodecInformation {
        sampling_frequency: sbc::sampling_frequency::SF_48000,
        channel_mode: sbc::channel_mode::STEREO,
        block_length: sbc::block_length::BL_8,
        subbands: sbc::subbands::S_8,
        allocation_method: sbc::allocation_method::SNR,
        minimum_bitpool_value: 2,
        maximum_bitpool_value: 35,
    });
    let pdus = int
        .reconfigure(sink_seid, vec![new_codec.to_capability()])
        .unwrap();
    let (int_events, acp_events) = exchange(&mut int, &mut acp, pdus);
    assert_eq!(
        int_events,
        vec![AvdtpEvent::StreamReconfigured { seid: source_seid }]
    );
    assert_eq!(
        acp_events,
        vec![AvdtpEvent::StreamReconfigured { seid: sink_seid }]
    );
    // The codec entry is replaced; media transport survives.
    let sink = acp.get_local_endpoint_by_seid(sink_seid).unwrap();
    assert_eq!(
        sink.configuration,
        vec![
            ServiceCapability::media_transport(),
            new_codec.to_capability(),
        ]
    );
    assert_eq!(sink.state, StreamState::Open);
}

#[test]
fn test_l2cap_registration_and_connect() {
    let mut manager = ClassicChannelManager::new();
    avdtp::register_server(&mut manager).unwrap();
    assert!(manager.is_server_registered(avdtp::AVDTP_PSM));

    let mut client_manager = ClassicChannelManager::new();
    let (cid, request) = avdtp::connect_channel(&mut client_manager, MTU).unwrap();
    assert_eq!(request.psm.get(), avdtp::AVDTP_PSM);
    assert_eq!(request.source_cid.get(), cid);
}
