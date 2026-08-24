// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Classic HID through the real simulated BR/EDR link — the **two-PSM** case.
//!
//! HID is the profile that forced the dispatch change: Control (0x0011) and
//! Interrupt (0x0013) are one device on two PSMs, and the host used to
//! resolve a handler by a single `psm()`. What these tests are really
//! checking is that the two channels stay *told apart*: the same `DATA`
//! header means "the report you asked for" on Control and "a key went down"
//! on Interrupt, and a handler that confused them would report phantom
//! keystrokes for every GET_REPORT it answered.
//!
//! Everything asserted here crossed `simble::controller::sim`: an inquiry, a
//! page, an ACL, two L2CAP channel handshakes and HIDP on top of them.

use simble::classic::hid::{
    HidDeviceEvent, HidHostEvent, handshake_code, protocol_mode, report_type,
};
use simble::device::classic_hid::HidInput;
use simble::device::classic_host::scan_enable;
use simble::device::keyboard_scene::KeyboardScene;
use simble::device::profile_scene::LinkPhase;
use simble::devices::helpers::hid_reports::{KeyboardReport, keycode, modifier};

const STEPS: usize = 4000;

fn key(usage: u8) -> KeyboardReport {
    KeyboardReport {
        modifiers: modifier::NONE,
        keys: [usage, 0, 0, 0, 0, 0],
    }
}

#[test]
fn test_a_computer_opens_control_then_interrupt_on_a_found_keyboard() {
    let mut scene = KeyboardScene::new();
    assert!(
        scene.run_until_connected(STEPS),
        "HID never connected: link {:?}, error {:?}",
        scene.phase(),
        scene.error()
    );

    assert_eq!(scene.phase(), LinkPhase::Connected);
    assert!(scene.computer().is_connected());
    assert!(scene.keyboard().is_connected());

    // Two channels, two PSMs, both open — on *both* hosts. Before
    // `ProtocolHandler::psms` the second registration could not exist.
    assert!(scene.computer_host().channel_is_open(0x0011));
    assert!(scene.computer_host().channel_is_open(0x0013));
    assert!(scene.keyboard_host().channel_is_open(0x0011));
    assert!(scene.keyboard_host().channel_is_open(0x0013));
}

#[test]
fn test_typing_crosses_the_interrupt_channel_and_decodes() {
    let mut scene = KeyboardScene::new();
    assert!(scene.run_until_connected(STEPS), "HID never connected");

    // "cab", each key pressed and released — a real keyboard sends an
    // all-zero report on release, and without it a host sees one long press.
    let typed = [keycode::KEY_C, keycode::KEY_A, keycode::KEY_B];
    for usage in typed {
        scene.keyboard_mut().press(key(usage));
        scene.keyboard_mut().press(KeyboardReport::default());
    }

    let arrived = scene.run_until(STEPS, |scene| scene.computer().input().len() >= 6);
    assert!(
        arrived,
        "only {} reports arrived",
        scene.computer().input().len()
    );

    // Every report was decoded as a keyboard report, not left raw.
    assert!(
        scene
            .computer()
            .input()
            .iter()
            .all(|input| matches!(input, HidInput::Keyboard(_))),
        "a report was not decoded: {:?}",
        scene.computer().input()
    );
    // And the press/release pairs read back as exactly what was typed.
    assert_eq!(scene.computer().typed_usages(), typed.to_vec());
}

#[test]
fn test_a_control_transaction_does_not_look_like_typing() {
    // The whole point of routing by channel. A GET_REPORT is answered with
    // a `DATA` PDU whose header is byte-identical to an input report; the
    // difference is only which channel it came back on.
    let mut scene = KeyboardScene::new();
    assert!(scene.run_until_connected(STEPS), "HID never connected");

    scene.computer_mut().get_report(report_type::INPUT, 0, None);
    let answered = scene.run_until(STEPS, |scene| !scene.computer().events().is_empty());
    assert!(answered, "GET_REPORT was never answered");

    assert_eq!(
        scene.computer().events(),
        &[HidHostEvent::ControlData {
            report_type: report_type::INPUT,
            // Report id 0 followed by the eight zero bytes the keyboard was
            // built with.
            payload: vec![0, 0, 0, 0, 0, 0, 0, 0, 0],
        }]
    );
    // Nothing arrived on the interrupt channel, so nothing was typed.
    assert!(
        scene.computer().input().is_empty(),
        "a control-channel response was counted as input: {:?}",
        scene.computer().input()
    );
}

#[test]
fn test_set_protocol_and_set_report_reach_the_keyboard() {
    let mut scene = KeyboardScene::new();
    assert!(scene.run_until_connected(STEPS), "HID never connected");
    assert_eq!(scene.keyboard().protocol_mode(), protocol_mode::REPORT);

    scene.computer_mut().set_protocol(protocol_mode::BOOT);
    // Output report 0 is the LED byte: 0x02 is Caps Lock.
    scene
        .computer_mut()
        .set_report(report_type::OUTPUT, 0, &[0x02]);

    let done = scene.run_until(STEPS, |scene| scene.computer().events().len() >= 2);
    assert!(
        done,
        "the keyboard answered {} of 2",
        scene.computer().events().len()
    );

    // Both transactions were accepted, and both changed the keyboard.
    assert_eq!(
        scene.computer().events(),
        &[
            HidHostEvent::Handshake(handshake_code::SUCCESSFUL),
            HidHostEvent::Handshake(handshake_code::SUCCESSFUL),
        ]
    );
    assert_eq!(scene.keyboard().protocol_mode(), protocol_mode::BOOT);
    assert_eq!(
        scene.keyboard().report(report_type::OUTPUT, 0),
        Some(&[0x02][..])
    );
    assert_eq!(
        scene.keyboard().events(),
        &[
            HidDeviceEvent::ProtocolSet(protocol_mode::BOOT),
            HidDeviceEvent::ReportSet {
                report_type: report_type::OUTPUT,
                report_id: 0,
                data: vec![0x02],
            },
        ]
    );
}

#[test]
fn test_a_refused_get_report_leaves_the_keyboard_unchanged() {
    let mut scene = KeyboardScene::new();
    assert!(scene.run_until_connected(STEPS), "HID never connected");

    // Report id 9 was never declared. HIDP 7.4.2: the answer is a
    // HANDSHAKE, not a DATA — and it must change nothing.
    scene.computer_mut().get_report(report_type::INPUT, 9, None);
    let answered = scene.run_until(STEPS, |scene| !scene.computer().events().is_empty());
    assert!(answered, "the undeclared GET_REPORT was never answered");

    assert_eq!(
        scene.computer().events(),
        &[HidHostEvent::Handshake(
            handshake_code::ERR_INVALID_REPORT_ID
        )]
    );
    // A rejection that still mutates state is the bug shape: nothing was
    // declared, nothing was typed, and the declared report is untouched.
    assert_eq!(scene.keyboard().report(report_type::INPUT, 9), None);
    assert_eq!(
        scene.keyboard().report(report_type::INPUT, 0),
        Some(&[0u8; 8][..])
    );
    assert!(scene.keyboard().events().is_empty());
    assert!(scene.computer().input().is_empty());
    // And the link is still up: a refused transaction is not a disconnect.
    assert!(scene.computer().is_connected());
    assert!(scene.keyboard().is_connected());
}

#[test]
fn test_an_undeclared_set_report_is_refused_without_storing_anything() {
    let mut scene = KeyboardScene::new();
    assert!(scene.run_until_connected(STEPS), "HID never connected");

    scene
        .computer_mut()
        .set_report(report_type::FEATURE, 3, &[0xAB, 0xCD]);
    let answered = scene.run_until(STEPS, |scene| !scene.computer().events().is_empty());
    assert!(answered, "the undeclared SET_REPORT was never answered");

    assert_eq!(
        scene.computer().events(),
        &[HidHostEvent::Handshake(
            handshake_code::ERR_INVALID_REPORT_ID
        )]
    );
    assert_eq!(
        scene.keyboard().report(report_type::FEATURE, 3),
        None,
        "a refused SET_REPORT stored the report anyway"
    );
    assert!(
        scene.keyboard().events().is_empty(),
        "a refused SET_REPORT was reported as accepted"
    );
}

#[test]
fn test_a_keyboard_that_cannot_be_found_is_not_connected_to() {
    let mut scene = KeyboardScene::with_scan_enable(scan_enable::PAGE_ONLY);
    scene.run_until(STEPS, |scene| scene.phase() == LinkPhase::Failed);

    assert_eq!(scene.phase(), LinkPhase::Failed);
    assert!(
        scene
            .error()
            .is_some_and(|e| e.starts_with("inquiry did not find")),
        "wrong reason: {:?}",
        scene.error()
    );
    assert!(!scene.computer().is_connected());
    assert!(!scene.keyboard().is_connected());
    assert!(scene.computer().input().is_empty());
}

#[test]
fn test_an_output_report_on_the_interrupt_channel_reaches_the_keyboard() {
    // The other direction on the interrupt channel: a host driving the LEDs
    // without a control-channel round trip. It must arrive as an output
    // report and *not* be mistaken for a control transaction.
    let mut scene = KeyboardScene::new();
    assert!(scene.run_until_connected(STEPS), "HID never connected");

    scene.computer_mut().send_output_report(vec![0x00, 0x02]);
    let arrived = scene.run_until(STEPS, |scene| !scene.keyboard().output_reports().is_empty());
    assert!(arrived, "the output report never arrived");

    let reports = scene.keyboard().output_reports();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].report_type, report_type::OUTPUT);
    assert_eq!(reports[0].payload, vec![0x00, 0x02]);
    // It went to the interrupt path, so no control transaction happened.
    assert!(
        scene.keyboard().events().is_empty(),
        "an interrupt-channel report was handled as a control transaction"
    );
}
