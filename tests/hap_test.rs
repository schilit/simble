// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Hearing Access Service (HAS) tests: the Hearing Aid Features bitfield, the preset
//! Control Point state machine driven through GATT writes (read presets, rename,
//! active-preset selection with next/previous wrap-around), its error paths, and the
//! server-side preset list APIs.

use simble::att::error_code as att_error_code;
use simble::gatt::GattDatabase;
use simble::profiles::hap::{
    HearingAccessService, HearingAidFeatures, HearingAidType, PresetRecord, change_id, error_code,
    opcode, preset_properties,
};

// Same fixture presets as the reference HAP scenarios: indices deliberately out of
// insertion order (1, 50, 5) plus one non-writable, unavailable record (78).
fn foo_preset() -> PresetRecord {
    PresetRecord::new(1, "foo preset")
}

fn bar_preset() -> PresetRecord {
    PresetRecord::new(50, "bar preset")
}

fn foobar_preset() -> PresetRecord {
    PresetRecord::new(5, "foobar preset")
}

fn unavailable_preset() -> PresetRecord {
    PresetRecord {
        index: 78,
        writable: false,
        available: false,
        name: "foobar preset".to_string(),
    }
}

fn server_features() -> HearingAidFeatures {
    HearingAidFeatures {
        hearing_aid_type: HearingAidType::Monaural,
        preset_synchronization_supported: false,
        independent_presets: false,
        dynamic_presets: false,
        writable_presets_supported: true,
    }
}

fn new_has(db: &mut GattDatabase) -> HearingAccessService {
    HearingAccessService::register(
        db,
        server_features(),
        &[
            foo_preset(),
            bar_preset(),
            foobar_preset(),
            unavailable_preset(),
        ],
    )
}

/// `[0x02, IsLast]` + record bytes: one Read Preset Response indication payload.
fn read_preset_response(is_last: bool, record: &PresetRecord) -> Vec<u8> {
    let mut payload = vec![opcode::READ_PRESET_RESPONSE, u8::from(is_last)];
    payload.extend_from_slice(&record.to_bytes());
    payload
}

#[test]
fn test_init_service() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    let features_byte = db.read(has.features_value_handle, 0).unwrap()[0];
    assert_eq!(
        HearingAidFeatures::from_byte(features_byte),
        Some(server_features())
    );
    // Monaural (0b01) + writable presets supported (bit 5).
    assert_eq!(features_byte, 0b10_0001);

    // The lowest preset index starts active.
    assert_eq!(has.active_preset_index(), foo_preset().index);
    assert_eq!(
        db.read(has.active_preset_index_value_handle, 0).unwrap(),
        &[foo_preset().index]
    );
    assert!(has.take_control_point_indications().is_empty());
}

#[test]
fn test_preset_record_round_trip() {
    let record = unavailable_preset();
    let bytes = record.to_bytes();
    assert_eq!(bytes[0], 78);
    assert_eq!(bytes[1], 0); // neither WRITABLE nor IS_AVAILABLE
    assert_eq!(PresetRecord::parse(&bytes), Some(record));

    let writable = foo_preset().to_bytes();
    assert_eq!(
        writable[1],
        preset_properties::WRITABLE | preset_properties::IS_AVAILABLE
    );
}

#[test]
fn test_read_all_presets() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    db.write(
        has.preset_control_point_value_handle,
        &[opcode::READ_PRESETS_REQUEST, 1, 0xFF],
    )
    .unwrap();

    // Responses come in increasing index order with IsLast set only on the final one.
    assert_eq!(
        has.take_control_point_indications(),
        vec![
            read_preset_response(false, &foo_preset()),
            read_preset_response(false, &foobar_preset()),
            read_preset_response(false, &bar_preset()),
            read_preset_response(true, &unavailable_preset()),
        ]
    );
    // Drained: nothing more pending.
    assert!(has.take_control_point_indications().is_empty());
}

#[test]
fn test_read_partial_presets() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // Start at index 3 (no record has that exact index) and ask for two.
    db.write(
        has.preset_control_point_value_handle,
        &[opcode::READ_PRESETS_REQUEST, 3, 2],
    )
    .unwrap();

    assert_eq!(
        has.take_control_point_indications(),
        vec![
            read_preset_response(false, &foobar_preset()),
            read_preset_response(true, &bar_preset()),
        ]
    );
}

#[test]
fn test_read_presets_out_of_range() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // StartIndex 0x00 is reserved.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::READ_PRESETS_REQUEST, 0, 1],
        ),
        Err(error_code::OUT_OF_RANGE)
    );
    // NumPresets 0x00 requests nothing.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::READ_PRESETS_REQUEST, 1, 0],
        ),
        Err(error_code::OUT_OF_RANGE)
    );
    // No record at or above the start index.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::READ_PRESETS_REQUEST, 100, 1],
        ),
        Err(error_code::OUT_OF_RANGE)
    );
    // Missing NumPresets parameter.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::READ_PRESETS_REQUEST, 1],
        ),
        Err(error_code::INVALID_PARAMETERS_LENGTH)
    );
    assert!(has.take_control_point_indications().is_empty());
}

#[test]
fn test_set_active_preset_valid() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    db.write(
        has.preset_control_point_value_handle,
        &[opcode::SET_ACTIVE_PRESET, bar_preset().index],
    )
    .unwrap();

    assert_eq!(has.active_preset_index(), bar_preset().index);
    assert_eq!(
        db.read(has.active_preset_index_value_handle, 0).unwrap(),
        &[bar_preset().index]
    );
}

#[test]
fn test_set_active_preset_invalid() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // An unavailable preset cannot become active.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::SET_ACTIVE_PRESET, unavailable_preset().index],
        ),
        Err(error_code::PRESET_OPERATION_NOT_POSSIBLE)
    );
    // Neither can a nonexistent one.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::SET_ACTIVE_PRESET, 42],
        ),
        Err(error_code::PRESET_OPERATION_NOT_POSSIBLE)
    );
    assert_eq!(has.active_preset_index(), foo_preset().index);
}

#[test]
fn test_set_next_preset() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    db.write(
        has.preset_control_point_value_handle,
        &[opcode::SET_NEXT_PRESET],
    )
    .unwrap();

    // Next by index order from 1 among available presets {1, 5, 50} is 5.
    assert_eq!(has.active_preset_index(), foobar_preset().index);
    assert_eq!(
        db.read(has.active_preset_index_value_handle, 0).unwrap(),
        &[foobar_preset().index]
    );
}

#[test]
fn test_set_next_preset_will_loop_to_first() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // Cycling skips the unavailable record (78) and wraps 1 -> 5 -> 50 -> 1.
    for expected in [foobar_preset(), bar_preset(), foo_preset()] {
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::SET_NEXT_PRESET],
        )
        .unwrap();
        assert_eq!(has.active_preset_index(), expected.index);
        assert_eq!(
            db.read(has.active_preset_index_value_handle, 0).unwrap(),
            &[expected.index]
        );
    }
}

#[test]
fn test_set_previous_preset_will_loop_to_last() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    db.write(
        has.preset_control_point_value_handle,
        &[opcode::SET_PREVIOUS_PRESET],
    )
    .unwrap();

    // Previous from the first available preset wraps to the last available one (50,
    // not the unavailable 78).
    assert_eq!(has.active_preset_index(), bar_preset().index);
}

#[test]
fn test_next_preset_with_no_alternative_is_rejected() {
    let mut db = GattDatabase::new();
    let has = HearingAccessService::register(&mut db, server_features(), &[foo_preset()]);

    // Wrapping around a one-element list lands on the current preset: nothing to do.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::SET_NEXT_PRESET],
        ),
        Err(error_code::PRESET_OPERATION_NOT_POSSIBLE)
    );
}

#[test]
fn test_write_preset_name() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    let mut pdu = vec![opcode::WRITE_PRESET_NAME, foo_preset().index];
    pdu.extend_from_slice(b"renamed");
    db.write(has.preset_control_point_value_handle, &pdu)
        .unwrap();

    let renamed = has.preset(foo_preset().index).unwrap();
    assert_eq!(renamed.name, "renamed");

    // The rename is reported as a Preset Changed generic update: [0x03, ChangeId,
    // IsLast, PrevIndex] + the updated record.
    let mut expected = vec![
        opcode::PRESET_CHANGED,
        change_id::GENERIC_UPDATE,
        1,
        foo_preset().index,
    ];
    expected.extend_from_slice(&renamed.to_bytes());
    assert_eq!(has.take_control_point_indications(), vec![expected]);
}

#[test]
fn test_write_preset_name_not_allowed() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // Non-writable record.
    let mut pdu = vec![opcode::WRITE_PRESET_NAME, unavailable_preset().index];
    pdu.extend_from_slice(b"nope");
    assert_eq!(
        db.write(has.preset_control_point_value_handle, &pdu),
        Err(error_code::WRITE_NAME_NOT_ALLOWED)
    );

    // Nonexistent record.
    let mut pdu = vec![opcode::WRITE_PRESET_NAME, 42];
    pdu.extend_from_slice(b"nope");
    assert_eq!(
        db.write(has.preset_control_point_value_handle, &pdu),
        Err(error_code::WRITE_NAME_NOT_ALLOWED)
    );
    assert_eq!(
        has.preset(unavailable_preset().index).unwrap().name,
        unavailable_preset().name
    );
}

#[test]
fn test_write_preset_name_invalid_length() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // Empty name.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::WRITE_PRESET_NAME, foo_preset().index],
        ),
        Err(error_code::INVALID_PARAMETERS_LENGTH)
    );

    // 41 bytes exceeds the 40-byte maximum.
    let mut pdu = vec![opcode::WRITE_PRESET_NAME, foo_preset().index];
    pdu.extend_from_slice(&[b'x'; 41]);
    assert_eq!(
        db.write(has.preset_control_point_value_handle, &pdu),
        Err(error_code::INVALID_PARAMETERS_LENGTH)
    );
    assert_eq!(has.preset(foo_preset().index).unwrap().name, "foo preset");
}

#[test]
fn test_synchronized_locally_opcodes_require_feature_bit() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // The fixture's features report preset synchronization unsupported.
    for op in [
        opcode::SET_ACTIVE_PRESET_SYNCHRONIZED_LOCALLY,
        opcode::SET_NEXT_PRESET_SYNCHRONIZED_LOCALLY,
        opcode::SET_PREVIOUS_PRESET_SYNCHRONIZED_LOCALLY,
    ] {
        assert_eq!(
            db.write(has.preset_control_point_value_handle, &[op, 1]),
            Err(error_code::PRESET_SYNCHRONIZATION_NOT_SUPPORTED)
        );
    }
}

#[test]
fn test_synchronized_locally_opcodes_with_support() {
    let mut db = GattDatabase::new();
    let features = HearingAidFeatures {
        hearing_aid_type: HearingAidType::Binaural,
        preset_synchronization_supported: true,
        ..server_features()
    };
    let has = HearingAccessService::register(
        &mut db,
        features,
        &[foo_preset(), foobar_preset(), bar_preset()],
    );

    db.write(
        has.preset_control_point_value_handle,
        &[
            opcode::SET_ACTIVE_PRESET_SYNCHRONIZED_LOCALLY,
            bar_preset().index,
        ],
    )
    .unwrap();
    assert_eq!(has.active_preset_index(), bar_preset().index);

    db.write(
        has.preset_control_point_value_handle,
        &[opcode::SET_NEXT_PRESET_SYNCHRONIZED_LOCALLY],
    )
    .unwrap();
    assert_eq!(has.active_preset_index(), foo_preset().index);

    db.write(
        has.preset_control_point_value_handle,
        &[opcode::SET_PREVIOUS_PRESET_SYNCHRONIZED_LOCALLY],
    )
    .unwrap();
    assert_eq!(has.active_preset_index(), bar_preset().index);
}

#[test]
fn test_invalid_opcode_and_empty_write() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    assert_eq!(
        db.write(has.preset_control_point_value_handle, &[0x7F]),
        Err(error_code::INVALID_OPCODE)
    );
    assert_eq!(
        db.write(has.preset_control_point_value_handle, &[]),
        Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)
    );
}

#[test]
fn test_server_delete_preset() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // The active preset cannot be deleted.
    assert_eq!(
        has.delete_preset(foo_preset().index),
        Err(error_code::PRESET_OPERATION_NOT_POSSIBLE)
    );

    has.delete_preset(foobar_preset().index).unwrap();
    assert!(has.preset(foobar_preset().index).is_none());
    assert_eq!(
        has.take_control_point_indications(),
        vec![vec![
            opcode::PRESET_CHANGED,
            change_id::PRESET_RECORD_DELETED,
            1,
            foobar_preset().index,
        ]]
    );

    // Deleting a record that no longer exists is rejected.
    assert_eq!(
        has.delete_preset(foobar_preset().index),
        Err(error_code::PRESET_OPERATION_NOT_POSSIBLE)
    );
}

#[test]
fn test_server_preset_availability() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    // The active preset cannot become unavailable.
    assert_eq!(
        has.set_preset_unavailable(foo_preset().index),
        Err(error_code::PRESET_OPERATION_NOT_POSSIBLE)
    );

    has.set_preset_unavailable(bar_preset().index).unwrap();
    assert!(!has.preset(bar_preset().index).unwrap().available);

    has.set_preset_available(unavailable_preset().index)
        .unwrap();
    assert!(has.preset(unavailable_preset().index).unwrap().available);

    assert_eq!(
        has.take_control_point_indications(),
        vec![
            vec![
                opcode::PRESET_CHANGED,
                change_id::PRESET_RECORD_UNAVAILABLE,
                1,
                bar_preset().index,
            ],
            vec![
                opcode::PRESET_CHANGED,
                change_id::PRESET_RECORD_AVAILABLE,
                1,
                unavailable_preset().index,
            ],
        ]
    );

    // A now-unavailable preset is skipped when cycling and rejected as active.
    assert_eq!(
        db.write(
            has.preset_control_point_value_handle,
            &[opcode::SET_ACTIVE_PRESET, bar_preset().index],
        ),
        Err(error_code::PRESET_OPERATION_NOT_POSSIBLE)
    );
}

#[test]
fn test_server_generic_update() {
    let mut db = GattDatabase::new();
    let has = new_has(&mut db);

    let updated = PresetRecord::new(50, "louder bar");
    has.generic_update(foobar_preset().index, updated.clone());

    assert_eq!(has.preset(50).unwrap().name, "louder bar");
    let mut expected = vec![
        opcode::PRESET_CHANGED,
        change_id::GENERIC_UPDATE,
        1,
        foobar_preset().index,
    ];
    expected.extend_from_slice(&updated.to_bytes());
    assert_eq!(has.take_control_point_indications(), vec![expected]);
}
