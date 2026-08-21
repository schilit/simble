// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Volume Offset Control Service (VOCS) tests: Volume Offset State get/set round-trips,
//! Change_Counter rejection of stale and repeated writes, offset range validation, and
//! Audio Location / Audio Output Description get-set - driven entirely through the public
//! GATT database API.

use simble::gatt::GattDatabase;
use simble::profiles::bap::audio_location;
use simble::profiles::vocs::{VolumeOffsetControlService, VolumeOffsetState, error_code, opcode};

fn register(db: &mut GattDatabase) -> VolumeOffsetControlService {
    VolumeOffsetControlService::register(db, audio_location::NOT_ALLOWED, "")
}

fn set_volume_offset_pdu(change_counter: u8, volume_offset: i16) -> Vec<u8> {
    let mut buf = vec![opcode::SET_VOLUME_OFFSET, change_counter];
    buf.extend_from_slice(&volume_offset.to_le_bytes());
    buf
}

#[test]
fn test_init_service_state() {
    let mut db = GattDatabase::new();
    let vocs = register(&mut db);

    assert_eq!(
        db.read(vocs.volume_offset_state_value_handle, 0).unwrap(),
        &[0, 0, 0]
    );
    assert_eq!(
        db.read(vocs.audio_location_value_handle, 0).unwrap(),
        &audio_location::NOT_ALLOWED.to_le_bytes()
    );
    assert_eq!(
        db.read(vocs.audio_output_description_value_handle, 0)
            .unwrap(),
        b""
    );
}

#[test]
fn test_wrong_opcode_is_rejected() {
    let mut db = GattDatabase::new();
    let mut vocs = register(&mut db);
    assert_eq!(
        vocs.write_control_point(&mut db, &[0xFF]),
        Err(error_code::OPCODE_NOT_SUPPORTED)
    );
}

#[test]
fn test_wrong_change_counter_is_rejected() {
    let mut db = GattDatabase::new();
    let mut vocs = register(&mut db);

    assert_eq!(
        vocs.write_control_point(&mut db, &set_volume_offset_pdu(1, 0)),
        Err(error_code::INVALID_CHANGE_COUNTER)
    );
    assert_eq!(
        db.read(vocs.volume_offset_state_value_handle, 0).unwrap(),
        &[0, 0, 0]
    );
}

#[test]
fn test_volume_offset_out_of_range_is_rejected() {
    let mut db = GattDatabase::new();
    let mut vocs = register(&mut db);

    assert_eq!(
        vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, -256)),
        Err(error_code::VALUE_OUT_OF_RANGE)
    );
    assert_eq!(
        vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, 256)),
        Err(error_code::VALUE_OUT_OF_RANGE)
    );
}

#[test]
fn test_set_volume_offset() {
    let mut db = GattDatabase::new();
    let mut vocs = register(&mut db);

    vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, -255))
        .unwrap();

    assert_eq!(
        vocs.volume_offset_state(),
        VolumeOffsetState {
            volume_offset: -255,
            change_counter: 1,
        }
    );
    assert_eq!(
        db.read(vocs.volume_offset_state_value_handle, 0).unwrap(),
        &[0x01, 0xFF, 1]
    );
}

// Each successful Set Volume Offset advances Change_Counter, so a write reusing a stale
// counter value (e.g. replayed or racing another client) must be rejected even though it
// would have succeeded the first time.
#[test]
fn test_set_volume_offset_requires_fresh_change_counter_each_time() {
    let mut db = GattDatabase::new();
    let mut vocs = register(&mut db);

    vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, 10))
        .unwrap();
    assert_eq!(
        vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, 20)),
        Err(error_code::INVALID_CHANGE_COUNTER)
    );

    vocs.write_control_point(&mut db, &set_volume_offset_pdu(1, 20))
        .unwrap();
    assert_eq!(vocs.volume_offset_state().volume_offset, 20);
    assert_eq!(vocs.volume_offset_state().change_counter, 2);
}

#[test]
fn test_set_audio_location() {
    let mut db = GattDatabase::new();
    let vocs = register(&mut db);

    db.write(
        vocs.audio_location_value_handle,
        &audio_location::FRONT_LEFT.to_le_bytes(),
    )
    .unwrap();
    assert_eq!(
        db.read(vocs.audio_location_value_handle, 0).unwrap(),
        &audio_location::FRONT_LEFT.to_le_bytes()
    );
}

#[test]
fn test_set_audio_output_description() {
    let mut db = GattDatabase::new();
    let vocs = register(&mut db);

    db.write(vocs.audio_output_description_value_handle, b"Left Speaker")
        .unwrap();
    assert_eq!(
        db.read(vocs.audio_output_description_value_handle, 0)
            .unwrap(),
        b"Left Speaker"
    );
}
