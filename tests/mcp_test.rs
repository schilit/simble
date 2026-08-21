// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Media Control Service (MCS/GMCS) tests: characteristic updates a client would
//! observe (media state, track title/duration/position) and the Media Control Point
//! state machine driven through GATT writes.

use simble::att::error_code as att_error_code;
use simble::gatt::GattDatabase;
use simble::profiles::mcp::{
    MediaControlPointOpcode, MediaControlService, MediaState, TRACK_LENGTH_UNKNOWN,
    opcodes_supported, result_code,
};

fn new_gmcs(db: &mut GattDatabase) -> MediaControlService {
    MediaControlService::register_generic(db, "Simble Player", 1)
}

fn write_opcode(
    db: &mut GattDatabase,
    gmcs: &MediaControlService,
    op: MediaControlPointOpcode,
) -> [u8; 2] {
    db.write(gmcs.control_point_value_handle, &[op as u8])
        .expect("control point write itself succeeds");
    gmcs.last_control_point_notification()
        .expect("operation produced a notification")
}

#[test]
fn test_initial_values() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);

    assert_eq!(
        db.read(gmcs.media_player_name_value_handle, 0).unwrap(),
        b"Simble Player"
    );
    assert_eq!(
        db.read(gmcs.media_state_value_handle, 0).unwrap(),
        &[MediaState::Paused as u8]
    );
    assert_eq!(
        db.read(gmcs.track_duration_value_handle, 0).unwrap(),
        &TRACK_LENGTH_UNKNOWN.to_le_bytes()
    );
    assert_eq!(
        db.read(gmcs.track_position_value_handle, 0).unwrap(),
        &0i32.to_le_bytes()
    );
    assert_eq!(
        db.read(gmcs.opcodes_supported_value_handle, 0).unwrap(),
        &opcodes_supported::DEFAULT.to_le_bytes()
    );
    assert_eq!(
        db.read(gmcs.content_control_id_value_handle, 0).unwrap(),
        &[1]
    );
}

#[test]
fn test_generic_and_plain_variants_use_distinct_service_uuids() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);
    let mcs = MediaControlService::register(&mut db, "App Player", 2);

    let gmcs_decl = db.read(gmcs.service_handle, 0).unwrap();
    let mcs_decl = db.read(mcs.service_handle, 0).unwrap();
    assert_eq!(gmcs_decl, &0x1849u16.to_le_bytes());
    assert_eq!(mcs_decl, &0x1848u16.to_le_bytes());
}

#[test]
fn test_update_media_state() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);

    gmcs.set_media_state(&mut db, MediaState::Playing);

    assert_eq!(gmcs.media_state(), MediaState::Playing);
    assert_eq!(
        db.read(gmcs.media_state_value_handle, 0).unwrap(),
        &[MediaState::Playing as u8]
    );
}

#[test]
fn test_update_track() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);

    // 180.00 seconds in 0.01-second units.
    gmcs.set_track(&mut db, "My Song", 18000);

    assert_eq!(
        db.read(gmcs.track_title_value_handle, 0).unwrap(),
        b"My Song"
    );
    assert_eq!(
        db.read(gmcs.track_duration_value_handle, 0).unwrap(),
        &18000i32.to_le_bytes()
    );
    assert_eq!(
        db.read(gmcs.track_position_value_handle, 0).unwrap(),
        &0i32.to_le_bytes()
    );
}

#[test]
fn test_client_write_seeks_track_position() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);
    gmcs.set_track(&mut db, "My Song", 18000);

    // Clients seek by writing the Track Position attribute directly (MCS 3.6); the
    // attribute value is the position the state machine sees.
    db.write(gmcs.track_position_value_handle, &1000i32.to_le_bytes())
        .unwrap();
    assert_eq!(gmcs.track_position(&db), 1000);
    assert_eq!(
        db.read(gmcs.track_position_value_handle, 0).unwrap(),
        &1000i32.to_le_bytes()
    );
}

#[test]
fn test_play_pause_stop_state_machine() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);
    gmcs.set_track(&mut db, "My Song", 18000);

    assert_eq!(
        write_opcode(&mut db, &gmcs, MediaControlPointOpcode::Play),
        [MediaControlPointOpcode::Play as u8, result_code::SUCCESS]
    );
    assert_eq!(gmcs.media_state(), MediaState::Playing);
    assert_eq!(
        db.read(gmcs.media_state_value_handle, 0).unwrap(),
        &[MediaState::Playing as u8]
    );

    assert_eq!(
        write_opcode(&mut db, &gmcs, MediaControlPointOpcode::Pause),
        [MediaControlPointOpcode::Pause as u8, result_code::SUCCESS]
    );
    assert_eq!(gmcs.media_state(), MediaState::Paused);

    assert_eq!(
        write_opcode(&mut db, &gmcs, MediaControlPointOpcode::FastForward),
        [
            MediaControlPointOpcode::FastForward as u8,
            result_code::SUCCESS
        ]
    );
    assert_eq!(gmcs.media_state(), MediaState::Seeking);

    db.write(gmcs.track_position_value_handle, &500i32.to_le_bytes())
        .unwrap();
    assert_eq!(
        write_opcode(&mut db, &gmcs, MediaControlPointOpcode::Stop),
        [MediaControlPointOpcode::Stop as u8, result_code::SUCCESS]
    );
    assert_eq!(gmcs.media_state(), MediaState::Paused);
    // Stop rewinds to the start of the track.
    assert_eq!(gmcs.track_position(&db), 0);
}

#[test]
fn test_move_relative() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);
    gmcs.set_track(&mut db, "My Song", 18000);
    db.write(gmcs.track_position_value_handle, &1000i32.to_le_bytes())
        .unwrap();

    let mut pdu = vec![MediaControlPointOpcode::MoveRelative as u8];
    pdu.extend_from_slice(&500i32.to_le_bytes());
    db.write(gmcs.control_point_value_handle, &pdu).unwrap();
    assert_eq!(gmcs.track_position(&db), 1500);

    // Positive offsets past the end clamp to the track duration.
    let mut pdu = vec![MediaControlPointOpcode::MoveRelative as u8];
    pdu.extend_from_slice(&99999i32.to_le_bytes());
    db.write(gmcs.control_point_value_handle, &pdu).unwrap();
    assert_eq!(gmcs.track_position(&db), 18000);

    // Negative offsets move backwards and clamp at the start of the track.
    let mut pdu = vec![MediaControlPointOpcode::MoveRelative as u8];
    pdu.extend_from_slice(&(-99999i32).to_le_bytes());
    db.write(gmcs.control_point_value_handle, &pdu).unwrap();
    assert_eq!(gmcs.track_position(&db), 0);

    // Move Relative without its offset parameter can't be completed.
    db.write(
        gmcs.control_point_value_handle,
        &[MediaControlPointOpcode::MoveRelative as u8],
    )
    .unwrap();
    assert_eq!(
        gmcs.last_control_point_notification().unwrap(),
        [
            MediaControlPointOpcode::MoveRelative as u8,
            result_code::COMMAND_CANNOT_BE_COMPLETED
        ]
    );
}

#[test]
fn test_next_track_rewinds_position() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);
    gmcs.set_track(&mut db, "My Song", 18000);
    db.write(gmcs.track_position_value_handle, &1000i32.to_le_bytes())
        .unwrap();

    assert_eq!(
        write_opcode(&mut db, &gmcs, MediaControlPointOpcode::NextTrack),
        [
            MediaControlPointOpcode::NextTrack as u8,
            result_code::SUCCESS
        ]
    );
    assert_eq!(gmcs.track_position(&db), 0);
}

#[test]
fn test_unsupported_opcodes_notify_not_supported() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);

    // Segment navigation is outside `opcodes_supported::DEFAULT`.
    assert_eq!(
        write_opcode(&mut db, &gmcs, MediaControlPointOpcode::NextSegment),
        [
            MediaControlPointOpcode::NextSegment as u8,
            result_code::OPCODE_NOT_SUPPORTED
        ]
    );

    // An opcode value MCS doesn't define at all.
    db.write(gmcs.control_point_value_handle, &[0xFF]).unwrap();
    assert_eq!(
        gmcs.last_control_point_notification().unwrap(),
        [0xFF, result_code::OPCODE_NOT_SUPPORTED]
    );
}

#[test]
fn test_inactive_player_rejects_commands() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);
    gmcs.set_media_state(&mut db, MediaState::Inactive);

    assert_eq!(
        write_opcode(&mut db, &gmcs, MediaControlPointOpcode::Play),
        [
            MediaControlPointOpcode::Play as u8,
            result_code::MEDIA_PLAYER_INACTIVE
        ]
    );
    assert_eq!(gmcs.media_state(), MediaState::Inactive);
}

#[test]
fn test_empty_control_point_write_is_rejected() {
    let mut db = GattDatabase::new();
    let gmcs = new_gmcs(&mut db);

    assert_eq!(
        db.write(gmcs.control_point_value_handle, &[]),
        Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)
    );
}
