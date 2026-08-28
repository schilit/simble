use super::*;
use crate::device::central_device::CentralPhase;
use crate::device::scripted_peripheral::CccdSubscription;
use crate::gap::AdvertisingData;
use crate::gap::build_adv_payload_with_extras;
use crate::scripting::test_script::run_test_script;
use crate::transport::scan_report::{address_from_ws_url, ws_url_with_wire_address};
use crate::types::AddressType;

#[test]
fn test_ws_url_carries_the_wire_byte_order() {
    // netsim reads the URL address LSB-first, so a page writing display
    // order would advertise the address reversed and be unreachable.
    let url = "ws://localhost:7681/v1/websocket/bt?name=web-speaker&address=CC:1E:57:00:00:06";
    assert_eq!(
        ws_url_with_wire_address(url),
        "ws://localhost:7681/v1/websocket/bt?name=web-speaker&address=06:00:00:57:1E:CC"
    );
    // The identity stays the address the page asked for.
    assert_eq!(
        address_from_ws_url(url),
        Some("CC:1E:57:00:00:06".parse().unwrap())
    );
    // A URL without an address is passed through untouched.
    let plain = "ws://localhost:7681/v1/websocket/bt?name=x";
    assert_eq!(ws_url_with_wire_address(plain), plain);
}

#[test]
fn test_address_is_parsed_from_the_netsim_url() {
    // The browser path stamps identity from the WebSocket URL; without it
    // a page-hosted device pairs with the script engine's placeholder
    // address and a real stack rejects the pairing.
    let url = "ws://localhost:7681/v1/websocket/bt?name=web-speaker&address=CC:1E:57:00:00:06";
    assert_eq!(
        address_from_ws_url(url),
        Some("CC:1E:57:00:00:06".parse().unwrap())
    );
    // Order-independent, and absent means absent.
    assert_eq!(
        address_from_ws_url("ws://h/p?address=AA:BB:CC:00:00:01&name=x"),
        Some("AA:BB:CC:00:00:01".parse().unwrap())
    );
    assert_eq!(address_from_ws_url("ws://h/p?name=x"), None);
}

#[test]
fn test_scene_stamps_on_air_identity_onto_the_device() {
    // The script engine allocates placeholder addresses (every session
    // starts at :01), but SMP computes with device.address — the scene
    // must overwrite it with the address actually on the air.
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("A");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
        "#;
    scene
        .add_peripheral("AA:BB:CC:00:00:07".parse().unwrap(), script)
        .unwrap();
    let status = scene.peripheral_status_json(0).unwrap();
    assert!(
        status.contains("AA:BB:CC:00:00:07"),
        "device should carry its on-air address: {status}"
    );
}

#[test]
fn test_connection_complete_records_peer_address_type() {
    // Byte 8 of LE Connection Complete is the peer address type; SMP
    // mixes it into pairing crypto, so it must reach the connection.
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("B");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
        "#;
    let index = scene
        .add_peripheral("AA:BB:CC:00:00:08".parse().unwrap(), script)
        .unwrap();
    scene.tick(0.1); // bring-up

    // LE Connection Complete with peer type 0x00 (public).
    let mut event = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x00];
    event.extend_from_slice(&[0xB9, 0x62, 0xF7, 0xD6, 0x79, 0x7C]); // peer LE
    event.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
    let channel = scene.devices[index].channel.clone();
    let SceneRole::Peripheral(p) = &mut scene.devices[index].role else {
        panic!("expected peripheral");
    };
    p.handle_packet(&channel, &event).unwrap();
    p.primary().with_server(|s| {
        let conn = s.device.connections.get(&0x0040).expect("connected");
        assert_eq!(conn.peer_address_type, AddressType::Public);
    });
}

#[test]
fn test_ltk_request_event_gets_a_reply_with_the_session_key() {
    // Encryption start over a real controller: the LE Long Term Key
    // Request event must be answered with the key SMP recorded on the
    // connection, or pairing fails on the peer.
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("C");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
        "#;
    let index = scene
        .add_peripheral("AA:BB:CC:00:00:09".parse().unwrap(), script)
        .unwrap();
    scene.tick(0.1); // bring-up

    let channel = scene.devices[index].channel.clone();
    while channel.poll_host_packet().is_some() {} // drain bring-up

    let SceneRole::Peripheral(p) = &mut scene.devices[index].role else {
        panic!("expected peripheral");
    };
    // Connect, then plant the key SMP would have recorded.
    let mut connect = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x01];
    connect.extend_from_slice(&[0x11; 6]);
    connect.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
    p.handle_packet(&channel, &connect).unwrap();
    let key = [0xAB; 16];
    p.primary().with_server(|s| {
        s.device.connections.get_mut(&0x0040).unwrap().ltk = Some(key);
    });

    // LE Long Term Key Request: subevent 0x05, handle, rand(8), ediv(2).
    let mut event = vec![0x04, 0x3E, 0x0D, 0x05, 0x40, 0x00];
    event.extend_from_slice(&[0x00; 10]);
    p.handle_packet(&channel, &event).unwrap();

    let reply = channel.poll_host_packet().expect("a reply must be queued");
    assert_eq!(&reply[..3], &[0x01, 0x1A, 0x20], "LTK Request Reply");
    assert_eq!(&reply[6..22], &key, "carrying the session key");
}

#[test]
fn test_scanner_hears_script_staged_advertising_extras() {
    // The beacon idiom: a script stages service data + manufacturer data
    // and the on-air advertisement must carry both to a scanner.
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("Beacon");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
                android::PROPERTY_READ, android::PERMISSION_READ);
            level.set_value([88]);
            bas.add_characteristic(level);
            server.add_service(bas);
            server.advertise_service_data(0xFE2C, [0x00, 0x11, 0x22]);
            server.advertise_manufacturer_data(0x00E0, [0x01]);
        "#;
    scene
        .add_peripheral("AA:BB:CC:00:00:01".parse().unwrap(), script)
        .unwrap();
    let scanner = scene.add_scanner("AA:BB:CC:00:00:02".parse().unwrap());
    for _ in 0..3 {
        scene.tick(0.1);
    }

    let reports = scene.scanner_reports_json(scanner);
    assert!(
        reports.contains("fe2c") || reports.contains("FE2C"),
        "{reports}"
    );
    assert!(
        reports.contains("001122") || reports.contains("[0,17,34]"),
        "service data bytes should be on the air: {reports}"
    );
    assert!(
        reports.contains("00E0"),
        "manufacturer company id should be on the air: {reports}"
    );
}

/// `add_pacs` / `add_ascs` call the Rust profile registrars, which write
/// into the GATT database rather than the script's service list — so
/// nothing in the script surface proves they landed. Read them back.
/// `push_event` + `on_event` are the host→script event path; the
/// dispatch had no test, so a script could stop receiving events
/// without anything noticing.
#[test]
fn test_pushed_events_reach_the_script_handler() {
    let script = r#"
            let server = android::BluetoothGattServer("evt");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let level = android::BluetoothGattCharacteristic(uuid::BATTERY_LEVEL,
                android::PROPERTY_READ, android::PERMISSION_READ);
            level.set_value([100]);
            bas.add_characteristic(level);
            server.add_service(bas);
            // `this` is the persistent state map bound by the runtime.
            fn on_event(server, event) {
                if event.event == "ui" && event.action == "drain" {
                    let level = server.value(uuid::BATTERY_LEVEL)[0];
                    server.update_value(uuid::BATTERY_LEVEL, [level - event.amount]);
                    server.emit("drained", #{ to: level - event.amount });
                }
            }
        "#;
    let mut peripheral = ScriptedPeripheral::run_script(script).unwrap();
    let channel = HciChannel::new();

    peripheral.push_event("ui", r#"{"action":"drain","amount":7}"#);
    peripheral.tick(&channel, 0.1).unwrap();

    let level = peripheral
        .primary()
        .with_server(|s| {
            s.device
                .gatt_db
                .value_handle_for_uuid(crate::types::Uuid::Uuid16(0x2A19))
                .and_then(|h| s.device.gatt_db.value(h).map(|v| v.to_vec()))
        })
        .unwrap();
    assert_eq!(level, vec![93], "the handler ran and applied the payload");

    let emitted = peripheral.take_emitted();
    assert_eq!(emitted.len(), 1, "the script's emit reached the host");
    assert!(
        emitted[0].contains("\"drained\"") && emitted[0].contains("93"),
        "emitted payload: {}",
        emitted[0]
    );
    assert!(
        peripheral.take_emitted().is_empty(),
        "draining is destructive"
    );
}

#[test]
fn test_profile_registrar_bindings_build_real_services() {
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("LEA");
            server.add_pacs(0x03, 0x00);
            server.add_ascs([0x01], []);
            server.add_ras();
        "#;
    let index = scene
        .add_peripheral("AA:BB:CC:00:00:51".parse().unwrap(), script)
        .unwrap();
    let status = scene.peripheral_status_json(index).unwrap();
    // The registrars write to the database; check there, not in status.
    let handles: Vec<u16> = [0x2BC9, 0x2BC4, 0x2BC6, 0x2C15]
        .iter()
        .map(|&uuid| {
            let SceneRole::Peripheral(p) = &scene.devices[index].role else {
                panic!("expected a peripheral");
            };
            p.primary().with_server(|s| {
                s.device
                    .gatt_db
                    .value_handle_for_uuid(crate::types::Uuid::Uuid16(uuid))
                    .unwrap_or_else(|| panic!("characteristic {uuid:#06X} missing"))
            })
        })
        .collect();
    assert_eq!(handles.len(), 4, "sink PAC, sink ASE, ASE CP, ranging data");
    assert!(!status.is_empty());
}

/// The HOGP keyboard the `web/hid/` page hosts, without its demo `tick` —
/// the page drives the reports itself. Kept here so the scene test and the
/// page exercise the same GATT layout.
#[cfg(test)]
const HOGP_KEYBOARD_SCRIPT: &str = r#"
        let server = android::BluetoothGattServer("SimKeyboard");
        let hid = android::BluetoothGattService(uuid::from_u16(0x1812), android::SERVICE_TYPE_PRIMARY);
        let map = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4B),
            android::PROPERTY_READ, android::PERMISSION_READ);
        map.set_value([
            0x05, 0x01, 0x09, 0x06, 0xA1, 0x01,
            0x05, 0x07, 0x19, 0xE0, 0x29, 0xE7, 0x15, 0x00, 0x25, 0x01,
            0x75, 0x01, 0x95, 0x08, 0x81, 0x02,
            0x95, 0x01, 0x75, 0x08, 0x81, 0x01,
            0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0x65,
            0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xC0,
        ]);
        hid.add_characteristic(map);
        let report = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A4D),
            android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);
        report.set_value([0, 0, 0, 0, 0, 0, 0, 0]);
        report.add_descriptor(android::BluetoothGattDescriptor(
            uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
            android::PERMISSION_READ | android::PERMISSION_WRITE));
        hid.add_characteristic(report);
        server.add_service(hid);
    "#;

/// The whole HOGP loop over the in-process radio: a scripted keyboard
/// peripheral, a central that discovers it, reads the Report Map, decides
/// it is a keyboard, subscribes, and turns the notified reports back into
/// text. Both endpoints are simble's, so this proves the plumbing, not the
/// report format — `hid_reports.rs` pins that against the published usage
/// tables.
#[test]
fn test_a_central_discovers_a_hogp_keyboard_and_decodes_its_typing() {
    let keyboard_address = "AA:BB:CC:00:00:60".parse().unwrap();
    let mut scene = SceneEngine::new();
    let keyboard = scene
        .add_peripheral(keyboard_address, HOGP_KEYBOARD_SCRIPT)
        .unwrap();
    let host = scene.add_central("AA:BB:CC:00:00:61".parse().unwrap(), keyboard_address);

    let mut t = 0.0;
    let mut started = false;
    for _ in 0..80 {
        scene.tick(t);
        t += 0.05;
        if !started {
            started = scene.central_start_hid(host);
        }
    }
    assert!(
        started,
        "the central never finished discovering the keyboard"
    );

    let identified: serde_json::Value =
        serde_json::from_str(&scene.central_hid_events_json(host)).unwrap();
    assert_eq!(
        identified["kind"], "keyboard",
        "the Report Map is what says so: {identified}"
    );
    assert_eq!(identified["ready"], true);

    // Type "hi" as a real keyboard does: a report per key down, an empty
    // report per key up. One report per tick, because the notification is
    // raised by the change in the value.
    let reports: [[u8; 8]; 4] = [
        [0, 0, 0x0B, 0, 0, 0, 0, 0], // h
        [0; 8],
        [0, 0, 0x0C, 0, 0, 0, 0, 0], // i
        [0; 8],
    ];
    let mut typed = String::new();
    for report in reports {
        scene
            .peripheral_set_value(keyboard, "2A4D", &report)
            .unwrap();
        scene.tick(t);
        t += 0.05;
        let decoded: serde_json::Value =
            serde_json::from_str(&scene.central_hid_events_json(host)).unwrap();
        for event in decoded["events"].as_array().unwrap() {
            if event["type"] == "key_down"
                && let Some(c) = event["character"].as_str()
            {
                typed.push_str(c);
            }
        }
    }
    assert_eq!(
        typed, "hi",
        "the host decoded the reports that crossed the radio"
    );
}

/// `send_audio` and `take_audio` are the script's half of the media
/// plane; neither had a test.
#[test]
fn test_script_can_send_and_receive_audio_sdus() {
    let mut scene = SceneEngine::new();
    let sink_script = r#"
            let server = android::BluetoothGattServer("sink");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
            // Drain what arrived and record how much, so a test can see it.
            fn tick(server, t) {
                let frames = server.take_audio();
                if frames.len() > 0 {
                    server.update_value(uuid::BATTERY_LEVEL, [frames.len()]);
                }
            }
        "#;
    let sink = scene
        .add_peripheral("AA:BB:CC:00:00:52".parse().unwrap(), sink_script)
        .unwrap();
    let source = scene.add_central(
        "AA:BB:CC:00:00:53".parse().unwrap(),
        "AA:BB:CC:00:00:52".parse().unwrap(),
    );
    for _ in 0..40 {
        scene.tick(0.02);
    }
    for _ in 0..3 {
        assert!(scene.central_send_audio(source, &[0xAA; 8]));
        scene.tick(0.02);
    }
    scene.tick(0.02);
    let status = scene.peripheral_status_json(sink).unwrap();
    // The script drained the SDUs and wrote the count into the battery
    // level, so a non-zero value proves take_audio saw them.
    assert!(
        !status.contains("\"value\": \"00\""),
        "the script's take_audio should have seen SDUs: {status}"
    );
}

#[test]
fn test_script_staged_service_uuids_reach_the_advertisement() {
    // A service built by a Rust profile registrar exists only in the
    // GATT database, so the script stages its UUID explicitly. That
    // staging used to be dropped, leaving the device advertising no
    // services — invisible to a scanner filtering on one.
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("Ranger");
            server.add_ras();
            server.advertise_service_uuid(0x185B);
        "#;
    scene
        .add_peripheral("AA:BB:CC:00:00:41".parse().unwrap(), script)
        .unwrap();
    let scanner = scene.add_scanner("AA:BB:CC:00:00:42".parse().unwrap());
    for _ in 0..3 {
        scene.tick(0.1);
    }
    let reports = scene.scanner_reports_json(scanner);
    assert!(
        reports.contains("185B"),
        "the staged service UUID must be on the air: {reports}"
    );
}

#[test]
fn test_beacon_advertises_non_connectable() {
    // advertise_connectable(false) must put ADV_NONCONN_IND (0x03) in the
    // LE Set Advertising Parameters and send no scan-response command.
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("Beacon");
            let bas = android::BluetoothGattService(uuid::BATTERY_SERVICE, android::SERVICE_TYPE_PRIMARY);
            bas.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::BATTERY_LEVEL, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(bas);
            server.advertise_manufacturer_data(0x004C, [0x02, 0x15]);
            server.advertise_connectable(false);
        "#;
    let _ = &mut scene;
    // Drive queue_start on a standalone channel so the bring-up commands
    // are inspectable (a live scene's link would consume them).
    let mut p = ScriptedPeripheral::run_script(script).unwrap();
    p.set_identity("AA:BB:CC:00:00:0A".parse().unwrap());
    let channel = HciChannel::new();
    p.queue_start(&channel).unwrap();
    let mut adv_type = None;
    let mut saw_scan_rsp = false;
    while let Some(pkt) = channel.poll_host_packet() {
        // H4 command (0x01), opcode LE Set Advertising Parameters 0x2006;
        // params start at index 3, advertising type is the 5th param byte.
        if pkt.len() >= 9 && pkt[0] == 0x01 && pkt[1] == 0x06 && pkt[2] == 0x20 {
            // [0..4]=H4/opcode/len, [4..8]=interval min+max, [8]=adv type.
            adv_type = Some(pkt[8]);
        }
        if pkt.len() >= 3 && pkt[0] == 0x01 && pkt[1] == 0x09 && pkt[2] == 0x20 {
            saw_scan_rsp = true;
        }
    }
    assert_eq!(adv_type, Some(0x03), "should be ADV_NONCONN_IND");
    assert!(
        !saw_scan_rsp,
        "a non-connectable beacon sends no scan response"
    );
}

#[test]
fn test_large_service_data_drops_name_to_fit() {
    // A 24-byte service-data payload (Quick Share nudge) fills the packet;
    // the builder must drop the name entirely, not emit a stub, and not
    // reject the advertisement.
    let mut extras = AdvertisingData::new();
    extras.service_data_16.push((0xFE2C, vec![0xAB; 24]));
    let payload = build_adv_payload_with_extras("QuickShare", &[], Some(&extras))
        .expect("nameless beacon must fit in 31 bytes");
    assert!(payload.len() <= 31, "len {}", payload.len());
    // Service data present…
    assert!(payload.windows(2).any(|w| w == [0x2C, 0xFE]));
    // …and no Complete Local Name AD type (0x09).
    assert!(
        !payload.contains(&0x09),
        "name should be absent: {payload:?}"
    );
}

#[test]
fn test_indicate_subscription_sends_indications_not_notifications() {
    // CCCD bit 1 is Indicate. The flush path used to send a notification
    // whatever the client asked for and to ignore bit 1 entirely, so an
    // Indicate-only characteristic (several SIG profiles mandate one)
    // delivered nothing at all.
    let script = r#"
            let server = android::BluetoothGattServer("ind");
            let svc = android::BluetoothGattService(uuid::from_u16(0x181D), android::SERVICE_TYPE_PRIMARY);
            let ch = android::BluetoothGattCharacteristic(uuid::from_u16(0x2A9D),
                android::PROPERTY_READ | android::PROPERTY_INDICATE, android::PERMISSION_READ);
            ch.set_value([0x00, 0x01]);
            ch.add_descriptor(android::BluetoothGattDescriptor(
                uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
                android::PERMISSION_READ | android::PERMISSION_WRITE));
            svc.add_characteristic(ch);
            server.add_service(svc);
            fn tick(server, t) {
                server.update_value(uuid::from_u16(0x2A9D), [0x00, 2 + t.to_int() % 5]);
            }
        "#;
    let mut peripheral = ScriptedPeripheral::run_script(script).unwrap();
    let channel = HciChannel::new();
    let drain = |channel: &HciChannel| {
        let mut out = Vec::new();
        while let Some(p) = channel.poll_host_packet() {
            out.push(p);
        }
        out
    };
    peripheral.queue_start(&channel).unwrap();
    let _ = drain(&channel);

    let mut connect = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x00];
    connect.extend_from_slice(&[0x11; 6]);
    connect.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
    peripheral.handle_packet(&channel, &connect).unwrap();
    let _ = drain(&channel);

    // Subscribe for indications (CCCD = 0x0002), not notifications.
    let watch = peripheral.watched[0].clone();
    let cccd = watch.cccd_handle.expect("characteristic declares a CCCD");
    peripheral.primary().with_server(|s| {
        let _ = s.device.gatt_db.set_value(cccd, &[0x02, 0x00]);
    });
    assert_eq!(
        peripheral.cccd_subscription(&watch),
        CccdSubscription::Indicate
    );

    peripheral.tick(&channel, 1.0).unwrap();
    let packets = drain(&channel);
    let att_opcodes: Vec<u8> = packets
        .iter()
        .filter(|p| p.len() > 9)
        .map(|p| p[9])
        .collect();
    assert!(
        att_opcodes.contains(&0x1D),
        "an Indicate subscriber must get ATT Handle Value Indication (0x1D), got {att_opcodes:02X?}"
    );
    assert!(
        !att_opcodes.contains(&0x1B),
        "and not a notification (0x1B)"
    );
}

#[test]
fn test_central_starts_discovery_on_either_connection_complete() {
    // The central used to test raw bytes for subevent 0x01 only, so a
    // controller reporting the Enhanced variant (0x0A) — what an
    // address-resolving stack sends — never started discovery. Both
    // must work now that it shares the peripheral's typed parser.
    for subevent in [0x01u8, 0x0A] {
        let mut central = CentralDevice::new("AA:BB:CC:00:00:31".parse().unwrap());
        let channel = HciChannel::new();
        let mut event = vec![0x04, 0x3E, 0x13, subevent, 0x00, 0x40, 0x00, 0x01, 0x00];
        event.extend_from_slice(&[0x11; 6]);
        event.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        central.consume(&channel, &event);
        assert_eq!(
            central.phase,
            CentralPhase::ExchangingMtu,
            "subevent {subevent:#04X} should start discovery"
        );
        assert!(
            channel.poll_host_packet().is_some(),
            "an MTU exchange request must go out"
        );
    }

    // A failed connection starts nothing.
    let mut central = CentralDevice::new("AA:BB:CC:00:00:31".parse().unwrap());
    let channel = HciChannel::new();
    let mut failed = vec![0x04, 0x3E, 0x13, 0x01, 0x02, 0x40, 0x00, 0x01, 0x00];
    failed.extend_from_slice(&[0x11; 6]);
    failed.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
    central.consume(&channel, &failed);
    assert_ne!(central.phase, CentralPhase::ExchangingMtu);
}

#[test]
fn test_audio_streams_from_central_to_peripheral() {
    // The media plane end to end: a central streams isochronous SDUs over
    // the simulated radio and the peripheral receives them in order.
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("sink");
            let vcs = android::BluetoothGattService(uuid::VOLUME_CONTROL_SERVICE, android::SERVICE_TYPE_PRIMARY);
            vcs.add_characteristic(android::BluetoothGattCharacteristic(
                uuid::VOLUME_STATE, android::PROPERTY_READ, android::PERMISSION_READ));
            server.add_service(vcs);
        "#;
    let sink = scene
        .add_peripheral("AA:BB:CC:00:00:20".parse().unwrap(), script)
        .unwrap();
    let source = scene.add_central(
        "AA:BB:CC:00:00:21".parse().unwrap(),
        "AA:BB:CC:00:00:20".parse().unwrap(),
    );
    // Let the connection come up.
    for _ in 0..40 {
        scene.tick(0.02);
    }

    // Three "codec frames" worth of audio.
    let frames: Vec<Vec<u8>> = (0u8..3).map(|i| vec![i; 8]).collect();
    for frame in &frames {
        assert!(
            scene.central_send_audio(source, frame),
            "the central must have a connection to stream over"
        );
        scene.tick(0.02);
    }
    scene.tick(0.02);

    let received = scene.peripheral_take_audio(sink);
    assert_eq!(received, frames, "every SDU arrives, in order");
    // Draining is destructive: a second take sees nothing new.
    assert!(scene.peripheral_take_audio(sink).is_empty());
}

#[test]
fn test_scene_scanner_sees_scripted_peripheral() {
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("SceneHRM");
            let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let hr = android::BluetoothGattCharacteristic(
                uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ,
            );
            hr.set_value([0x00, 72]);
            hrs.add_characteristic(hr);
            server.add_service(hrs);
        "#;
    scene
        .add_peripheral("AA:BB:CC:00:00:01".parse().unwrap(), script)
        .unwrap();
    let scanner = scene.add_scanner("AA:BB:CC:00:00:02".parse().unwrap());
    assert_eq!(scene.device_count(), 2);

    // A few ticks: bring-up, advertise, route.
    for _ in 0..3 {
        scene.tick(0.1);
    }

    let reports = scene.scanner_reports_json(scanner);
    assert!(
        reports.contains("SceneHRM"),
        "scanner should have seen the peripheral by name; got {reports}"
    );
    // The peripheral's own GATT status is available for a server view.
    assert!(
        scene
            .peripheral_status_json(0)
            .unwrap()
            .contains("SceneHRM")
    );
}

#[test]
fn test_central_connects_and_discovers_peripheral() {
    let mut scene = SceneEngine::new();
    let script = r#"
            let server = android::BluetoothGattServer("SceneHRM");
            let hrs = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let hr = android::BluetoothGattCharacteristic(
                uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ,
            );
            hr.set_value([0x00, 72]);
            hrs.add_characteristic(hr);
            server.add_service(hrs);
        "#;
    let peripheral_addr = "AA:BB:CC:00:00:01".parse().unwrap();
    scene.add_peripheral(peripheral_addr, script).unwrap();
    let central = scene.add_central("AA:BB:CC:00:00:02".parse().unwrap(), peripheral_addr);

    // Connect + MTU + service/characteristic discovery is a handful of
    // round-trips; each takes a couple of ticks through the Link.
    for _ in 0..40 {
        scene.tick(0.1);
    }

    let json = scene.central_status_json(central).unwrap();
    // The central discovered the Heart Rate service and its measurement
    // characteristic on the peer — two real devices, connected in-process.
    assert!(
        json.contains("\"connected\":true"),
        "central should be connected; got {json}"
    );
    assert!(
        json.contains("180D"),
        "should discover Heart Rate service; got {json}"
    );
    assert!(
        json.contains("2A37"),
        "should discover HR Measurement char; got {json}"
    );
}

#[test]
fn test_run_test_script_pass_fail_and_compile_error() {
    // A passing assertion.
    assert!(
        run_test_script(
            "let s = android::BluetoothGattServer(\"t\"); assert(s.name == \"t\", \"name\");"
        )
        .is_ok()
    );
    // A failing assertion surfaces its message.
    let err = run_test_script("assert(1 == 2, \"one is not two\");").unwrap_err();
    assert!(
        err.contains("one is not two") || err.to_lowercase().contains("assert"),
        "got {err}"
    );
    // A compile error is reported as such.
    assert!(run_test_script("@@ not rhai @@").is_err());
}
