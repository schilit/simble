// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Central-role scripting: a Rhai script that is a GATT *client*.
//!
//! **What these tests prove, and what they do not.** Both endpoints here are
//! simble's, so every one of them would pass while both halves were wrong in
//! the same way — the exact failure mode this repo has hit repeatedly (an
//! `LE Create Connection` with 12 of its 25 parameter bytes passed every
//! simulated test and was rejected by the first real controller). They prove
//! the *scripting surface*: that a script's `connect`/`subscribe`/`read`/
//! `write` reach the state machine, that callbacks fire with the right
//! arguments, and that a failed `assert` inside a callback is reported.
//!
//! The evidence about the wire is `tests/interop/gatt_client.py`, which
//! points the same scripted central at a **Bumble**-hosted peripheral over
//! netsim. Nothing here is a substitute for that.

use simble::scene::SceneEngine;
use simble::types::Address;

/// The peripheral every test connects to, unless it needs a different shape.
const HEART_RATE_PERIPHERAL: &str = r#"
let server = android::BluetoothGattServer("HRM");
let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 72]);
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
hrs.add_characteristic(hr);
server.add_service(hrs);

fn tick(server, t) {
    let bpm = 68 + (t * 2.0).to_int() % 9;
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, bpm]);
}
"#;

fn peripheral_address() -> Address {
    "AA:BB:CC:00:00:01".parse().unwrap()
}

fn central_address() -> Address {
    "AA:BB:CC:00:00:99".parse().unwrap()
}

/// Runs a peripheral and a scripted central together for `ticks` scene steps,
/// returning the scene and the two device indices.
fn run(peripheral: &str, central: &str, ticks: usize) -> (SceneEngine, usize, usize) {
    let mut scene = SceneEngine::new();
    let p = scene
        .add_peripheral(peripheral_address(), peripheral)
        .expect("peripheral script runs");
    let c = scene
        .add_scripted_central(central_address(), central)
        .expect("central script runs");
    for i in 0..ticks {
        scene.tick(i as f64 * 0.05);
    }
    (scene, p, c)
}

/// The messages a scripted central emitted, as `(event, payload)` pairs.
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

#[test]
fn a_scripted_central_connects_discovers_and_subscribes_without_any_rust() {
    let central = r#"
let client = android::BluetoothGatt("Probe");
client.connect("AA:BB:CC:00:00:01");

fn on_connection_state_change(client, connected) {
    if connected { client.emit("up", client.peer); }
}
fn on_services_discovered(client) {
    client.emit("services", client.services().len());
    client.subscribe(uuid::HEART_RATE_MEASUREMENT);
}
fn on_subscribed(client, uuid) {
    client.emit("subscribed", uuid.to_string());
}
fn on_characteristic_changed(client, uuid, value) {
    assert(value.len() == 2, "heart rate measurement is flags + bpm");
    client.emit("bpm", value[1]);
}
"#;
    let (mut scene, _, c) = run(HEART_RATE_PERIPHERAL, central, 60);
    assert_eq!(
        scene.scripted_central(c).and_then(|c| c.failure()),
        None,
        "no assertion failed"
    );
    let messages = emitted(&mut scene, c);
    let kinds: Vec<&str> = messages.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(kinds[0], "up");
    assert_eq!(kinds[1], "services");
    assert_eq!(kinds[2], "subscribed");
    assert_eq!(messages[0].1, "AA:BB:CC:00:00:01");
    assert_eq!(messages[1].1, 1);
    assert_eq!(messages[2].1, "2A37");
    let beats: Vec<i64> = messages
        .iter()
        .filter(|(kind, _)| kind == "bpm")
        .filter_map(|(_, payload)| payload.as_i64())
        .collect();
    assert!(
        beats.len() > 3,
        "notifications keep arriving, got {beats:?}"
    );
    assert!(beats.iter().all(|&b| (60..90).contains(&b)));
}

#[test]
fn subscribing_finds_the_cccd_even_when_another_descriptor_precedes_it() {
    // `value_handle + 1` is the common layout, not a rule (Vol 3, Part G,
    // Section 3.3.3): a User Description descriptor may come first. A client
    // that assumes the layout writes 0x0001 over the description and
    // subscribes to nothing — and against a simble server that write even
    // succeeds, so the bug is invisible without a test shaped like this one.
    let peripheral = r#"
let server = android::BluetoothGattServer("Described");
let svc = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
hr.set_value([0x00, 72]);
// Characteristic User Description (0x2901) BEFORE the CCCD.
let desc = android::BluetoothGattDescriptor(uuid::from_u16(0x2901), android::PERMISSION_READ);
desc.set_value([0x62, 0x70, 0x6D]); // "bpm"
hr.add_descriptor(desc);
hr.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
svc.add_characteristic(hr);
server.add_service(svc);

fn tick(server, t) {
    server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, 68 + t.to_int() % 5]);
}
"#;
    let central = r#"
let client = android::BluetoothGatt("Probe");
client.connect("AA:BB:CC:00:00:01");
fn on_services_discovered(client) { client.subscribe(uuid::HEART_RATE_MEASUREMENT); }
fn on_characteristic_changed(client, uuid, value) { client.emit("bpm", value[1]); }
"#;
    let (mut scene, _, c) = run(peripheral, central, 80);
    assert_eq!(scene.scripted_central(c).and_then(|c| c.failure()), None);
    let messages = emitted(&mut scene, c);
    assert!(
        messages.iter().any(|(kind, _)| kind == "bpm"),
        "the CCCD was found past the user description; got {messages:?}"
    );
}

#[test]
fn indications_are_confirmed_so_the_server_can_send_a_second_one() {
    // Only one indication may be outstanding (Vol 3, Part F, Section
    // 3.4.7.2). A client that never sends Handle Value Confirmation gets
    // exactly one indication and then silence — which looks like the server
    // stopping, not the client failing.
    let peripheral = r#"
let server = android::BluetoothGattServer("Indicator");
let svc = android::BluetoothGattService(uuid::from_u16(0x1805), android::SERVICE_TYPE_PRIMARY);
// Indicate-only, deliberately: it also proves the CCCD write carries 0x0002.
let ct = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A2B),
    android::PROPERTY_READ | android::PROPERTY_INDICATE, android::PERMISSION_READ);
ct.set_value([0x00]);
ct.add_descriptor(android::BluetoothGattDescriptor(
    uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
    android::PERMISSION_READ | android::PERMISSION_WRITE));
svc.add_characteristic(ct);
server.add_service(svc);

fn tick(server, t) {
    server.update_value(uuid::from_u16(0x2A2B), [t.to_int() % 200]);
}
"#;
    let central = r#"
let client = android::BluetoothGatt("Probe");
client.connect("AA:BB:CC:00:00:01");
fn on_services_discovered(client) { client.subscribe(uuid::from_u16(0x2A2B)); }
fn on_characteristic_changed(client, uuid, value) { client.emit("tick", value[0]); }
"#;
    let (mut scene, _, c) = run(peripheral, central, 200);
    let indications = emitted(&mut scene, c)
        .into_iter()
        .filter(|(kind, _)| kind == "tick")
        .count();
    assert!(
        indications > 1,
        "a second indication only arrives if the first was confirmed, got {indications}"
    );
}

#[test]
fn a_read_returns_the_peripherals_value_to_the_callback() {
    let central = r#"
let client = android::BluetoothGatt("Probe");
client.connect("AA:BB:CC:00:00:01");
fn on_services_discovered(client) { client.read(uuid::HEART_RATE_MEASUREMENT); }
fn on_characteristic_read(client, uuid, value) {
    assert(uuid == uuid::HEART_RATE_MEASUREMENT, "the read that was asked for");
    client.emit("read", value[1]);
}
"#;
    let (mut scene, _, c) = run(HEART_RATE_PERIPHERAL, central, 40);
    assert_eq!(scene.scripted_central(c).and_then(|c| c.failure()), None);
    let messages = emitted(&mut scene, c);
    let read = messages
        .iter()
        .find(|(kind, _)| kind == "read")
        .expect("the read completed");
    assert!((60..90).contains(&read.1.as_i64().unwrap()));
}

#[test]
fn a_write_from_the_script_lands_in_the_peripherals_database() {
    let peripheral = r#"
let server = android::BluetoothGattServer("Lamp");
let svc = android::BluetoothGattService(uuid::from_u16(0xFFE0), android::SERVICE_TYPE_PRIMARY);
let colour = android::BluetoothGattCharacteristic(uuid::from_u16(0xFFE9),
    android::PROPERTY_READ | android::PROPERTY_WRITE,
    android::PERMISSION_READ | android::PERMISSION_WRITE);
colour.set_value([0x00, 0x00, 0x00]);
svc.add_characteristic(colour);
server.add_service(svc);
"#;
    let central = r#"
let client = android::BluetoothGatt("Switch");
client.connect("AA:BB:CC:00:00:01");
fn on_services_discovered(client) {
    client.write(uuid::from_u16(0xFFE9), [0xFF, 0x40, 0x10]);
}
fn on_characteristic_write(client, uuid, status) {
    assert(status == 0, "the write was accepted");
    client.emit("wrote", status);
}
"#;
    let (mut scene, p, c) = run(peripheral, central, 40);
    assert_eq!(scene.scripted_central(c).and_then(|c| c.failure()), None);
    assert!(
        emitted(&mut scene, c)
            .iter()
            .any(|(kind, _)| kind == "wrote"),
        "the write callback fired"
    );
    let status = scene.peripheral_status_json(p).expect("peripheral status");
    assert!(
        status.contains("FF4010"),
        "the peripheral holds what the script wrote: {status}"
    );
}

#[test]
fn a_write_command_completes_without_the_peer_answering() {
    // A Write Command is unacknowledged (Vol 3, Part F, Section 3.4.5.3), so
    // a client that waits for a response waits forever. The callback must
    // fire when the command goes out.
    let peripheral = r#"
let server = android::BluetoothGattServer("Lamp");
let svc = android::BluetoothGattService(uuid::from_u16(0xFFE0), android::SERVICE_TYPE_PRIMARY);
let colour = android::BluetoothGattCharacteristic(uuid::from_u16(0xFFE9),
    android::PROPERTY_WRITE_NO_RESPONSE, android::PERMISSION_WRITE);
colour.set_value([0x00]);
svc.add_characteristic(colour);
server.add_service(svc);
"#;
    let central = r#"
let client = android::BluetoothGatt("Switch");
client.connect("AA:BB:CC:00:00:01");
fn on_services_discovered(client) {
    client.write_command(uuid::from_u16(0xFFE9), [0x07]);
}
fn on_characteristic_write(client, uuid, status) { client.emit("wrote", status); }
"#;
    let (mut scene, p, c) = run(peripheral, central, 40);
    assert!(
        emitted(&mut scene, c)
            .iter()
            .any(|(kind, _)| kind == "wrote")
    );
    let status = scene.peripheral_status_json(p).expect("peripheral status");
    assert!(status.contains("\"value\":\"07\""), "{status}");
}

#[test]
fn a_failed_assert_inside_a_callback_becomes_the_scripts_failure() {
    let central = r#"
let client = android::BluetoothGatt("Probe");
client.connect("AA:BB:CC:00:00:01");
fn on_services_discovered(client) {
    assert(client.services().len() == 99, "impossible service count");
}
"#;
    let (scene, _, c) = run(HEART_RATE_PERIPHERAL, central, 40);
    let failure = scene
        .scripted_central(c)
        .and_then(|c| c.failure())
        .expect("the assertion failed");
    assert!(
        failure.contains("impossible service count"),
        "the message names the assertion: {failure}"
    );
}

#[test]
fn naming_a_characteristic_the_peer_does_not_have_fails_instead_of_hanging() {
    let central = r#"
let client = android::BluetoothGatt("Probe");
client.connect("AA:BB:CC:00:00:01");
fn on_services_discovered(client) { client.read(uuid::from_u16(0x2A19)); }
fn on_error(client, message) { client.emit("error", message); }
"#;
    let (mut scene, _, c) = run(HEART_RATE_PERIPHERAL, central, 40);
    let messages = emitted(&mut scene, c);
    let error = messages
        .iter()
        .find(|(kind, _)| kind == "error")
        .expect("an error was reported");
    let text = error.1.as_str().unwrap_or_default();
    assert!(text.contains("read"), "{text}");
    assert!(text.contains("2A19"), "{text}");
    // It is also a script failure: a test that names a UUID the device does
    // not expose has found a real disagreement, not a timing problem.
    assert!(
        scene
            .scripted_central(c)
            .and_then(|c| c.failure())
            .is_some()
    );
}

#[test]
fn on_event_sees_the_same_stream_the_named_handlers_do() {
    let central = r#"
let client = android::BluetoothGatt("Probe");
client.connect("AA:BB:CC:00:00:01");
fn on_event(client, event) {
    client.emit("event", event.event);
    if event.event == "services_discovered" {
        client.subscribe(uuid::HEART_RATE_MEASUREMENT);
    }
}
"#;
    let (mut scene, _, c) = run(HEART_RATE_PERIPHERAL, central, 60);
    let kinds: Vec<String> = emitted(&mut scene, c)
        .into_iter()
        .filter(|(kind, _)| kind == "event")
        .filter_map(|(_, payload)| payload.as_str().map(str::to_string))
        .collect();
    for expected in [
        "connected",
        "mtu_changed",
        "services_discovered",
        "subscription_changed",
        "characteristic_changed",
    ] {
        assert!(
            kinds.iter().any(|k| k == expected),
            "on_event saw {expected}; got {kinds:?}"
        );
    }
}

#[test]
fn a_central_script_that_never_builds_a_client_is_rejected_with_a_usable_message() {
    let mut scene = SceneEngine::new();
    let error = scene
        .add_scripted_central(central_address(), "let x = 1 + 1;")
        .expect_err("no client in the script");
    assert!(error.contains("android::BluetoothGatt"), "{error}");
}

#[test]
fn every_catalog_central_connects_to_the_peripheral_it_was_written_for() {
    // The catalog's client scripts are what `example` hands an agent, so a
    // broken one is a broken first impression. Each is run against the
    // peripheral its summary names.
    for example in simble::devices::catalog::CENTRAL_EXAMPLES {
        let peripheral = simble::devices::catalog::script(example.peer)
            .unwrap_or_else(|| panic!("{} names an unknown peer", example.name));
        let mut scene = SceneEngine::new();
        scene
            .add_peripheral(peripheral_address(), peripheral)
            .unwrap_or_else(|e| panic!("{}: peer script rejected: {e}", example.name));
        let c = scene
            .add_scripted_central(central_address(), example.script)
            .unwrap_or_else(|e| panic!("{}: script rejected: {e}", example.name));
        for i in 0..80 {
            scene.tick(i as f64 * 0.05);
        }
        let central = scene.scripted_central(c).expect("a scripted central");
        assert_eq!(
            central.failure(),
            None,
            "{} reported a failure",
            example.name
        );
        let status = scene.central_status_json(c).unwrap_or_default();
        assert!(
            status.contains("\"phase\":\"ready\""),
            "{} never finished discovery: {status}",
            example.name
        );
    }
}

/// A script can find its own peer, without being told an address.
///
/// This is the case an address cannot cover. A phone advertises from a
/// resolvable private address that rotates and that Android will not disclose
/// even to its own app, so a script that meets one has nothing to write down —
/// only a service to look for. Proven against a Pixel 9 Pro before it was
/// written here; this is the part that can run without one.
///
/// Both endpoints are simble's, so what this proves is the scripting surface:
/// that `start_scan` reaches the controller as scan bring-up, that
/// advertising reports become `on_scan_result` with a map the script can read,
/// and that connecting from inside that handler aims the client.
#[test]
fn a_scripted_central_finds_its_own_peer_by_scanning() {
    let central = r#"
let client = android::BluetoothGatt("Finder");
let scanner = android::BluetoothLeScanner();
scanner.start_scan();

fn on_scan_result(client, result) {
    // Repeats of the same advertisement keep arriving; connecting twice would
    // have the second attempt fight the first.
    if this.aimed == () {
        this.aimed = true;
        assert(result.address != "", "a report carries an address");
        client.emit("saw", result.address);
        client.connect(result.address);
    }
}
fn on_services_discovered(client) {
    client.emit("services", client.services().len());
}
"#;
    let (mut scene, _, c) = run(HEART_RATE_PERIPHERAL, central, 80);
    assert_eq!(
        scene.scripted_central(c).and_then(|c| c.failure()),
        None,
        "no assertion failed"
    );
    let messages = emitted(&mut scene, c);
    let kinds: Vec<&str> = messages.iter().map(|(kind, _)| kind.as_str()).collect();
    assert_eq!(
        kinds.first(),
        Some(&"saw"),
        "the scan reported before anything else happened, got {kinds:?}"
    );
    assert_eq!(
        messages[0].1, "AA:BB:CC:00:00:01",
        "the address came from the advertisement, not from the script"
    );
    assert!(
        kinds.contains(&"services"),
        "connecting from on_scan_result reached discovery, got {kinds:?}"
    );
}
