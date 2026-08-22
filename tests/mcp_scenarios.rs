// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Use-case coverage for the `simble mcp` server's built-in (`self`) scene —
//! the realistic tool sequences an agent runs, driven through the public
//! `Server::request` entry point (no pipe, no netsim, no hardware).

use serde_json::{Value, json};
use simble::mcp::Server;

/// Call a tool and return the full JSON-RPC response.
fn call(server: &mut Server, name: &str, arguments: Value) -> Value {
    server
        .request(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }))
        .expect("tools/call returns a response")
}

fn is_ok(resp: &Value) -> bool {
    resp["result"]["isError"] == Value::Bool(false)
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

const HRM: &str = r#"
    let server = android::BluetoothGattServer("HRM");
    let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
    let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
        android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
    hr.set_value([0x00, 72]);
    hrs.add_characteristic(hr);
    server.add_service(hrs);
"#;

const BATTERY: &str = r#"
    let server = android::BluetoothGattServer("Batt");
    let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
    let lvl = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
        android::PROPERTY_READ, android::PERMISSION_READ);
    lvl.set_value([85]);
    bas.add_characteristic(lvl);
    server.add_service(bas);
"#;

/// The AI loop: a broken script fails lint, the fix lints clean, and run_test
/// then passes — all without touching the scene.
#[test]
fn scenario_generate_check_fix_loop() {
    let mut s = Server::default();

    let broken = call(&mut s, "lint", json!({"script": "let x = ;"}));
    assert!(!is_ok(&broken), "broken script should fail lint");

    let fixed = r#"let srv = android::BluetoothGattServer("X"); assert(srv.name == "X", "n");"#;
    assert!(is_ok(&call(&mut s, "lint", json!({"script": fixed}))));
    let run = call(&mut s, "run_test", json!({"script": fixed}));
    assert!(is_ok(&run) && text(&run).contains("PASS"));
}

/// Two peripherals share the scene; `scan` hears both on the air and `status`
/// lists them.
#[test]
fn scenario_multi_device_scan_and_status() {
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM}));
    call(&mut s, "add_peripheral", json!({"script": BATTERY}));

    let scan = text(&call(&mut s, "scan", json!({})));
    assert!(scan.contains("HRM"), "scan should hear HRM: {scan}");
    assert!(scan.contains("Batt"), "scan should hear Batt: {scan}");

    let status = text(&call(&mut s, "status", json!({})));
    assert!(status.contains("HRM") && status.contains("Batt"));
}

/// Connect to the battery peripheral and read its level (a single-byte value
/// at byte 0), then assert it is a sane percentage.
#[test]
fn scenario_battery_connect_and_read_level() {
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": BATTERY}));
    assert!(is_ok(&call(&mut s, "connect", json!({}))));

    let read = call(&mut s, "read", json!({"uuid": "2A19"}));
    assert!(is_ok(&read), "read battery level: {read}");

    // Byte 0 of the battery level, asserted <= 100 (a valid percentage).
    let ok = call(
        &mut s,
        "assert",
        json!({"uuid": "2A19", "byte": 0, "op": "<=", "value": 100}),
    );
    assert!(is_ok(&ok), "85 <= 100 should PASS: {ok}");
    let bad = call(
        &mut s,
        "assert",
        json!({"uuid": "2A19", "byte": 0, "op": ">", "value": 100}),
    );
    assert!(!is_ok(&bad), "85 > 100 should FAIL");
}

/// The headline use case: build an HRM, connect, and monitor "HR < 200" — as a
/// single read (assert) and as a window (assert_over).
#[test]
fn scenario_monitor_heart_rate() {
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM}));
    call(&mut s, "connect", json!({}));

    let once = call(
        &mut s,
        "assert",
        json!({"uuid": "2A37", "op": "<", "value": 200}),
    );
    assert!(is_ok(&once), "single-shot HR < 200 should PASS: {once}");

    let over = call(
        &mut s,
        "assert_over",
        json!({"uuid": "2A37", "op": "<", "value": 200, "seconds": 0.5}),
    );
    assert!(is_ok(&over), "HR < 200 over time should PASS: {over}");
    assert!(text(&over).contains("PASS"));
}

/// One device, two services: connect once and read a characteristic from each.
#[test]
fn scenario_two_services_one_device() {
    const COMBINED: &str = r#"
        let server = android::BluetoothGattServer("Wearable");
        let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
        let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
            android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
        hr.set_value([0x00, 65]);
        hrs.add_characteristic(hr);
        server.add_service(hrs);
        let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
        let lvl = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
            android::PROPERTY_READ, android::PERMISSION_READ);
        lvl.set_value([90]);
        bas.add_characteristic(lvl);
        server.add_service(bas);
    "#;
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": COMBINED}));
    call(&mut s, "connect", json!({}));

    assert!(is_ok(&call(
        &mut s,
        "assert",
        json!({"uuid": "2A37", "op": "==", "value": 65})
    )));
    assert!(is_ok(&call(
        &mut s,
        "assert",
        json!({"uuid": "2A19", "byte": 0, "op": "==", "value": 90})
    )));
}
