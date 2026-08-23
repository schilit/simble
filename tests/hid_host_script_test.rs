// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `android::BluetoothHidHost` driven from a script, against the catalog's own
//! HOGP devices.
//!
//! Both ends are simble's, so these prove the *binding* — that a script can
//! reach the HID host, that discovery drives the Report Map read and the
//! report subscription on its own, and that what comes back is decoded input
//! rather than bytes. They are not evidence about the wire; the spec-value
//! assertions in `src/device/hid_host.rs` and `tests/interop/` are that.
//!
//! The interesting one is
//! [`an_output_report_declared_first_does_not_steal_the_input_subscription`]:
//! it fails outright if the host queues its subscription by UUID, which is
//! what every other profile binding in this crate does.

use simble::devices::catalog;
use simble::transport::wasm_ws::SceneEngine;
use simble::types::Address;

fn peripheral_address() -> Address {
    "AA:BB:CC:00:00:01".parse().unwrap()
}

fn central_address() -> Address {
    "AA:BB:CC:00:00:99".parse().unwrap()
}

/// Runs a peripheral and a scripted HID host together, returning the scene and
/// the host's device index.
fn run(peripheral: &str, host: &str, ticks: usize) -> (SceneEngine, usize) {
    let mut scene = SceneEngine::new();
    scene
        .add_peripheral(peripheral_address(), peripheral)
        .expect("peripheral script runs");
    let c = scene
        .add_scripted_central(central_address(), host)
        .expect("host script runs");
    for i in 0..ticks {
        scene.tick(i as f64 * 0.05);
    }
    (scene, c)
}

/// The `(event, payload)` pairs a scripted host emitted.
fn emitted(scene: &mut SceneEngine, index: usize) -> Vec<(String, serde_json::Value)> {
    scene
        .scripted_central_mut(index)
        .expect("a scripted central")
        .take_emitted()
        .into_iter()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(&line).expect("emit is JSON");
            (
                value["event"].as_str().unwrap_or_default().to_string(),
                value["payload"].clone(),
            )
        })
        .collect()
}

fn failure(scene: &SceneEngine, index: usize) -> Option<String> {
    scene
        .scripted_central(index)
        .and_then(|c| c.failure())
        .map(str::to_string)
}

/// The catalog keyboard types "hello" from its own `tick`. A scripted host
/// connects, identifies it from the Report Map, and reads that word back as
/// characters — never touching a report byte in Rhai.
#[test]
fn a_scripted_host_identifies_a_keyboard_and_decodes_what_it_types() {
    let host = r#"
let host = android::BluetoothHidHost("Computer");
host.connect("AA:BB:CC:00:00:01");

fn on_identified(host, kind, report_map) {
    assert(kind == "keyboard", "the Report Map's first Application Collection");
    assert(report_map.len() > 0, "the descriptor came back with it");
    host.emit("kind", kind);
}
fn on_key_down(host, key) {
    if key.character != () { host.emit("typed", key.character); }
}
"#;
    // Long enough for more than one pass of the keyboard's h-e-l-l-o loop: the
    // host joins mid-cycle, so a single pass can start at any letter.
    let (mut scene, c) = run(catalog::script("hid_keyboard").unwrap(), host, 320);
    assert_eq!(failure(&scene, c), None, "no assertion failed");

    let messages = emitted(&mut scene, c);
    let kinds: Vec<&str> = messages.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds.first(),
        Some(&"kind"),
        "identification precedes any keystroke: {kinds:?}"
    );
    let typed: String = messages
        .iter()
        .filter(|(kind, _)| kind == "typed")
        .filter_map(|(_, payload)| payload.as_str())
        .collect();
    assert!(
        typed.contains("hello"),
        "the keyboard's own tick types 'hello' on a loop, got {typed:?}"
    );
}

/// Nothing in the script says "read the Report Map" or "subscribe": a HID host
/// does that the moment it discovers the service, exactly as Android's does.
#[test]
fn discovery_alone_drives_the_report_map_read_and_the_subscription() {
    let host = r#"
let host = android::BluetoothHidHost("Computer");
host.connect("AA:BB:CC:00:00:01");

fn on_services_discovered(host) {
    // Discovery has finished but the plan it triggers has not run yet.
    host.emit("ready_at_discovery", host.ready);
}
fn tick(host, t) {
    if host.ready { host.emit("ready", host.kind); }
}
"#;
    let (mut scene, c) = run(catalog::script("hid_keyboard").unwrap(), host, 80);
    assert_eq!(failure(&scene, c), None);

    let messages = emitted(&mut scene, c);
    assert_eq!(
        messages
            .iter()
            .find(|(k, _)| k == "ready_at_discovery")
            .map(|(_, v)| v.clone()),
        Some(serde_json::json!(false)),
        "the host is not ready until it has actually read the map"
    );
    let ready: Vec<&str> = messages
        .iter()
        .filter(|(k, _)| k == "ready")
        .filter_map(|(_, v)| v.as_str())
        .collect();
    assert!(
        !ready.is_empty() && ready.iter().all(|k| *k == "keyboard"),
        "the host became ready on its own, got {ready:?}"
    );
}

/// A report that arrives before the Report Map has been parsed is
/// uninterpretable, and the host drops it. The script must therefore never see
/// a key event that precedes `on_identified`.
#[test]
fn no_input_is_reported_before_the_peer_has_been_identified() {
    let host = r#"
let host = android::BluetoothHidHost("Computer");
host.connect("AA:BB:CC:00:00:01");

fn on_input(host, event) {
    if event.type == "identified" { this.seen = true; }
    else { assert(this.seen == true, "input arrived before the Report Map"); }
}
"#;
    let (scene, c) = run(catalog::script("hid_keyboard").unwrap(), host, 120);
    assert_eq!(failure(&scene, c), None);
}

/// The mouse's relative motion arrives as signed displacement, and a held
/// button produces one edge rather than one per report.
#[test]
fn a_scripted_host_decodes_pointer_motion_and_button_edges() {
    // The catalog mouse walks the pointer in a square and clicks; this asserts
    // on the shape of what the host decodes, not on the exact path.
    let host = r#"
let host = android::BluetoothHidHost("Computer");
host.connect("AA:BB:CC:00:00:02");

fn on_identified(host, kind, report_map) {
    assert(kind == "mouse", "a mouse Report Map");
    host.emit("kind", kind);
}
fn on_pointer(host, dx, dy, wheel) {
    assert(dx != 0 || dy != 0 || wheel != 0, "a still mouse reports nothing");
    host.emit("moved", dx);
}
fn on_button_down(host, button) { host.emit("down", button); }
fn on_button_up(host, button) { host.emit("up", button); }
"#;
    let mut scene = SceneEngine::new();
    scene
        .add_peripheral(
            "AA:BB:CC:00:00:02".parse().unwrap(),
            catalog::script("hid_mouse").unwrap(),
        )
        .expect("mouse script runs");
    let c = scene
        .add_scripted_central(central_address(), host)
        .expect("host script runs");
    for i in 0..160 {
        scene.tick(i as f64 * 0.05);
    }
    assert_eq!(failure(&scene, c), None);

    let messages = emitted(&mut scene, c);
    assert!(
        messages.iter().any(|(k, v)| k == "kind" && v == "mouse"),
        "the mouse was identified from its Report Map"
    );
    let moves = messages.iter().filter(|(k, _)| k == "moved").count();
    assert!(moves > 0, "the pointer moved: {messages:?}");
    // Every button-down is matched by a button-up: a held button is one edge.
    let downs = messages.iter().filter(|(k, _)| k == "down").count();
    let ups = messages.iter().filter(|(k, _)| k == "up").count();
    assert!(
        downs.abs_diff(ups) <= 1,
        "button edges pair up, got {downs} down / {ups} up"
    );
}

/// `on_report` is Android's own surface — `ACTION_REPORT` carries the report
/// bytes and nothing more — and it must arrive alongside the decoded events,
/// carrying the same bytes those were decoded from.
#[test]
fn the_raw_report_reaches_the_script_beside_its_decoded_meaning() {
    let host = r#"
let host = android::BluetoothHidHost("Computer");
host.connect("AA:BB:CC:00:00:01");

fn on_report(host, report) {
    assert(report.len() == 8, "a boot-shape keyboard report");
    // `host.report` is the same bytes the decoder just consumed.
    assert(host.report.len() == report.len(), "the host's view agrees");
    host.emit("report_len", report.len());
}
"#;
    let (mut scene, c) = run(catalog::script("hid_keyboard").unwrap(), host, 120);
    assert_eq!(failure(&scene, c), None);
    let messages = emitted(&mut scene, c);
    assert!(
        messages.iter().filter(|(k, _)| k == "report_len").count() > 3,
        "reports keep arriving: {messages:?}"
    );
}

/// The case that forces handle-addressed operations.
///
/// HOGP allows several Report characteristics (0x2A4D) in one HID service —
/// an output report for keyboard LEDs, then the input report. A host that
/// queues its subscription *by UUID* resolves to whichever came first in
/// discovery order, so it writes the CCCD of the output report, receives no
/// input, and decodes nothing. `HidPlan` names handles precisely so this
/// cannot happen, and the binding subscribes by handle.
#[test]
fn an_output_report_declared_first_does_not_steal_the_input_subscription() {
    let peripheral = r#"
let server = android::BluetoothGattServer("LedKeyboard");
let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);

let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
    android::PROPERTY_READ, android::PERMISSION_READ);
map.set_value([
    0x05, 0x01, 0x09, 0x06, 0xA1, 0x01,
    0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7,
    0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
    0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
    0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65,
    0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00,
    0xC0,
]);
hid.add_characteristic(map);

// The OUTPUT report first — the LED report a host writes, never notified.
// It shares the Report UUID with the input report that follows it.
let leds = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_WRITE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
leds.set_value([0]);
leds.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let led_ref = android::BluetoothGattDescriptor(uuid::from_u16(0x2908), android::PERMISSION_READ);
led_ref.set_value([0x02, 0x02]); // report ID 2, type 2 (Output)
leds.add_descriptor(led_ref);
hid.add_characteristic(leds);

// The INPUT report second.
let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
report.set_value([0, 0, 0, 0, 0, 0, 0, 0]);
report.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
let input_ref = android::BluetoothGattDescriptor(uuid::from_u16(0x2908), android::PERMISSION_READ);
input_ref.set_value([0x01, 0x01]); // report ID 1, type 1 (Input)
report.add_descriptor(input_ref);
hid.add_characteristic(report);
server.add_service(hid);
"#;
    let host = r#"
let host = android::BluetoothHidHost("Computer");
host.connect("AA:BB:CC:00:00:01");
"#;
    let (scene, c) = run(peripheral, host, 120);
    assert_eq!(failure(&scene, c), None);

    // Which CCCD was actually written is the whole question, so read it off the
    // host's own view rather than inferring it from a notification.
    let status = scene.scripted_central(c).expect("host").status_json();
    let view: serde_json::Value = serde_json::from_str(&status).expect("status is JSON");
    let reports: Vec<&serde_json::Value> = view["services"]
        .as_array()
        .expect("services")
        .iter()
        .flat_map(|s| s["characteristics"].as_array().expect("chrs"))
        .filter(|c| c["uuid"] == "2A4D")
        .collect();
    assert_eq!(
        reports.len(),
        2,
        "the peer published two Report characteristics"
    );

    const NOTIFY: u64 = 0x10;
    for characteristic in reports {
        let notifies = characteristic["properties"].as_u64().unwrap_or(0) & NOTIFY != 0;
        let subscribed = characteristic["subscribed"].as_bool().unwrap_or(false);
        assert_eq!(
            subscribed, notifies,
            "the CCCD written must be the notifying report's, not whichever \
             0x2A4D discovery found first: {characteristic}"
        );
    }
}

/// A peer with no HID service leaves the host inert rather than guessing.
#[test]
fn a_peer_that_is_not_a_hid_device_leaves_the_host_unready() {
    let peripheral = r#"
let server = android::BluetoothGattServer("Thermometer");
let svc = android::BluetoothGattService(uuid::from_u16(0x1809),
    android::SERVICE_TYPE_PRIMARY);
let t = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A1C),
    android::PROPERTY_READ, android::PERMISSION_READ);
t.set_value([0x00, 0xDC, 0x08, 0x00, 0xFE]);
svc.add_characteristic(t);
server.add_service(svc);
"#;
    let host = r#"
let host = android::BluetoothHidHost("Computer");
host.connect("AA:BB:CC:00:00:01");

fn on_services_discovered(host) {
    host.emit("kind", host.kind);
    host.emit("ready", host.ready);
}
fn on_input(host, event) {
    assert(false, "a non-HID peer produced HID input");
}
"#;
    let (mut scene, c) = run(peripheral, host, 80);
    assert_eq!(failure(&scene, c), None);
    let messages = emitted(&mut scene, c);
    assert!(
        messages.iter().any(|(k, v)| k == "kind" && v == "unknown"),
        "nothing was claimed about a peer with no Report Map: {messages:?}"
    );
    assert!(messages.iter().any(|(k, v)| k == "ready" && v == false));
}

/// `emit` must be able to carry bytes.
///
/// It could not: `rhai::serde::from_dynamic` rejects a Rhai blob outright, so
/// `host.emit("map", report_map)` failed at runtime with "invalid type: byte
/// array" — and every interesting value in a protocol simulator *is* bytes.
/// The page needs exactly this to show a Report Map, and it is the shape of
/// emit any script reporting a characteristic value would reach for.
#[test]
fn a_script_can_emit_the_bytes_it_received() {
    let host = r#"
let host = android::BluetoothHidHost("Computer");
host.connect("AA:BB:CC:00:00:01");

fn on_identified(host, kind, report_map) {
    // A bare blob, and one nested inside a map — the page uses the latter.
    host.emit("map", report_map);
    host.emit("wrapped", #{ kind: kind, bytes: report_map });
}
"#;
    let (mut scene, c) = run(catalog::script("hid_keyboard").unwrap(), host, 80);
    assert_eq!(failure(&scene, c), None, "emitting bytes is not an error");

    let messages = emitted(&mut scene, c);
    let map = messages
        .iter()
        .find(|(k, _)| k == "map")
        .map(|(_, v)| v.clone())
        .expect("the Report Map was emitted");
    let bytes = map
        .as_array()
        .expect("a blob arrives as an array of numbers");
    assert!(bytes.len() > 8, "the whole descriptor came through");
    // A USB HID report descriptor starts with Usage Page (Generic Desktop).
    assert_eq!(bytes[0], 0x05, "first descriptor byte");
    assert_eq!(bytes[1], 0x01, "Generic Desktop");

    let wrapped = messages
        .iter()
        .find(|(k, _)| k == "wrapped")
        .map(|(_, v)| v.clone())
        .expect("the nested form was emitted");
    assert_eq!(wrapped["kind"], "keyboard");
    assert_eq!(
        wrapped["bytes"].as_array().map(Vec::len),
        Some(bytes.len()),
        "a blob nested in a map survives too"
    );
}

/// Every catalog central example still runs — including the HID host entry.
#[test]
fn the_hid_host_catalog_entry_runs_against_its_named_peer() {
    let example = catalog::central("hid_host").expect("hid_host is in the catalog");
    let peer = catalog::script(example.peer).expect("its peer is in the catalog");
    let (scene, c) = run(peer, example.script, 120);
    assert_eq!(
        failure(&scene, c),
        None,
        "the catalog's HID host example asserts cleanly"
    );
}
