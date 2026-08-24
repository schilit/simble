use super::*;

/// Ranging Data with `count` tones at 2 MHz spacing.
fn sample_data(count: u8) -> RangingData {
    RangingData {
        ranging_counter: 0x0ABC,
        config_id: 3,
        selected_tx_power: -4,
        antenna_paths_mask: 0x01,
        reference_power_level: -58,
        tones: (0..count)
            .map(|n| Tone {
                channel: n * 2,
                i: 2000 - i16::from(n) * 40,
                q: -1000 + i16::from(n) * 30,
                quality: 0,
            })
            .collect(),
    }
}

#[test]
fn test_ranging_data_survives_a_round_trip_through_the_wire_format() {
    let data = sample_data(19);
    let parsed = RangingData::parse(&data.to_bytes()).expect("parsed");
    assert_eq!(parsed, data);
}

#[test]
fn test_the_ranging_counter_and_config_id_share_two_octets() {
    // The counter is 12 bits and the configuration id 4; packing them
    // wrongly would make a receiver reject every procedure as a mismatch.
    // Note config_id 7 fits in three bits, so this case alone cannot tell
    // a correct 4-bit field from a truncating 3-bit one — see
    // `test_config_id_uses_the_full_four_bit_field` for that.
    let data = RangingData {
        ranging_counter: 0x0FFF,
        config_id: 7,
        ..sample_data(3)
    };
    let bytes = data.to_bytes();
    assert_eq!(u16::from_le_bytes([bytes[0], bytes[1]]), 0x7FFF);
    let parsed = RangingData::parse(&bytes).unwrap();
    assert_eq!(parsed.ranging_counter, 0x0FFF);
    assert_eq!(parsed.config_id, 7);
}

/// RAS v1.0 §3.2.1.2 Table 3.7 gives the Ranging Header as a 12-bit
/// Ranging Counter followed by a **4-bit** Configuration ID, filling the
/// first two octets exactly. This code masked both write and parse with
/// `& 0x07`, so ids 8–15 were truncated on the way out and mis-read on the
/// way in.
///
/// The asserts are deliberately on the *wire bits* as well as the
/// round trip: with a symmetrically wrong mask on both sides, a
/// write→parse round trip of id 8 still yields 0, and of id 15 still
/// yields 7 — the round trip alone proves nothing. Only the raw header
/// word pins the field width down.
#[test]
fn test_config_id_uses_the_full_four_bit_field() {
    for (config_id, expected_nibble) in [(8u8, 0x8u16), (15, 0xF)] {
        let data = RangingData {
            ranging_counter: 0x0ABC,
            config_id,
            ..sample_data(3)
        };
        let bytes = data.to_bytes();

        // Wire check: counter in bits 0–11, config id in bits 12–15.
        let header = u16::from_le_bytes([bytes[0], bytes[1]]);
        assert_eq!(
            header,
            0x0ABC | (expected_nibble << 12),
            "config id {config_id} must occupy bits 12-15 of the ranging header"
        );

        // Parse check: the same nibble comes back, not a 3-bit remnant.
        let parsed = RangingData::parse(&bytes).expect("parsed");
        assert_eq!(parsed.ranging_counter, 0x0ABC);
        assert_eq!(
            parsed.config_id, config_id,
            "config id {config_id} must survive the round trip"
        );
    }
}

/// A header word that arrives from a peer with bit 15 set must be read as
/// a config id of 8–15, not have that bit dropped on the floor. Built as
/// raw bytes rather than via `to_bytes`, so a symmetrical mask bug on both
/// sides cannot hide it.
#[test]
fn test_config_id_bit_15_is_read_from_foreign_bytes() {
    let mut bytes = sample_data(2).to_bytes();
    bytes[0] = 0x34;
    bytes[1] = 0xF2; // header 0xF234: counter 0x234, config id 0xF.
    let parsed = RangingData::parse(&bytes).expect("parsed");
    assert_eq!(parsed.ranging_counter, 0x234);
    assert_eq!(parsed.config_id, 0x0F);

    bytes[1] = 0x82; // header 0x8234: counter 0x234, config id 0x8.
    let parsed = RangingData::parse(&bytes).expect("parsed");
    assert_eq!(parsed.config_id, 0x08);
}

#[test]
fn test_a_truncated_body_is_refused() {
    let bytes = sample_data(5).to_bytes();
    assert!(RangingData::parse(&bytes[..8]).is_none());
    assert!(RangingData::parse(&[]).is_none());
    // A step whose declared length runs off the end must not be read.
    assert!(RangingData::parse(&bytes[..bytes.len() - 2]).is_none());
}

#[test]
fn test_segments_reassemble_into_the_body_they_came_from() {
    let body = sample_data(19).to_bytes();
    assert!(body.len() > 60, "worth segmenting: {}", body.len());
    let segments = segment(&body, 60);
    assert!(segments.len() > 1);
    assert_eq!(segments[0][0] & segmentation::FIRST, segmentation::FIRST);
    assert_eq!(
        segments.last().unwrap()[0] & segmentation::LAST,
        segmentation::LAST
    );

    let mut reassembler = Reassembler::new();
    let mut rebuilt = None;
    for s in &segments {
        rebuilt = reassembler.push(s).or(rebuilt);
    }
    assert_eq!(rebuilt.as_deref(), Some(body.as_slice()));
}

#[test]
fn test_a_body_that_fits_is_one_segment_flagged_both_first_and_last() {
    let body = sample_data(2).to_bytes();
    let segments = segment(&body, 512);
    assert_eq!(segments.len(), 1);
    assert_eq!(
        segments[0][0] & 0x03,
        segmentation::FIRST | segmentation::LAST
    );
    assert_eq!(
        Reassembler::new().push(&segments[0]).as_deref(),
        Some(&body[..])
    );
}

#[test]
fn test_a_dropped_segment_discards_the_body_rather_than_stitching_it() {
    let body = sample_data(19).to_bytes();
    let segments = segment(&body, 60);
    assert!(segments.len() >= 3);
    let mut reassembler = Reassembler::new();
    reassembler.push(&segments[0]);
    // Segment 1 never arrives.
    for s in &segments[2..] {
        assert!(
            reassembler.push(s).is_none(),
            "a gap must not produce a body"
        );
    }
    // And the next complete stream is accepted normally.
    let mut delivered = None;
    for s in &segments {
        delivered = reassembler.push(s).or(delivered);
    }
    assert_eq!(delivered.as_deref(), Some(body.as_slice()));
}

#[test]
fn test_joining_mid_stream_yields_nothing() {
    let segments = segment(&sample_data(19).to_bytes(), 60);
    let mut reassembler = Reassembler::new();
    for s in &segments[1..] {
        assert!(reassembler.push(s).is_none());
    }
}

#[test]
fn test_ranging_data_carries_tones_not_a_distance() {
    // Guards the property that makes RAS what it is: the receiver gets
    // the peer's raw measurements and computes the range itself.
    let subevent = SubeventResult {
        connection_handle: 0x0040,
        config_id: 1,
        procedure_counter: 12,
        reference_power_level: -55,
        num_antenna_paths: 1,
        tones: sample_data(19).tones,
    };
    let data = RangingData::from_subevent(&subevent, 0);
    assert_eq!(data.ranging_counter, 12);
    assert_eq!(data.tones.len(), 19);
    assert_eq!(
        RangingData::parse(&data.to_bytes()).unwrap().tones.len(),
        19
    );
}

#[test]
fn test_ranging_service_registration_and_update() {
    let mut db = GattDatabase::new();
    let ras = RangingService::register(&mut db);

    assert_eq!(ras.service_handle, 1);
    assert_eq!(ras.features_handle, 2);
    assert_eq!(ras.features_value_handle, 3);
    assert_eq!(ras.realtime_data_handle, 4);
    assert_eq!(ras.realtime_data_value_handle, 5);

    // Read Features
    let features = db.read(ras.features_value_handle, 0).unwrap();
    assert_eq!(features, &[0x03, 0x00, 0x00, 0x00]);

    // Update Ranging Data (e.g. 2.45 meters, confidence 0.95)
    let payload = ras.update_ranging_data(&mut db, 2.45, 0.95).unwrap();
    assert_eq!(payload.len(), 8);

    let read_back = db.read(ras.realtime_data_value_handle, 0).unwrap();
    assert_eq!(read_back, &payload);
}
