// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Apple Notification Center Service (ANCS) tests: Notification Source packets from
//! host-side notification-center changes, the Control Point command set with its
//! Data Source responses and ANCS error codes, and the accessory-side client's
//! command encoding and fragmented Data Source reassembly. Scenarios are derived
//! from the ANCS Specification's wire formats (no upstream test suite exists for
//! this profile).

use simble::att::error_code as att_error_code;
use simble::gatt::GattDatabase;
use simble::profiles::ancs::{
    ActionId, AncsClient, AppAttributeId, CategoryId, CommandId, DataSourceResponse, EventId,
    NotificationAttributeId, NotificationAttributeRequest, NotificationCenterService,
    NotificationContent, NotificationEvent, ancs_uuid, error_code, event_flags,
};

fn sample_content() -> NotificationContent {
    NotificationContent {
        category_id: CategoryId::Email,
        event_flags: event_flags::IMPORTANT,
        app_identifier: "com.apple.mobilemail".to_string(),
        title: "Lunch?".to_string(),
        subtitle: "Team".to_string(),
        message: "Meet at noon".to_string(),
        date: "20260820T120000".to_string(),
        positive_action_label: "Reply".to_string(),
        negative_action_label: "Dismiss".to_string(),
    }
}

/// Convenience: the Data Source response to a Control Point write that succeeds.
fn command(db: &mut GattDatabase, ancs: &NotificationCenterService, payload: &[u8]) -> Vec<u8> {
    db.write(ancs.control_point_value_handle, payload)
        .expect("command accepted");
    ancs.last_data_source_notification()
        .expect("command produced a Data Source response")
}

#[test]
fn test_service_uuids_match_apple_assignments() {
    // The Uuid128 arrays are stored little-endian; Display renders canonical order.
    assert_eq!(
        ancs_uuid::APPLE_NOTIFICATION_CENTER_SERVICE.to_string(),
        "7905f431-b5ce-4e99-a40f-4b1e122d00d0"
    );
    assert_eq!(
        ancs_uuid::NOTIFICATION_SOURCE.to_string(),
        "9fbf120d-6301-42d9-8c58-25e699a21dbd"
    );
    assert_eq!(
        ancs_uuid::CONTROL_POINT.to_string(),
        "69d1d8f3-45e1-49a8-9821-9bbdfdaad9d9"
    );
    assert_eq!(
        ancs_uuid::DATA_SOURCE.to_string(),
        "22eac6e9-24d6-4bb5-be44-b36ace7c7bfb"
    );
}

#[test]
fn test_post_notification_emits_notification_source_packet() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);

    let uid = ancs.post_notification(&mut db, sample_content());

    let packet = ancs
        .last_notification_source_packet()
        .expect("posting emitted a packet");
    let event = NotificationEvent::from_bytes(&packet).expect("valid packet");
    assert_eq!(event.event_id, EventId::NotificationAdded);
    // Action flags are derived from the non-empty action labels.
    assert_eq!(
        event.event_flags,
        event_flags::IMPORTANT | event_flags::POSITIVE_ACTION | event_flags::NEGATIVE_ACTION
    );
    assert_eq!(event.category_id, CategoryId::Email);
    assert_eq!(event.category_count, 1);
    assert_eq!(event.notification_uid, uid);

    // The packet is also parked as the Notification Source characteristic value.
    assert_eq!(
        db.read(ancs.notification_source_value_handle, 0).unwrap(),
        &packet
    );
}

#[test]
fn test_category_count_tracks_same_category_notifications() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);

    ancs.post_notification(&mut db, sample_content());
    let uid2 = ancs.post_notification(&mut db, sample_content());

    let event =
        NotificationEvent::from_bytes(&ancs.last_notification_source_packet().unwrap()).unwrap();
    assert_eq!(event.category_count, 2);

    // Removing one drops the count back and reports the Removed event.
    assert!(ancs.remove_notification(&mut db, uid2));
    let event =
        NotificationEvent::from_bytes(&ancs.last_notification_source_packet().unwrap()).unwrap();
    assert_eq!(event.event_id, EventId::NotificationRemoved);
    assert_eq!(event.notification_uid, uid2);
    assert_eq!(event.category_count, 1);
    assert_eq!(ancs.notification_count(), 1);
}

#[test]
fn test_modify_notification_emits_modified_event() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    let uid = ancs.post_notification(&mut db, sample_content());

    let mut updated = sample_content();
    updated.title = "Lunch moved".to_string();
    assert!(ancs.modify_notification(&mut db, uid, updated));

    let event =
        NotificationEvent::from_bytes(&ancs.last_notification_source_packet().unwrap()).unwrap();
    assert_eq!(event.event_id, EventId::NotificationModified);
    assert_eq!(event.notification_uid, uid);

    // A UID that was never posted can't be modified or removed.
    assert!(!ancs.modify_notification(&mut db, 999, sample_content()));
    assert!(!ancs.remove_notification(&mut db, 999));
}

#[test]
fn test_get_notification_attributes_response_layout() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    let uid = ancs.post_notification(&mut db, sample_content());

    // CommandID, UID, AppIdentifier (no length), Title (max length 65535).
    let mut request = vec![CommandId::GetNotificationAttributes as u8];
    request.extend_from_slice(&uid.to_le_bytes());
    request.push(NotificationAttributeId::AppIdentifier as u8);
    request.push(NotificationAttributeId::Title as u8);
    request.extend_from_slice(&u16::MAX.to_le_bytes());

    let response = command(&mut db, &ancs, &request);

    let mut expected = vec![CommandId::GetNotificationAttributes as u8];
    expected.extend_from_slice(&uid.to_le_bytes());
    expected.push(NotificationAttributeId::AppIdentifier as u8);
    expected.extend_from_slice(&(b"com.apple.mobilemail".len() as u16).to_le_bytes());
    expected.extend_from_slice(b"com.apple.mobilemail");
    expected.push(NotificationAttributeId::Title as u8);
    expected.extend_from_slice(&(b"Lunch?".len() as u16).to_le_bytes());
    expected.extend_from_slice(b"Lunch?");
    assert_eq!(response, expected);

    // The response is also parked as the Data Source characteristic value.
    assert_eq!(
        db.read(ancs.data_source_value_handle, 0).unwrap(),
        &expected
    );
}

#[test]
fn test_get_notification_attributes_truncates_and_reports_sizes() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    let uid = ancs.post_notification(&mut db, sample_content());

    // Message truncated to 4 bytes; Message Size reports the full byte length as
    // an ASCII decimal string; Date echoes the stored yyyyMMdd'T'HHmmSS string.
    let mut request = vec![CommandId::GetNotificationAttributes as u8];
    request.extend_from_slice(&uid.to_le_bytes());
    request.push(NotificationAttributeId::Message as u8);
    request.extend_from_slice(&4u16.to_le_bytes());
    request.push(NotificationAttributeId::MessageSize as u8);
    request.push(NotificationAttributeId::Date as u8);

    let response = command(&mut db, &ancs, &request);

    let mut expected = vec![CommandId::GetNotificationAttributes as u8];
    expected.extend_from_slice(&uid.to_le_bytes());
    expected.push(NotificationAttributeId::Message as u8);
    expected.extend_from_slice(&4u16.to_le_bytes());
    expected.extend_from_slice(b"Meet");
    expected.push(NotificationAttributeId::MessageSize as u8);
    expected.extend_from_slice(&2u16.to_le_bytes());
    expected.extend_from_slice(b"12"); // "Meet at noon" is 12 bytes
    expected.push(NotificationAttributeId::Date as u8);
    expected.extend_from_slice(&15u16.to_le_bytes());
    expected.extend_from_slice(b"20260820T120000");
    assert_eq!(response, expected);
}

#[test]
fn test_absent_attributes_come_back_with_length_zero() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    let uid = ancs.post_notification(
        &mut db,
        NotificationContent {
            category_id: CategoryId::Social,
            ..Default::default()
        },
    );

    let mut request = vec![CommandId::GetNotificationAttributes as u8];
    request.extend_from_slice(&uid.to_le_bytes());
    request.push(NotificationAttributeId::Subtitle as u8);
    request.extend_from_slice(&u16::MAX.to_le_bytes());

    let response = command(&mut db, &ancs, &request);
    let mut expected = vec![CommandId::GetNotificationAttributes as u8];
    expected.extend_from_slice(&uid.to_le_bytes());
    expected.push(NotificationAttributeId::Subtitle as u8);
    expected.extend_from_slice(&0u16.to_le_bytes());
    assert_eq!(response, expected);
}

#[test]
fn test_control_point_error_codes() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    let uid = ancs.post_notification(&mut db, sample_content());

    // Empty write carries no CommandID at all.
    assert_eq!(
        db.write(ancs.control_point_value_handle, &[]),
        Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)
    );

    // A CommandID ANCS doesn't define.
    assert_eq!(
        db.write(ancs.control_point_value_handle, &[0x77]),
        Err(error_code::UNKNOWN_COMMAND)
    );

    // Truncated Get Notification Attributes (UID cut short).
    assert_eq!(
        db.write(
            ancs.control_point_value_handle,
            &[CommandId::GetNotificationAttributes as u8, 0x00, 0x00]
        ),
        Err(error_code::INVALID_COMMAND)
    );

    // A UID that doesn't refer to an existing notification.
    let mut request = vec![CommandId::GetNotificationAttributes as u8];
    request.extend_from_slice(&0xDEAD_BEEFu32.to_le_bytes());
    request.push(NotificationAttributeId::Title as u8);
    request.extend_from_slice(&u16::MAX.to_le_bytes());
    assert_eq!(
        db.write(ancs.control_point_value_handle, &request),
        Err(error_code::INVALID_PARAMETER)
    );

    // A text attribute request missing its 2-byte max length.
    let mut request = vec![CommandId::GetNotificationAttributes as u8];
    request.extend_from_slice(&uid.to_le_bytes());
    request.push(NotificationAttributeId::Title as u8);
    assert_eq!(
        db.write(ancs.control_point_value_handle, &request),
        Err(error_code::INVALID_COMMAND)
    );

    // An AttributeID ANCS doesn't define.
    let mut request = vec![CommandId::GetNotificationAttributes as u8];
    request.extend_from_slice(&uid.to_le_bytes());
    request.push(0x55);
    assert_eq!(
        db.write(ancs.control_point_value_handle, &request),
        Err(error_code::INVALID_PARAMETER)
    );
}

#[test]
fn test_get_app_attributes() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    ancs.register_app("com.apple.mobilemail", "Mail");

    let mut request = vec![CommandId::GetAppAttributes as u8];
    request.extend_from_slice(b"com.apple.mobilemail\0");
    request.push(AppAttributeId::DisplayName as u8);

    let response = command(&mut db, &ancs, &request);
    let mut expected = vec![CommandId::GetAppAttributes as u8];
    expected.extend_from_slice(b"com.apple.mobilemail\0");
    expected.push(AppAttributeId::DisplayName as u8);
    expected.extend_from_slice(&4u16.to_le_bytes());
    expected.extend_from_slice(b"Mail");
    assert_eq!(response, expected);

    // An app identifier that isn't installed on the simulated phone.
    let mut request = vec![CommandId::GetAppAttributes as u8];
    request.extend_from_slice(b"com.example.unknown\0");
    request.push(AppAttributeId::DisplayName as u8);
    assert_eq!(
        db.write(ancs.control_point_value_handle, &request),
        Err(error_code::INVALID_PARAMETER)
    );

    // A request without the NUL terminator is improperly formatted.
    let mut request = vec![CommandId::GetAppAttributes as u8];
    request.extend_from_slice(b"com.apple.mobilemail");
    assert_eq!(
        db.write(ancs.control_point_value_handle, &request),
        Err(error_code::INVALID_COMMAND)
    );
}

#[test]
fn test_perform_notification_action() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    let uid = ancs.post_notification(&mut db, sample_content());

    let mut request = vec![CommandId::PerformNotificationAction as u8];
    request.extend_from_slice(&uid.to_le_bytes());
    request.push(ActionId::Positive as u8);
    db.write(ancs.control_point_value_handle, &request)
        .expect("action accepted");

    assert_eq!(
        ancs.last_performed_action(),
        Some((uid, ActionId::Positive))
    );
    // Performing an action dismisses the notification and reports it removed.
    assert_eq!(ancs.notification_count(), 0);
    let event =
        NotificationEvent::from_bytes(&ancs.last_notification_source_packet().unwrap()).unwrap();
    assert_eq!(event.event_id, EventId::NotificationRemoved);
    assert_eq!(event.notification_uid, uid);

    // The notification is gone now, so acting on it again is Invalid Parameter.
    assert_eq!(
        db.write(ancs.control_point_value_handle, &request),
        Err(error_code::INVALID_PARAMETER)
    );
}

#[test]
fn test_perform_action_without_matching_action_label_fails() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    // No action labels => no action flags => actions can't be performed.
    let uid = ancs.post_notification(
        &mut db,
        NotificationContent {
            message: "hi".to_string(),
            ..Default::default()
        },
    );

    let mut request = vec![CommandId::PerformNotificationAction as u8];
    request.extend_from_slice(&uid.to_le_bytes());
    request.push(ActionId::Negative as u8);
    assert_eq!(
        db.write(ancs.control_point_value_handle, &request),
        Err(error_code::ACTION_FAILED)
    );
    // The failed action leaves the notification in place.
    assert_eq!(ancs.notification_count(), 1);
}

#[test]
fn test_client_command_encodings() {
    let mut client = AncsClient::new();

    // Text attributes always carry a max length (default 65535); others never do.
    let command = client.create_get_notification_attributes_command(
        7,
        &[
            NotificationAttributeRequest::new(NotificationAttributeId::AppIdentifier),
            NotificationAttributeRequest::with_max_length(NotificationAttributeId::Title, 32),
            NotificationAttributeRequest::new(NotificationAttributeId::Message),
        ],
    );
    assert_eq!(
        command,
        vec![0x00, 7, 0, 0, 0, 0x00, 0x01, 32, 0, 0x03, 0xFF, 0xFF]
    );

    let mut client = AncsClient::new();
    let command = client.create_get_app_attributes_command("a.b", &[AppAttributeId::DisplayName]);
    assert_eq!(command, b"\x01a.b\0\x00");

    let command =
        client.create_perform_notification_action_command(0x0102_0304, ActionId::Negative);
    assert_eq!(command, vec![0x02, 0x04, 0x03, 0x02, 0x01, 0x01]);
}

#[test]
fn test_client_parses_notification_source() {
    let event = NotificationEvent {
        event_id: EventId::NotificationAdded,
        event_flags: event_flags::SILENT,
        category_id: CategoryId::Social,
        category_count: 3,
        notification_uid: 42,
    };
    assert_eq!(
        AncsClient::parse_notification_source(&event.to_bytes()),
        Some(event)
    );
    // Short packets don't parse.
    assert_eq!(AncsClient::parse_notification_source(&[0, 0, 4]), None);
}

#[test]
fn test_client_reassembles_fragmented_data_source_response() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    let uid = ancs.post_notification(&mut db, sample_content());

    let mut client = AncsClient::new();
    let request = client.create_get_notification_attributes_command(
        uid,
        &[
            NotificationAttributeRequest::new(NotificationAttributeId::Title),
            NotificationAttributeRequest::new(NotificationAttributeId::Message),
        ],
    );
    let response = command(&mut db, &ancs, &request);

    // Deliver the response in MTU-sized (20-byte) fragments, as a real Data Source
    // notification stream would.
    let mut reassembled = None;
    for fragment in response.chunks(20) {
        assert!(reassembled.is_none(), "response completed too early");
        reassembled = client.on_data_source_notification(fragment);
    }
    assert_eq!(
        reassembled,
        Some(DataSourceResponse::NotificationAttributes {
            notification_uid: uid,
            attributes: vec![
                (NotificationAttributeId::Title, "Lunch?".to_string()),
                (NotificationAttributeId::Message, "Meet at noon".to_string()),
            ],
        })
    );

    // Once complete, stray fragments are ignored until the next command.
    assert_eq!(client.on_data_source_notification(&[0x00]), None);
}

#[test]
fn test_client_round_trips_app_attributes() {
    let mut db = GattDatabase::new();
    let ancs = NotificationCenterService::register(&mut db);
    ancs.register_app("com.apple.MobileSMS", "Messages");

    let mut client = AncsClient::new();
    let request = client
        .create_get_app_attributes_command("com.apple.MobileSMS", &[AppAttributeId::DisplayName]);
    let response = command(&mut db, &ancs, &request);

    assert_eq!(
        client.on_data_source_notification(&response),
        Some(DataSourceResponse::AppAttributes {
            app_identifier: "com.apple.MobileSMS".to_string(),
            attributes: vec![(AppAttributeId::DisplayName, "Messages".to_string())],
        })
    );
}

#[test]
fn test_client_discards_mismatched_response() {
    let mut client = AncsClient::new();
    client.create_get_notification_attributes_command(
        1,
        &[NotificationAttributeRequest::new(
            NotificationAttributeId::Title,
        )],
    );

    // Response for UID 2 while UID 1 is outstanding: the exchange is dropped.
    let mut response = vec![CommandId::GetNotificationAttributes as u8];
    response.extend_from_slice(&2u32.to_le_bytes());
    response.push(NotificationAttributeId::Title as u8);
    response.extend_from_slice(&1u16.to_le_bytes());
    response.push(b'x');
    assert_eq!(client.on_data_source_notification(&response), None);

    // The matching response arriving later is ignored too: reassembly was reset.
    let mut response = vec![CommandId::GetNotificationAttributes as u8];
    response.extend_from_slice(&1u32.to_le_bytes());
    response.push(NotificationAttributeId::Title as u8);
    response.extend_from_slice(&1u16.to_le_bytes());
    response.push(b'x');
    assert_eq!(client.on_data_source_notification(&response), None);
}
