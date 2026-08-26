use super::*;
use crate::att::opcode;
use crate::l2cap::AclPacketBoundary;
use crate::l2cap::{L2capHeader, cid};
use zerocopy::IntoBytes;

fn drain_host_packets(channel: &HciChannel) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(packet) = channel.poll_host_packet() {
        out.push(packet);
    }
    out
}

fn opcode_of(command: &[u8]) -> u16 {
    assert_eq!(command[0], h4_type::HCI_COMMAND);
    u16::from_le_bytes([command[1], command[2]])
}

#[test]
fn test_scanner_start_queues_reset_masks_params_enable() {
    let channel = HciChannel::new();
    queue_scanner_start(&channel).unwrap();
    let commands = drain_host_packets(&channel);
    let opcodes: Vec<u16> = commands.iter().map(|c| opcode_of(c)).collect();
    assert_eq!(opcodes, vec![0x0C03, 0x0C01, 0x2001, 0x200B, 0x200C]);
    // Both event masks fully open (LE Meta Events are masked by default).
    // Event_Mask stops at bit 61: 62-63 are reserved and setting them gets
    // the whole command rejected by real hardware.
    assert_eq!(
        &commands[1][4..12],
        &crate::device::host::EVENT_MASK_ALL[..]
    );
    assert_eq!(&commands[2][4..12], &[0xFF; 8]);
    // Active scanning (scan-type byte 0x01) so advertisers' scan-response
    // data (names) is solicited via SCAN_REQ, not just passively observed.
    assert_eq!(commands[3][4], 0x01);
    // Scan enable with duplicate filtering off.
    assert_eq!(&commands[4][4..], &[0x01, 0x00]);
}

/// Builds one LE Advertising Report event around `data`.
fn adv_report_event(event_type: u8, data: &[u8], rssi: i8) -> Vec<u8> {
    let mut packet = vec![
        h4_type::HCI_EVENT,
        hci_event_code::LE_META,
        (12 + data.len()) as u8, // subevent, count, 9-byte report header, data, RSSI
        le_subevent::ADVERTISING_REPORT,
        0x01,       // one report
        event_type, // ADV_IND etc.
        0x00,       // public address
        0x01,
        0x02,
        0x03,
        0x04,
        0x05,
        0x06, // address (little-endian)
        data.len() as u8,
    ];
    packet.extend_from_slice(data);
    packet.push(rssi as u8);
    packet
}

/// Everything a device can put in an advertisement must survive being
/// encoded and decoded again. simble owns both halves — `AdvertisingData`
/// builds and `parse_scan_reports` decodes — so this round trip is the
/// cheapest guard there is against a field being silently dropped on the
/// way out. `advertise_service_uuid` shipped broken precisely because
/// nothing checked the encoder against the decoder.
#[test]
fn test_every_advertised_field_survives_the_round_trip() {
    let mut extras = AdvertisingData::new();
    extras.service_uuids_16.push(0x185B); // staged by advertise_service_uuid
    extras
        .service_data_16
        .push((0xFE2C, vec![0x00, 0x11, 0x22]));
    extras = extras.with_manufacturer_data(0x00E0, &[0xAB]);

    let payload = build_adv_payload_with_extras("Ranger", &[0x180F], Some(&extras))
        .expect("fits in 31 bytes");
    let reports = parse_scan_reports(&adv_report_event(0x00, &payload, -55));
    assert_eq!(reports.len(), 1);
    let report = &reports[0];

    assert_eq!(report.name.as_deref(), Some("Ranger"), "name survives");
    assert!(
        report.service_uuids.iter().any(|u| u == "180F"),
        "the device's own service survives: {:?}",
        report.service_uuids
    );
    assert!(
        report.service_uuids.iter().any(|u| u == "185B"),
        "a staged service UUID survives: {:?}",
        report.service_uuids
    );
    assert!(
        report
            .service_data
            .iter()
            .any(|d| d.tag == "FE2C" && d.data == "001122"),
        "service data survives: {:?}",
        report.service_data
    );
    let mfg = report
        .manufacturer_data
        .as_ref()
        .expect("manufacturer data");
    assert_eq!((mfg.tag.as_str(), mfg.data.as_str()), ("00E0", "AB"));
    assert!(report.flags.is_some(), "flags survive");
}

#[test]
fn test_parse_scan_reports_decodes_ad_structures() {
    let mut data = vec![0x02, ad_type::FLAGS, 0x06];
    data.extend_from_slice(&[0x08, ad_type::COMPLETE_LOCAL_NAME]);
    data.extend_from_slice(b"web-hrm");
    data.extend_from_slice(&[0x03, ad_type::COMPLETE_16BIT_UUIDS, 0x0D, 0x18]);
    data.extend_from_slice(&[
        0x05,
        ad_type::MANUFACTURER_SPECIFIC_DATA,
        0xE0,
        0x00,
        0xAB,
        0xCD,
    ]);
    let packet = adv_report_event(0x00, &data, -42);

    let reports = parse_scan_reports(&packet);
    assert_eq!(reports.len(), 1);
    let report = &reports[0];
    assert_eq!(report.address, "06:05:04:03:02:01");
    assert_eq!(report.address_type, "public");
    assert!(report.connectable);
    assert!(!report.scan_response);
    assert_eq!(report.rssi, -42);
    assert_eq!(report.name.as_deref(), Some("web-hrm"));
    assert_eq!(report.flags, Some(0x06));
    assert_eq!(report.service_uuids, vec!["180D".to_string()]);
    let manufacturer = report.manufacturer_data.as_ref().unwrap();
    assert_eq!(manufacturer.tag, "00E0");
    assert_eq!(manufacturer.data, "ABCD");
}

/// A scanner must surface AD type 0x2E, or a set member is discoverable
/// and still not identifiable as a member — the crypto reaches the air and
/// stops at the antenna.
///
/// The payload here is written out as literal octets from CSIS (Section
/// 4.9 for the layout, Appendix A for the values: SIRK
/// `457d7d0921a1fd22cecd8c86dd72cccd`, prand `0x69f563`, hash `0x1948da`)
/// rather than built with `csip::rsi`, so a decoder keyed to the wrong AD
/// type or reading the halves backwards cannot agree with a builder that
/// makes the same mistake.
#[test]
fn test_a_scan_report_decodes_a_resolvable_set_identifier() {
    let mut data = vec![0x02, ad_type::FLAGS, 0x06];
    // Length 7, AD type 0x2E, then hash 1948DA and prand 69F563, each
    // written least significant octet first.
    data.extend_from_slice(&[0x07, 0x2E, 0xDA, 0x48, 0x19, 0x63, 0xF5, 0x69]);
    let reports = parse_scan_reports(&adv_report_event(0x00, &data, -50));
    let report = &reports[0];
    assert_eq!(
        report.resolvable_set_identifier.as_deref(),
        Some("DA481963F569"),
        "six octets: hash 1948DA then prand 69F563, each reversed for the wire"
    );
    // And it resolves against the SIRK those octets were generated from.
    let sirk = crate::crypto::smp_crypto::rev(&[
        0x45, 0x7d, 0x7d, 0x09, 0x21, 0xa1, 0xfd, 0x22, 0xce, 0xcd, 0x8c, 0x86, 0xdd, 0x72, 0xcc,
        0xcd,
    ]);
    assert!(crate::profiles::csip::rsi_matches(
        &sirk,
        &[0xDA, 0x48, 0x19, 0x63, 0xF5, 0x69]
    ));
}

#[test]
fn test_a_set_member_advertisement_reaches_the_scanner_intact() {
    // The end-to-end path the earbud catalog device takes: script stages
    // an RSI, the host builds the payload, the scanner reads it back.
    let sirk = [
        0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A, 0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03,
        0xB8,
    ];
    let identifier = crate::profiles::csip::rsi(&sirk, &[0x69, 0xF5, 0x63]);
    let mut extras = AdvertisingData::new();
    extras.service_uuids_16.push(0x1846);
    extras.resolvable_set_identifier = Some(identifier.to_vec());

    let payload =
        build_adv_payload_with_extras("Earbud L", &[], Some(&extras)).expect("fits in 31");
    let reports = parse_scan_reports(&adv_report_event(0x00, &payload, -60));
    let report = &reports[0];

    assert_eq!(report.name.as_deref(), Some("Earbud L"));
    assert!(report.service_uuids.iter().any(|u| u == "1846"));
    let seen = report
        .resolvable_set_identifier
        .as_deref()
        .expect("the scanner saw the set identity");
    assert_eq!(seen, hex(&identifier));
    let decoded: Vec<u8> = (0..6)
        .map(|i| u8::from_str_radix(&seen[i * 2..i * 2 + 2], 16).unwrap())
        .collect();
    assert!(
        crate::profiles::csip::rsi_matches(&sirk, &decoded),
        "a coordinator holding the SIRK recognises the member"
    );
}

#[test]
fn test_a_128_bit_service_uuid_survives_the_builder_and_the_scanner() {
    // The builder half only landed after the scanner could already read
    // 0x06/0x07, so this is the first time both ends are exercised
    // together. `Uuid`'s Display reverses the octets, so a report that
    // shows the textual UUID proves the payload carried the little-endian
    // form.
    let uuid = [
        0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x2C, 0xFE, 0x00,
        0x00,
    ];
    let extras = AdvertisingData::new().with_service_uuid_128(uuid);
    let payload = build_adv_payload_with_extras("Custom", &[], Some(&extras)).expect("fits");
    assert!(
        payload.windows(17).any(|w| w[0] == 0x07 && w[1..] == uuid),
        "AD type 0x07 followed by the UUID least significant octet first"
    );
    let reports = parse_scan_reports(&adv_report_event(0x00, &payload, -60));
    assert_eq!(
        reports[0].service_uuids,
        vec!["0000fe2c-0000-1000-8000-00805f9b34fb".to_string()]
    );
}

#[test]
fn test_parse_scan_reports_ignores_other_packets() {
    assert!(
        parse_scan_reports(&[h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00]).is_empty()
    );
    assert!(parse_scan_reports(&[h4_type::HCI_ACL_DATA, 0x40, 0x00, 0x00, 0x00]).is_empty());
    // Truncated report body: no panic, no report.
    assert!(
        parse_scan_reports(&[
            h4_type::HCI_EVENT,
            hci_event_code::LE_META,
            0x04,
            le_subevent::ADVERTISING_REPORT,
            0x01,
            0x00,
            0x00
        ])
        .is_empty()
    );
}

#[test]
fn test_build_adv_payload_fits_31_bytes_and_keeps_name() {
    let payload = build_adv_payload("web-hrm", &[0x180D]).expect("fits");
    assert!(payload.len() <= 31);
    assert!(payload.windows(7).any(|w| w == b"web-hrm"));
    assert!(payload.windows(2).any(|w| w == [0x0D, 0x18]));

    // An oversized name drops the UUID list and trims, never overflows.
    let long = "a-device-name-well-past-the-thirty-one-byte-advertising-limit";
    let payload = build_adv_payload(long, &[0x180D, 0x180F, 0x1812]).expect("trims to fit");
    assert!(payload.len() <= 31);
    assert!(!payload.windows(2).any(|w| w == [0x0D, 0x18]));
    assert!(payload.windows(9).any(|w| w == b"a-device-"));
}

fn le_connection_complete(handle: u16, peer_le: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![
        h4_type::HCI_EVENT,
        hci_event_code::LE_META,
        19,
        le_subevent::CONNECTION_COMPLETE,
        0x00, // status
    ];
    packet.extend_from_slice(&handle.to_le_bytes());
    packet.push(0x01); // role: peripheral
    packet.push(0x00); // peer address type
    packet.extend_from_slice(&peer_le);
    packet.extend_from_slice(&[0x28, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]); // interval etc.
    packet
}

fn acl_packet(handle: u16, l2cap: &[u8]) -> Vec<u8> {
    let mut packet = vec![h4_type::HCI_ACL_DATA];
    packet.extend_from_slice(
        HciAclHeader::new(
            handle,
            AclPacketBoundary::FirstAutoFlushable,
            l2cap.len() as u16,
        )
        .as_bytes(),
    );
    packet.extend_from_slice(l2cap);
    packet
}

/// Runs the shipped default script and walks the full peripheral life
/// cycle a browser session would: start (advertising bring-up), connect,
/// subscribe via a real CCCD write, script tick driving a real
/// notification, then disconnect and re-advertise.
#[test]
fn test_default_script_full_lifecycle() {
    let mut peripheral =
        ScriptedPeripheral::run_script(DEFAULT_HEART_RATE_SCRIPT).expect("default script runs");
    assert_eq!(peripheral.device_name(), "web-thermometer");
    assert!(peripheral.tick_defined);

    let channel = HciChannel::new();
    peripheral.queue_start(&channel).unwrap();
    let commands = drain_host_packets(&channel);
    let opcodes: Vec<u16> = commands.iter().map(|c| opcode_of(c)).collect();
    // 0x2074 (LE Set Host Feature) declares CIS host support, without
    // which a controller refuses to open an isochronous stream. 0x2005
    // (LE Set Random Address) precedes the advertising parameters because a
    // scripted peripheral names the address it advertises from, and only a
    // random address can be set — a controller's public address is its
    // silicon's.
    assert_eq!(
        opcodes,
        vec![
            0x0C03, 0x0C01, 0x2001, 0x2074, 0x2005, 0x2006, 0x2008, 0x2009, 0x200A
        ]
    );
    // Advertising data carries the script device's name and the
    // Environmental Sensing service UUID (0x181A) the script declared.
    // Found by opcode, not by position: this indexed commands[5] and broke
    // the day a command was inserted before it, which says nothing about
    // advertising data.
    let adv_data = commands
        .iter()
        .find(|c| opcode_of(c) == 0x2008)
        .expect("LE Set Advertising Data was queued");
    assert!(adv_data.windows(15).any(|w| w == b"web-thermometer"));
    assert!(adv_data.windows(2).any(|w| w == [0x1A, 0x18]));

    // Central connects.
    let peer = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    peripheral
        .handle_packet(&channel, &le_connection_complete(0x0040, peer))
        .unwrap();
    let status = peripheral.status_json();
    assert!(status.contains("\"connected\":true"));

    // Central subscribes: real ATT write to the CCCD the script added.
    let watch = peripheral.watched[0].clone();
    let cccd = watch.cccd_handle.expect("script added a CCCD");
    let mut write = vec![opcode::WRITE_REQ];
    write.extend_from_slice(&cccd.to_le_bytes());
    write.extend_from_slice(&[0x01, 0x00]);
    peripheral
        .handle_packet(
            &channel,
            &acl_packet(0x0040, &L2capHeader::serialize(cid::ATT, &write)),
        )
        .unwrap();
    // The Write Response went out as ACL data.
    let responses = drain_host_packets(&channel);
    assert!(
        responses
            .iter()
            .any(|p| p[0] == h4_type::HCI_ACL_DATA && p.ends_with(&[opcode::WRITE_RSP]))
    );
    assert_eq!(
        peripheral.cccd_subscription(&watch),
        CccdSubscription::Notify
    );

    // Script tick updates the temperature, which becomes a notification.
    peripheral.tick(&channel, 2.0).unwrap();
    assert!(
        peripheral.last_error.is_none(),
        "{:?}",
        peripheral.last_error
    );
    let packets = drain_host_packets(&channel);
    let notification = packets
        .iter()
        .find(|p| p[0] == h4_type::HCI_ACL_DATA && p.contains(&opcode::HANDLE_VALUE_NTF))
        .expect("tick produced a notification");
    // Temperature (0x2A6E): a signed 16-bit little-endian value in
    // hundredths of a degree C — the last two bytes of the notification.
    let value = &notification[notification.len() - 2..];
    let centi = i16::from_le_bytes([value[0], value[1]]);
    assert!((2000..=2300).contains(&centi), "centi {centi}");

    // Same value on the next tick at the same t: no duplicate notification.
    peripheral.tick(&channel, 2.0).unwrap();
    assert!(drain_host_packets(&channel).is_empty());

    // Disconnect: state clears and advertising is re-enabled.
    let disconnect = vec![
        h4_type::HCI_EVENT,
        hci_event_code::DISCONNECTION_COMPLETE,
        4,
        0x00,
        0x40,
        0x00,
        0x13,
    ];
    peripheral.handle_packet(&channel, &disconnect).unwrap();
    assert!(peripheral.status_json().contains("\"connected\":false"));
    let commands = drain_host_packets(&channel);
    assert_eq!(commands.len(), 1);
    assert_eq!(opcode_of(&commands[0]), 0x200A);
    assert_eq!(commands[0][4], 0x01);
}

#[test]
fn test_script_errors_surface_as_strings() {
    let compile_error = ScriptedPeripheral::run_script("let x = ;")
        .err()
        .expect("syntax error");
    assert!(!compile_error.is_empty());

    let no_server = ScriptedPeripheral::run_script("let x = 1 + 1;")
        .err()
        .expect("no server created");
    assert!(no_server.contains("BluetoothGattServer"));

    // A broken tick doesn't kill the device — the error is recorded.
    let script = r#"
            let server = android::BluetoothGattServer("web-hrm");
            fn tick(server, t) { nonexistent_function(); }
        "#;
    let mut peripheral = ScriptedPeripheral::run_script(script).unwrap();
    let channel = HciChannel::new();
    peripheral.tick(&channel, 0.1).unwrap();
    assert!(peripheral.last_error.is_some());
    assert!(peripheral.status_json().contains("last_error"));
}

#[test]
fn test_update_value_extension_writes_the_real_database() {
    let script = r#"
            let server = android::BluetoothGattServer("dev");
            let svc = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);
            let chr = android::BluetoothGattCharacteristic(
                uuid::HEART_RATE_MEASUREMENT,
                android::PROPERTY_READ | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ,
            );
            chr.set_value([0x00, 60]);
            svc.add_characteristic(chr);
            server.add_service(svc);
            server.update_value(uuid::HEART_RATE_MEASUREMENT, [0x00, 99]);
        "#;
    let peripheral = ScriptedPeripheral::run_script(script).unwrap();
    let watch = &peripheral.watched[0];
    assert_eq!(peripheral.attribute_value(watch).unwrap(), vec![0x00, 99]);
}

#[test]
fn test_status_json_reports_characteristic_properties() {
    // The generic viewer renders R/W/N/I chips from the raw property
    // bitmask, so the status snapshot must expose it per characteristic.
    let peripheral =
        ScriptedPeripheral::run_script(DEFAULT_HEART_RATE_SCRIPT).expect("default script runs");
    let status = peripheral.status_json();
    assert!(status.contains("\"properties\":"), "{status}");
    // The Temperature characteristic is READ (0x02) | NOTIFY (0x10) = 0x12.
    let expected = (BluetoothGattCharacteristic::PROPERTY_READ
        | BluetoothGattCharacteristic::PROPERTY_NOTIFY) as i64;
    assert!(
        status.contains(&format!("\"properties\":{expected}")),
        "{status}"
    );
}

#[test]
fn test_session_builds_device_incrementally() {
    // The API Explorer's model: one Rhai statement per Execute, with
    // `let`-bound objects (svc1, chr1, …) persisting in the shared scope
    // and usable by later Executes.
    let mut session = ScriptedPeripheral::new_session();
    assert!(!session.has_server());
    assert!(session.status_json().contains("\"services\":[]"));

    // A `let` binding returns unit and produces no events.
    let outcome = session
        .eval_line(r#"let server = android::BluetoothGattServer("explorer");"#)
        .unwrap();
    assert_eq!(outcome.value, "()");
    assert!(outcome.events.is_empty());
    assert!(session.has_server());
    assert_eq!(session.device_name(), "explorer");

    session
            .eval_line(
                r#"let svc1 = android::BluetoothGattService(uuid::HEART_RATE_SERVICE, android::SERVICE_TYPE_PRIMARY);"#,
            )
            .unwrap();
    session
            .eval_line(
                r#"let chr1 = android::BluetoothGattCharacteristic(uuid::HEART_RATE_MEASUREMENT, android::PROPERTY_READ | android::PROPERTY_NOTIFY, android::PERMISSION_READ);"#,
            )
            .unwrap();
    // svc1 and chr1 survive from earlier Executes in the shared scope.
    session.eval_line("svc1.add_characteristic(chr1);").unwrap();
    // add_service fires the on_service_added callback -> a session event.
    let added = session.eval_line("server.add_service(svc1);").unwrap();
    assert!(
        added.events.iter().any(|e| e.contains("service_added")),
        "events: {:?}",
        added.events
    );

    let status = session.status_json();
    assert!(status.contains("180D"), "{status}"); // Heart Rate service
    assert!(status.contains("2A37"), "{status}"); // HR Measurement char

    // A returned expression renders its value (get_service -> a service).
    let got = session
        .eval_line("server.get_service(uuid::HEART_RATE_SERVICE)")
        .unwrap();
    assert!(!got.value.is_empty());

    // The built device is hostable: advertising bring-up carries its name.
    assert!(session.has_server());
    let channel = HciChannel::new();
    session.queue_start(&channel).unwrap();
    let commands = drain_host_packets(&channel);
    // By opcode rather than position, for the same reason as above.
    let adv_data = commands
        .iter()
        .find(|c| opcode_of(c) == 0x2008)
        .expect("LE Set Advertising Data was queued");
    assert!(adv_data.windows(8).any(|w| w == b"explorer"));
}

#[test]
fn test_set_characteristic_value_host_write() {
    // The lightbulb page's colour picker writes a custom 128-bit "colour"
    // characteristic from the host side; the write must land in the live
    // database (and thus be visible to the viewer and notifiable).
    let script = r#"
            let server = android::BluetoothGattServer("web-lightbulb");
            let svc = android::BluetoothGattService(
                uuid::of("f0ff0001-1234-5678-90ab-cdef01234567"),
                android::SERVICE_TYPE_PRIMARY,
            );
            let color = android::BluetoothGattCharacteristic(
                uuid::of("f0ff0002-1234-5678-90ab-cdef01234567"),
                android::PROPERTY_READ | android::PROPERTY_WRITE | android::PROPERTY_NOTIFY,
                android::PERMISSION_READ | android::PERMISSION_WRITE,
            );
            color.set_value([0x33, 0xcc, 0xff]);
            let cccd = android::BluetoothGattDescriptor(
                uuid::CLIENT_CHARACTERISTIC_CONFIGURATION,
                android::PERMISSION_READ | android::PERMISSION_WRITE,
            );
            color.add_descriptor(cccd);
            svc.add_characteristic(color);
            server.add_service(svc);
        "#;
    let mut peripheral = ScriptedPeripheral::run_script(script).unwrap();
    peripheral
        .set_characteristic_value("f0ff0002-1234-5678-90ab-cdef01234567", &[0xff, 0x00, 0x00])
        .unwrap();
    let status = peripheral.status_json();
    assert!(status.contains("FF0000"), "{status}");
    // A bad UUID is a clean error, not a panic.
    assert!(peripheral.set_characteristic_value("nope", &[0]).is_err());
    // A UUID with no matching characteristic errors too.
    assert!(peripheral.set_characteristic_value("2A19", &[0]).is_err());
}

#[test]
fn test_session_eval_error_surfaces_as_json() {
    let mut session = ScriptedPeripheral::new_session();
    // A runtime error comes back as ok:false with the message, not a panic.
    let json = session.eval_line_json("nonexistent_function(1, 2)");
    assert!(json.contains("\"ok\":false"), "{json}");
    assert!(json.contains("\"error\":"), "{json}");
    // The session is still usable afterwards.
    let json = session.eval_line_json(r#"let server = android::BluetoothGattServer("ok");"#);
    assert!(json.contains("\"ok\":true"), "{json}");
}

#[test]
fn test_demo_advertiser_bring_up() {
    // The scanner page's self-spun demo devices advertise via this path;
    // verify the HCI sequence and that the payload carries the name,
    // service UUID, and manufacturer data.
    let channel = HciChannel::new();
    queue_advertiser_start(
        &channel,
        "Simble Beacon",
        0x180F,
        0x0059,
        &[0x01, 0x02, 0x03, 0x04],
    )
    .unwrap();
    let commands = drain_host_packets(&channel);
    let opcodes: Vec<u16> = commands.iter().map(|c| opcode_of(c)).collect();
    // 0x2074 (LE Set Host Feature) declares CIS host support, without
    // which a controller refuses to open an isochronous stream.
    assert_eq!(
        opcodes,
        vec![0x0C03, 0x0C01, 0x2001, 0x2006, 0x2008, 0x2009, 0x200A]
    );
    let adv_data = &commands[4];
    assert!(adv_data.windows(13).any(|w| w == b"Simble Beacon"));
    assert!(adv_data.windows(2).any(|w| w == [0x0F, 0x18])); // service 0x180F
    assert!(adv_data.windows(4).any(|w| w == [0x01, 0x02, 0x03, 0x04])); // mfg data
    // Enable advertising is the last command, with the enable flag set.
    assert_eq!(commands[6][4], 0x01);
}

#[test]
fn test_demo_adv_payload_trims_to_limit() {
    // A very long name still yields a legal (<= 31-byte) payload.
    let long = "a-demo-advertiser-name-well-past-the-legacy-advertising-limit";
    let payload = build_demo_adv_payload(long, 0x181A, 0, &[]).expect("trims to fit");
    assert!(payload.len() <= 31, "payload {} bytes", payload.len());
}
