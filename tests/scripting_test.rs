// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! End-to-end tests for the Rhai scripting bindings: scripts build real
//! `android::*`-shaped servers over real `VirtualDevice`s, Rust drives real
//! ATT traffic at them, and scripts observe the results through the event
//! queue (`wait_for` / `take_events`) and judge them with `assert` — the
//! loop the "Simble API Explorer" spec's testing section describes.

use rhai::Scope;

use simble::att::opcode;
use simble::l2cap::{L2capHeader, cid};
use simble::profiles::bap::{audio_location, context_type};
use simble::profiles::pacs::pacs_uuid;
use simble::scripting::{ScriptGattServer, new_engine};
use simble::types::Uuid;
use simble::{Address, HeartRateService};

const HRM_SCRIPT: &str = r#"
    let server = android::BluetoothGattServer("HRM");

    let hrs = android::BluetoothGattService(
        uuid::HEART_RATE_SERVICE,
        android::SERVICE_TYPE_PRIMARY,
    );
    let chr = android::BluetoothGattCharacteristic(
        uuid::HEART_RATE_MEASUREMENT,
        android::PROPERTY_READ | android::PROPERTY_WRITE | android::PROPERTY_NOTIFY,
        android::PERMISSION_READ | android::PERMISSION_WRITE,
    );
    chr.set_value([75]);
    hrs.add_characteristic(chr);
    server.add_service(hrs);
"#;

/// Runs `HRM_SCRIPT`, returning the engine, the scope (still holding the
/// script's `server` variable), and the shared server handle.
fn build_hrm() -> (rhai::Engine, Scope<'static>, ScriptGattServer) {
    let engine = new_engine();
    let mut scope = Scope::new();
    engine
        .run_with_scope(&mut scope, HRM_SCRIPT)
        .expect("HRM script runs");
    let server: ScriptGattServer = scope
        .get_value("server")
        .expect("script-built server is reachable from Rust");
    (engine, scope, server)
}

/// Drives a real ATT read through the underlying `VirtualDevice` and
/// returns the response payload (opcode + value).
fn att_read(server: &ScriptGattServer, conn_handle: u16, attr_handle: u16) -> Vec<u8> {
    let mut read_req = vec![opcode::READ_REQ];
    read_req.extend_from_slice(&attr_handle.to_le_bytes());
    let l2cap_read = L2capHeader::serialize(cid::ATT, &read_req);
    let resp = server.with_server(|s| {
        s.device
            .process_l2cap_packet(conn_handle, &l2cap_read)
            .expect("read processes")
            .expect("read produces a response")
    });
    let (_, payload) = L2capHeader::parse(&resp).expect("valid L2CAP response");
    payload.to_vec()
}

#[test]
fn test_script_built_server_answers_real_att_read_and_script_observes_events() {
    let (engine, mut scope, server) = build_hrm();

    // The script's builder calls registered real attributes in the real
    // GattDatabase — fetch the assigned value handle through the wrapper.
    let value_handle = server.with_server(|s| {
        s.get_service(HeartRateService::SERVICE_UUID)
            .expect("service registered")
            .characteristics[0]
            .value_handle
            .expect("handle assigned")
    });

    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;
    server.with_server(|s| s.device.on_connected(conn_handle, peer));

    let payload = att_read(&server, conn_handle, value_handle);
    assert_eq!(payload[0], opcode::READ_RSP);
    assert_eq!(&payload[1..], &[75u8], "value set by the script, via ATT");

    // A follow-up eval on the same engine+scope observes the queued events
    // and judges them with `assert` — the script side of the round trip.
    engine
        .run_with_scope(
            &mut scope,
            r#"
                wait_for "connected" {
                    assert(event.server == "HRM", "event names its server");
                }
                wait_for "characteristic_read" {
                    assert(event.uuid == uuid::HEART_RATE_MEASUREMENT, "read hit the HR measurement");
                    assert(event.offset == 0, "read at offset 0");
                    assert(event.request_id == 0, "first allocated request id");
                }
            "#,
        )
        .expect("event-observing script passes its asserts");
}

#[test]
fn test_script_drives_notification_and_observes_notification_sent() {
    let (engine, mut scope, server) = build_hrm();

    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    server.with_server(|s| s.device.on_connected(0x0040, peer));

    engine
        .run_with_scope(
            &mut scope,
            r#"
                let svc = server.get_service(uuid::HEART_RATE_SERVICE);
                let chr = svc.get_characteristic(uuid::HEART_RATE_MEASUREMENT);
                wait_for "connected" {
                    let status = server.notify_characteristic_changed(event.device, chr, false, [80]);
                    assert(status == android::GATT_SUCCESS, "notify succeeded");
                }
                wait_for "notification_sent" {
                    assert(event.status == android::GATT_SUCCESS, "notification confirmed sent");
                }
            "#,
        )
        .expect("notify script passes its asserts");
}

#[test]
fn test_real_att_write_reaches_script_event_queue_and_database() {
    let (engine, mut scope, server) = build_hrm();

    let peer = Address::from_be_bytes([0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F]);
    let conn_handle = 0x0007;
    let value_handle = server.with_server(|s| {
        s.get_service(HeartRateService::SERVICE_UUID)
            .unwrap()
            .characteristics[0]
            .value_handle
            .unwrap()
    });
    server.with_server(|s| s.device.on_connected(conn_handle, peer));

    let mut write_req = vec![opcode::WRITE_REQ];
    write_req.extend_from_slice(&value_handle.to_le_bytes());
    write_req.push(82u8);
    let l2cap_write = L2capHeader::serialize(cid::ATT, &write_req);
    server.with_server(|s| {
        s.device
            .process_l2cap_packet(conn_handle, &l2cap_write)
            .expect("write processes")
            .expect("write produces a response")
    });

    engine
        .run_with_scope(
            &mut scope,
            r#"
                wait_for "characteristic_write" {
                    assert(event.value == blob(1, 82), "script sees the written bytes");
                    assert(event.response_needed, "WRITE_REQ needs a response");
                }
            "#,
        )
        .expect("write-observing script passes its asserts");

    // The write went through the real GattDatabase, not a cosmetic queue.
    assert_eq!(
        server.with_server(|s| s.device.gatt_db.read(value_handle, 0).unwrap().to_vec()),
        vec![82u8]
    );
}

#[test]
fn test_constants_match_their_rust_sources() {
    let engine = new_engine();

    // uuid::* resolve to the same Uuid values as the Rust consts.
    assert_eq!(
        engine.eval::<Uuid>("uuid::HEART_RATE_SERVICE").unwrap(),
        HeartRateService::SERVICE_UUID
    );
    assert_eq!(
        engine.eval::<Uuid>("uuid::SINK_PAC").unwrap(),
        pacs_uuid::SINK_PAC
    );
    assert_eq!(
        engine
            .eval::<Uuid>("uuid::CLIENT_CHARACTERISTIC_CONFIGURATION")
            .unwrap(),
        Uuid::from_u16(0x2902)
    );
    assert_eq!(
        engine.eval::<Uuid>(r#"uuid::of("180D")"#).unwrap(),
        HeartRateService::SERVICE_UUID
    );

    // android::* integer constants line up with the Android layer's values.
    assert_eq!(
        engine.eval::<i64>("android::PROPERTY_NOTIFY").unwrap(),
        0x10
    );
    assert_eq!(
        engine.eval::<i64>("android::PERMISSION_WRITE").unwrap(),
        0x10
    );
    assert_eq!(engine.eval::<i64>("android::GATT_FAILURE").unwrap(), 257);
    assert_eq!(
        engine
            .eval::<i64>("android::SERVICE_TYPE_SECONDARY")
            .unwrap(),
        1
    );

    // audio::* bitmasks come straight from profiles::bap.
    assert_eq!(
        engine.eval::<i64>("audio::location::STEREO").unwrap(),
        audio_location::STEREO as i64
    );
    assert_eq!(
        engine.eval::<i64>("audio::context::MEDIA").unwrap(),
        context_type::MEDIA as i64
    );

    // Module constants are immutable to scripts.
    assert!(engine.eval::<i64>("android::PROPERTY_READ = 99").is_err());
}

#[test]
fn test_assert_failure_surfaces_as_script_error_with_message() {
    let engine = new_engine();
    let err = engine
        .run(r#"assert(1 == 2, "one is not two")"#)
        .expect_err("failing assert errors");
    assert!(
        err.to_string().contains("one is not two"),
        "error carries the message: {err}"
    );

    engine
        .run(r#"assert(2 == 2, "unused")"#)
        .expect("passing assert is silent");
}

#[test]
fn test_operations_limit_stops_infinite_loop() {
    let engine = new_engine();
    let err = engine
        .run("loop { }")
        .expect_err("infinite loop is stopped");
    assert!(
        matches!(*err, rhai::EvalAltResult::ErrorTooManyOperations(_)),
        "expected operations-limit error, got: {err}"
    );
}

#[test]
fn test_wait_for_matches_and_returns_block_value() {
    let (engine, mut scope, server) = build_hrm();
    let peer = Address::from_be_bytes([1, 2, 3, 4, 5, 6]);
    server.with_server(|s| s.device.on_connected(0x0001, peer));

    let value: i64 = engine
        .eval_with_scope(&mut scope, r#"wait_for "connected" { 7 }"#)
        .expect("wait_for matches the queued event");
    assert_eq!(value, 7);
}

#[test]
fn test_wait_for_without_pending_event_errors() {
    let (engine, mut scope, _server) = build_hrm();
    // Only "service_added" is queued; "connected" never happened.
    let err = engine
        .run_with_scope(&mut scope, r#"wait_for "connected" { }"#)
        .expect_err("no matching event pending");
    assert!(
        err.to_string().contains("wait_for") && err.to_string().contains("connected"),
        "error names the missing event: {err}"
    );
}

#[test]
fn test_wait_for_discards_earlier_non_matching_events() {
    let (engine, mut scope, server) = build_hrm();
    let peer = Address::from_be_bytes([1, 2, 3, 4, 5, 6]);
    server.with_server(|s| s.device.on_connected(0x0001, peer));
    server.with_server(|s| s.device.on_disconnected(0x0001));

    engine
        .run_with_scope(
            &mut scope,
            r#"
                // Skips past service_added and connected to the disconnect...
                wait_for "disconnected" { }
            "#,
        )
        .expect("later event matches");

    // ...which consumed the earlier "connected" (drain semantics).
    let _ = engine
        .run_with_scope(&mut scope, r#"wait_for "connected" { }"#)
        .expect_err("earlier events were drained");
}

#[test]
fn test_take_events_drains_per_server_and_globally() {
    let (engine, mut scope, server) = build_hrm();
    let peer = Address::from_be_bytes([1, 2, 3, 4, 5, 6]);
    server.with_server(|s| s.device.on_connected(0x0001, peer));

    engine
        .run_with_scope(
            &mut scope,
            r#"
                let events = server.take_events();
                assert(events.len == 2, "service_added + connected");
                assert(events[0].event == "service_added", "builder event first");
                assert(events[1].event == "connected", "then the connection");
                assert(take_events().len == 0, "per-server drain emptied the session queue");
            "#,
        )
        .expect("take_events script passes its asserts");
}
