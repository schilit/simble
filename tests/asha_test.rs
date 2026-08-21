// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! ASHA service tests: the ReadOnlyProperties wire layout, the LE_PSM_OUT value, the
//! AudioControlPoint Start/Stop/Status commands with their AudioStatus outcomes, the
//! Volume characteristic, and the service-data advertisement helper.

use simble::att::error_code as att_error_code;
use simble::gap::ad_type;
use simble::gatt::GattDatabase;
use simble::profiles::asha::{
    AshaService, ReadOnlyProperties, audio_status, audio_type, codec, opcode, peripheral_status,
};

const HI_SYNC_ID: [u8; 8] = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];

fn default_properties() -> ReadOnlyProperties {
    ReadOnlyProperties {
        protocol_version: 0x01,
        capabilities: 0,
        hi_sync_id: HI_SYNC_ID,
        feature_map: 0x01,
        render_delay_milliseconds: 0,
        supported_codecs: simble::profiles::asha::supported_codecs::G722_16KHZ,
    }
}

fn new_asha(db: &mut GattDatabase) -> AshaService {
    AshaService::register(db, default_properties(), 0x0080)
}

#[test]
fn test_read_only_properties_layout() {
    let mut db = GattDatabase::new();
    // Distinct values in every field so any layout mixup shows.
    let properties = ReadOnlyProperties {
        protocol_version: 0x01,
        capabilities: 0x02,
        hi_sync_id: HI_SYNC_ID,
        feature_map: 0x03,
        render_delay_milliseconds: 0x04,
        supported_codecs: 0x05,
    };
    let asha = AshaService::register(&mut db, properties, 0x0080);

    let value = db.read(asha.read_only_properties_value_handle, 0).unwrap();
    let mut expected = vec![0x01, 0x02];
    expected.extend_from_slice(&HI_SYNC_ID);
    // FeatureMap, RenderDelay (2 LE), reserved (2), SupportedCodecs (2 LE).
    expected.extend_from_slice(&[0x03, 0x04, 0x00, 0x00, 0x00, 0x05, 0x00]);
    assert_eq!(value, expected);
    assert_eq!(value.len(), 17);

    assert_eq!(ReadOnlyProperties::parse(value), Some(properties));
}

#[test]
fn test_get_psm() {
    let mut db = GattDatabase::new();
    let asha = AshaService::register(&mut db, default_properties(), 0x0093);

    assert_eq!(asha.psm, 0x0093);
    assert_eq!(
        db.read(asha.le_psm_out_value_handle, 0).unwrap(),
        &0x0093u16.to_le_bytes()
    );
}

#[test]
fn test_audio_control_point_start() {
    let mut db = GattDatabase::new();
    let asha = new_asha(&mut db);

    db.write(
        asha.audio_control_point_value_handle,
        &[opcode::START, codec::G722_16KHZ, audio_type::MEDIA, 0, 1],
    )
    .unwrap();

    assert_eq!(
        db.read(asha.audio_status_value_handle, 0).unwrap(),
        &[audio_status::OK]
    );
    assert_eq!(asha.active_codec(), Some(codec::G722_16KHZ));
    assert_eq!(asha.audio_type(), Some(audio_type::MEDIA));
    assert_eq!(asha.volume(), Some(0));
    assert_eq!(asha.other_state(), Some(1));
}

#[test]
fn test_audio_control_point_stop() {
    let mut db = GattDatabase::new();
    let asha = new_asha(&mut db);

    db.write(
        asha.audio_control_point_value_handle,
        &[opcode::START, codec::G722_16KHZ, audio_type::MEDIA, 0, 1],
    )
    .unwrap();
    db.write(asha.audio_control_point_value_handle, &[opcode::STOP])
        .unwrap();

    assert_eq!(
        db.read(asha.audio_status_value_handle, 0).unwrap(),
        &[audio_status::OK]
    );
    assert_eq!(asha.active_codec(), None);
    assert_eq!(asha.audio_type(), None);
    assert_eq!(asha.volume(), None);
    assert_eq!(asha.other_state(), None);
}

#[test]
fn test_audio_control_point_status() {
    let mut db = GattDatabase::new();
    let asha = new_asha(&mut db);

    // Provoke a non-OK AudioStatus first so we can see Status not touching it.
    db.write(asha.audio_control_point_value_handle, &[0x7F])
        .unwrap();
    assert_eq!(
        db.read(asha.audio_status_value_handle, 0).unwrap(),
        &[audio_status::UNKNOWN_COMMAND]
    );

    db.write(
        asha.audio_control_point_value_handle,
        &[
            opcode::STATUS,
            peripheral_status::OTHER_PERIPHERAL_CONNECTED,
        ],
    )
    .unwrap();

    assert_eq!(
        asha.last_peripheral_status(),
        Some(peripheral_status::OTHER_PERIPHERAL_CONNECTED)
    );
    // Status produces no AudioStatus update.
    assert_eq!(
        db.read(asha.audio_status_value_handle, 0).unwrap(),
        &[audio_status::UNKNOWN_COMMAND]
    );
}

#[test]
fn test_audio_control_point_error_paths() {
    let mut db = GattDatabase::new();
    let asha = new_asha(&mut db);

    // A Start command missing parameters reports ILLEGAL_PARAMETERS via AudioStatus;
    // the write itself succeeds (the control point allows Write Without Response).
    db.write(
        asha.audio_control_point_value_handle,
        &[opcode::START, codec::G722_16KHZ],
    )
    .unwrap();
    assert_eq!(
        db.read(asha.audio_status_value_handle, 0).unwrap(),
        &[audio_status::ILLEGAL_PARAMETERS]
    );
    assert_eq!(asha.active_codec(), None);

    // An unknown opcode reports UNKNOWN_COMMAND.
    db.write(asha.audio_control_point_value_handle, &[0x42])
        .unwrap();
    assert_eq!(
        db.read(asha.audio_status_value_handle, 0).unwrap(),
        &[audio_status::UNKNOWN_COMMAND]
    );

    // Only a fully empty write is an ATT-level error.
    assert_eq!(
        db.write(asha.audio_control_point_value_handle, &[]),
        Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)
    );
}

#[test]
fn test_volume_write() {
    let mut db = GattDatabase::new();
    let asha = new_asha(&mut db);

    assert_eq!(asha.volume(), None);
    // ASHA volume is a signed byte; -60 on the wire is 0xC4.
    db.write(asha.volume_value_handle, &[0xC4]).unwrap();
    assert_eq!(asha.volume(), Some(-60));

    assert_eq!(
        db.write(asha.volume_value_handle, &[]),
        Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)
    );
}

#[test]
fn test_advertising_data() {
    let mut db = GattDatabase::new();
    let mut properties = default_properties();
    properties.capabilities = 0x02;
    let asha = AshaService::register(&mut db, properties, 0x0080);

    // Service data payload: version, capabilities, then only the 4 least significant
    // bytes of the HiSyncId.
    assert_eq!(
        asha.advertising_service_data(),
        vec![0x01, 0x02, 0x00, 0x01, 0x02, 0x03]
    );

    // Full AD structure: length, Service Data 16-bit type, UUID 0xFDF0 LE, payload.
    let bytes = asha.advertising_data().to_bytes();
    let mut expected = vec![9, ad_type::SERVICE_DATA_16BIT, 0xF0, 0xFD];
    expected.extend_from_slice(&asha.advertising_service_data());
    assert_eq!(bytes, expected);
}
