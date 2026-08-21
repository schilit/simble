// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Port of Bumble's avrcp_test.py test suite: the command/response/event
//! codec round trips, AV/C frame parsing, the AVCTP and AVRCP assemblers,
//! SDP records, and the controller/target command flows (driven
//! synchronously back-to-back instead of over the async device stack).
//! `test_passthrough_key_event_exception` is not ported: it exercises
//! Python delegate exceptions, whose visible wire behavior — a REJECTED
//! pass-through response — is covered by the rejected-key test. Also adds
//! new spec-derived tests for the AVC frame codec and the AVCTP
//! fragmentation/PID-routing layers, which Bumble's suite does not cover.

use simble::classic::avc::{
    self, CommandFrame, CommandType, Frame, PassThrough, ResponseCode, ResponseFrame, operation_id,
};
use simble::classic::avctp::{self, AvctpEvent, MessageAssembler, write_message};
use simble::classic::avrcp::{AVRCP_PID, BLUETOOTH_SIG_COMPANY_ID};
use simble::classic::avrcp::{
    ApplicationSetting, AvrcpEvent, BrowseableItem, CapabilityList, Command,
    ControllerServiceRecord, Event, FolderItem, MAXIMUM_VOLUME, MediaAttribute, MediaElementItem,
    MediaPlayerItem, NO_TRACK_UID, PLAYBACK_POSITION_UNAVAILABLE, PduAssembler, Protocol, Response,
    SettingText, TargetServiceRecord, application_setting_attribute, battery_status, capability_id,
    character_set_id, controller_features, event_id, find_controller_services,
    find_target_services, folder_type, major_player_type, media_attribute_id, media_element_type,
    pdu_id, play_status, player_feature, player_sub_type, repeat_mode_status, scope,
    shuffle_status, status_code, target_features, write_pdu,
};
use simble::classic::sdp::{SdpClient, SdpServer};

const MTU: u16 = 672;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

/// Sends `pdus` from `a` to `b`, then ping-pongs responses until both sides
/// go quiet, returning the events each side observed.
fn exchange(
    a: &mut Protocol,
    b: &mut Protocol,
    pdus: Vec<Vec<u8>>,
) -> (Vec<AvrcpEvent>, Vec<AvrcpEvent>) {
    let mut a_events = Vec::new();
    let mut b_events = Vec::new();
    let mut to_b = pdus;
    let mut to_a: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        let mut next_to_a = Vec::new();
        for pdu in to_b.drain(..) {
            let (out, events) = b.receive(&pdu);
            next_to_a.extend(out);
            b_events.extend(events);
        }
        let mut next_to_b = Vec::new();
        for pdu in to_a.drain(..) {
            let (out, events) = a.receive(&pdu);
            next_to_b.extend(out);
            a_events.extend(events);
        }
        to_a = next_to_a;
        to_b = next_to_b;
        if to_a.is_empty() && to_b.is_empty() {
            break;
        }
    }
    (a_events, b_events)
}

fn protocol_pair() -> (Protocol, Protocol) {
    (Protocol::new(MTU), Protocol::new(MTU))
}

// ---------------------------------------------------------------------------
// Codec round trips
// ---------------------------------------------------------------------------

#[test]
fn test_command_round_trip() {
    let commands = vec![
        Command::GetPlayStatus,
        Command::GetCapabilities {
            capability_id: capability_id::COMPANY_ID,
        },
        Command::SetAbsoluteVolume { volume: 5 },
        Command::GetElementAttributes {
            identifier: 999,
            attribute_ids: vec![
                media_attribute_id::ALBUM_NAME,
                media_attribute_id::ARTIST_NAME,
            ],
        },
        Command::RegisterNotification {
            event_id: event_id::ADDRESSED_PLAYER_CHANGED,
            playback_interval: 123,
        },
        Command::Search {
            character_set_id: character_set_id::UTF_8,
            search_string: "Simble!".into(),
        },
        Command::PlayItem {
            scope: scope::MEDIA_PLAYER_LIST,
            uid: 0,
            uid_counter: 1,
        },
        Command::ListPlayerApplicationSettingAttributes,
        Command::ListPlayerApplicationSettingValues {
            attribute_id: application_setting_attribute::REPEAT_MODE,
        },
        Command::GetCurrentPlayerApplicationSettingValue {
            attribute_ids: vec![
                application_setting_attribute::REPEAT_MODE,
                application_setting_attribute::SHUFFLE_ON_OFF,
            ],
        },
        Command::SetPlayerApplicationSettingValue {
            settings: vec![ApplicationSetting {
                attribute_id: application_setting_attribute::REPEAT_MODE,
                value_id: repeat_mode_status::ALL_TRACK_REPEAT,
            }],
        },
        Command::GetPlayerApplicationSettingAttributeText {
            attribute_ids: vec![
                application_setting_attribute::REPEAT_MODE,
                application_setting_attribute::SHUFFLE_ON_OFF,
            ],
        },
        Command::GetPlayerApplicationSettingValueText {
            attribute_id: application_setting_attribute::REPEAT_MODE,
            value_ids: vec![
                repeat_mode_status::ALL_TRACK_REPEAT,
                repeat_mode_status::GROUP_REPEAT,
            ],
        },
        Command::InformDisplayableCharacterSet {
            character_set_ids: vec![character_set_id::UTF_8],
        },
        Command::InformBatteryStatusOfCt {
            battery_status: battery_status::NORMAL,
        },
        Command::SetAddressedPlayer { player_id: 1 },
        Command::SetBrowsedPlayer { player_id: 1 },
        Command::GetFolderItems {
            scope: scope::NOW_PLAYING,
            start_item: 0,
            end_item: 1,
            attribute_ids: vec![media_attribute_id::ARTIST_NAME],
        },
        Command::ChangePath {
            uid_counter: 1,
            direction: simble::classic::avrcp::direction::DOWN,
            folder_uid: 2,
        },
        Command::GetItemAttributes {
            scope: scope::NOW_PLAYING,
            uid: 0,
            uid_counter: 1,
            attribute_ids: vec![media_attribute_id::DEFAULT_COVER_ART],
        },
        Command::GetTotalNumberOfItems {
            scope: scope::NOW_PLAYING,
        },
        Command::AddToNowPlaying {
            scope: scope::NOW_PLAYING,
            uid: 0,
            uid_counter: 1,
        },
    ];
    for command in commands {
        assert_eq!(
            Command::parse(command.pdu_id(), &command.to_parameters()),
            Some(command.clone()),
            "round trip failed for {command:?}"
        );
    }
}

#[test]
fn test_event_round_trip() {
    let events = vec![
        Event::UidsChanged { uid_counter: 7 },
        Event::TrackChanged { uid: 12356 },
        Event::VolumeChanged { volume: 9 },
        Event::PlaybackStatusChanged {
            play_status: play_status::PLAYING,
        },
        Event::AddressedPlayerChanged {
            player_id: 9,
            uid_counter: 10,
        },
        Event::AvailablePlayersChanged,
        Event::PlaybackPosChanged {
            playback_position: 1314,
        },
        Event::NowPlayingContentChanged,
        Event::PlayerApplicationSettingChanged {
            settings: vec![ApplicationSetting {
                attribute_id: application_setting_attribute::REPEAT_MODE,
                value_id: repeat_mode_status::ALL_TRACK_REPEAT,
            }],
        },
        Event::Generic {
            event_id: event_id::SYSTEM_STATUS_CHANGED,
            data: vec![0x01],
        },
    ];
    for event in events {
        assert_eq!(
            Event::parse(&event.to_bytes()),
            Some(event.clone()),
            "round trip failed for {event:?}"
        );
    }
}

#[test]
fn test_response_round_trip() {
    let responses = vec![
        Response::GetPlayStatus {
            song_length: 1010,
            song_position: 13,
            play_status: play_status::PAUSED,
        },
        Response::GetCapabilities {
            capabilities: CapabilityList::EventIds(vec![
                event_id::ADDRESSED_PLAYER_CHANGED,
                event_id::BATT_STATUS_CHANGED,
            ]),
        },
        Response::GetCapabilities {
            capabilities: CapabilityList::CompanyIds(vec![BLUETOOTH_SIG_COMPANY_ID]),
        },
        Response::RegisterNotification {
            event: Event::PlaybackPosChanged {
                playback_position: 38,
            },
        },
        Response::SetAbsoluteVolume { volume: 99 },
        Response::GetElementAttributes {
            attributes: vec![MediaAttribute {
                attribute_id: media_attribute_id::ALBUM_NAME,
                character_set_id: character_set_id::UTF_8,
                value: "White Album".into(),
            }],
        },
        Response::ListPlayerApplicationSettingAttributes {
            attribute_ids: vec![
                application_setting_attribute::REPEAT_MODE,
                application_setting_attribute::SHUFFLE_ON_OFF,
            ],
        },
        Response::ListPlayerApplicationSettingValues {
            value_ids: vec![
                repeat_mode_status::ALL_TRACK_REPEAT,
                repeat_mode_status::GROUP_REPEAT,
            ],
        },
        Response::GetCurrentPlayerApplicationSettingValue {
            settings: vec![ApplicationSetting {
                attribute_id: application_setting_attribute::REPEAT_MODE,
                value_id: repeat_mode_status::ALL_TRACK_REPEAT,
            }],
        },
        Response::SetPlayerApplicationSettingValue,
        Response::GetPlayerApplicationSettingAttributeText {
            entries: vec![SettingText {
                id: application_setting_attribute::REPEAT_MODE,
                character_set_id: character_set_id::UTF_8,
                text: "Repeat".into(),
            }],
        },
        Response::GetPlayerApplicationSettingValueText {
            entries: vec![SettingText {
                id: repeat_mode_status::ALL_TRACK_REPEAT,
                character_set_id: character_set_id::UTF_8,
                text: "All track repeat".into(),
            }],
        },
        Response::InformDisplayableCharacterSet,
        Response::InformBatteryStatusOfCt,
        Response::SetAddressedPlayer {
            status: status_code::OPERATION_COMPLETED,
        },
        Response::SetBrowsedPlayer {
            status: status_code::OPERATION_COMPLETED,
            uid_counter: 1,
            number_of_items: 2,
            character_set_id: character_set_id::UTF_8,
            folder_names: vec!["folder1".into(), "folder2".into()],
        },
        Response::GetFolderItems {
            status: status_code::OPERATION_COMPLETED,
            uid_counter: 1,
            items: vec![
                BrowseableItem::MediaPlayer(MediaPlayerItem {
                    player_id: 1,
                    major_player_type: major_player_type::AUDIO,
                    player_sub_type: player_sub_type::AUDIO_BOOK,
                    play_status: play_status::FWD_SEEK,
                    features: player_feature::ADD_TO_NOW_PLAYING,
                    character_set_id: character_set_id::UTF_8,
                    displayable_name: "Woo".into(),
                }),
                BrowseableItem::Folder(FolderItem {
                    folder_uid: 1,
                    folder_type: folder_type::ALBUMS,
                    is_playable: true,
                    character_set_id: character_set_id::UTF_8,
                    displayable_name: "Album".into(),
                }),
                BrowseableItem::MediaElement(MediaElementItem {
                    media_element_uid: 1,
                    media_type: media_element_type::AUDIO,
                    character_set_id: character_set_id::UTF_8,
                    displayable_name: "Song".into(),
                    attributes: vec![],
                }),
            ],
        },
        Response::ChangePath {
            status: status_code::OPERATION_COMPLETED,
            number_of_items: 2,
        },
        Response::GetItemAttributes {
            status: status_code::OPERATION_COMPLETED,
            attributes: vec![MediaAttribute {
                attribute_id: media_attribute_id::GENRE,
                character_set_id: character_set_id::UTF_8,
                value: "uuddlrlrabab".into(),
            }],
        },
        Response::GetTotalNumberOfItems {
            status: status_code::OPERATION_COMPLETED,
            uid_counter: 1,
            number_of_items: 2,
        },
        Response::Search {
            status: status_code::OPERATION_COMPLETED,
            uid_counter: 1,
            number_of_items: 2,
        },
        Response::PlayItem {
            status: status_code::OPERATION_COMPLETED,
        },
        Response::AddToNowPlaying {
            status: status_code::OPERATION_COMPLETED,
        },
    ];
    for response in responses {
        assert_eq!(
            Response::parse(response.pdu_id(), &response.to_parameters()),
            Some(response.clone()),
            "round trip failed for {response:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// AV/C frames
// ---------------------------------------------------------------------------

#[test]
fn test_frame_parser() {
    // The first 4 bits of an AV/C frame must be zero.
    assert_eq!(Frame::parse(&hex("11480000")), None);

    // Extended subunit ID 5 + extension byte 2 = ID 7.
    let Some(Frame::Command(frame)) = Frame::parse(&hex("014D0208")) else {
        panic!("expected command frame");
    };
    assert_eq!(frame.subunit_type, avc::subunit_type::PANEL);
    assert_eq!(frame.subunit_id, 7);
    assert_eq!(frame.opcode, 8);

    // Double extension: 5 + 254 + 1 = 260.
    let Some(Frame::Command(frame)) = Frame::parse(&hex("014DFF0108")) else {
        panic!("expected command frame");
    };
    assert_eq!(frame.subunit_type, avc::subunit_type::PANEL);
    assert_eq!(frame.subunit_id, 260);
    assert_eq!(frame.opcode, 8);

    let Some(Frame::Command(frame)) = Frame::parse(&hex("0148000019581000000103")) else {
        panic!("expected command frame");
    };
    assert_eq!(frame.ctype, CommandType::Status);
    assert_eq!(frame.subunit_type, avc::subunit_type::PANEL);
    assert_eq!(frame.subunit_id, 0);
    assert_eq!(frame.opcode, avc::opcode::VENDOR_DEPENDENT);
}

#[test]
fn test_vendor_dependent_command() {
    let bytes = hex("0148000019581000000103");
    let Some(Frame::Command(frame)) = Frame::parse(&bytes) else {
        panic!("expected command frame");
    };
    let vendor = frame.as_vendor_dependent().expect("vendor dependent");
    assert_eq!(vendor.company_id, 0x1958);
    assert_eq!(vendor.data, hex("1000000103"));

    let rebuilt = CommandFrame::vendor_dependent(
        CommandType::Status,
        avc::subunit_type::PANEL,
        0,
        0x1958,
        hex("1000000103"),
    );
    assert_eq!(rebuilt.to_bytes(), bytes);
}

#[test]
fn test_passthrough_commands() {
    let play_pressed = CommandFrame::pass_through(
        CommandType::Control,
        avc::subunit_type::PANEL,
        0,
        &PassThrough {
            pressed: true,
            operation_id: operation_id::PLAY,
            operation_data: Vec::new(),
        },
    );
    let bytes = play_pressed.to_bytes();
    let Some(Frame::Command(parsed)) = Frame::parse(&bytes) else {
        panic!("expected command frame");
    };
    let pass_through = parsed.as_pass_through().expect("pass through");
    assert_eq!(pass_through.operation_id, operation_id::PLAY);
    assert!(pass_through.pressed);
    assert_eq!(parsed.to_bytes(), bytes);
}

// New spec-derived test: response frames, including the released state flag
// (bit 7 of the first PASS THROUGH operand) and operation data.
#[test]
fn test_avc_response_frame_round_trip() {
    let released = ResponseFrame::pass_through(
        ResponseCode::Accepted,
        avc::subunit_type::PANEL,
        0,
        &PassThrough {
            pressed: false,
            operation_id: operation_id::VOLUME_UP,
            operation_data: vec![0x01, 0x02],
        },
    );
    let bytes = released.to_bytes();
    // ACCEPTED, PANEL/0, PASS THROUGH, released VOLUME_UP, 2 data bytes.
    assert_eq!(bytes, vec![0x09, 0x48, 0x7C, 0xC1, 0x02, 0x01, 0x02]);
    let Some(Frame::Response(parsed)) = Frame::parse(&bytes) else {
        panic!("expected response frame");
    };
    assert_eq!(parsed.response, ResponseCode::Accepted);
    let pass_through = parsed.as_pass_through().expect("pass through");
    assert!(!pass_through.pressed);
    assert_eq!(pass_through.operation_id, operation_id::VOLUME_UP);
    assert_eq!(pass_through.operation_data, vec![0x01, 0x02]);

    let vendor = ResponseFrame::vendor_dependent(
        ResponseCode::ImplementedOrStable,
        avc::subunit_type::PANEL,
        0,
        BLUETOOTH_SIG_COMPANY_ID,
        vec![0x10, 0x00, 0x00, 0x00],
    );
    assert_eq!(
        Frame::parse(&vendor.to_bytes()),
        Some(Frame::Response(vendor))
    );
}

// New spec-derived test: reserved and malformed encodings must all parse to
// None (AV/C General Specification 4.1, sections 7.1 and 7.3.4).
#[test]
fn test_avc_reserved_encodings_rejected() {
    // Subunit ID 6 is reserved.
    assert_eq!(Frame::parse(&[0x01, 0x4E, 0x00]), None);
    // Extended subunit types are not supported.
    assert_eq!(Frame::parse(&[0x01, 0xF0, 0x00]), None);
    // Extended subunit ID with reserved extension byte 0.
    assert_eq!(Frame::parse(&[0x01, 0x4D, 0x00, 0x08]), None);
    // Reserved ctype and response code values.
    assert_eq!(Frame::parse(&[0x05, 0x48, 0x00]), None);
    assert_eq!(Frame::parse(&[0x0E, 0x48, 0x00]), None);
    // Truncated frames.
    assert_eq!(Frame::parse(&[]), None);
    assert_eq!(Frame::parse(&[0x01]), None);
    assert_eq!(Frame::parse(&[0x01, 0x48]), None);
}

// ---------------------------------------------------------------------------
// AVCTP transport
// ---------------------------------------------------------------------------

/// Packs an AVCTP response header byte: label, packet type, C/R = response.
fn response_header(label: u8, packet_type: u8) -> u8 {
    (label << 4) | (packet_type << 2) | (1 << 1)
}

// Port of Bumble's assembler scenarios, with the fragment layout per AVCTP
// spec 1.4 section 6.2: continue/end packets carry only the 1-byte header
// (Bumble's assembler expects a PID in every fragment, which the spec only
// defines for single and start packets).
#[test]
fn test_avctp_message_assembler() {
    let mut assembler = MessageAssembler::new();

    // Single packet response.
    let mut pdu = vec![response_header(1, 0b00), 0x11, 0x22];
    pdu.push(0x01);
    assert_eq!(
        assembler.on_pdu(&pdu),
        Some(avctp::Message {
            transaction_label: 1,
            is_command: false,
            ipid: false,
            pid: 0x1122,
            payload: vec![0x01],
        })
    );

    // An unterminated START is superseded by a new SINGLE.
    let payload = vec![0x01, 0x02, 0x03];
    let mut start = vec![response_header(1, 0b01), 3, 0x11, 0x22];
    start.push(payload[0]);
    assert_eq!(assembler.on_pdu(&start), None);
    let mut single = vec![response_header(1, 0b00), 0x11, 0x22];
    single.extend_from_slice(&payload);
    assert_eq!(
        assembler.on_pdu(&single),
        Some(avctp::Message {
            transaction_label: 1,
            is_command: false,
            ipid: false,
            pid: 0x1122,
            payload: payload.clone(),
        })
    );

    // A three-fragment message reassembles.
    let mut start = vec![response_header(1, 0b01), 3, 0x11, 0x22];
    start.push(payload[0]);
    assert_eq!(assembler.on_pdu(&start), None);
    let continue_pdu = vec![response_header(1, 0b10), payload[1]];
    assert_eq!(assembler.on_pdu(&continue_pdu), None);
    let end = vec![response_header(1, 0b11), payload[2]];
    assert_eq!(
        assembler.on_pdu(&end),
        Some(avctp::Message {
            transaction_label: 1,
            is_command: false,
            ipid: false,
            pid: 0x1122,
            payload,
        })
    );

    // A lone END with no transaction in progress is dropped.
    assert_eq!(assembler.on_pdu(&[response_header(1, 0b11), 0x01]), None);
}

// New spec-derived test: exact header packing (AVCTP spec 1.4, section 6.1).
#[test]
fn test_avctp_write_message_packing() {
    // Command, label 3: label << 4, type 00, C/R 0, IPID 0.
    assert_eq!(
        write_message(3, true, false, AVRCP_PID, &[0x01, 0x02], MTU),
        vec![vec![0x30, 0x11, 0x0E, 0x01, 0x02]]
    );
    // IPID response, label 5: C/R 1, IPID 1.
    assert_eq!(
        write_message(5, false, true, AVRCP_PID, &[], MTU),
        vec![vec![0x53, 0x11, 0x0E]]
    );
}

// New spec-derived test: fragmentation writes start/continue/end packets
// that reassemble to the original message, with a correct packet count.
#[test]
fn test_avctp_fragmentation_round_trip() {
    let payload: Vec<u8> = (0..100u8).collect();
    let pdus = write_message(7, true, false, AVRCP_PID, &payload, 16);
    assert!(pdus.len() > 1);
    // Start packet: label 7, type 01, packet count, then the PID.
    assert_eq!(pdus[0][0], (7 << 4) | (0b01 << 2));
    assert_eq!(usize::from(pdus[0][1]), pdus.len());
    assert_eq!(&pdus[0][2..4], &AVRCP_PID.to_be_bytes());
    // Every packet fits the MTU.
    assert!(pdus.iter().all(|pdu| pdu.len() <= 16));

    let mut assembler = MessageAssembler::new();
    let mut message = None;
    for pdu in &pdus {
        assert!(message.is_none());
        message = assembler.on_pdu(pdu);
    }
    assert_eq!(
        message,
        Some(avctp::Message {
            transaction_label: 7,
            is_command: true,
            ipid: false,
            pid: AVRCP_PID,
            payload,
        })
    );
}

// New spec-derived test: PID routing. Commands for a registered PID are
// delivered; commands for an unknown PID draw an IPID response, which the
// sender surfaces as an InvalidPid event (AVCTP spec 1.4, section 5.2).
#[test]
fn test_avctp_protocol_pid_routing() {
    let mut a = avctp::Protocol::new(MTU);
    let mut b = avctp::Protocol::new(MTU);
    b.register_pid(AVRCP_PID);

    // Registered PID: delivered as a command event.
    let pdus = a.send_command(0, AVRCP_PID, &[0xAA]);
    let (out, events) = b.receive(&pdus[0]);
    assert!(out.is_empty());
    assert_eq!(
        events,
        vec![AvctpEvent::Command {
            transaction_label: 0,
            pid: AVRCP_PID,
            payload: vec![0xAA],
        }]
    );

    // Unregistered PID: an automatic IPID response comes back.
    let pdus = a.send_command(1, 0x9999, &[0xBB]);
    let (out, events) = b.receive(&pdus[0]);
    assert!(events.is_empty());
    assert_eq!(out.len(), 1);
    let (back, events) = a.receive(&out[0]);
    assert!(back.is_empty());
    assert_eq!(
        events,
        vec![AvctpEvent::InvalidPid {
            transaction_label: 1,
            pid: 0x9999,
        }]
    );
}

// New spec-derived test: robustness against invalid fragment sequences.
#[test]
fn test_avctp_assembler_robustness() {
    let mut assembler = MessageAssembler::new();

    // A command with the IPID bit set is invalid and dropped.
    assert_eq!(assembler.on_pdu(&[0x01, 0x11, 0x22]), None);

    // A CONTINUE with a mismatched transaction label kills the transaction.
    assert_eq!(
        assembler.on_pdu(&[(1 << 4) | (0b01 << 2), 2, 0x11, 0x22, 0x01]),
        None
    );
    assert_eq!(assembler.on_pdu(&[(2 << 4) | (0b10 << 2), 0x02]), None);
    // ... after which even a matching END finds no transaction.
    assert_eq!(assembler.on_pdu(&[(1 << 4) | (0b11 << 2), 0x03]), None);
}

// ---------------------------------------------------------------------------
// AVRCP PDU assembler
// ---------------------------------------------------------------------------

#[test]
fn test_avrcp_pdu_assembler() {
    let mut assembler = PduAssembler::new();

    // Single packet.
    let mut pdu = vec![0x10, 0b00, 0x00, 0x01];
    pdu.push(0x01);
    assert_eq!(assembler.on_pdu(&pdu), Some((0x10, vec![0x01])));

    // An unterminated START is superseded by a new SINGLE.
    let parameter = vec![0x01, 0x02, 0x03];
    let mut start = vec![0x10, 0b01, 0x00, 0x03];
    start.extend_from_slice(&parameter);
    assert_eq!(assembler.on_pdu(&start), None);
    let mut single = vec![0x10, 0b00, 0x00, 0x03];
    single.extend_from_slice(&parameter);
    assert_eq!(assembler.on_pdu(&single), Some((0x10, parameter.clone())));

    // Three fragments reassemble.
    assert_eq!(assembler.on_pdu(&[0x10, 0b01, 0x00, 0x01, 0x01]), None);
    assert_eq!(assembler.on_pdu(&[0x10, 0b10, 0x00, 0x01, 0x02]), None);
    assert_eq!(
        assembler.on_pdu(&[0x10, 0b11, 0x00, 0x01, 0x03]),
        Some((0x10, parameter))
    );

    // A lone END with no PDU in progress is dropped.
    assert_eq!(
        assembler.on_pdu(&[0x10, 0b11, 0x00, 0x03, 0x01, 0x02, 0x03]),
        None
    );
}

// New spec-derived test: write_pdu fragmentation round-trips through the
// assembler (AVRCP spec 6.3.1).
#[test]
fn test_avrcp_write_pdu_fragmentation() {
    let parameters: Vec<u8> = (0..10u8).collect();
    let pdus = write_pdu(pdu_id::GET_ELEMENT_ATTRIBUTES, &parameters, 4);
    assert_eq!(pdus.len(), 3);
    assert_eq!(pdus[0][1] & 3, 0b01);
    assert_eq!(pdus[1][1] & 3, 0b10);
    assert_eq!(pdus[2][1] & 3, 0b11);

    let mut assembler = PduAssembler::new();
    let mut complete = None;
    for pdu in &pdus {
        assert!(complete.is_none());
        complete = assembler.on_pdu(pdu);
    }
    assert_eq!(complete, Some((pdu_id::GET_ELEMENT_ATTRIBUTES, parameters)));

    // A PDU that fits is a single packet.
    let pdus = write_pdu(pdu_id::GET_PLAY_STATUS, &[], 512);
    assert_eq!(pdus.len(), 1);
    assert_eq!(pdus[0], vec![pdu_id::GET_PLAY_STATUS, 0b00, 0x00, 0x00]);
}

// ---------------------------------------------------------------------------
// Controller/target flows
// ---------------------------------------------------------------------------

#[test]
fn test_get_supported_events() {
    let (mut controller, mut target) = protocol_pair();

    let pdus = controller.get_supported_events().unwrap();
    let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::SupportedEventsReceived(vec![])]
    );

    target.supported_events = vec![event_id::VOLUME_CHANGED];
    let pdus = controller.get_supported_events().unwrap();
    let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::SupportedEventsReceived(vec![
            event_id::VOLUME_CHANGED
        ])]
    );
}

#[test]
fn test_passthrough_key_event() {
    let (mut controller, mut target) = protocol_pair();

    for (key, pressed) in [
        (operation_id::PLAY, true),
        (operation_id::PLAY, false),
        (operation_id::PAUSE, true),
        (operation_id::PAUSE, false),
    ] {
        let pdus = controller.send_key_event(key, pressed).unwrap();
        let (controller_events, target_events) = exchange(&mut controller, &mut target, pdus);
        assert_eq!(
            target_events,
            vec![AvrcpEvent::KeyEvent {
                operation_id: key,
                pressed,
                data: vec![],
            }]
        );
        assert_eq!(
            controller_events,
            vec![AvrcpEvent::PassThroughResponse {
                response: ResponseCode::Accepted,
                operation_id: key,
                pressed,
            }]
        );
    }
}

#[test]
fn test_passthrough_key_event_rejected() {
    let (mut controller, mut target) = protocol_pair();
    target.key_event_response = ResponseCode::Rejected;

    let pdus = controller.send_key_event(operation_id::PLAY, true).unwrap();
    let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::PassThroughResponse {
            response: ResponseCode::Rejected,
            operation_id: operation_id::PLAY,
            pressed: true,
        }]
    );
}

#[test]
fn test_set_volume() {
    let (mut controller, mut target) = protocol_pair();

    for volume in 0..=MAXIMUM_VOLUME {
        let pdus = controller.set_absolute_volume(volume).unwrap();
        let (controller_events, target_events) = exchange(&mut controller, &mut target, pdus);
        assert_eq!(target_events, vec![AvrcpEvent::VolumeSet { volume }]);
        assert_eq!(target.volume, volume);
        assert_eq!(
            controller_events,
            vec![AvrcpEvent::VolumeAccepted { volume }]
        );
    }
}

#[test]
fn test_get_playback_status() {
    let (mut controller, mut target) = protocol_pair();

    for status in [
        play_status::STOPPED,
        play_status::PLAYING,
        play_status::PAUSED,
        play_status::FWD_SEEK,
        play_status::REV_SEEK,
        play_status::ERROR,
    ] {
        target.playback_status = status;
        let pdus = controller.get_play_status().unwrap();
        let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
        assert_eq!(
            controller_events,
            vec![AvrcpEvent::PlayStatusReceived {
                song_length: PLAYBACK_POSITION_UNAVAILABLE,
                song_position: PLAYBACK_POSITION_UNAVAILABLE,
                play_status: status,
            }]
        );
    }
}

#[test]
fn test_get_supported_company_ids() {
    let (mut controller, mut target) = protocol_pair();

    let pdus = controller.get_supported_company_ids().unwrap();
    let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::SupportedCompanyIdsReceived(vec![
            BLUETOOTH_SIG_COMPANY_ID
        ])]
    );
}

#[test]
fn test_list_player_application_settings() {
    let (mut controller, mut target) = protocol_pair();
    let expected: Vec<(u8, Vec<u8>)> = vec![
        (
            application_setting_attribute::REPEAT_MODE,
            vec![
                repeat_mode_status::ALL_TRACK_REPEAT,
                repeat_mode_status::GROUP_REPEAT,
                repeat_mode_status::SINGLE_TRACK_REPEAT,
                repeat_mode_status::OFF,
            ],
        ),
        (
            application_setting_attribute::SHUFFLE_ON_OFF,
            vec![
                shuffle_status::OFF,
                shuffle_status::ALL_TRACKS_SHUFFLE,
                shuffle_status::GROUP_SHUFFLE,
            ],
        ),
    ];
    target.supported_player_app_settings = expected.clone();

    let pdus = controller.list_player_app_setting_attributes().unwrap();
    let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
    let attribute_ids = match &controller_events[..] {
        [AvrcpEvent::AppSettingAttributesReceived(ids)] => ids.clone(),
        other => panic!("unexpected events: {other:?}"),
    };
    assert_eq!(
        attribute_ids,
        expected.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );

    for (attribute_id, values) in &expected {
        let pdus = controller
            .list_player_app_setting_values(*attribute_id)
            .unwrap();
        let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
        assert_eq!(
            controller_events,
            vec![AvrcpEvent::AppSettingValuesReceived {
                attribute_id: *attribute_id,
                value_ids: values.clone(),
            }]
        );
    }
}

#[test]
fn test_get_set_player_app_settings() {
    let (mut controller, mut target) = protocol_pair();

    let settings = vec![
        ApplicationSetting {
            attribute_id: application_setting_attribute::REPEAT_MODE,
            value_id: repeat_mode_status::ALL_TRACK_REPEAT,
        },
        ApplicationSetting {
            attribute_id: application_setting_attribute::SHUFFLE_ON_OFF,
            value_id: shuffle_status::GROUP_SHUFFLE,
        },
    ];
    let pdus = controller.set_player_app_settings(&settings).unwrap();
    let (controller_events, target_events) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        target_events,
        vec![AvrcpEvent::PlayerAppSettingsSet {
            settings: settings.clone(),
        }]
    );
    assert_eq!(target.player_app_settings, settings);
    assert_eq!(controller_events, vec![AvrcpEvent::AppSettingsAccepted]);

    let pdus = controller
        .get_current_player_app_settings(&[
            application_setting_attribute::REPEAT_MODE,
            application_setting_attribute::SHUFFLE_ON_OFF,
        ])
        .unwrap();
    let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::CurrentAppSettingsReceived(settings)]
    );
}

#[test]
fn test_play_item() {
    let (mut controller, mut target) = protocol_pair();

    let pdus = controller
        .play_item(scope::MEDIA_PLAYER_LIST, 0, 1)
        .unwrap();
    let (controller_events, target_events) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        target_events,
        vec![AvrcpEvent::PlayItemRequested {
            scope: scope::MEDIA_PLAYER_LIST,
            uid: 0,
            uid_counter: 1,
        }]
    );
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::PlayItemCompleted {
            status: status_code::OPERATION_COMPLETED,
        }]
    );
}

// ---------------------------------------------------------------------------
// Notification monitoring
// ---------------------------------------------------------------------------

/// Registers `event_id` and returns the INTERIM snapshot event.
fn register_interim(controller: &mut Protocol, target: &mut Protocol, event_id: u8) -> Event {
    let pdus = controller.register_notification(event_id, 0).unwrap();
    let (controller_events, target_events) = exchange(controller, target, pdus);
    assert!(
        target_events.contains(&AvrcpEvent::NotificationRegistered { event_id }),
        "registration not observed by target"
    );
    match &controller_events[..] {
        [
            AvrcpEvent::NotificationReceived {
                event,
                interim: true,
            },
        ] => event.clone(),
        other => panic!("expected interim notification, got {other:?}"),
    }
}

/// Delivers a target-side CHANGED notification and returns the event the
/// controller observed.
fn deliver_changed(controller: &mut Protocol, target: &mut Protocol, pdus: Vec<Vec<u8>>) -> Event {
    assert!(!pdus.is_empty(), "no listener was registered");
    let (target_events, controller_events) = exchange(target, controller, pdus);
    assert!(target_events.is_empty());
    match &controller_events[..] {
        [
            AvrcpEvent::NotificationReceived {
                event,
                interim: false,
            },
        ] => event.clone(),
        other => panic!("expected changed notification, got {other:?}"),
    }
}

#[test]
fn test_monitor_volume() {
    let (mut controller, mut target) = protocol_pair();
    target.supported_events = vec![event_id::VOLUME_CHANGED];

    for volume in 0..=MAXIMUM_VOLUME {
        target.volume = 0;
        let interim = register_interim(&mut controller, &mut target, event_id::VOLUME_CHANGED);
        assert_eq!(interim, Event::VolumeChanged { volume: 0 });

        let pdus = target.notify_volume_changed(volume);
        let changed = deliver_changed(&mut controller, &mut target, pdus);
        assert_eq!(changed, Event::VolumeChanged { volume });
    }
}

#[test]
fn test_monitor_playback_status() {
    let (mut controller, mut target) = protocol_pair();
    target.supported_events = vec![event_id::PLAYBACK_STATUS_CHANGED];

    for status in [
        play_status::STOPPED,
        play_status::PLAYING,
        play_status::PAUSED,
        play_status::FWD_SEEK,
        play_status::REV_SEEK,
        play_status::ERROR,
    ] {
        target.playback_status = play_status::STOPPED;
        let interim = register_interim(
            &mut controller,
            &mut target,
            event_id::PLAYBACK_STATUS_CHANGED,
        );
        assert_eq!(
            interim,
            Event::PlaybackStatusChanged {
                play_status: play_status::STOPPED,
            }
        );

        let pdus = target.notify_playback_status_changed(status);
        let changed = deliver_changed(&mut controller, &mut target, pdus);
        assert_eq!(
            changed,
            Event::PlaybackStatusChanged {
                play_status: status
            }
        );
    }
}

#[test]
fn test_monitor_now_playing_content() {
    let (mut controller, mut target) = protocol_pair();
    target.supported_events = vec![event_id::NOW_PLAYING_CONTENT_CHANGED];

    for _ in 0..2 {
        let interim = register_interim(
            &mut controller,
            &mut target,
            event_id::NOW_PLAYING_CONTENT_CHANGED,
        );
        assert_eq!(interim, Event::NowPlayingContentChanged);

        let pdus = target.notify_now_playing_content_changed();
        let changed = deliver_changed(&mut controller, &mut target, pdus);
        assert_eq!(changed, Event::NowPlayingContentChanged);
    }
}

#[test]
fn test_monitor_track_changed() {
    let (mut controller, mut target) = protocol_pair();
    target.supported_events = vec![event_id::TRACK_CHANGED];

    let interim = register_interim(&mut controller, &mut target, event_id::TRACK_CHANGED);
    assert_eq!(interim, Event::TrackChanged { uid: NO_TRACK_UID });

    let pdus = target.notify_track_changed(1);
    let changed = deliver_changed(&mut controller, &mut target, pdus);
    assert_eq!(changed, Event::TrackChanged { uid: 1 });
}

#[test]
fn test_monitor_uid_changed() {
    let (mut controller, mut target) = protocol_pair();
    target.supported_events = vec![event_id::UIDS_CHANGED];

    let interim = register_interim(&mut controller, &mut target, event_id::UIDS_CHANGED);
    assert_eq!(interim, Event::UidsChanged { uid_counter: 0 });

    let pdus = target.notify_uids_changed(1);
    let changed = deliver_changed(&mut controller, &mut target, pdus);
    assert_eq!(changed, Event::UidsChanged { uid_counter: 1 });
}

#[test]
fn test_monitor_addressed_player() {
    let (mut controller, mut target) = protocol_pair();
    target.supported_events = vec![event_id::ADDRESSED_PLAYER_CHANGED];

    let interim = register_interim(
        &mut controller,
        &mut target,
        event_id::ADDRESSED_PLAYER_CHANGED,
    );
    assert_eq!(
        interim,
        Event::AddressedPlayerChanged {
            player_id: 0,
            uid_counter: 0,
        }
    );

    let pdus = target.notify_addressed_player_changed(1, 1);
    let changed = deliver_changed(&mut controller, &mut target, pdus);
    assert_eq!(
        changed,
        Event::AddressedPlayerChanged {
            player_id: 1,
            uid_counter: 1,
        }
    );
}

#[test]
fn test_monitor_player_app_settings() {
    let (mut controller, mut target) = protocol_pair();
    target.supported_events = vec![event_id::PLAYER_APPLICATION_SETTING_CHANGED];
    target.player_app_settings = vec![ApplicationSetting {
        attribute_id: application_setting_attribute::REPEAT_MODE,
        value_id: repeat_mode_status::ALL_TRACK_REPEAT,
    }];

    let interim = register_interim(
        &mut controller,
        &mut target,
        event_id::PLAYER_APPLICATION_SETTING_CHANGED,
    );
    assert_eq!(
        interim,
        Event::PlayerApplicationSettingChanged {
            settings: vec![ApplicationSetting {
                attribute_id: application_setting_attribute::REPEAT_MODE,
                value_id: repeat_mode_status::ALL_TRACK_REPEAT,
            }],
        }
    );

    let pdus = target.notify_player_app_settings_changed(&[ApplicationSetting {
        attribute_id: application_setting_attribute::REPEAT_MODE,
        value_id: repeat_mode_status::GROUP_REPEAT,
    }]);
    let changed = deliver_changed(&mut controller, &mut target, pdus);
    assert_eq!(
        changed,
        Event::PlayerApplicationSettingChanged {
            settings: vec![ApplicationSetting {
                attribute_id: application_setting_attribute::REPEAT_MODE,
                value_id: repeat_mode_status::GROUP_REPEAT,
            }],
        }
    );
}

// New spec-derived test: registering an event the target does not support
// draws a NOT IMPLEMENTED response (AVRCP spec 6.7.2).
#[test]
fn test_register_notification_unsupported() {
    let (mut controller, mut target) = protocol_pair();

    let pdus = controller
        .register_notification(event_id::VOLUME_CHANGED, 0)
        .unwrap();
    let (controller_events, target_events) = exchange(&mut controller, &mut target, pdus);
    assert!(target_events.is_empty());
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::CommandNotImplemented {
            pdu_id: pdu_id::REGISTER_NOTIFICATION,
        }]
    );
}

// New spec-derived test: GetElementAttributes served end-to-end from the
// target's track metadata, with and without an attribute filter.
#[test]
fn test_get_element_attributes() {
    let (mut controller, mut target) = protocol_pair();
    let title = MediaAttribute {
        attribute_id: media_attribute_id::TITLE,
        character_set_id: character_set_id::UTF_8,
        value: "Song".into(),
    };
    let album = MediaAttribute {
        attribute_id: media_attribute_id::ALBUM_NAME,
        character_set_id: character_set_id::UTF_8,
        value: "Album".into(),
    };
    target.element_attributes = vec![title.clone(), album.clone()];

    let pdus = controller
        .get_element_attributes(0, &[media_attribute_id::ALBUM_NAME])
        .unwrap();
    let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::ElementAttributesReceived(vec![album.clone()])]
    );

    // An empty attribute list requests everything.
    let pdus = controller.get_element_attributes(0, &[]).unwrap();
    let (controller_events, _) = exchange(&mut controller, &mut target, pdus);
    assert_eq!(
        controller_events,
        vec![AvrcpEvent::ElementAttributesReceived(vec![title, album])]
    );
}

// New spec-derived test: a peer with no AVRCP service answers with IPID,
// surfaced as an InvalidPid event.
#[test]
fn test_invalid_pid_response() {
    let mut controller = Protocol::new(MTU);
    // A bare AVCTP endpoint with no AVRCP PID registered.
    let mut bare_peer = avctp::Protocol::new(MTU);

    let pdus = controller.get_play_status().unwrap();
    let (ipid_pdus, events) = bare_peer.receive(&pdus[0]);
    assert!(events.is_empty());
    assert_eq!(ipid_pdus.len(), 1);
    let (out, events) = controller.receive(&ipid_pdus[0]);
    assert!(out.is_empty());
    assert_eq!(events, vec![AvrcpEvent::InvalidPid { pid: AVRCP_PID }]);
}

// ---------------------------------------------------------------------------
// SDP records
// ---------------------------------------------------------------------------

#[test]
fn test_find_sdp_records() {
    let controller_record = ControllerServiceRecord {
        service_record_handle: 0x10001,
        avctp_version: (1, 4),
        avrcp_version: (1, 6),
        supported_features: controller_features::CATEGORY_1
            | controller_features::SUPPORTS_BROWSING,
    };
    let target_record = TargetServiceRecord {
        service_record_handle: 0x10002,
        avctp_version: (1, 4),
        avrcp_version: (1, 6),
        supported_features: target_features::CATEGORY_1 | target_features::SUPPORTS_BROWSING,
    };

    let mut server = SdpServer::new();
    server
        .service_records
        .insert(0x10001, controller_record.to_service_attributes());
    server
        .service_records
        .insert(0x10002, target_record.to_service_attributes());
    let mut client = SdpClient::new();

    let controller_records =
        find_controller_services(&mut client, |req| server.handle_request(req, 1024)).unwrap();
    assert_eq!(controller_records, vec![controller_record]);

    let target_records =
        find_target_services(&mut client, |req| server.handle_request(req, 1024)).unwrap();
    assert_eq!(target_records, vec![target_record]);
}
