// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Apple Media Service (AMS) tests: Remote Command playback state machine, Entity
//! Update subscriptions with truncation, Entity Attribute full-value read-back,
//! AMS error codes, and the accessory-side client's payload building and media
//! state caching. Scenarios are derived from the AMS Specification's wire formats
//! (no upstream test suite exists for this profile).

use simble::gatt::GattDatabase;
use simble::profiles::ams::{
    ALL_REMOTE_COMMANDS, AmsClient, ENTITY_UPDATE_MAX_VALUE_LEN, EntityId, MediaService,
    PlaybackInfo, PlaybackState, PlayerAttributeId, QueueAttributeId, RemoteCommandId, RepeatMode,
    ShuffleMode, TrackAttributeId, ams_uuid, entity_update_flags, error_code,
};

fn new_ams(db: &mut GattDatabase) -> MediaService {
    MediaService::register(db, "Simble Music")
}

fn write_command(db: &mut GattDatabase, ams: &MediaService, command: RemoteCommandId) {
    db.write(ams.remote_command_value_handle, &[command as u8])
        .expect("supported command accepted");
}

fn subscribe(db: &mut GattDatabase, ams: &MediaService, entity: EntityId, attributes: &[u8]) {
    let mut payload = vec![entity as u8];
    payload.extend_from_slice(attributes);
    db.write(ams.entity_update_value_handle, &payload)
        .expect("subscription accepted");
}

#[test]
fn test_service_uuids_match_apple_assignments() {
    // The Uuid128 arrays are stored little-endian; Display renders canonical order.
    assert_eq!(
        ams_uuid::APPLE_MEDIA_SERVICE.to_string(),
        "89d3502b-0f36-433a-8ef4-c502ad55f8dc"
    );
    assert_eq!(
        ams_uuid::REMOTE_COMMAND.to_string(),
        "9b3c81d8-57b1-4a8a-b8df-0e56f7ca51c2"
    );
    assert_eq!(
        ams_uuid::ENTITY_UPDATE.to_string(),
        "2f7cabce-808d-411f-9a0c-bb92ba96c102"
    );
    assert_eq!(
        ams_uuid::ENTITY_ATTRIBUTE.to_string(),
        "c6b2f38c-23ab-46d8-a6ab-a3a870bbd5d7"
    );
}

#[test]
fn test_initial_supported_commands_payload() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);

    // The Remote Command notify value lists every supported RemoteCommandID.
    let expected: Vec<u8> = ALL_REMOTE_COMMANDS.iter().map(|&c| c as u8).collect();
    assert_eq!(
        db.read(ams.remote_command_value_handle, 0).unwrap(),
        &expected
    );
    assert_eq!(ams.supported_commands(), ALL_REMOTE_COMMANDS.to_vec());
}

#[test]
fn test_play_pause_toggle_state_machine() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);
    assert_eq!(ams.playback_state(), PlaybackState::Paused);

    write_command(&mut db, &ams, RemoteCommandId::Play);
    assert_eq!(ams.playback_state(), PlaybackState::Playing);

    write_command(&mut db, &ams, RemoteCommandId::Pause);
    assert_eq!(ams.playback_state(), PlaybackState::Paused);

    write_command(&mut db, &ams, RemoteCommandId::TogglePlayPause);
    assert_eq!(ams.playback_state(), PlaybackState::Playing);
    write_command(&mut db, &ams, RemoteCommandId::TogglePlayPause);
    assert_eq!(ams.playback_state(), PlaybackState::Paused);
}

#[test]
fn test_volume_and_mode_commands() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);

    write_command(&mut db, &ams, RemoteCommandId::VolumeUp);
    assert!((ams.volume() - (0.5 + 1.0 / 16.0)).abs() < f32::EPSILON);
    // Volume clamps at 1.0 no matter how many increments arrive.
    for _ in 0..20 {
        write_command(&mut db, &ams, RemoteCommandId::VolumeUp);
    }
    assert_eq!(ams.volume(), 1.0);

    write_command(&mut db, &ams, RemoteCommandId::AdvanceShuffleMode);
    assert_eq!(ams.shuffle_mode(), ShuffleMode::One);
    write_command(&mut db, &ams, RemoteCommandId::AdvanceShuffleMode);
    assert_eq!(ams.shuffle_mode(), ShuffleMode::All);
    write_command(&mut db, &ams, RemoteCommandId::AdvanceShuffleMode);
    assert_eq!(ams.shuffle_mode(), ShuffleMode::Off);

    write_command(&mut db, &ams, RemoteCommandId::AdvanceRepeatMode);
    assert_eq!(ams.repeat_mode(), RepeatMode::One);
}

#[test]
fn test_next_and_previous_track_move_queue_index() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);
    ams.set_queue(&mut db, 0, 3);

    write_command(&mut db, &ams, RemoteCommandId::NextTrack);
    assert_eq!(ams.queue_index(), 1);
    write_command(&mut db, &ams, RemoteCommandId::NextTrack);
    write_command(&mut db, &ams, RemoteCommandId::NextTrack);
    // Wraps at the end of the queue.
    assert_eq!(ams.queue_index(), 0);
    write_command(&mut db, &ams, RemoteCommandId::PreviousTrack);
    assert_eq!(ams.queue_index(), 2);

    // A track change restarts playback time.
    ams.set_playback_info(
        &mut db,
        PlaybackInfo {
            playback_state: PlaybackState::Playing,
            playback_rate: 1.0,
            elapsed_time: 42.0,
        },
    );
    write_command(&mut db, &ams, RemoteCommandId::NextTrack);
    assert_eq!(ams.playback_info().elapsed_time, 0.0);
}

#[test]
fn test_unsupported_and_unknown_commands_rejected() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);
    ams.set_supported_commands(&mut db, &[RemoteCommandId::Play, RemoteCommandId::Pause]);

    // The notify payload shrinks to the new supported set.
    assert_eq!(
        db.read(ams.remote_command_value_handle, 0).unwrap(),
        &[RemoteCommandId::Play as u8, RemoteCommandId::Pause as u8]
    );

    // A defined command outside the supported set.
    assert_eq!(
        db.write(
            ams.remote_command_value_handle,
            &[RemoteCommandId::NextTrack as u8]
        ),
        Err(error_code::INVALID_COMMAND)
    );
    // A RemoteCommandID AMS doesn't define, and malformed lengths.
    assert_eq!(
        db.write(ams.remote_command_value_handle, &[0x7F]),
        Err(error_code::INVALID_COMMAND)
    );
    assert_eq!(
        db.write(ams.remote_command_value_handle, &[]),
        Err(error_code::INVALID_COMMAND)
    );
    assert_eq!(
        db.write(ams.remote_command_value_handle, &[0, 1]),
        Err(error_code::INVALID_COMMAND)
    );
}

#[test]
fn test_subscription_gets_immediate_initial_values() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);

    subscribe(
        &mut db,
        &ams,
        EntityId::Player,
        &[
            PlayerAttributeId::Name as u8,
            PlayerAttributeId::PlaybackInfo as u8,
        ],
    );

    // iOS sends the current value of each attribute upon subscription.
    let updates = ams.take_entity_updates();
    assert_eq!(updates.len(), 2);
    assert_eq!(
        updates[0],
        [
            &[
                EntityId::Player as u8,
                PlayerAttributeId::Name as u8,
                0u8 // flags
            ],
            "Simble Music".as_bytes()
        ]
        .concat()
    );
    assert_eq!(
        updates[1],
        [
            &[
                EntityId::Player as u8,
                PlayerAttributeId::PlaybackInfo as u8,
                0u8
            ],
            "0,1,0".as_bytes() // Paused, rate 1, elapsed 0
        ]
        .concat()
    );
}

#[test]
fn test_updates_only_for_subscribed_attributes() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);

    subscribe(
        &mut db,
        &ams,
        EntityId::Track,
        &[TrackAttributeId::Title as u8],
    );
    ams.take_entity_updates(); // drop the initial-value update

    ams.set_track(&mut db, "Artist", "Album", "Song", 200.0);

    // Only Track Title was subscribed, so artist/album/duration/playback changes
    // produce nothing.
    let updates = ams.take_entity_updates();
    assert_eq!(
        updates,
        vec![
            [
                &[EntityId::Track as u8, TrackAttributeId::Title as u8, 0u8],
                "Song".as_bytes()
            ]
            .concat()
        ]
    );

    // Unsubscribed volume changes are silent too.
    ams.set_volume(&mut db, 0.25);
    assert!(ams.take_entity_updates().is_empty());
}

#[test]
fn test_long_value_truncated_with_flag_and_full_read_back() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);
    subscribe(
        &mut db,
        &ams,
        EntityId::Track,
        &[TrackAttributeId::Title as u8],
    );
    ams.take_entity_updates();

    let long_title = "An Extremely Long Track Title Indeed";
    assert!(long_title.len() > ENTITY_UPDATE_MAX_VALUE_LEN);
    ams.set_track(&mut db, "A", "B", long_title, 100.0);

    let updates = ams.take_entity_updates();
    assert_eq!(updates.len(), 1);
    let update = &updates[0];
    assert_eq!(update[2], entity_update_flags::TRUNCATED);
    assert_eq!(
        &update[3..],
        &long_title.as_bytes()[..ENTITY_UPDATE_MAX_VALUE_LEN]
    );

    // The full value comes back through Entity Attribute: write the selector, then
    // read the characteristic.
    db.write(
        ams.entity_attribute_value_handle,
        &[EntityId::Track as u8, TrackAttributeId::Title as u8],
    )
    .expect("selector accepted");
    assert_eq!(
        db.read(ams.entity_attribute_value_handle, 0).unwrap(),
        long_title.as_bytes()
    );
}

#[test]
fn test_entity_update_and_entity_attribute_errors() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);

    // Unknown EntityID, unknown AttributeID, and an empty subscription write.
    assert_eq!(
        db.write(ams.entity_update_value_handle, &[9, 0]),
        Err(error_code::INVALID_COMMAND)
    );
    assert_eq!(
        db.write(ams.entity_update_value_handle, &[EntityId::Player as u8, 9]),
        Err(error_code::INVALID_COMMAND)
    );
    assert_eq!(
        db.write(ams.entity_update_value_handle, &[]),
        Err(error_code::INVALID_COMMAND)
    );

    // Entity Attribute: selecting an attribute the entity doesn't have.
    assert_eq!(
        db.write(
            ams.entity_attribute_value_handle,
            &[EntityId::Queue as u8, 9]
        ),
        Err(error_code::ABSENT_ATTRIBUTE)
    );
    // Malformed selector length.
    assert_eq!(
        db.write(ams.entity_attribute_value_handle, &[EntityId::Queue as u8]),
        Err(error_code::INVALID_COMMAND)
    );
}

#[test]
fn test_queue_attribute_wire_values() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);
    subscribe(
        &mut db,
        &ams,
        EntityId::Queue,
        &[
            QueueAttributeId::Index as u8,
            QueueAttributeId::Count as u8,
            QueueAttributeId::ShuffleMode as u8,
            QueueAttributeId::RepeatMode as u8,
        ],
    );
    ams.take_entity_updates();

    ams.set_queue(&mut db, 4, 17);
    ams.set_shuffle_mode(&mut db, ShuffleMode::All);
    ams.set_repeat_mode(&mut db, RepeatMode::One);

    // All queue values ride the wire as ASCII decimal strings.
    let updates = ams.take_entity_updates();
    let values: Vec<&[u8]> = updates.iter().map(|u| &u[3..]).collect();
    assert_eq!(values, vec![b"4" as &[u8], b"17", b"2", b"1"]);
}

#[test]
fn test_client_payload_builders() {
    let client = AmsClient::new();
    assert_eq!(
        client.create_remote_command(RemoteCommandId::TogglePlayPause),
        vec![2]
    );
    assert_eq!(
        client.create_entity_update_subscription(
            EntityId::Track,
            &[
                TrackAttributeId::Artist as u8,
                TrackAttributeId::Title as u8
            ]
        ),
        vec![2, 0, 2]
    );
    assert_eq!(
        client.create_entity_attribute_selector(EntityId::Player, PlayerAttributeId::Name as u8),
        vec![0, 0]
    );
}

#[test]
fn test_client_caches_entity_updates_end_to_end() {
    let mut db = GattDatabase::new();
    let ams = new_ams(&mut db);
    let mut client = AmsClient::new();

    // Supported commands arrive as a Remote Command notification.
    client.on_remote_command_notification(db.read(ams.remote_command_value_handle, 0).unwrap());
    assert_eq!(client.supported_commands, ALL_REMOTE_COMMANDS.to_vec());

    subscribe(
        &mut db,
        &ams,
        EntityId::Player,
        &client.create_entity_update_subscription(
            EntityId::Player,
            &[
                PlayerAttributeId::Name as u8,
                PlayerAttributeId::PlaybackInfo as u8,
                PlayerAttributeId::Volume as u8,
            ],
        )[1..],
    );
    subscribe(
        &mut db,
        &ams,
        EntityId::Track,
        &[
            TrackAttributeId::Artist as u8,
            TrackAttributeId::Title as u8,
            TrackAttributeId::Duration as u8,
        ],
    );

    ams.set_track(&mut db, "Artist", "Album", "Song", 187.5);
    ams.set_volume(&mut db, 0.75);
    write_command(&mut db, &ams, RemoteCommandId::Play);

    for update in ams.take_entity_updates() {
        client
            .on_entity_update_notification(&update)
            .expect("well-formed update");
    }

    assert_eq!(client.player_name, "Simble Music");
    assert_eq!(client.track_artist, "Artist");
    assert_eq!(client.track_title, "Song");
    assert_eq!(client.track_duration, 187.5);
    assert_eq!(client.volume, 0.75);
    assert_eq!(client.playback_info.playback_state, PlaybackState::Playing);
}

#[test]
fn test_client_skips_caching_truncated_values() {
    let mut client = AmsClient::new();
    let mut payload = vec![
        EntityId::Track as u8,
        TrackAttributeId::Title as u8,
        entity_update_flags::TRUNCATED,
    ];
    payload.extend_from_slice(b"A Very Long Title");

    let update = client
        .on_entity_update_notification(&payload)
        .expect("well-formed update");
    assert!(update.truncated());
    // The cached title stays untouched until the full value is fetched through
    // the Entity Attribute characteristic.
    assert_eq!(client.track_title, "");

    client.cache_full_value(
        EntityId::Track,
        TrackAttributeId::Title as u8,
        b"A Very Long Title Indeed",
    );
    assert_eq!(client.track_title, "A Very Long Title Indeed");

    // Malformed notifications don't parse.
    assert_eq!(client.on_entity_update_notification(&[2, 2]), None);
    assert_eq!(client.on_entity_update_notification(&[9, 0, 0, b'x']), None);
}
