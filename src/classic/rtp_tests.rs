use super::*;

/// Byte-for-byte against Bumble's `MediaPacket.__bytes__` (rtp.py):
/// `version<<6 | padding<<5 | extension<<4 | len(csrc)`, then
/// `marker<<7 | payload_type`, then `>HII` — big-endian sequence
/// number, timestamp and SSRC.
#[test]
fn test_rtp_header_matches_the_reference_layout() {
    let packet = MediaPacket {
        sequence_number: 0x1234,
        timestamp: 0xDEAD_BEEF,
        ssrc: 0x0000_0001,
        payload_type: 96,
        marker: false,
        payload: vec![0xAA, 0xBB],
    };
    let bytes = packet.to_bytes();
    assert_eq!(
        &bytes[..RtpHeader::LEN],
        &[
            0x80, // version 2, no padding/extension, 0 CSRCs
            0x60, // marker 0, payload type 96
            0x12, 0x34, // sequence number, big-endian
            0xDE, 0xAD, 0xBE, 0xEF, // timestamp, big-endian
            0x00, 0x00, 0x00, 0x01, // SSRC, big-endian
        ]
    );
    assert_eq!(&bytes[RtpHeader::LEN..], &[0xAA, 0xBB]);
    assert_eq!(MediaPacket::parse(&bytes).unwrap(), packet);
}

#[test]
fn test_marker_bit_round_trips() {
    let packet = MediaPacket {
        sequence_number: 1,
        timestamp: 0,
        ssrc: 0,
        payload_type: 96,
        marker: true,
        payload: vec![0x01],
    };
    let bytes = packet.to_bytes();
    assert_eq!(bytes[1], 0x80 | 96, "marker sets the top bit");
    assert!(MediaPacket::parse(&bytes).unwrap().marker);
}

#[test]
fn test_csrc_list_is_skipped_to_find_the_payload() {
    // Two CSRCs: the payload starts 8 bytes after the fixed header.
    let mut bytes = vec![0x82, 0x60, 0x00, 0x07];
    bytes.extend_from_slice(&[0x00; 8]); // timestamp + ssrc
    bytes.extend_from_slice(&[0x11, 0x11, 0x11, 0x11]); // CSRC 1
    bytes.extend_from_slice(&[0x22, 0x22, 0x22, 0x22]); // CSRC 2
    bytes.extend_from_slice(&[0xF0, 0x0D]);
    let packet = MediaPacket::parse(&bytes).unwrap();
    assert_eq!(packet.sequence_number, 7);
    assert_eq!(packet.payload, vec![0xF0, 0x0D]);
}

#[test]
fn test_padding_is_stripped() {
    // Padding bit set; final octet counts the padding, itself included.
    let mut bytes = vec![0xA0, 0x60, 0x00, 0x01];
    bytes.extend_from_slice(&[0x00; 8]);
    bytes.extend_from_slice(&[0xAB, 0x00, 0x00, 0x03]);
    let packet = MediaPacket::parse(&bytes).unwrap();
    assert_eq!(packet.payload, vec![0xAB], "3 padding octets removed");
}

#[test]
fn test_malformed_packets_are_rejected_not_guessed() {
    assert_eq!(MediaPacket::parse(&[]), Err(MediaPacketError::Truncated));
    assert_eq!(
        MediaPacket::parse(&[0x80, 0x60, 0x00]),
        Err(MediaPacketError::Truncated),
        "shorter than a fixed header"
    );
    // Version 1 is not RTP as AVDTP uses it.
    let mut wrong_version = vec![0x40, 0x60];
    wrong_version.extend_from_slice(&[0x00; 10]);
    assert_eq!(
        MediaPacket::parse(&wrong_version),
        Err(MediaPacketError::UnsupportedVersion(1))
    );
    // CSRC count claims four identifiers that are not present.
    let mut short_csrc = vec![0x84, 0x60];
    short_csrc.extend_from_slice(&[0x00; 10]);
    assert_eq!(
        MediaPacket::parse(&short_csrc),
        Err(MediaPacketError::Truncated)
    );
    // Padding longer than the payload.
    let mut bad_padding = vec![0xA0, 0x60];
    bad_padding.extend_from_slice(&[0x00; 10]);
    bad_padding.push(0x7F);
    assert_eq!(
        MediaPacket::parse(&bad_padding),
        Err(MediaPacketError::BadPadding)
    );
}

/// Matches Bumble's `SbcAudioExtractor` read of the header
/// (`speaker.py`): F = bit 7, S = bit 6, L = bit 5, count = low nibble.
#[test]
fn test_sbc_payload_header_bit_layout() {
    assert_eq!(SbcPayloadHeader::unfragmented(5).to_byte(), 0x05);
    let fragment = SbcPayloadHeader {
        fragmented: true,
        start: true,
        last: false,
        frame_count: 3,
    };
    assert_eq!(fragment.to_byte(), 0xC3);
    assert_eq!(SbcPayloadHeader::from_byte(0xC3), fragment);
    // Last fragment: F and L set, S clear.
    assert_eq!(
        SbcPayloadHeader::from_byte(0xA1),
        SbcPayloadHeader {
            fragmented: true,
            start: false,
            last: true,
            frame_count: 1,
        }
    );
}

#[test]
fn test_whole_frames_pack_into_one_payload() {
    let frames = vec![vec![0x11; 10], vec![0x22; 10], vec![0x33; 10]];
    let payloads = packetize_sbc(&frames, 64);
    assert_eq!(payloads.len(), 1, "all three fit");
    let parsed = SbcPayload::parse(&payloads[0]).unwrap();
    assert!(!parsed.header.fragmented);
    assert_eq!(parsed.header.frame_count, 3);
    assert_eq!(parsed.data.len(), 30);

    // Reassembly hands an unfragmented payload straight back.
    let mut reassembler = SbcReassembler::new();
    assert_eq!(reassembler.push(&payloads[0]), Some(parsed.data));
}

#[test]
fn test_frames_spill_into_further_payloads() {
    // 4 frames of 20 bytes, 31-byte payload budget (30 usable).
    let frames = vec![vec![0xAA; 20]; 4];
    let payloads = packetize_sbc(&frames, 31);
    assert_eq!(payloads.len(), 4, "one frame each: two would exceed 30");
    for payload in &payloads {
        let parsed = SbcPayload::parse(payload).unwrap();
        assert_eq!(parsed.header.frame_count, 1);
        assert!(!parsed.header.fragmented);
    }
}

#[test]
fn test_an_oversized_frame_is_fragmented_and_reassembled() {
    // One frame far larger than the payload budget.
    let frame: Vec<u8> = (0..250u32).map(|i| i as u8).collect();
    let payloads = packetize_sbc(std::slice::from_ref(&frame), 101); // 100 usable
    assert_eq!(payloads.len(), 3, "250 bytes in 100-byte chunks");

    let first = SbcPayload::parse(&payloads[0]).unwrap().header;
    assert!(first.fragmented && first.start && !first.last);
    assert_eq!(
        first.frame_count, 3,
        "three fragments remain, including this"
    );
    let last = SbcPayload::parse(&payloads[2]).unwrap().header;
    assert!(last.fragmented && !last.start && last.last);

    let mut reassembler = SbcReassembler::new();
    assert_eq!(reassembler.push(&payloads[0]), None);
    assert!(reassembler.is_reassembling());
    assert_eq!(reassembler.push(&payloads[1]), None);
    assert_eq!(
        reassembler.push(&payloads[2]),
        Some(frame),
        "the original frame comes back byte for byte"
    );
    assert!(!reassembler.is_reassembling());
}

#[test]
fn test_lost_fragments_do_not_splice_unrelated_audio() {
    let frame: Vec<u8> = vec![0x5A; 250];
    let payloads = packetize_sbc(std::slice::from_ref(&frame), 101);

    // A continuation with no start cannot complete: the head is lost.
    let mut reassembler = SbcReassembler::new();
    assert_eq!(reassembler.push(&payloads[1]), None);
    assert_eq!(reassembler.push(&payloads[2]), None);

    // A start arriving mid-frame drops the stale bytes rather than
    // concatenating two different frames.
    let mut reassembler = SbcReassembler::new();
    reassembler.push(&payloads[0]);
    reassembler.push(&payloads[0]); // restart
    reassembler.push(&payloads[1]);
    let completed = reassembler.push(&payloads[2]).expect("completes");
    assert_eq!(completed.len(), frame.len(), "no doubled prefix");
}

#[test]
fn test_packetize_refuses_a_budget_with_no_room() {
    // One byte is the header itself; nothing would be left for audio.
    assert!(packetize_sbc(&[vec![0x01; 4]], 1).is_empty());
    assert!(packetize_sbc(&[vec![0x01; 4]], 0).is_empty());
}
