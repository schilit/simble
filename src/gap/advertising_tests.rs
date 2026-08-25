use super::*;

#[test]
fn test_ad_structures_round_trip_the_builder() {
    let bytes = AdvertisingData::new()
        .with_flags(flags::LE_GENERAL_DISCOVERABLE)
        .with_name("Simble")
        .with_service_data_16(0x1852, &[0xEF, 0xCD, 0xAB])
        .to_bytes();
    let parsed: Vec<_> = ad_structures(&bytes).collect();
    assert_eq!(parsed[0], (ad_type::FLAGS, &[0x02][..]));
    assert_eq!(parsed[1], (ad_type::COMPLETE_LOCAL_NAME, &b"Simble"[..]));
    assert_eq!(
        service_data_16(&bytes, 0x1852),
        Some(&[0xEF, 0xCD, 0xAB][..])
    );
    assert_eq!(service_data_16(&bytes, 0x1851), None);
}

#[test]
fn test_a_truncated_structure_ends_the_walk() {
    // Length 5 with only 2 value bytes present: the fragment a controller
    // hands over when it reports incomplete data.
    let truncated = [0x02, 0x01, 0x06, 0x05, 0x16, 0x51, 0x18];
    let parsed: Vec<_> = ad_structures(&truncated).collect();
    assert_eq!(parsed.len(), 1, "the flags structure, and nothing after it");
    assert_eq!(service_data_16(&truncated, 0x1851), None);
}

#[test]
fn test_advertising_data_builder() {
    let ad = AdvertisingData::new()
        .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
        .with_name("Simble-HRM")
        .with_service_uuid_16(0x180D)
        .with_service_data_16(0xFEAA, &[0x00, 0x10]);

    let bytes = ad.to_bytes();

    // Verify Flags element: [0x02, 0x01, 0x06]
    assert_eq!(&bytes[0..3], &[0x02, 0x01, 0x06]);

    // Verify Name element: [0x0B, 0x09, 'S', 'i', 'm', 'b', 'l', 'e', '-', 'H', 'R', 'M']
    assert_eq!(bytes[3], 11);
    assert_eq!(bytes[4], 0x09);
    assert_eq!(&bytes[5..15], b"Simble-HRM");

    // Verify Service UUID element: [0x03, 0x03, 0x0D, 0x18]
    assert_eq!(&bytes[15..19], &[0x03, 0x03, 0x0D, 0x18]);

    // Verify Service Data element: [0x05, 0x16, 0xAA, 0xFE, 0x00, 0x10]
    assert_eq!(&bytes[19..25], &[0x05, 0x16, 0xAA, 0xFE, 0x00, 0x10]);
}

/// A well-known 128-bit UUID with a known textual form, so the expected
/// bytes below can be read off the spec instead of off the encoder:
/// Nearby Share's `0000FE2C-0000-1000-8000-00805F9B34FB`.
const FE2C_128: [u8; 16] = [
    0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x2C, 0xFE, 0x00,
    0x00,
];

#[test]
fn test_a_128_bit_service_uuid_goes_out_least_significant_octet_first() {
    // The one assertion a builder/scanner round trip cannot make: both
    // ends are ours, so a reversed UUID (or AD type 0x06 where 0x07
    // belongs) round-trips perfectly and is still invisible to a phone.
    // These bytes are the spec's, not the encoder's.
    let bytes = AdvertisingData::new()
        .with_service_uuid_128(FE2C_128)
        .to_bytes();
    assert_eq!(bytes[0], 0x11, "length: 1 type octet + 16 UUID octets");
    assert_eq!(
        bytes[1], 0x07,
        "Complete List of 128-bit Service UUIDs, not 0x06"
    );
    assert_eq!(
        &bytes[2..18],
        &[
            0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x2C, 0xFE,
            0x00, 0x00
        ],
        "0000FE2C-0000-1000-8000-00805F9B34FB reversed: FB is the LSO"
    );
    assert_eq!(bytes.len(), 18, "one structure and nothing else");
}

#[test]
fn test_a_reversed_128_bit_uuid_is_a_different_advertisement() {
    // The perturbation that the round trip is blind to: feeding the
    // big-endian bytes produces a different, wrong payload. If this ever
    // passes, the encoder is reversing the caller's bytes for it.
    let mut big_endian = FE2C_128;
    big_endian.reverse();
    let right = AdvertisingData::new()
        .with_service_uuid_128(FE2C_128)
        .to_bytes();
    let wrong = AdvertisingData::new()
        .with_service_uuid_128(big_endian)
        .to_bytes();
    assert_ne!(right, wrong, "byte order has to reach the wire");
    assert_eq!(wrong[2], 0x00, "the big-endian form leads with 0x00");
}

#[test]
fn test_128_bit_uuids_share_one_structure_and_survive_the_walk() {
    let other = [0xAAu8; 16];
    let bytes = AdvertisingData::new()
        .with_service_uuid_128(FE2C_128)
        .with_service_uuid_128(other)
        .to_bytes();
    let parsed: Vec<_> = ad_structures(&bytes).collect();
    assert_eq!(parsed.len(), 1, "one list, not one structure per UUID");
    let (kind, value) = parsed[0];
    assert_eq!(kind, ad_type::COMPLETE_128BIT_UUIDS);
    assert_eq!(&value[..16], &FE2C_128, "in the order they were added");
    assert_eq!(&value[16..], &other);
}

/// The RSI from the CSIS Appendix A sample data: SIRK
/// `457d7d0921a1fd22cecd8c86dd72cccd`, prand `0x69f563`, hash `0x1948da`.
/// Written here as wire octets — least significant first, hash before
/// prand (CSIS Section 4.9) — so the AD structure below is checked against
/// the specification rather than against `csip::rsi`.
const SAMPLE_RSI: [u8; 6] = [0xDA, 0x48, 0x19, 0x63, 0xF5, 0x69];

#[test]
fn test_the_resolvable_set_identifier_structure_is_type_0x2e_and_six_octets() {
    let bytes = AdvertisingData::new()
        .with_resolvable_set_identifier(&SAMPLE_RSI)
        .to_bytes();
    assert_eq!(
        bytes,
        vec![0x07, 0x2E, 0xDA, 0x48, 0x19, 0x63, 0xF5, 0x69],
        "length 7, AD type 0x2E, then hash(3) || prand(3)"
    );
    assert_eq!(
        resolvable_set_identifier(&bytes),
        Some(&SAMPLE_RSI[..]),
        "and the scanner side reads the same six octets back"
    );
}

#[test]
fn test_a_structure_of_the_wrong_length_is_not_read_as_an_rsi() {
    // 0x2E carrying five octets is not an RSI. Resolving it anyway would
    // hand a coordinator a confident answer about a device that never
    // claimed set membership.
    let short = [0x06, 0x2E, 0xDA, 0x48, 0x19, 0x63, 0xF5];
    assert_eq!(resolvable_set_identifier(&short), None);
    assert_eq!(resolvable_set_identifier(&[0x02, 0x01, 0x06]), None);
}

#[test]
fn test_an_rsi_survives_a_full_advertisement_intact() {
    // The RSI is emitted after the service UUIDs, so this also pins that
    // it is not swallowed by the preceding structure's length.
    let bytes = AdvertisingData::new()
        .with_flags(flags::LE_GENERAL_DISCOVERABLE | flags::BR_EDR_NOT_SUPPORTED)
        .with_name("Earbud L")
        .with_service_uuid_16(0x1846)
        .with_resolvable_set_identifier(&SAMPLE_RSI)
        .to_bytes();
    assert!(bytes.len() <= MAX_ADV_LEN, "{} bytes", bytes.len());
    assert_eq!(resolvable_set_identifier(&bytes), Some(&SAMPLE_RSI[..]));
}

/// Extras that cannot be made to fit by any amount of trimming: 24 octets
/// of service data plus a full 128-bit UUID list is 44 octets before the
/// name is even considered.
fn unfittable_extras() -> AdvertisingData {
    let mut extras = AdvertisingData::new().with_service_uuid_128([0xAA; 16]);
    extras.service_data_16.push((0xFE2C, vec![0xAB; 24]));
    extras
}

#[test]
fn test_an_overflowing_payload_is_rejected_not_returned_oversized() {
    // The point of unifying the three trim loops. An oversized payload is
    // written into a fixed 32-byte HCI parameter block, the controller
    // rejects the command, and the device silently never transmits — so
    // every path has to fail loudly instead.
    let extras = unfittable_extras();
    let err = build_adv_payload_with_extras("Beacon", &[0x180F], Some(&extras))
        .expect_err("44 octets of extras cannot fit in 31");
    assert!(err.to_string().contains("31-byte legacy limit"), "{err}");
}

#[test]
fn test_every_builder_path_trims_rather_than_overflowing() {
    // The same 61-character name through all three entry points. None may
    // return more than 31 bytes, and none may return an error, because a
    // name is always trimmable down to nothing.
    let long = "a-device-name-well-past-the-thirty-one-byte-advertising-limit";
    let plain = build_adv_payload(long, &[0x180D, 0x180F, 0x1812]).expect("trims");
    let with_extras =
        build_adv_payload_with_extras(long, &[0x180D], Some(&AdvertisingData::new()))
            .expect("trims");
    for payload in [&plain, &with_extras] {
        assert!(payload.len() <= MAX_ADV_LEN, "{} bytes", payload.len());
        assert!(
            payload.windows(9).any(|w| w == b"a-device-"),
            "the name is trimmed from the tail, and what is left is real"
        );
        assert!(
            payload.contains(&ad_type::SHORTENED_LOCAL_NAME),
            "a trimmed name must be labelled Shortened Local Name (0x08): \
             the scan response still carries the whole name, and emitting the \
             stump as Complete makes the two on-air names contradict each \
             other rather than nest"
        );
        assert!(
            !payload.contains(&[0x09u8][0]) || {
                // 0x09 may appear as a data byte; assert no *AD structure*
                // of type Complete Local Name survives.
                let mut i = 0;
                let mut found = false;
                while i < payload.len() {
                    let len = payload[i] as usize;
                    if len == 0 { break; }
                    if payload.get(i + 1) == Some(&ad_type::COMPLETE_LOCAL_NAME) {
                        found = true;
                    }
                    i += 1 + len;
                }
                !found
            },
            "and no Complete Local Name structure remains beside it"
        );
    }
}

#[test]
fn test_an_untrimmed_name_is_still_a_complete_local_name() {
    let payload = build_adv_payload("short", &[]).expect("fits");
    // Walk the AD structures: the name must be there, typed Complete.
    let mut i = 0;
    let mut name_type = None;
    while i < payload.len() {
        let len = payload[i] as usize;
        if len == 0 {
            break;
        }
        let ad = payload[i + 1];
        if ad == ad_type::COMPLETE_LOCAL_NAME || ad == ad_type::SHORTENED_LOCAL_NAME {
            name_type = Some(ad);
        }
        i += 1 + len;
    }
    assert_eq!(
        name_type,
        Some(ad_type::COMPLETE_LOCAL_NAME),
        "a name that fits whole is Complete — 0x08 is only for a stump"
    );
}

#[test]
fn test_the_uuid_list_is_dropped_before_the_name_is_touched() {
    // 23 characters: flags (3) + name (2 + 23) + one 16-bit UUID (4) is 32
    // octets, one over. The degradation order says the UUID goes, not a
    // character of the name.
    let name = "Twenty-three-characters";
    assert_eq!(name.len(), 23);
    let payload = build_adv_payload(name, &[0x180D]).expect("fits once the UUID is dropped");
    assert!(
        payload.windows(name.len()).any(|w| w == name.as_bytes()),
        "the whole name survives"
    );
    assert!(
        !payload.windows(2).any(|w| w == [0x0D, 0x18]),
        "the UUID list is what gave way"
    );
}

#[test]
fn test_staged_128_bit_uuids_reach_the_air_through_the_extras_path() {
    // `advertise_service_uuid`'s 16-bit sibling shipped broken because
    // nothing checked that a staged field reached the payload. Same guard
    // for the 128-bit list.
    let extras = AdvertisingData::new().with_service_uuid_128(FE2C_128);
    let payload = build_adv_payload_with_extras("Custom", &[], Some(&extras)).expect("fits");
    let list = ad_structures(&payload)
        .find(|(kind, _)| *kind == ad_type::COMPLETE_128BIT_UUIDS)
        .expect("the staged 128-bit UUID is advertised");
    assert_eq!(list.1, &FE2C_128);
}
