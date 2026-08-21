// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Extended Advertising (Core 5.0), Periodic Advertising (5.0), and Encrypted
//! Advertising Data (5.4) tests, written from the Bluetooth Core Spec.

use simble::crypto::{ccm_decrypt, ccm_encrypt};
use simble::gap::ead::{
    ENCRYPTED_ADVERTISING_DATA_AD_TYPE, KEY_MATERIAL_CHARACTERISTIC_UUID, KeyMaterial, MIC_SIZE,
    RANDOMIZER_SIZE, decrypt_ad, encrypt_ad,
};
use simble::packets::HciCommand;
use simble::packets::ext_adv::{
    AdvSetError, AdvertisingEnableEntry, AdvertisingSets, ExtendedAdvertisingReportHeader,
    LeAdvertisingSetTerminatedEvent, LeExtendedAdvertisingReportEvent,
    LePeriodicAdvertisingCreateSync, LePeriodicAdvertisingReportEventHeader,
    LePeriodicAdvertisingSyncEstablishedEvent, LePeriodicAdvertisingSyncLostEvent,
    LeScanRequestReceivedEvent, LeSetExtendedAdvertisingDataHeader,
    LeSetExtendedAdvertisingEnableHeader, LeSetExtendedAdvertisingParameters,
    LeSetExtendedScanParametersHeader, LeSetPeriodicAdvertisingDataHeader,
    LeSetPeriodicAdvertisingParameters, ScanPhyParameters, U24, adv_event_properties, adv_phy,
    data_operation, ext_adv_opcode, ext_adv_report_event_type, ext_adv_subevent_code,
};
use zerocopy::byteorder::{LittleEndian, U16};
use zerocopy::{FromBytes, IntoBytes, Ref};

fn u16le(value: u16) -> U16<LittleEndian> {
    U16::from_bytes(value.to_le_bytes())
}

fn extended_params(handle: u8) -> LeSetExtendedAdvertisingParameters {
    LeSetExtendedAdvertisingParameters {
        advertising_handle: handle,
        advertising_event_properties: u16le(
            adv_event_properties::CONNECTABLE | adv_event_properties::INCLUDE_TX_POWER,
        ),
        primary_advertising_interval_min: U24::new(0x000020),
        primary_advertising_interval_max: U24::new(0x0000A0),
        primary_advertising_channel_map: 0x07,
        own_address_type: 0x01,
        peer_address_type: 0x00,
        peer_address: [0; 6],
        advertising_filter_policy: 0x00,
        advertising_tx_power: 0x7F,
        primary_advertising_phy: adv_phy::LE_1M,
        secondary_advertising_max_skip: 0x00,
        secondary_advertising_phy: adv_phy::LE_2M,
        advertising_sid: 0x05,
        scan_request_notification_enable: 0x01,
    }
}

#[test]
fn test_opcode_and_subevent_values() {
    assert_eq!(
        LeSetExtendedAdvertisingParameters::OP_CODE.get(),
        ext_adv_opcode::LE_SET_EXTENDED_ADVERTISING_PARAMETERS.get()
    );
    assert_eq!(
        ext_adv_opcode::LE_SET_ADVERTISING_SET_RANDOM_ADDRESS.get(),
        0x2035
    );
    assert_eq!(
        ext_adv_opcode::LE_PERIODIC_ADVERTISING_TERMINATE_SYNC.get(),
        0x2046
    );
    assert_eq!(ext_adv_subevent_code::LE_EXTENDED_ADVERTISING_REPORT, 0x0D);
    assert_eq!(
        ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_SYNC_ESTABLISHED,
        0x0E
    );
    assert_eq!(ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_REPORT, 0x0F);
    assert_eq!(
        ext_adv_subevent_code::LE_PERIODIC_ADVERTISING_SYNC_LOST,
        0x10
    );
    assert_eq!(ext_adv_subevent_code::LE_ADVERTISING_SET_TERMINATED, 0x12);
    assert_eq!(ext_adv_subevent_code::LE_SCAN_REQUEST_RECEIVED, 0x13);
}

#[test]
fn test_extended_advertising_parameters_roundtrip() {
    let params = extended_params(2);
    let bytes = params.as_bytes();
    // 7.8.53: fixed 25-octet parameter block.
    assert_eq!(bytes.len(), 25);
    // The 3-octet interval fields land at offsets 3..6 and 6..9.
    assert_eq!(&bytes[3..6], &[0x20, 0x00, 0x00]);
    assert_eq!(&bytes[6..9], &[0xA0, 0x00, 0x00]);

    let parsed = LeSetExtendedAdvertisingParameters::read_from_bytes(bytes).expect("parse");
    assert_eq!(parsed.advertising_handle, 2);
    assert_eq!(parsed.primary_advertising_interval_min.get(), 0x000020);
    assert_eq!(parsed.primary_advertising_interval_max.get(), 0x0000A0);
    assert_eq!(
        parsed.advertising_event_properties.get(),
        adv_event_properties::CONNECTABLE | adv_event_properties::INCLUDE_TX_POWER
    );
    assert_eq!(parsed.advertising_sid, 5);
}

#[test]
fn test_extended_advertising_data_command_roundtrip() {
    let data = [0x02, 0x01, 0x06, 0x03, 0x03, 0x0F, 0x18];
    let bytes =
        LeSetExtendedAdvertisingDataHeader::serialize(1, data_operation::COMPLETE, 0x01, &data);
    let (header, parsed_data) = LeSetExtendedAdvertisingDataHeader::parse(&bytes).expect("parse");
    assert_eq!(header.advertising_handle, 1);
    assert_eq!(header.operation, data_operation::COMPLETE);
    assert_eq!(header.fragment_preference, 0x01);
    assert_eq!(parsed_data, &data);

    // Truncated payloads must be rejected.
    assert!(LeSetExtendedAdvertisingDataHeader::parse(&bytes[..bytes.len() - 1]).is_none());
}

#[test]
fn test_extended_advertising_enable_array_roundtrip() {
    let entries = [
        AdvertisingEnableEntry {
            advertising_handle: 0,
            duration: u16le(0),
            max_extended_advertising_events: 0,
        },
        AdvertisingEnableEntry {
            advertising_handle: 7,
            duration: u16le(0x0BB8),
            max_extended_advertising_events: 5,
        },
    ];
    let bytes = LeSetExtendedAdvertisingEnableHeader::serialize(true, &entries);
    // 7.8.56: header (2) plus 4 octets per set.
    assert_eq!(bytes.len(), 2 + 2 * 4);
    let (header, parsed) = LeSetExtendedAdvertisingEnableHeader::parse(&bytes).expect("parse");
    assert_eq!(header.enable, 1);
    assert_eq!(header.num_sets, 2);
    assert_eq!(parsed[1].advertising_handle, 7);
    assert_eq!(parsed[1].duration.get(), 0x0BB8);
    assert_eq!(parsed[1].max_extended_advertising_events, 5);
}

#[test]
fn test_extended_scan_parameters_per_phy_roundtrip() {
    // Scanning on LE 1M (bit 0) and LE Coded (bit 2) requires two entries.
    let entries = [
        ScanPhyParameters {
            scan_type: 1,
            scan_interval: u16le(0x0010),
            scan_window: u16le(0x0010),
        },
        ScanPhyParameters {
            scan_type: 0,
            scan_interval: u16le(0x0100),
            scan_window: u16le(0x0080),
        },
    ];
    let bytes = LeSetExtendedScanParametersHeader::serialize(0x01, 0x00, 0b101, &entries);
    let (header, parsed) = LeSetExtendedScanParametersHeader::parse(&bytes).expect("parse");
    assert_eq!(header.scanning_phys, 0b101);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].scan_type, 1);
    assert_eq!(parsed[1].scan_interval.get(), 0x0100);
}

#[test]
fn test_periodic_advertising_create_sync_roundtrip() {
    let cmd = LePeriodicAdvertisingCreateSync {
        options: 0x00,
        advertising_sid: 0x0A,
        advertiser_address_type: 0x01,
        advertiser_address: [0x11, 0x22, 0x33, 0x44, 0x55, 0x66],
        skip: u16le(0x0005),
        sync_timeout: u16le(0x0C80),
        sync_cte_type: 0x00,
    };
    let bytes = cmd.as_bytes();
    // 7.8.67: fixed 14-octet parameter block.
    assert_eq!(bytes.len(), 14);
    let parsed = LePeriodicAdvertisingCreateSync::read_from_bytes(bytes).expect("parse");
    assert_eq!(parsed.advertising_sid, 0x0A);
    assert_eq!(parsed.sync_timeout.get(), 0x0C80);
}

#[test]
fn test_fragmented_data_reassembly_into_manager() {
    let mut sets = AdvertisingSets::new();
    sets.set_parameters(&extended_params(4)).unwrap();

    sets.set_advertising_data(4, data_operation::FIRST_FRAGMENT, &[0x10; 200])
        .unwrap();
    // First/intermediate fragments must not become visible until Last commits.
    assert!(sets.get(4).unwrap().advertising_data.is_empty());
    sets.set_advertising_data(4, data_operation::INTERMEDIATE_FRAGMENT, &[0x20; 200])
        .unwrap();
    sets.set_advertising_data(4, data_operation::LAST_FRAGMENT, &[0x30; 51])
        .unwrap();

    let assembled = &sets.get(4).unwrap().advertising_data;
    assert_eq!(assembled.len(), 451);
    assert_eq!(assembled[0], 0x10);
    assert_eq!(assembled[200], 0x20);
    assert_eq!(assembled[450], 0x30);

    // A fresh Complete operation replaces the assembled data outright.
    sets.set_advertising_data(4, data_operation::COMPLETE, &[0xAA, 0xBB])
        .unwrap();
    assert_eq!(sets.get(4).unwrap().advertising_data, vec![0xAA, 0xBB]);
}

#[test]
fn test_manager_data_length_limit_and_error_paths() {
    let mut sets = AdvertisingSets::new();
    sets.set_parameters(&extended_params(0)).unwrap();

    // 7.8.57: at most 1650 octets of advertising data per set.
    assert_eq!(
        sets.set_advertising_data(0, data_operation::COMPLETE, &[0u8; 1651]),
        Err(AdvSetError::MemoryCapacityExceeded)
    );
    sets.set_advertising_data(0, data_operation::FIRST_FRAGMENT, &[0u8; 1650])
        .unwrap();
    assert_eq!(
        sets.set_advertising_data(0, data_operation::LAST_FRAGMENT, &[0u8; 1]),
        Err(AdvSetError::MemoryCapacityExceeded)
    );

    // Unknown handles map to the Unknown Advertising Identifier status.
    assert_eq!(
        sets.set_advertising_data(9, data_operation::COMPLETE, &[]),
        Err(AdvSetError::UnknownAdvertisingIdentifier)
    );
    assert_eq!(
        sets.set_periodic_enable(true, 9),
        Err(AdvSetError::UnknownAdvertisingIdentifier)
    );
}

#[test]
fn test_manager_enable_lifecycle() {
    let mut sets = AdvertisingSets::new();
    sets.set_parameters(&extended_params(1)).unwrap();
    sets.set_advertising_data(1, data_operation::COMPLETE, &[0x02, 0x01, 0x06])
        .unwrap();

    let enable_entry = [AdvertisingEnableEntry {
        advertising_handle: 1,
        duration: u16le(100),
        max_extended_advertising_events: 3,
    }];
    sets.set_enable(true, &enable_entry).unwrap();
    let set = sets.get(1).unwrap();
    assert!(set.enabled);
    assert_eq!(set.duration, 100);
    assert_eq!(set.max_extended_advertising_events, 3);

    // Parameter changes and removal are disallowed while advertising.
    assert_eq!(
        sets.set_parameters(&extended_params(1)),
        Err(AdvSetError::CommandDisallowed)
    );
    assert_eq!(sets.remove(1), Err(AdvSetError::CommandDisallowed));
    assert_eq!(sets.clear(), Err(AdvSetError::CommandDisallowed));

    // Disable-all via the empty entry list (7.8.56).
    sets.set_enable(false, &[]).unwrap();
    assert!(!sets.get(1).unwrap().enabled);
    sets.remove(1).unwrap();
    assert!(sets.get(1).is_none());
}

#[test]
fn test_manager_periodic_advertising_lifecycle() {
    let mut sets = AdvertisingSets::new();
    sets.set_parameters(&extended_params(2)).unwrap();

    // Periodic data before periodic parameters is disallowed (7.8.62).
    assert_eq!(
        sets.set_periodic_data(2, data_operation::COMPLETE, &[1]),
        Err(AdvSetError::CommandDisallowed)
    );

    let periodic = LeSetPeriodicAdvertisingParameters {
        advertising_handle: 2,
        periodic_advertising_interval_min: u16le(0x0018),
        periodic_advertising_interval_max: u16le(0x0030),
        periodic_advertising_properties: u16le(1 << 6),
    };
    sets.set_periodic_parameters(&periodic).unwrap();

    sets.set_periodic_data(2, data_operation::FIRST_FRAGMENT, &[1, 2])
        .unwrap();
    // Enabling mid-reassembly would advertise undefined data (7.8.63).
    assert_eq!(
        sets.set_periodic_enable(true, 2),
        Err(AdvSetError::CommandDisallowed)
    );
    sets.set_periodic_data(2, data_operation::LAST_FRAGMENT, &[3])
        .unwrap();
    assert_eq!(sets.get(2).unwrap().periodic_data, vec![1, 2, 3]);

    sets.set_periodic_enable(true, 2).unwrap();
    assert!(sets.get(2).unwrap().periodic_enabled);
    assert_eq!(
        sets.set_periodic_parameters(&periodic),
        Err(AdvSetError::CommandDisallowed)
    );
    sets.set_periodic_enable(false, 2).unwrap();
}

#[test]
fn test_periodic_advertising_data_command_roundtrip() {
    let data = [0x05, 0x16, 0x0F, 0x18, 0x64, 0x00];
    let bytes = LeSetPeriodicAdvertisingDataHeader::serialize(3, data_operation::COMPLETE, &data);
    let (header, parsed) = LeSetPeriodicAdvertisingDataHeader::parse(&bytes).expect("parse");
    assert_eq!(header.advertising_handle, 3);
    assert_eq!(header.operation, data_operation::COMPLETE);
    assert_eq!(parsed, &data);
}

#[test]
fn test_multi_report_extended_advertising_report_roundtrip() {
    let make_header = |sid: u8, data_len: u8| ExtendedAdvertisingReportHeader {
        event_type: u16le(
            ext_adv_report_event_type::CONNECTABLE | ext_adv_report_event_type::SCANNABLE,
        ),
        address_type: 0x01,
        address: [0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5],
        primary_phy: adv_phy::LE_1M,
        secondary_phy: adv_phy::LE_CODED,
        advertising_sid: sid,
        tx_power: 9,
        rssi: -70,
        periodic_advertising_interval: u16le(0x0100),
        direct_address_type: 0x00,
        direct_address: [0; 6],
        data_length: data_len,
    };

    let data_a = [0x02, 0x01, 0x06];
    let data_b = [0x07, 0x09, b'S', b'i', b'm', b'b', b'l', b'e'];
    let bytes = LeExtendedAdvertisingReportEvent::serialize(&[
        (make_header(1, data_a.len() as u8), &data_a),
        (make_header(2, data_b.len() as u8), &data_b),
    ]);
    assert_eq!(bytes[0], 2); // Num_Reports

    let reports = LeExtendedAdvertisingReportEvent::parse(&bytes).expect("parse");
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].0.advertising_sid, 1);
    assert_eq!(reports[0].1, &data_a);
    assert_eq!(reports[1].0.advertising_sid, 2);
    assert_eq!(reports[1].0.rssi, -70);
    assert_eq!(reports[1].0.periodic_advertising_interval.get(), 0x0100);
    assert_eq!(reports[1].1, &data_b);

    // Truncating the final report's data must fail the whole event.
    assert!(LeExtendedAdvertisingReportEvent::parse(&bytes[..bytes.len() - 1]).is_none());
    // A zero report count is outside the spec's 1..=0x0A range.
    assert!(LeExtendedAdvertisingReportEvent::parse(&[0]).is_none());
}

#[test]
fn test_periodic_sync_established_report_and_lost_roundtrips() {
    let established = LePeriodicAdvertisingSyncEstablishedEvent {
        status: 0x00,
        sync_handle: u16le(0x0002),
        advertising_sid: 0x0F,
        advertiser_address_type: 0x00,
        advertiser_address: [1, 2, 3, 4, 5, 6],
        advertiser_phy: adv_phy::LE_2M,
        periodic_advertising_interval: u16le(0x0050),
        advertiser_clock_accuracy: 0x01,
    };
    let parsed = LePeriodicAdvertisingSyncEstablishedEvent::read_from_bytes(established.as_bytes())
        .expect("parse");
    assert_eq!(parsed.sync_handle.get(), 0x0002);
    assert_eq!(parsed.periodic_advertising_interval.get(), 0x0050);

    let data = [0xDE, 0xAD, 0xBE, 0xEF];
    let report_bytes =
        LePeriodicAdvertisingReportEventHeader::serialize(0x0002, 0x7F, -50, 0xFF, 0x00, &data);
    let (report, report_data) =
        LePeriodicAdvertisingReportEventHeader::parse(&report_bytes).expect("parse");
    assert_eq!(report.sync_handle.get(), 0x0002);
    assert_eq!(report.rssi, -50);
    assert_eq!(report_data, &data);

    let lost = LePeriodicAdvertisingSyncLostEvent {
        sync_handle: u16le(0x0002),
    };
    let parsed =
        LePeriodicAdvertisingSyncLostEvent::read_from_bytes(lost.as_bytes()).expect("parse");
    assert_eq!(parsed.sync_handle.get(), 0x0002);
}

#[test]
fn test_set_terminated_and_scan_request_received_roundtrips() {
    let terminated = LeAdvertisingSetTerminatedEvent {
        status: 0x00,
        advertising_handle: 3,
        connection_handle: u16le(0x0040),
        num_completed_extended_advertising_events: 2,
    };
    let parsed =
        LeAdvertisingSetTerminatedEvent::read_from_bytes(terminated.as_bytes()).expect("parse");
    assert_eq!(parsed.advertising_handle, 3);
    assert_eq!(parsed.connection_handle.get(), 0x0040);

    let scan_req = LeScanRequestReceivedEvent {
        advertising_handle: 3,
        scanner_address_type: 0x01,
        scanner_address: [9, 8, 7, 6, 5, 4],
    };
    let bytes = scan_req.as_bytes();
    let (parsed, rest) =
        Ref::<&[u8], LeScanRequestReceivedEvent>::from_prefix(bytes).expect("parse");
    assert!(rest.is_empty());
    assert_eq!(parsed.scanner_address, [9, 8, 7, 6, 5, 4]);
}

#[test]
fn test_ccm_rfc3610_packet_vector_1() {
    // RFC 3610 Section 8, Packet Vector #1 (M=8, L=2).
    let key = [
        0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE,
        0xCF,
    ];
    let nonce = [
        0x00, 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,
    ];
    let aad = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
    let plaintext: Vec<u8> = (0x08..=0x1E).collect();
    let expected: [u8; 31] = [
        0x58, 0x8C, 0x97, 0x9A, 0x61, 0xC6, 0x63, 0xD2, 0xF0, 0x66, 0xD0, 0xC2, 0xC0, 0xF9, 0x89,
        0x80, 0x6D, 0x5F, 0x6B, 0x61, 0xDA, 0xC3, 0x84, 0x17, 0xE8, 0xD1, 0x2C, 0xFD, 0xF9, 0x26,
        0xE0,
    ];

    let out = ccm_encrypt(&key, &nonce, &aad, &plaintext, 8);
    assert_eq!(out, expected);
    let decrypted = ccm_decrypt(&key, &nonce, &aad, &out, 8).expect("MIC must verify");
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_ead_encrypt_decrypt_roundtrip() {
    let km = KeyMaterial {
        session_key: [
            0x57, 0x83, 0xD5, 0x21, 0x56, 0xAD, 0x6F, 0x0E, 0x63, 0x88, 0x27, 0x4E, 0xC6, 0x70,
            0x2E, 0xE0,
        ],
        iv: [0x6E, 0x9F, 0x4A, 0x12, 0x70, 0x15, 0x05, 0xA9],
    };
    let randomizer = [0x18, 0xE1, 0x57, 0xCA, 0xDE];
    // Inner AD structure: Complete Local Name "Short Mini-Bus".
    let plaintext: &[u8] = &[
        0x0F, 0x09, 0x53, 0x68, 0x6F, 0x72, 0x74, 0x20, 0x4D, 0x69, 0x6E, 0x69, 0x2D, 0x42, 0x75,
        0x73,
    ];

    let ad = encrypt_ad(&km, &randomizer, plaintext);
    assert_eq!(ad[0] as usize, ad.len() - 1);
    assert_eq!(ad[1], ENCRYPTED_ADVERTISING_DATA_AD_TYPE);
    assert_eq!(&ad[2..2 + RANDOMIZER_SIZE], &randomizer);
    assert_eq!(ad.len(), 2 + RANDOMIZER_SIZE + plaintext.len() + MIC_SIZE);

    let decrypted = decrypt_ad(&km, &ad[2..]).expect("MIC must verify");
    assert_eq!(decrypted, plaintext);
}

#[test]
fn test_ead_tamper_and_wrong_key_rejection() {
    let km = KeyMaterial {
        session_key: [0x11; 16],
        iv: [0x22; 8],
    };
    let randomizer = [1, 2, 3, 4, 5];
    let ad = encrypt_ad(&km, &randomizer, &[0x02, 0x0A, 0x08]);

    let mut wrong_key = km;
    wrong_key.session_key[15] ^= 0x01;
    assert!(decrypt_ad(&wrong_key, &ad[2..]).is_none());

    let mut tampered = ad.clone();
    let mid = tampered.len() / 2;
    tampered[mid] ^= 0x01;
    assert!(decrypt_ad(&km, &tampered[2..]).is_none());

    let mut bad_mic = ad;
    *bad_mic.last_mut().unwrap() ^= 0xFF;
    assert!(decrypt_ad(&km, &bad_mic[2..]).is_none());
}

#[test]
fn test_key_material_characteristic_serialization() {
    assert_eq!(KEY_MATERIAL_CHARACTERISTIC_UUID, 0x2B88);
    let km = KeyMaterial {
        session_key: [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D,
            0x0E, 0x0F,
        ],
        iv: [0xF0, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7],
    };
    let bytes = km.to_bytes();
    assert_eq!(bytes.len(), KeyMaterial::LENGTH);
    // Session Key occupies the first 16 octets, IV the trailing 8 (GSS 0x2B88).
    assert_eq!(&bytes[..16], &km.session_key);
    assert_eq!(&bytes[16..], &km.iv);
    assert_eq!(KeyMaterial::from_bytes(&bytes), Some(km));
    assert_eq!(KeyMaterial::from_bytes(&bytes[..16]), None);
}
