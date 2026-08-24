use super::*;
use crate::client::{DiscoveredCharacteristic, DiscoveredService};
use crate::devices::helpers::hid_reports::{
    KEYBOARD_REPORT_MAP, MOUSE_REPORT_MAP, modifier, mouse_button,
};

/// A HID service whose Report Map is at 0x0010 and Report at 0x0012.
fn hid_service(properties: u8) -> DiscoveredService {
    DiscoveredService {
        start_handle: 0x000F,
        end_handle: 0x0014,
        uuid: Uuid::from_u16(hogp_uuid::HID_SERVICE),
        characteristics: vec![
            DiscoveredCharacteristic {
                declaration_handle: 0x000F,
                value_handle: 0x0010,
                properties: CharacteristicProperties::READ,
                uuid: Uuid::from_u16(hogp_uuid::REPORT_MAP),
                descriptors: Vec::new(),
            },
            DiscoveredCharacteristic {
                declaration_handle: 0x0011,
                value_handle: 0x0012,
                properties,
                uuid: Uuid::from_u16(hogp_uuid::REPORT),
                descriptors: Vec::new(),
            },
        ],
    }
}

/// A host that has read `report_map` and subscribed to handle 0x0012.
fn host_for(report_map: &[u8]) -> HidHost {
    let mut host = HidHost::new();
    host.plan(&[hid_service(
        CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
    )]);
    host.on_read(0x0010, report_map);
    host.drain_events();
    host
}

#[test]
fn test_discovery_finds_the_report_map_to_read_and_the_report_to_subscribe() {
    let mut host = HidHost::new();
    let plan = host.plan(&[hid_service(
        CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
    )]);
    assert_eq!(plan.read, vec![0x0010]);
    assert_eq!(plan.subscribe, vec![0x0012]);
}

#[test]
fn test_a_report_that_cannot_notify_is_not_subscribed() {
    // An output report shares the Report UUID; writing its CCCD is an
    // error, and it can never deliver input.
    let mut host = HidHost::new();
    let plan = host.plan(&[hid_service(CharacteristicProperties::WRITE)]);
    assert_eq!(plan.read, vec![0x0010]);
    assert!(plan.subscribe.is_empty());
}

#[test]
fn test_a_peer_without_a_hid_service_produces_no_plan() {
    let mut host = HidHost::new();
    let plan = host.plan(&[DiscoveredService {
        start_handle: 0x0001,
        end_handle: 0x0005,
        uuid: Uuid::from_u16(0x180D), // Heart Rate
        characteristics: Vec::new(),
    }]);
    assert!(plan.is_empty());
    assert_eq!(host.kind(), HidKind::Unknown);
}

#[test]
fn test_the_report_map_is_what_identifies_the_device() {
    let mut host = HidHost::new();
    host.plan(&[hid_service(
        CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
    )]);
    assert_eq!(host.kind(), HidKind::Unknown);
    host.on_read(0x0010, KEYBOARD_REPORT_MAP);
    assert_eq!(host.kind(), HidKind::Keyboard);
    assert_eq!(
        host.drain_events(),
        vec![HidEvent::Identified {
            kind: HidKind::Keyboard,
            report_map: KEYBOARD_REPORT_MAP.to_vec(),
        }]
    );

    let mut mouse = HidHost::new();
    mouse.plan(&[hid_service(CharacteristicProperties::NOTIFY)]);
    mouse.on_read(0x0010, MOUSE_REPORT_MAP);
    assert_eq!(mouse.kind(), HidKind::Mouse);
}

#[test]
fn test_reports_arriving_before_the_report_map_are_not_guessed_at() {
    let mut host = HidHost::new();
    host.plan(&[hid_service(CharacteristicProperties::NOTIFY)]);
    host.on_notification(0x0012, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
    assert!(
        host.drain_events().is_empty(),
        "eight bytes only mean a keyboard once a descriptor says so"
    );
}

#[test]
fn test_typing_a_word_decodes_to_that_word() {
    let mut host = host_for(KEYBOARD_REPORT_MAP);
    // The device's own encoder is deliberately not used here: these are
    // the usage IDs from the published Keyboard/Keypad page for "Hi!",
    // with Shift held for the capital and the bang.
    let reports: &[[u8; 8]] = &[
        [modifier::LSHIFT, 0, 0x0B, 0, 0, 0, 0, 0], // Shift + h
        [0, 0, 0, 0, 0, 0, 0, 0],
        [0, 0, 0x0C, 0, 0, 0, 0, 0], // i
        [0, 0, 0, 0, 0, 0, 0, 0],
        [modifier::LSHIFT, 0, 0x1E, 0, 0, 0, 0, 0], // Shift + 1
        [0, 0, 0, 0, 0, 0, 0, 0],
    ];
    let mut typed = String::new();
    for report in reports {
        host.on_notification(0x0012, report);
        for event in host.drain_events() {
            if let HidEvent::KeyDown {
                character: Some(c), ..
            } = event
            {
                typed.push(c);
            }
        }
    }
    assert_eq!(typed, "Hi!");
}

#[test]
fn test_holding_a_key_across_reports_types_one_character() {
    let mut host = host_for(KEYBOARD_REPORT_MAP);
    // 'l' goes down and stays down while 'o' is added — the classic
    // rollover case. Decoding each report as a keystroke gives "llo".
    host.on_notification(0x0012, &[0, 0, 0x0F, 0, 0, 0, 0, 0]);
    host.on_notification(0x0012, &[0, 0, 0x0F, 0x12, 0, 0, 0, 0]);
    host.on_notification(0x0012, &[0, 0, 0, 0, 0, 0, 0, 0]);
    let typed: String = host
        .drain_events()
        .into_iter()
        .filter_map(|e| match e {
            HidEvent::KeyDown {
                character: Some(c), ..
            } => Some(c),
            _ => None,
        })
        .collect();
    assert_eq!(typed, "lo");
}

#[test]
fn test_a_release_is_reported_for_every_key_that_was_held() {
    let mut host = host_for(KEYBOARD_REPORT_MAP);
    host.on_notification(0x0012, &[0, 0, 0x04, 0x05, 0, 0, 0, 0]);
    host.drain_events();
    host.on_notification(0x0012, &[0, 0, 0, 0, 0, 0, 0, 0]);
    assert_eq!(
        host.drain_events(),
        vec![
            HidEvent::KeyUp { usage: 0x04 },
            HidEvent::KeyUp { usage: 0x05 },
        ]
    );
}

#[test]
fn test_a_rollover_report_does_not_release_the_keys_being_held() {
    let mut host = host_for(KEYBOARD_REPORT_MAP);
    host.on_notification(0x0012, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
    host.drain_events();
    host.on_notification(0x0012, &[0, 0, 1, 1, 1, 1, 1, 1]);
    assert_eq!(host.drain_events(), vec![HidEvent::RollOver]);
    // 'a' is still held, so pressing 'b' next reports only 'b'.
    host.on_notification(0x0012, &[0, 0, 0x04, 0x05, 0, 0, 0, 0]);
    assert!(matches!(
        host.drain_events().as_slice(),
        [HidEvent::KeyDown { usage: 0x05, .. }]
    ));
}

#[test]
fn test_a_leftward_move_decodes_as_negative_motion() {
    let mut host = host_for(MOUSE_REPORT_MAP);
    host.on_notification(0x0012, &[0x00, 0xFB, 0x00, 0x00]);
    assert_eq!(
        host.drain_events(),
        vec![HidEvent::Pointer {
            dx: -5,
            dy: 0,
            wheel: 0
        }]
    );
}

#[test]
fn test_a_held_mouse_button_is_pressed_once() {
    let mut host = host_for(MOUSE_REPORT_MAP);
    host.on_notification(0x0012, &[mouse_button::LEFT, 3, 0, 0]);
    host.on_notification(0x0012, &[mouse_button::LEFT, 4, 0, 0]);
    host.on_notification(0x0012, &[mouse_button::NONE, 0, 0, 0]);
    let buttons: Vec<_> = host
        .drain_events()
        .into_iter()
        .filter(|e| !matches!(e, HidEvent::Pointer { .. }))
        .collect();
    assert_eq!(
        buttons,
        vec![
            HidEvent::ButtonDown { button: 1 },
            HidEvent::ButtonUp { button: 1 },
        ]
    );
}

#[test]
fn test_a_notification_on_an_unrelated_handle_is_ignored() {
    let mut host = host_for(KEYBOARD_REPORT_MAP);
    // The Battery Level notification a HOGP device also sends is not a
    // report, and decoding it would type a character per battery update.
    host.on_notification(0x0030, &[0, 0, 0x04, 0, 0, 0, 0, 0]);
    assert!(host.drain_events().is_empty());
}

#[test]
fn test_the_keyboard_device_and_this_host_agree_on_the_wire() {
    // The one place the two halves meet: the notification a `BleKeyboard`
    // actually puts on the air is fed straight into the host. This is a
    // consistency check between simble's own encoder and decoder, so it
    // proves nothing about a real keyboard — the spec-value assertions in
    // the tests above are what pin the format down.
    use crate::devices::keyboard::BleKeyboard;
    use crate::l2cap::L2capHeader;
    use crate::types::Address;

    let mut keyboard = BleKeyboard::new("SimKeyboard", Address::from_be_bytes([1, 2, 3, 4, 5, 6]));
    let mut host = host_for(KEYBOARD_REPORT_MAP);
    let mut typed = String::new();
    for packet in keyboard.type_text("hi there") {
        let (_, att) = L2capHeader::parse(&packet).expect("L2CAP");
        // opcode(1) + handle(2), then the report.
        host.on_notification(0x0012, &att[3..]);
        for event in host.drain_events() {
            if let HidEvent::KeyDown {
                character: Some(c), ..
            } = event
            {
                typed.push(c);
            }
        }
    }
    assert_eq!(typed, "hi there");
}
