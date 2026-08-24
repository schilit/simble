use super::*;

const HRM: &str = r#"
        let server = android::BluetoothGattServer("HRM");
        let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
        hrs.add_characteristic(android::BluetoothGattCharacteristic(
            uuid::HEART_RATE_MEASUREMENT,
            android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ));
        server.add_service(hrs);
    "#;

fn call(server: &mut Server, name: &str, args: Value) -> Value {
    server
        .handle(&json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": args },
        }))
        .unwrap()
}

#[test]
fn test_initialize_and_tools_list() {
    let mut s = Server::default();
    let init = s
        .handle(&json!({"jsonrpc":"2.0","id":1,"method":"initialize"}))
        .unwrap();
    assert_eq!(init["result"]["serverInfo"]["name"], "simble");
    assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);

    let list = s
        .handle(&json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}))
        .unwrap();
    let names: Vec<&str> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "lint",
        "run_test",
        "run_on",
        "add_peripheral",
        "tick",
        "status",
    ] {
        assert!(names.contains(&expected), "missing tool {expected}");
    }
}

#[test]
fn add_central_points_a_scripted_client_at_the_peripheral_the_scene_allocated() {
    // The script names an address it cannot know — MCP allocates them —
    // so the tool re-points it and says so. Without that, every client
    // script an agent copied out of `example` would sit in "connecting".
    let mut s = Server::default();
    call(
        &mut s,
        "add_peripheral",
        json!({ "script": catalog::script("hrm").unwrap() }),
    );
    let added = call(
        &mut s,
        "add_central",
        json!({ "script": catalog::script("hrm_client").unwrap() }),
    );
    assert_eq!(added["result"]["isError"], false);
    let text = added["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pointed at"), "{text}");
    assert!(text.contains("\"phase\": \"ready\""), "{text}");
    assert!(text.contains("2A37"), "{text}");
}

#[test]
fn add_central_reports_a_failed_assertion_as_a_tool_error() {
    // A client script is a test; if its assertions do not hold, the agent
    // must be told so rather than reading a healthy-looking GATT dump.
    let mut s = Server::default();
    call(
        &mut s,
        "add_peripheral",
        json!({ "script": catalog::script("hrm").unwrap() }),
    );
    let added = call(
        &mut s,
        "add_central",
        json!({ "script": r#"
                let client = android::BluetoothGatt("Probe");
                client.connect("AA:BB:CC:00:00:01");
                fn on_services_discovered(client) {
                    assert(client.services().len() == 99, "impossible service count");
                }
            "# }),
    );
    assert_eq!(added["result"]["isError"], true);
    let text = added["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("impossible service count"), "{text}");
}

#[test]
fn add_central_is_refused_on_netsim_where_the_far_side_plays_the_central() {
    let mut s = Server::default();
    call(&mut s, "run_on", json!({ "target": "netsim" }));
    let added = call(&mut s, "add_central", json!({ "script": "let c = 1;" }));
    assert_eq!(added["result"]["isError"], true);
    let text = added["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("self-mode only"), "{text}");
}

#[test]
fn test_run_test_pass_and_fail() {
    let mut s = Server::default();
    let pass = call(
        &mut s,
        "run_test",
        json!({"script": r#"let x = android::BluetoothGattServer("t"); assert(x.name == "t", "n");"#}),
    );
    assert_eq!(pass["result"]["isError"], false);
    assert!(
        pass["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("PASS")
    );

    let fail = call(
        &mut s,
        "run_test",
        json!({"script": r#"assert(1 == 2, "nope");"#}),
    );
    assert_eq!(fail["result"]["isError"], true);
}

#[test]
fn test_lint_without_running() {
    let mut s = Server::default();
    assert_eq!(
        call(&mut s, "lint", json!({"script": "let a = 1;"}))["result"]["isError"],
        false
    );
    assert_eq!(
        call(&mut s, "lint", json!({"script": "let a = ;"}))["result"]["isError"],
        true
    );
}

#[test]
fn test_example_lists_serves_and_rejects() {
    let mut s = Server::default();

    let listing = call(&mut s, "example", json!({}));
    assert_eq!(listing["result"]["isError"], false);
    let text = listing["result"]["content"][0]["text"].as_str().unwrap();
    for example in EXAMPLES {
        let name = example.name;
        assert!(text.contains(name), "listing should name {name}: {text}");
    }

    let hrm = call(&mut s, "example", json!({"name": "hrm"}));
    assert_eq!(hrm["result"]["isError"], false);
    assert!(
        hrm["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("BluetoothGattServer")
    );

    let unknown = call(&mut s, "example", json!({"name": "toaster"}));
    assert_eq!(unknown["result"]["isError"], true);
}

#[test]
fn test_lookup_by_name_and_by_uuid() {
    let mut s = Server::default();

    let by_name = call(&mut s, "lookup", json!({"query": "therm"}));
    assert_eq!(by_name["result"]["isError"], false);
    let text = by_name["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("0x1809 service — Health Thermometer"),
        "{text}"
    );

    let chars = call(&mut s, "lookup", json!({"query": "temperature meas"}));
    assert!(
        chars["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("0x2A1C characteristic — Temperature Measurement")
    );

    let by_uuid = call(&mut s, "lookup", json!({"query": "0x181A"}));
    assert_eq!(by_uuid["result"]["isError"], false);
    assert!(
        by_uuid["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Environmental Sensing")
    );

    let miss = call(&mut s, "lookup", json!({"query": "FFFF"}));
    assert_eq!(miss["result"]["isError"], true);
    let broad = call(&mut s, "lookup", json!({"query": "e"}));
    let text = broad["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("more — narrow the query"), "capped: {text}");
}

#[test]
fn test_status_and_scan_annotate_sig_names() {
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM}));
    call(&mut s, "tick", json!({"seconds": 0.2}));

    let status = call(&mut s, "status", json!({}));
    let text = status["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Heart Rate Measurement"), "status: {text}");

    let scan = call(&mut s, "scan", json!({}));
    let text = scan["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("Heart Rate"), "scan: {text}");
}

#[test]
fn test_every_example_lints_runs_and_ticks() {
    // The samples are the served API docs — each must lint, join a live
    // scene, and tick without a script error.
    for &catalog::DeviceExample { name, script, .. } in EXAMPLES {
        let mut s = Server::default();
        let linted = call(&mut s, "lint", json!({"script": script}));
        assert_eq!(
            linted["result"]["isError"], false,
            "example {name} should lint: {linted}"
        );

        let added = call(&mut s, "add_peripheral", json!({"script": script}));
        assert_eq!(
            added["result"]["isError"], false,
            "example {name} should load: {added}"
        );

        call(&mut s, "tick", json!({"seconds": 1.0}));
        let status = call(&mut s, "status", json!({}));
        let text = status["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("\"last_error\": null") || text.contains("\"last_error\":null"),
            "example {name} should tick cleanly: {text}"
        );
    }
}

#[test]
fn test_scene_lifecycle_self_add_tick_status() {
    let mut s = Server::default();
    assert_eq!(
        call(&mut s, "run_on", json!({"target": "self"}))["result"]["isError"],
        false
    );

    let added = call(&mut s, "add_peripheral", json!({"script": HRM}));
    assert_eq!(added["result"]["isError"], false);

    call(&mut s, "tick", json!({"seconds": 0.2}));

    let status = call(&mut s, "status", json!({}));
    assert_eq!(status["result"]["isError"], false);
    let text = status["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"controller\": \"self\""));
    assert!(
        text.contains("HRM"),
        "status should name the device: {text}"
    );
}

#[test]
fn test_scan_hears_the_scripted_peripheral() {
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM}));
    let scan = call(&mut s, "scan", json!({}));
    assert_eq!(scan["result"]["isError"], false);
    let reports = scan["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        reports.contains("HRM"),
        "scanner should hear the HRM advert: {reports}"
    );
}

#[test]
fn test_scan_dedupes_accumulated_reports() {
    // Ticking between scans piles up duplicate adverts; scan must return
    // one entry per advertiser with a count, not the raw backlog.
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM}));
    call(&mut s, "scan", json!({}));
    call(&mut s, "tick", json!({"seconds": 3.0}));

    let scan = call(&mut s, "scan", json!({}));
    let text = scan["result"]["content"][0]["text"].as_str().unwrap();
    let reports: Vec<Value> = serde_json::from_str(text).unwrap();
    assert_eq!(reports.len(), 1, "one entry per advertiser: {text}");
    assert!(
        reports[0]["reports"].as_u64().unwrap() > 1,
        "backlog should be counted, not repeated: {text}"
    );
    assert_eq!(reports[0]["name"], "HRM");
}

/// Pulls a characteristic's hex value out of a status/read JSON blob and
/// decodes it to bytes. Used by the device tests to read a value without
/// depending on a particular byte offset the `assert` tool would need.
fn characteristic_value(json_text: &str, uuid: &str) -> Option<Vec<u8>> {
    let value: Value = serde_json::from_str(json_text.get(json_text.find('{')?..)?).ok()?;
    fn walk(node: &Value, uuid: &str) -> Option<String> {
        match node {
            Value::Object(map) => {
                if map.get("uuid").and_then(Value::as_str) == Some(uuid)
                    && let Some(Value::String(hex)) = map.get("value")
                {
                    return Some(hex.clone());
                }
                map.values().find_map(|v| walk(v, uuid))
            }
            Value::Array(items) => items.iter().find_map(|v| walk(v, uuid)),
            _ => None,
        }
    }
    let hex = walk(&value, uuid)?;
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Adds the named example to a fresh server and connects a central.
fn serve_example(name: &str) -> Server {
    let script = catalog::script(name).unwrap_or_else(|| panic!("no example named {name}"));
    let mut server = Server::default();
    let added = call(&mut server, "add_peripheral", json!({"script": script}));
    assert_eq!(added["result"]["isError"], false, "{name}: {added}");
    let connected = call(&mut server, "connect", json!({}));
    assert_eq!(connected["result"]["isError"], false, "{name}: {connected}");
    server
}

#[test]
fn test_smart_lock_control_point_locks_and_unlocks() {
    // The lock is the control-point idiom over a vendor service: a write
    // is a command, and the state characteristic is the result.
    const STATE: &str = "d3a70002-1f8a-4b2c-9a11-000000000001";
    const CONTROL: &str = "d3a70003-1f8a-4b2c-9a11-000000000001";
    let mut s = serve_example("smart_lock");

    // Starts locked.
    let locked = call(
        &mut s,
        "assert",
        json!({"uuid": STATE, "op": "==", "value": 1, "byte": 0}),
    );
    assert_eq!(locked["result"]["isError"], false, "{locked}");

    // 0x02 = unlock.
    call(&mut s, "write", json!({"uuid": CONTROL, "value": [0x02]}));
    call(&mut s, "tick", json!({"seconds": 0.2}));
    let unlocked = call(
        &mut s,
        "assert",
        json!({"uuid": STATE, "op": "==", "value": 0, "byte": 0}),
    );
    assert_eq!(unlocked["result"]["isError"], false, "{unlocked}");

    // The command is consumed, so the state holds until the next write.
    call(&mut s, "tick", json!({"seconds": 0.4}));
    let still = call(
        &mut s,
        "assert",
        json!({"uuid": STATE, "op": "==", "value": 0, "byte": 0}),
    );
    assert_eq!(still["result"]["isError"], false, "{still}");

    // 0x01 = lock again.
    call(&mut s, "write", json!({"uuid": CONTROL, "value": [0x01]}));
    call(&mut s, "tick", json!({"seconds": 0.2}));
    let relocked = call(
        &mut s,
        "assert",
        json!({"uuid": STATE, "op": "==", "value": 1, "byte": 0}),
    );
    assert_eq!(relocked["result"]["isError"], false, "{relocked}");
}

#[test]
fn test_hid_keyboard_emits_key_and_release_reports() {
    // A keystroke is two reports: the key held, then an empty report.
    // Byte 2 is the first key slot (after modifiers and the reserved byte).
    let mut s = serve_example("hid_keyboard");
    // The clock already advanced during connect, so sample a window
    // rather than assuming an exact `t`.
    let mut keys_seen = Vec::new();
    for _ in 0..8 {
        call(&mut s, "tick", json!({"seconds": 0.5}));
        let read = call(&mut s, "read", json!({"uuid": "2A4D"}));
        let text = read["result"]["content"][0]["text"].as_str().unwrap();
        if let Some(value) = characteristic_value(text, "2A4D")
            && value.len() >= 3
        {
            keys_seen.push(value[2]);
        }
    }
    assert!(
        keys_seen.iter().any(|&k| k != 0),
        "a key should be held at some point: {keys_seen:?}"
    );
    assert!(keys_seen.contains(&0), "and released again: {keys_seen:?}");

    // The report map must be readable and start with the HID descriptor
    // for Usage Page (Generic Desktop) — without it a host cannot decode
    // the reports at all.
    let map = call(&mut s, "read", json!({"uuid": "2A4B"}));
    assert_eq!(map["result"]["isError"], false, "{map}");
    assert!(
        map["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("0501"),
        "report map should be present: {map}"
    );
}

/// The example's Report Map is not decoration: it is the only thing that
/// tells a host these bytes are pointer motion. Checked with the same
/// descriptor walker a real host uses, so an edit that breaks the item
/// encoding fails here rather than silently producing a device nothing
/// can interpret.
#[test]
fn test_hid_mouse_report_map_identifies_a_mouse_to_a_host() {
    use crate::devices::helpers::hid_reports::top_level_usage;
    let mut s = serve_example("hid_mouse");

    let map = call(&mut s, "read", json!({"uuid": "2A4B"}));
    let text = map["result"]["content"][0]["text"].as_str().unwrap();
    let descriptor = characteristic_value(text, "2A4B").expect("report map");
    // Generic Desktop (0x01), Mouse (0x02).
    assert_eq!(top_level_usage(&descriptor), Some((0x01, 0x02)));

    call(&mut s, "tick", json!({"seconds": 0.5}));
    let read = call(&mut s, "read", json!({"uuid": "2A4D"}));
    let text = read["result"]["content"][0]["text"].as_str().unwrap();
    let report = characteristic_value(text, "2A4D").expect("input report");
    assert_eq!(
        report.len(),
        4,
        "the descriptor declares 3 relative axes plus the button byte"
    );
}

#[test]
fn test_cycling_counters_only_increase() {
    // Speed is computed by the phone from cumulative counts, so the
    // counter must be monotonic — a wrapping or resetting one reads as
    // a huge negative speed.
    let mut s = serve_example("cycling");
    call(&mut s, "tick", json!({"seconds": 3.0}));
    let at_three = call(
        &mut s,
        "assert",
        json!({"uuid": "2A5B", "op": "==", "value": 3, "byte": 1}),
    );
    assert_eq!(at_three["result"]["isError"], false, "{at_three}");

    call(&mut s, "tick", json!({"seconds": 4.0}));
    let later = call(
        &mut s,
        "assert",
        json!({"uuid": "2A5B", "op": ">", "value": 3, "byte": 1}),
    );
    assert_eq!(later["result"]["isError"], false, "{later}");

    // Feature bits advertise wheel + crank data.
    let feature = call(
        &mut s,
        "assert",
        json!({"uuid": "2A5C", "op": "==", "value": 0x03, "byte": 0}),
    );
    assert_eq!(feature["result"]["isError"], false, "{feature}");
}

#[test]
fn test_fitness_tracker_exposes_every_service() {
    // A wearable is several services on one server; the point of the
    // example is that they coexist and all stay live.
    let mut s = serve_example("fitness_tracker");
    call(&mut s, "tick", json!({"seconds": 1.0}));

    let status = call(&mut s, "status", json!({}));
    let text = status["result"]["content"][0]["text"].as_str().unwrap();
    for service in ["180D", "180F", "180A"] {
        assert!(text.contains(service), "missing service {service}: {text}");
    }
    // Read the device's own view rather than the central's: a device
    // mixing 16-bit and 128-bit services trips a discovery bug in
    // `CentralDevice` (phantom services, repeated characteristics), so
    // going through the central here would test that bug, not this
    // device. See docs/android-peripherals.md.
    let heart_rate = characteristic_value(text, "2A37").expect("heart rate present");
    assert!(
        heart_rate.len() >= 2 && heart_rate[1] >= 64,
        "heart rate should be live: {heart_rate:?}"
    );
    let battery = characteristic_value(text, "2A19").expect("battery present");
    assert_eq!(battery, vec![84]);
    let steps = characteristic_value(text, "f1e20002-8c3d-4a5b-9e6f-000000000001")
        .expect("step counter present");
    assert_eq!(steps.len(), 4, "steps are a 32-bit counter: {steps:?}");
}

#[test]
fn test_pulse_oximeter_and_scale_report_plausible_values() {
    let mut s = serve_example("pulse_oximeter");
    call(&mut s, "tick", json!({"seconds": 1.0}));
    // SpO2 is a percentage: anything above 100 is a decoding bug.
    let spo2 = call(
        &mut s,
        "assert",
        json!({"uuid": "2A5F", "op": "<=", "value": 100, "byte": 1}),
    );
    assert_eq!(spo2["result"]["isError"], false, "{spo2}");

    let mut scale = serve_example("weight_scale");
    call(&mut scale, "tick", json!({"seconds": 0.5}));
    let status = call(&mut scale, "status", json!({}));
    let text = status["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("181D"), "weight scale service: {text}");
    assert!(text.contains("181B"), "body composition service: {text}");
}

#[test]
fn test_beacons_are_non_connectable_broadcasters() {
    // A beacon's identity is its advertisement; it must not offer a
    // connection, or a scanner shows it as a connectable peripheral.
    for name in ["eddystone", "fast_pair"] {
        let script = catalog::script(name).unwrap();
        assert!(
            script.contains("advertise_connectable(false)"),
            "{name} must be broadcast-only"
        );
        assert!(
            script.contains("advertise_service_data"),
            "{name} must carry service data"
        );
    }
}

#[test]
fn test_ranging_devices_publish_distance_over_the_ranging_service() {
    // Channel Sounding's measurement is a controller procedure; what a
    // phone talks to is the Ranging Service, so that is what these
    // devices must actually expose and update.
    for name in ["ranging", "ranging_tag"] {
        let script = catalog::script(name).unwrap();
        let mut s = Server::default();
        let added = call(&mut s, "add_peripheral", json!({"script": script}));
        assert_eq!(added["result"]["isError"], false, "{name}: {added}");
        assert_eq!(
            call(&mut s, "connect", json!({}))["result"]["isError"],
            false
        );

        // Real-Time Ranging Data is [f32 metres, f32 confidence] LE.
        call(&mut s, "tick", json!({"seconds": 1.0}));
        let read = call(&mut s, "read", json!({"uuid": "2C15"}));
        assert_eq!(read["result"]["isError"], false, "{name}: {read}");
        let text = read["result"]["content"][0]["text"].as_str().unwrap();
        let value = text
            .split("\"2C15\"")
            .nth(1)
            .and_then(|t| t.split("\"value\":\"").nth(1))
            .and_then(|t| t.split('"').next())
            .unwrap_or("");
        assert_eq!(value.len(), 16, "{name}: 8 bytes of ranging data: {value}");
        let bytes: Vec<u8> = (0..8)
            .map(|i| u8::from_str_radix(&value[i * 2..i * 2 + 2], 16).unwrap())
            .collect();
        let metres = f32::from_le_bytes(bytes[0..4].try_into().unwrap());
        let confidence = f32::from_le_bytes(bytes[4..8].try_into().unwrap());
        assert!(
            (0.5..10.0).contains(&metres),
            "{name}: a plausible distance, got {metres}"
        );
        assert!(
            (0.0..=1.0).contains(&confidence),
            "{name}: confidence is a fraction, got {confidence}"
        );
    }
}

#[test]
fn test_volume_control_point_commands_change_state() {
    // The LE Audio control-point idiom end to end: write an opcode, the
    // device applies it and reports the new state.
    let script = catalog::script("volume").unwrap();
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": script}));
    assert_eq!(
        call(&mut s, "connect", json!({}))["result"]["isError"],
        false
    );

    // Set Absolute Volume (0x04) to 200.
    let wrote = call(
        &mut s,
        "write",
        json!({"uuid": "2B7E", "value": [0x04, 0x00, 200]}),
    );
    assert_eq!(wrote["result"]["isError"], false, "write: {wrote}");
    call(&mut s, "tick", json!({"seconds": 0.2}));
    let at_200 = call(
        &mut s,
        "assert",
        json!({"uuid": "2B7D", "op": "==", "value": 200, "byte": 0}),
    );
    assert_eq!(at_200["result"]["isError"], false, "{at_200}");

    // Relative Volume Down (0x00) steps by 16.
    call(
        &mut s,
        "write",
        json!({"uuid": "2B7E", "value": [0x00, 0x01]}),
    );
    call(&mut s, "tick", json!({"seconds": 0.2}));
    let stepped = call(
        &mut s,
        "assert",
        json!({"uuid": "2B7D", "op": "==", "value": 184, "byte": 0}),
    );
    assert_eq!(stepped["result"]["isError"], false, "{stepped}");

    // Mute (0x06) sets the mute byte without touching the volume.
    call(
        &mut s,
        "write",
        json!({"uuid": "2B7E", "value": [0x06, 0x02]}),
    );
    call(&mut s, "tick", json!({"seconds": 0.2}));
    let muted = call(
        &mut s,
        "assert",
        json!({"uuid": "2B7D", "op": "==", "value": 1, "byte": 1}),
    );
    assert_eq!(muted["result"]["isError"], false, "{muted}");
    let still_184 = call(
        &mut s,
        "assert",
        json!({"uuid": "2B7D", "op": "==", "value": 184, "byte": 0}),
    );
    assert_eq!(still_184["result"]["isError"], false, "{still_184}");
}

#[test]
fn test_write_setpoint_drives_the_thermostat() {
    // The settable-device flow: connect, write the custom setpoint, and
    // the script's tick converges the ESS temperature onto it.
    let script = catalog::script("thermostat").unwrap();
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": script}));
    assert_eq!(
        call(&mut s, "connect", json!({}))["result"]["isError"],
        false
    );

    let wrote = call(
        &mut s,
        "write",
        json!({"uuid": "5e7b0002-c0de-4a11-b1e5-0000c0ffee01", "value": [25]}),
    );
    assert_eq!(wrote["result"]["isError"], false, "write: {wrote}");

    call(&mut s, "tick", json!({"seconds": 2.0}));
    let held = call(
        &mut s,
        "assert",
        json!({"uuid": "2A6E", "op": "==", "value": 25}),
    );
    assert_eq!(
        held["result"]["isError"], false,
        "temperature should reach the written setpoint: {held}"
    );

    let missing = call(&mut s, "write", json!({"uuid": "BEEF", "value": [1]}));
    assert_eq!(missing["result"]["isError"], true);
}

#[test]
fn test_connect_read_assert_hr_below_200() {
    // The agentic flow behind "create a test that monitors HR < 200":
    // add a peripheral with HR = 72, connect a central, assert HR < 200.
    const HRM_72: &str = r#"
            let server = android::BluetoothGattServer("HRM");
            let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
            hr.set_value([0x00, 72]);
            hrs.add_characteristic(hr);
            server.add_service(hrs);
        "#;
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM_72}));
    let connected = call(&mut s, "connect", json!({}));
    assert_eq!(
        connected["result"]["isError"], false,
        "connect: {connected}"
    );

    let pass = call(
        &mut s,
        "assert",
        json!({"uuid": "2A37", "op": "<", "value": 200}),
    );
    assert_eq!(
        pass["result"]["isError"], false,
        "HR 72 < 200 should PASS: {pass}"
    );
    assert!(
        pass["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("PASS")
    );

    let fail = call(
        &mut s,
        "assert",
        json!({"uuid": "2A37", "op": ">", "value": 200}),
    );
    assert_eq!(fail["result"]["isError"], true, "HR 72 > 200 should FAIL");
}

#[test]
fn test_assert_over_monitors_notifications() {
    // A peripheral that updates HR every tick (fn tick + update_value), so
    // the monitor samples notified values over time.
    fn hrm(hr: u8) -> String {
        format!(
            r#"
                let server = android::BluetoothGattServer("HRM");
                let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
                let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
                    android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
                hr.set_value([0x00, {hr}]);
                hrs.add_characteristic(hr);
                server.add_service(hrs);
                fn tick(server, t) {{ server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, {hr}]); }}
            "#
        )
    }

    // Safe HR (72): monitoring "< 200" holds across all samples.
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": hrm(72)}));
    call(&mut s, "connect", json!({}));
    let ok = call(
        &mut s,
        "assert_over",
        json!({"uuid":"2A37","op":"<","value":200,"seconds":0.5}),
    );
    assert_eq!(
        ok["result"]["isError"], false,
        "72 < 200 over time should PASS: {ok}"
    );
    assert!(
        ok["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("PASS")
    );

    // Unsafe HR (220): monitoring "< 200" catches the violation.
    let mut s2 = Server::default();
    call(&mut s2, "add_peripheral", json!({"script": hrm(220)}));
    call(&mut s2, "connect", json!({}));
    let bad = call(
        &mut s2,
        "assert_over",
        json!({"uuid":"2A37","op":"<","value":200,"seconds":0.5}),
    );
    assert_eq!(
        bad["result"]["isError"], true,
        "220 < 200 should FAIL: {bad}"
    );
}

#[test]
fn test_add_peripheral_rejects_bad_script() {
    let mut s = Server::default();
    // Compiles, but builds no server -> rejected by run_script.
    let resp = call(&mut s, "add_peripheral", json!({"script": "let x = 1;"}));
    assert_eq!(resp["result"]["isError"], true);
}

#[test]
fn test_run_on_netsim_selects_the_backend() {
    // Selecting netsim succeeds without a running netsimd (connections
    // happen per-peripheral); central-side tools are then refused.
    let mut s = Server::default();
    let resp = call(&mut s, "run_on", json!({"target": "netsim"}));
    assert_eq!(resp["result"]["isError"], false, "{resp}");
    assert!(
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("netsim")
    );

    let scan = call(&mut s, "scan", json!({}));
    assert_eq!(scan["result"]["isError"], true);
    assert!(
        scan["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("self-mode only")
    );

    let unknown = call(&mut s, "run_on", json!({"target": "rootcanal"}));
    assert_eq!(unknown["result"]["isError"], true);
}

// --- run_on("usb") ------------------------------------------------------
//
// No test here touches a dongle: `run_on` only *selects* the backend, and
// `UsbScene` defers opening to the first `add_peripheral` exactly as the
// netsim scene defers its connection. What is covered is argument
// parsing, the dispatch, and the error paths; the live path — a real
// dongle advertising to a real phone — is not exercised by CI.

#[test]
fn test_run_on_usb_selects_the_dongle_backend() {
    let mut s = Server::default();
    let auto = call(&mut s, "run_on", json!({"target": "usb"}));
    assert_eq!(auto["result"]["isError"], false, "{auto}");
    let text = auto["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("runs on: usb"), "{text}");
    assert!(text.contains("first Bluetooth-class dongle"), "{text}");

    // An explicit dongle is echoed back normalized, so an agent can see
    // which one it actually asked for.
    let chosen = call(
        &mut s,
        "run_on",
        json!({"target": "usb", "device": "0A12:0001"}),
    );
    assert_eq!(chosen["result"]["isError"], false, "{chosen}");
    assert!(
        chosen["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("0a12:0001"),
        "{chosen}"
    );

    // Like every live backend, it is peripheral-only.
    let connect = call(&mut s, "connect", json!({}));
    assert_eq!(connect["result"]["isError"], true);
    let text = connect["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("self-mode only"), "{text}");
    assert!(text.contains("on usb"), "{text}");

    // …and status reports which controller is selected, with no device
    // on it and no hardware consulted.
    let status = call(&mut s, "status", json!({}));
    assert_eq!(status["result"]["isError"], false, "{status}");
    let text = status["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("\"controller\": \"usb\""), "{text}");
}

#[test]
fn test_run_on_usb_rejects_a_malformed_device_selector() {
    // A vid:pid typo must fail at the call that contains it, naming the
    // expected form — not several calls later as "dongle not found".
    let mut s = Server::default();
    let bad = call(
        &mut s,
        "run_on",
        json!({"target": "usb", "device": "0a120001"}),
    );
    assert_eq!(bad["result"]["isError"], true, "{bad}");
    let text = bad["result"]["content"][0]["text"].as_str().unwrap();
    assert!(
        text.contains("0a12:0001"),
        "names the expected form: {text}"
    );

    // A rejected selector leaves the previous scene alone rather than
    // half-switching to a backend that was never built.
    assert!(s.live.is_none(), "no backend selected on a bad selector");
}

#[test]
fn test_usb_add_peripheral_without_a_dongle_reports_it_as_a_device_error() {
    // A vid:pid that cannot exist, so the outcome is the same whether or
    // not the machine running the tests has a dongle plugged in. This is
    // the only place the USB path really tries to open hardware.
    let mut s = Server::default();
    call(
        &mut s,
        "run_on",
        json!({"target": "usb", "device": "ffff:ffff"}),
    );
    let added = call(&mut s, "add_peripheral", json!({"script": HRM}));
    assert_eq!(added["result"]["isError"], true, "{added}");
    let text = added["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("device rejected:"), "{text}");
    assert!(
        text.contains("dongle") || text.contains("USB"),
        "should say what could not be opened: {text}"
    );
}

#[test]
fn test_netsim_add_peripheral_unreachable_gives_hint() {
    // Bind-then-drop a listener to get a port that refuses connections,
    // so the test is deterministic whether or not a netsimd is running.
    let port = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap().port()
    };
    let mut scene = NetsimScene::new(&format!("ws://127.0.0.1:{port}"));
    let err = scene
        .add_peripheral("F0:DE:C0:00:00:01".parse().unwrap(), "let a = 1;")
        .unwrap_err();
    // A GATT-server-less script is rejected before any connection…
    assert!(err.contains("BluetoothGattServer"), "{err}");

    let err = scene
        .add_peripheral(
            "F0:DE:C0:00:00:01".parse().unwrap(),
            r#"let server = android::BluetoothGattServer("X");"#,
        )
        .unwrap_err();
    assert!(err.contains("netsimd"), "should carry the hint: {err}");
}

#[test]
fn test_notification_and_unknown_method() {
    let mut s = Server::default();
    assert!(
        s.handle(&json!({"jsonrpc":"2.0","method":"notifications/initialized"}))
            .is_none()
    );
    let err = s
        .handle(&json!({"jsonrpc":"2.0","id":9,"method":"nope"}))
        .unwrap();
    assert_eq!(err["error"]["code"], -32601);
}

// --- server→client notifications ----------------------------------------

/// A peripheral whose heart rate is fine until t = 1s and alarming after.
/// The CCCD is what makes the watch a *pushed* one: without it the central
/// has nothing to write and the peripheral never notifies.
const HRM_SPIKES: &str = r#"
        let server = android::BluetoothGattServer("HRM");
        let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
        let hr = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT,
            android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
        hr.set_value([0x00, 70]);
        hr.add_descriptor(android::BluetoothGattDescriptor(
            uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
            android::PERMISSION_READ | android::PERMISSION_WRITE));
        hrs.add_characteristic(hr);
        server.add_service(hrs);
        fn tick(server, t) {
            if t > 1.0 {
                server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, 220]);
            } else {
                server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, 70]);
            }
        }
    "#;

#[test]
fn test_a_subscribe_watch_pushes_an_id_less_notification_when_it_breaks() {
    // The asynchronous half of the monitor: arm a condition, run the
    // clock, and the server speaks first. The wire-visible difference
    // from a response is that there is no `id` — nothing is replying.
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM_SPIKES}));
    call(&mut s, "connect", json!({}));

    let armed = call(
        &mut s,
        "subscribe",
        json!({"uuid": "2A37", "op": "<", "value": 200}),
    );
    assert_eq!(armed["result"]["isError"], false, "{armed}");
    assert!(
        armed["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("watching"),
        "{armed}"
    );
    assert!(
        s.take_notifications().is_empty(),
        "a condition that holds says nothing"
    );

    let mut pushed = Vec::new();
    for _ in 0..20 {
        call(&mut s, "tick", json!({"seconds": 0.2}));
        pushed = s.take_notifications();
        if !pushed.is_empty() {
            break;
        }
    }
    assert_eq!(pushed.len(), 1, "one message for one violation: {pushed:?}");

    let note = &pushed[0];
    assert_eq!(note["jsonrpc"], "2.0");
    assert_eq!(note["method"], "notifications/message");
    assert!(
        note.get("id").is_none(),
        "a notification carries no id: {note}"
    );
    assert_eq!(note["params"]["level"], "warning");
    assert_eq!(note["params"]["logger"], "simble.monitor");
    assert_eq!(note["params"]["data"]["value"], 220);
    assert_eq!(note["params"]["data"]["expected"], "< 200");
    assert!(
        note["params"]["data"]["message"]
            .as_str()
            .unwrap()
            .contains("no longer < 200"),
        "{note}"
    );

    // A condition that stays broken announces itself once, not per tick.
    for _ in 0..5 {
        call(&mut s, "tick", json!({"seconds": 0.2}));
    }
    assert!(
        s.take_notifications().is_empty(),
        "a sustained violation must not spam the client"
    );
}

#[test]
fn test_subscribe_without_a_condition_stays_a_plain_subscribe() {
    // The watch is opt-in: the pre-existing tool call must behave exactly
    // as it did, pushing nothing however long the clock runs.
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM_SPIKES}));
    call(&mut s, "connect", json!({}));
    let plain = call(&mut s, "subscribe", json!({"uuid": "2A37"}));
    assert_eq!(plain["result"]["isError"], false, "{plain}");
    assert!(
        !plain["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("watching")
    );
    for _ in 0..10 {
        call(&mut s, "tick", json!({"seconds": 0.2}));
    }
    assert!(s.take_notifications().is_empty());
}

#[test]
fn test_subscribe_rejects_half_a_condition() {
    let mut s = Server::default();
    call(&mut s, "add_peripheral", json!({"script": HRM_SPIKES}));
    call(&mut s, "connect", json!({}));
    for half in [
        json!({"uuid": "2A37", "op": "<"}),
        json!({"uuid": "2A37", "value": 200}),
    ] {
        let resp = call(&mut s, "subscribe", half.clone());
        assert_eq!(resp["result"]["isError"], true, "{half}: {resp}");
        assert!(
            resp["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("op AND value"),
            "{resp}"
        );
    }
    let bad_op = call(
        &mut s,
        "subscribe",
        json!({"uuid": "2A37", "op": "=~", "value": 200}),
    );
    assert_eq!(bad_op["result"]["isError"], true, "{bad_op}");
}

// --- the actor loop -----------------------------------------------------

/// A sink that only publishes on `flush`, so a reader never observes half
/// a JSON object. `write_message` flushes once per complete message.
#[derive(Default)]
struct SharedOut {
    pending: Vec<u8>,
    sink: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
}

impl Write for SharedOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.pending.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let mut sink = self.sink.lock().unwrap();
        sink.extend_from_slice(&std::mem::take(&mut self.pending));
        Ok(())
    }
}

/// A reader whose `read` **blocks** until the test hands it a line — a
/// stand-in for a real stdin that is simply quiet. The loop under test
/// must make progress anyway.
struct BlockingLines {
    rx: mpsc::Receiver<String>,
    pending: Vec<u8>,
    pos: usize,
}

impl std::io::Read for BlockingLines {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        while self.pos == self.pending.len() {
            match self.rx.recv() {
                Ok(line) => {
                    self.pending = line.into_bytes();
                    self.pos = 0;
                }
                Err(_) => return Ok(0), // sender dropped: EOF
            }
        }
        let n = (self.pending.len() - self.pos).min(out.len());
        out[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Waits until the sink holds at least `n` complete messages.
fn wait_for_messages(
    sink: &std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    n: usize,
    what: &str,
) -> Vec<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let text = String::from_utf8(sink.lock().unwrap().clone()).unwrap();
        let messages: Vec<Value> = text
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("each line is one JSON message"))
            .collect();
        if messages.len() >= n {
            return messages;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "timed out waiting for {what} (have {messages:?})"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn test_actor_loop_pushes_notifications_while_input_is_idle() {
    // `serve_stdio` had no coverage at all (docs/test-strategy.md gap 7),
    // and the regression it hides is silent: reinstate a blocking read on
    // the input and every request is still answered, so the suite stays
    // green while the server can no longer pump a live backend or say
    // anything unprompted. Here nothing is ever sent until after the
    // server has spoken — a loop that blocks on input never gets there.
    let mut server = Server::default();
    server.push_notification("warning", "simble.test", json!({"event": "unprompted"}));

    let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let out = SharedOut {
        pending: Vec::new(),
        sink: sink.clone(),
    };
    let (tx, rx) = mpsc::channel::<String>();
    let input = BlockingLines {
        rx,
        pending: Vec::new(),
        pos: 0,
    };

    // The scene is non-`Send` (Rhai), so the loop stays on this thread and
    // the *test* is what runs beside it. A failed assertion or a timeout
    // in the driver drops `tx`, which is the loop's EOF, so a wedged loop
    // fails the test instead of hanging it.
    let driver = std::thread::spawn({
        let sink = sink.clone();
        move || {
            let messages = wait_for_messages(&sink, 1, "the unprompted notification");
            assert_eq!(messages[0]["method"], "notifications/message");
            assert!(
                messages[0].get("id").is_none(),
                "a notification carries no id: {}",
                messages[0]
            );

            // The same loop still answers a request that turns up later.
            tx.send("{\"jsonrpc\":\"2.0\",\"id\":7,\"method\":\"ping\"}\n".to_string())
                .unwrap();
            let messages = wait_for_messages(&sink, 2, "the ping response");
            assert_eq!(messages[1]["id"], 7);
            assert_eq!(messages[1]["result"], json!({}));
            drop(tx); // EOF
        }
    });

    serve_lines(server, std::io::BufReader::new(input), out).expect("the loop exits at EOF");
    driver.join().expect("the driver's assertions hold");
}

// --- MCP over WebSocket (`--ws-server`) ---------------------------------

/// A minimal RFC 6455 *client* for the scenario test, built from the same
/// codec the server uses (`transport::ws`) — the netsim client is
/// HCI-shaped, and MCP travels as text.
struct WsTestClient {
    stream: std::net::TcpStream,
    reader: crate::transport::ws::WsFrameReader,
}

impl WsTestClient {
    fn connect(addr: std::net::SocketAddr) -> Self {
        use std::io::Read;
        let mut stream = std::net::TcpStream::connect(addr).expect("connect");
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let request = format!(
            "GET /mcp HTTP/1.1\r\n\
                 Host: 127.0.0.1\r\n\
                 Upgrade: websocket\r\n\
                 Connection: Upgrade\r\n\
                 Sec-WebSocket-Key: {key}\r\n\
                 Sec-WebSocket-Version: 13\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).unwrap();
        let response = crate::transport::ws::read_http_headers(&mut stream).unwrap();
        assert!(response.starts_with("HTTP/1.1 101 "), "{response}");
        assert!(
            response.contains(&crate::transport::ws::expected_accept(key)),
            "{response}"
        );
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let _ = &mut stream as &mut dyn Read; // reads happen in `recv`
        Self {
            stream,
            reader: crate::transport::ws::WsFrameReader::default(),
        }
    }

    fn send(&mut self, request: &str) {
        let frame = crate::transport::ws::encode_frame(
            crate::transport::ws::OPCODE_TEXT,
            request.as_bytes(),
            Some(crate::transport::ws::mask_key()),
        );
        self.stream.write_all(&frame).unwrap();
    }

    fn recv(&mut self) -> Value {
        use std::io::Read;
        loop {
            if let Some(frame) = self.reader.next_frame() {
                assert_eq!(
                    frame.opcode,
                    crate::transport::ws::OPCODE_TEXT,
                    "JSON-RPC travels as text"
                );
                return serde_json::from_slice(&frame.payload).expect("a JSON message");
            }
            let mut chunk = [0u8; 4096];
            let n = self.stream.read(&mut chunk).expect("a server reply");
            assert!(n > 0, "server closed before replying");
            self.reader.feed(&chunk[..n]);
        }
    }
}

#[test]
fn test_ws_server_serves_initialize_and_a_tool_call() {
    // The same server, a different transport: one client connects,
    // handshakes, and drives it with real RFC 6455 text frames.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().unwrap();
    let session = std::thread::spawn(move || {
        let (stream, _peer) = listener.accept().expect("accept");
        serve_ws_client(stream)
    });

    let mut client = WsTestClient::connect(addr);

    client.send(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#);
    let init = client.recv();
    assert_eq!(init["id"], 1);
    assert_eq!(init["result"]["serverInfo"]["name"], "simble");
    assert_eq!(init["result"]["protocolVersion"], PROTOCOL_VERSION);

    // A tool call, over the socket, against a scene this connection owns.
    client.send(
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/call",
                "params":{"name":"lookup","arguments":{"query":"0x180D"}}}"#,
    );
    let looked_up = client.recv();
    assert_eq!(looked_up["id"], 2);
    assert_eq!(looked_up["result"]["isError"], false, "{looked_up}");
    assert!(
        looked_up["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("Heart Rate"),
        "{looked_up}"
    );

    // A JSON-RPC notification gets no reply, so the next thing read is
    // the response to the request after it — not a stray empty frame.
    client.send(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);
    client.send(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#);
    let pong = client.recv();
    assert_eq!(pong["id"], 3);
    assert_eq!(pong["result"], json!({}));

    // Closing the socket ends the session rather than wedging the loop.
    drop(client);
    assert!(
        session.join().expect("session thread").is_err(),
        "a client disconnect is reported as the end of the session"
    );
}
