// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A2DP media path: RTP packets crossing the AVDTP transport channel.
//!
//! `avdtp_test.rs` covers signaling — negotiating *what* will be streamed.
//! These tests cover the bytes themselves: a source packetizes SBC frames
//! into RTP, a sink receives them on a second L2CAP channel and hands whole
//! codec frames to whatever wants to play them.
//!
//! Nothing here decodes audio. Simble models the transport; SBC frames are
//! opaque payloads, exactly as LC3 frames are on the LE Audio ISO path.

use simble::classic::a2dp::{self, codec_type, sbc};
use simble::classic::avdtp::{AVDTP_PSM, MediaCodecCapabilities, MediaType, Protocol, StreamState};
use simble::classic::rtp::{MediaPacket, RtpHeader, SbcPayload};
use simble::l2cap::classic::ClassicChannelManager;
use simble::packets::l2cap_signaling::ConnectionRequestHeader;
use zerocopy::byteorder::little_endian::U16;

/// A conservative Classic L2CAP MTU, as a real A2DP link negotiates.
const MTU: u16 = 672;

fn sbc_capabilities() -> MediaCodecCapabilities {
    MediaCodecCapabilities {
        media_type: MediaType::Audio,
        media_codec_type: codec_type::SBC,
        media_codec_information: a2dp::SbcMediaCodecInformation {
            sampling_frequency: sbc::sampling_frequency::SF_44100,
            channel_mode: sbc::channel_mode::JOINT_STEREO,
            block_length: sbc::block_length::BL_16,
            subbands: sbc::subbands::S_8,
            allocation_method: sbc::allocation_method::LOUDNESS,
            minimum_bitpool_value: 2,
            maximum_bitpool_value: 53,
        }
        .to_bytes()
        .to_vec(),
    }
}

/// Shuttles PDUs between two protocol instances until both go quiet.
fn exchange(a: &mut Protocol, b: &mut Protocol, pdus: Vec<Vec<u8>>) {
    let mut to_b = pdus;
    let mut to_a: Vec<Vec<u8>> = Vec::new();
    for _ in 0..16 {
        if to_b.is_empty() && to_a.is_empty() {
            break;
        }
        let mut next_a = Vec::new();
        for pdu in to_b.drain(..) {
            let (out, _) = b.receive(&pdu);
            next_a.extend(out);
        }
        let mut next_b = Vec::new();
        for pdu in to_a.drain(..) {
            let (out, _) = a.receive(&pdu);
            next_b.extend(out);
        }
        to_a = next_a;
        to_b = next_b;
    }
}

/// Drives a source and a sink from Discover through to STREAMING, returning
/// `(source, sink, source_seid, sink_seid)`.
fn negotiate_to_streaming() -> (Protocol, Protocol, u8, u8) {
    let mut source = Protocol::new(MTU);
    let mut sink = Protocol::new(MTU);
    let source_seid = source.add_source(sbc_capabilities(), false);
    let sink_seid = sink.add_sink(sbc_capabilities());

    let pdus = source.discover().unwrap();
    exchange(&mut source, &mut sink, pdus);
    let pdus = source.get_capabilities(sink_seid).unwrap();
    exchange(&mut source, &mut sink, pdus);

    let pdus = source
        .set_configuration(
            sink_seid,
            source_seid,
            vec![
                simble::classic::avdtp::ServiceCapability::media_transport(),
                sbc_capabilities().to_capability(),
            ],
        )
        .unwrap();
    exchange(&mut source, &mut sink, pdus);
    let pdus = source.open(sink_seid).unwrap();
    exchange(&mut source, &mut sink, pdus);
    let pdus = source.start(&[sink_seid]).unwrap();
    exchange(&mut source, &mut sink, pdus);

    assert_eq!(
        source
            .get_local_endpoint_by_seid(source_seid)
            .unwrap()
            .state,
        StreamState::Streaming
    );
    assert_eq!(
        sink.get_local_endpoint_by_seid(sink_seid).unwrap().state,
        StreamState::Streaming
    );
    (source, sink, source_seid, sink_seid)
}

/// AVDTP's transport channel is a *second* L2CAP channel to the same PSM as
/// signaling (spec 7.1). If the channel manager refused a second connection
/// to an already-served PSM, media could never flow.
#[test]
fn test_a_second_l2cap_channel_opens_on_the_avdtp_psm() {
    let mut manager = ClassicChannelManager::new();
    manager.register_server(AVDTP_PSM).unwrap();

    let signaling = manager
        .on_connection_request(
            &ConnectionRequestHeader {
                psm: U16::from(AVDTP_PSM),
                source_cid: U16::from(0x0040),
            },
            MTU,
        )
        .unwrap();
    let media = manager
        .on_connection_request(
            &ConnectionRequestHeader {
                psm: U16::from(AVDTP_PSM),
                source_cid: U16::from(0x0041),
            },
            MTU,
        )
        .unwrap();

    assert_eq!(signaling.result.get(), 0, "signaling channel accepted");
    assert_eq!(media.result.get(), 0, "media channel accepted too");
    assert_ne!(
        signaling.destination_cid.get(),
        media.destination_cid.get(),
        "the two channels must have distinct CIDs"
    );
    assert_eq!(
        manager
            .get_channel(media.destination_cid.get())
            .unwrap()
            .psm,
        AVDTP_PSM
    );
}

/// The end-to-end path: negotiate, attach transport channels, stream frames,
/// then suspend and close.
#[test]
fn test_media_flows_from_source_to_sink_and_stops_on_close() {
    let (mut source, mut sink, source_seid, sink_seid) = negotiate_to_streaming();

    // The transport channel is opened by whoever owns L2CAP; each side
    // registers its own CID for the stream.
    let (source_cid, sink_cid) = (0x0041, 0x0051);
    source
        .attach_media_channel(source_seid, source_cid)
        .unwrap();
    sink.attach_media_channel(sink_seid, sink_cid).unwrap();
    assert!(source.has_media_channel(source_seid));
    assert!(sink.has_media_channel(sink_seid));

    // Three SBC frames, small enough to share one RTP payload.
    let frames: Vec<Vec<u8>> = (0..3u8).map(|i| vec![0xB0 | i; 40]).collect();
    let packets = source.send_media(source_seid, &frames, 1_000).unwrap();
    assert_eq!(packets.len(), 1, "three small frames pack into one packet");

    for packet in &packets {
        sink.on_media_pdu(sink_cid, packet).unwrap();
    }

    let received = sink.take_media();
    assert_eq!(received.len(), 1, "one payload carrying three frames");
    assert_eq!(received[0].seid, sink_seid);
    assert_eq!(received[0].timestamp, 1_000);
    assert_eq!(
        received[0].payload,
        frames.concat(),
        "codec bytes arrive unchanged"
    );
    assert!(sink.take_media().is_empty(), "draining is destructive");

    // Sequence numbers advance per packet.
    let more = source.send_media(source_seid, &frames, 2_000).unwrap();
    let first = MediaPacket::parse(&packets[0]).unwrap();
    let second = MediaPacket::parse(&more[0]).unwrap();
    assert_eq!(second.sequence_number, first.sequence_number + 1);
    assert_eq!(second.ssrc, first.ssrc, "same stream, same SSRC");

    // Suspend returns the stream to OPEN; media is no longer legal.
    let pdus = source.suspend(&[sink_seid]).unwrap();
    exchange(&mut source, &mut sink, pdus);
    assert_eq!(
        source
            .get_local_endpoint_by_seid(source_seid)
            .unwrap()
            .state,
        StreamState::Open
    );
    assert!(
        source.send_media(source_seid, &frames, 3_000).is_err(),
        "media before START (or after SUSPEND) is a protocol violation"
    );

    // Close tears the transport channel down with the stream.
    let pdus = source.close(sink_seid).unwrap();
    exchange(&mut source, &mut sink, pdus);
    assert!(
        !source.has_media_channel(source_seid),
        "closing the stream drops its media channel"
    );
    assert!(!sink.has_media_channel(sink_seid));
    assert!(
        sink.on_media_pdu(sink_cid, &packets[0]).is_err(),
        "media for a closed stream is refused"
    );
}

/// A frame too large for one payload is fragmented by the source and
/// reassembled by the sink, transparently to both.
#[test]
fn test_a_large_frame_is_fragmented_and_arrives_whole() {
    let (mut source, mut sink, source_seid, sink_seid) = negotiate_to_streaming();
    let (source_cid, sink_cid) = (0x0041, 0x0051);
    source
        .attach_media_channel(source_seid, source_cid)
        .unwrap();
    sink.attach_media_channel(sink_seid, sink_cid).unwrap();

    // Larger than the payload budget (MTU minus the 12-byte RTP header).
    let big: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
    let packets = source
        .send_media(source_seid, std::slice::from_ref(&big), 500)
        .unwrap();
    assert!(packets.len() > 1, "one frame spans several packets");

    // Every packet is well-formed RTP flagged as a fragment.
    for packet in &packets {
        let parsed = MediaPacket::parse(packet).unwrap();
        assert!(packet.len() <= usize::from(MTU), "each fits the MTU");
        let payload = SbcPayload::parse(&parsed.payload).unwrap();
        assert!(payload.header.fragmented);
    }

    // The sink emits nothing until the last fragment lands.
    for packet in &packets[..packets.len() - 1] {
        sink.on_media_pdu(sink_cid, packet).unwrap();
        assert!(sink.take_media().is_empty(), "still reassembling");
    }
    sink.on_media_pdu(sink_cid, packets.last().unwrap())
        .unwrap();

    let received = sink.take_media();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0].payload, big,
        "the frame comes back byte for byte"
    );
}

/// A sink that never drains must not grow without bound.
#[test]
fn test_the_media_queue_is_bounded() {
    let (mut source, mut sink, source_seid, sink_seid) = negotiate_to_streaming();
    source.attach_media_channel(source_seid, 0x0041).unwrap();
    sink.attach_media_channel(sink_seid, 0x0051).unwrap();

    let frame = vec![vec![0x77; 32]];
    for i in 0..400u32 {
        let packets = source.send_media(source_seid, &frame, i).unwrap();
        for packet in &packets {
            sink.on_media_pdu(0x0051, packet).unwrap();
        }
    }
    let received = sink.take_media();
    assert!(
        received.len() <= 256,
        "the queue caps at 256 frames, got {}",
        received.len()
    );
    assert_eq!(
        received.last().unwrap().timestamp,
        399,
        "the newest frame survives; the oldest are dropped"
    );
}

/// Malformed or misaddressed media must be reported, not played as noise.
#[test]
fn test_bad_media_is_rejected() {
    let (mut source, mut sink, source_seid, sink_seid) = negotiate_to_streaming();
    source.attach_media_channel(source_seid, 0x0041).unwrap();
    sink.attach_media_channel(sink_seid, 0x0051).unwrap();

    assert!(
        sink.on_media_pdu(0x0099, &[0x80, 0x60, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xAA])
            .is_err(),
        "a CID with no media channel"
    );
    assert!(
        sink.on_media_pdu(0x0051, &[0x80, 0x60, 0x00]).is_err(),
        "shorter than an RTP header"
    );
    assert!(sink.on_media_pdu(0x0051, &[]).is_err(), "an empty PDU");
    assert!(sink.take_media().is_empty(), "nothing was queued");
}

/// A media channel cannot be attached to a stream that is not open — there
/// would be nowhere to deliver.
#[test]
fn test_media_channel_requires_an_open_stream() {
    let mut sink = Protocol::new(MTU);
    let seid = sink.add_sink(sbc_capabilities());
    assert!(
        sink.attach_media_channel(seid, 0x0051).is_err(),
        "IDLE stream has no media channel"
    );
    assert!(
        sink.attach_media_channel(99, 0x0051).is_err(),
        "unknown SEID"
    );
}

/// The RTP header costs 12 bytes of every packet; the payload budget must
/// account for it or packets overflow the L2CAP MTU.
#[test]
fn test_packets_respect_the_negotiated_mtu() {
    let small_mtu = 64;
    let mut source = Protocol::new(small_mtu);
    let mut sink = Protocol::new(small_mtu);
    let source_seid = source.add_source(sbc_capabilities(), false);
    let sink_seid = sink.add_sink(sbc_capabilities());

    let pdus = source.discover().unwrap();
    exchange(&mut source, &mut sink, pdus);
    let pdus = source
        .set_configuration(
            sink_seid,
            source_seid,
            vec![
                simble::classic::avdtp::ServiceCapability::media_transport(),
                sbc_capabilities().to_capability(),
            ],
        )
        .unwrap();
    exchange(&mut source, &mut sink, pdus);
    let pdus = source.open(sink_seid).unwrap();
    exchange(&mut source, &mut sink, pdus);
    let pdus = source.start(&[sink_seid]).unwrap();
    exchange(&mut source, &mut sink, pdus);
    source.attach_media_channel(source_seid, 0x0041).unwrap();

    let frames: Vec<Vec<u8>> = (0..6u8).map(|i| vec![i; 30]).collect();
    let packets = source.send_media(source_seid, &frames, 0).unwrap();
    assert!(
        packets.len() > 1,
        "six 30-byte frames cannot share 52 bytes"
    );
    for packet in &packets {
        assert!(
            packet.len() <= usize::from(small_mtu),
            "packet of {} exceeds MTU {small_mtu}",
            packet.len()
        );
        assert!(packet.len() > RtpHeader::LEN, "and carries actual payload");
    }
}
