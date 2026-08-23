// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The profile bindings a script composes a device out of.
//!
//! A binding is only worth having if it wires up the *behaviour* in
//! `src/profiles/` — the state machine with the tests — rather than laying down
//! an inert set of attributes a script could have written itself. So these
//! tests do not stop at "the service is in the database": each one drives a
//! real ATT write at the device the script built and checks that the write was
//! validated, applied, and republished the way the Rust profile does it.
//!
//! That distinction is not theoretical. Before this, two catalog entries
//! reimplemented the Volume Control Point in Rhai by polling it from `tick`,
//! and neither could reject a stale change counter — Rhai has no way to fail an
//! ATT write. `stale_change_counter_is_rejected_through_a_script_built_device`
//! is the test that version could not pass.

use simble::att::opcode as att_opcode;
use simble::l2cap::{L2capHeader, cid};
use simble::profiles::csip;
use simble::profiles::vcp;
use simble::transport::wasm_ws::{ScriptedPeripheral, build_adv_payload_with_extras};
use simble::types::{Address, Uuid};

/// Builds the device a script describes and hands back its GATT server.
fn peripheral(script: &str) -> ScriptedPeripheral {
    ScriptedPeripheral::run_script(script).expect("script runs")
}

/// The error a script is expected to fail with. `ScriptedPeripheral` is not
/// `Debug`, so `expect_err` is not available.
fn script_error(script: &str) -> String {
    match ScriptedPeripheral::run_script(script) {
        Ok(_) => panic!("expected the script to be rejected, but it ran"),
        Err(e) => e,
    }
}

/// True if the device's database contains a characteristic with this UUID.
fn has_characteristic(p: &ScriptedPeripheral, uuid16: u16) -> bool {
    p.primary_server()
        .expect("script built a server")
        .with_server(|s| {
            s.device
                .gatt_db
                .value_handle_for_uuid(Uuid::Uuid16(uuid16))
                .is_some()
        })
}

fn value_handle(p: &ScriptedPeripheral, uuid16: u16) -> u16 {
    p.primary_server()
        .expect("script built a server")
        .with_server(|s| {
            s.device
                .gatt_db
                .value_handle_for_uuid(Uuid::Uuid16(uuid16))
                .unwrap_or_else(|| panic!("no characteristic {uuid16:#06X}"))
        })
}

fn read(p: &ScriptedPeripheral, uuid16: u16) -> Vec<u8> {
    let handle = value_handle(p, uuid16);
    p.primary_server()
        .expect("script built a server")
        .with_server(|s| s.device.gatt_db.value(handle).expect("value").to_vec())
}

/// Drives a real ATT Write Request at the device, through the same path a
/// connected central's write takes, and returns the ATT error code if it was
/// rejected.
fn att_write(p: &ScriptedPeripheral, uuid16: u16, payload: &[u8]) -> Result<(), u8> {
    let handle = value_handle(p, uuid16);
    let server = p.primary_server().expect("script built a server");
    server.with_server(|s| {
        let conn = 0x0040;
        s.device.on_connected(conn, Address::ANY);
        let mut req = vec![att_opcode::WRITE_REQ];
        req.extend_from_slice(&handle.to_le_bytes());
        req.extend_from_slice(payload);
        let l2cap = L2capHeader::serialize(cid::ATT, &req);
        let resp = s
            .device
            .process_l2cap_packet(conn, &l2cap)
            .expect("packet handled")
            .expect("a response");
        let (_, att) = L2capHeader::parse(&resp).expect("l2cap");
        if att[0] == att_opcode::ERROR_RSP {
            // [opcode, req_opcode, handle_lo, handle_hi, error_code]
            return Err(att[4]);
        }
        assert_eq!(att[0], att_opcode::WRITE_RSP);
        Ok(())
    })
}

// ---- Volume Control: android::BluetoothVolumeControl ----------------------

const VOLUME_SCRIPT: &str = r#"
let server = android::BluetoothGattServer("Speaker");
server.add_vcs(128, 0);
server.add_vocs(0x01, "Left");
server.add_aics(0, 255, "Line In");
"#;

#[test]
fn add_vcs_registers_the_volume_control_service() {
    let p = peripheral(VOLUME_SCRIPT);
    assert!(has_characteristic(&p, 0x2B7D), "Volume State");
    assert!(has_characteristic(&p, 0x2B7E), "Volume Control Point");
    assert!(has_characteristic(&p, 0x2B7F), "Volume Flags");
    assert_eq!(read(&p, 0x2B7D), vec![128, 0, 0]);
}

#[test]
fn add_vocs_and_add_aics_register_their_services() {
    let p = peripheral(VOLUME_SCRIPT);
    assert!(has_characteristic(&p, 0x2B80), "Volume Offset State");
    assert!(
        has_characteristic(&p, 0x2B82),
        "Volume Offset Control Point"
    );
    assert!(has_characteristic(&p, 0x2B77), "Audio Input State");
    assert!(has_characteristic(&p, 0x2B7B), "Audio Input Control Point");
}

// The whole reason the binding exists: a peer's write reaches the Rust state
// machine, not the control point's stored bytes.
#[test]
fn a_volume_control_point_write_drives_the_state_machine() {
    let p = peripheral(VOLUME_SCRIPT);

    // Set Absolute Volume to 200, with the current change counter (0).
    assert_eq!(
        att_write(&p, 0x2B7E, &[vcp::opcode::SET_ABSOLUTE_VOLUME, 0, 200]),
        Ok(())
    );
    // Applied, counter advanced, and republished so a subscriber sees it.
    assert_eq!(read(&p, 0x2B7D), vec![200, 0, 1]);
    // VCS 3.2.1.1 - the volume is now a user setting, not a power-on default.
    assert_eq!(read(&p, 0x2B7F), vec![vcp::VOLUME_SETTING_PERSISTED]);
}

// The test the Rhai reimplementation could not pass: it had no way to fail an
// ATT write, so it applied a command carrying a stale change counter that a
// real Volume Control Service rejects.
#[test]
fn stale_change_counter_is_rejected_through_a_script_built_device() {
    let p = peripheral(VOLUME_SCRIPT);

    // The counter is 0; claim it is 7.
    assert_eq!(
        att_write(&p, 0x2B7E, &[vcp::opcode::SET_ABSOLUTE_VOLUME, 7, 200]),
        Err(vcp::error_code::INVALID_CHANGE_COUNTER)
    );
    // Nothing moved.
    assert_eq!(read(&p, 0x2B7D), vec![128, 0, 0]);
}

#[test]
fn an_unsupported_volume_opcode_is_rejected() {
    let p = peripheral(VOLUME_SCRIPT);
    assert_eq!(
        att_write(&p, 0x2B7E, &[0x7F, 0]),
        Err(vcp::error_code::OPCODE_NOT_SUPPORTED)
    );
}

#[test]
fn a_vocs_offset_write_drives_its_own_state_machine() {
    let p = peripheral(VOLUME_SCRIPT);
    // Set Volume Offset: [opcode, change_counter, offset(2, LE signed)]
    assert_eq!(att_write(&p, 0x2B82, &[0x01, 0, 0x0A, 0x00]), Ok(()));
    assert_eq!(read(&p, 0x2B80), vec![0x0A, 0x00, 1]);
    // And it keeps its own counter, independent of VCS's.
    assert_eq!(
        att_write(&p, 0x2B82, &[0x01, 0, 0x0B, 0x00]),
        Err(vcp::error_code::INVALID_CHANGE_COUNTER)
    );
}

#[test]
fn add_vcs_rejects_a_volume_that_is_not_a_byte() {
    let err =
        script_error(r#"let server = android::BluetoothGattServer("x"); server.add_vcs(300, 0);"#);
    assert!(err.contains("not a byte"), "{err}");
}

// ---- Coordinated sets: android::BluetoothCsipSetCoordinator --------------

const EARBUD_SCRIPT: &str = r#"
let server = android::BluetoothGattServer("Earbud L");
let sirk = [0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A,
            0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03, 0xB8];
server.add_csis(sirk, 2, 1);
server.advertise_set_identity(sirk, [0x69, 0xF5, 0x63]);
"#;

#[test]
fn add_csis_registers_the_set_and_its_rank() {
    let p = peripheral(EARBUD_SCRIPT);
    // SIRK is published with a leading 0x01 "plaintext" type octet.
    assert_eq!(read(&p, 0x2B84)[0], 0x01);
    assert_eq!(read(&p, 0x2B85), vec![2]); // set size
    assert_eq!(read(&p, 0x2B87), vec![1]); // rank
}

// The advertised Resolvable Set Identifier must actually resolve against the
// SIRK the script provisioned — that is what makes the member findable as a
// member, and it is the first non-test caller `csip::sih` has ever had.
#[test]
fn the_advertised_set_identity_resolves_against_the_sirk() {
    let p = peripheral(EARBUD_SCRIPT);
    let rsi = p
        .primary_server()
        .expect("server")
        .with_server(|s| {
            s.device
                .advertising_data
                .as_ref()
                .and_then(|ad| ad.resolvable_set_identifier.clone())
        })
        .expect("the script staged an RSI");

    assert_eq!(rsi.len(), 6, "hash(3) || prand(3), CSIS Section 4.9");
    let sirk: [u8; 16] = [
        0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A, 0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03,
        0xB8,
    ];
    assert!(
        csip::rsi_matches(&sirk, &rsi),
        "resolves with the right SIRK"
    );

    let other = [0xFFu8; 16];
    assert!(!csip::rsi_matches(&other, &rsi), "and not with a wrong one");
}

/// The set identity has to survive the whole build, not just the binding call:
/// this reads the payload the host would hand the controller and looks for AD
/// type 0x2E in it.
#[test]
fn the_set_identity_reaches_the_advertising_payload() {
    let p = peripheral(EARBUD_SCRIPT);
    let payload = p.primary_server().expect("server").with_server(|s| {
        build_adv_payload_with_extras(&s.device.name, &[], s.device.advertising_data.as_ref())
            .expect("the payload fits in 31 bytes")
    });
    let sirk: [u8; 16] = [
        0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A, 0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03,
        0xB8,
    ];
    let advertised =
        simble::gap::resolvable_set_identifier(&payload).expect("0x2E is in the payload");
    assert!(csip::rsi_matches(&sirk, advertised));
}

/// The flagship scenario: two catalog devices, one set.
///
/// `earbud` alone proved the crypto could reach the air. A set of one is not a
/// set — this is the test that says a coordinator can pick *both* halves of a
/// pair out of the air and tell them apart by rank, which is what CSIP exists
/// to do.
#[test]
fn the_two_earbud_catalog_devices_are_one_coordinated_set() {
    let script_for = |name: &str| {
        simble::devices::catalog::EXAMPLES
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("catalog has no {name}"))
            .script
    };
    let left = peripheral(script_for("earbud"));
    let right = peripheral(script_for("earbud_right"));

    let sirk: [u8; 16] = [
        0x83, 0x8E, 0x68, 0x05, 0x53, 0xF1, 0x41, 0x5A, 0xA2, 0x65, 0xBB, 0xAF, 0xC6, 0xEA, 0x03,
        0xB8,
    ];

    // Same set size, distinct ranks — a coordinator locks members in rank
    // order, so two members claiming rank 1 would deadlock a real client.
    assert_eq!(read(&left, 0x2B85), vec![2], "left says the set has 2");
    assert_eq!(read(&right, 0x2B85), vec![2], "right agrees");
    assert_eq!(read(&left, 0x2B87), vec![1]);
    assert_eq!(read(&right, 0x2B87), vec![2]);

    // Same SIRK, published plaintext by both.
    assert_eq!(&read(&left, 0x2B84)[1..], &sirk);
    assert_eq!(&read(&right, 0x2B84)[1..], &sirk);

    let rsi_of = |p: &ScriptedPeripheral| {
        p.primary_server()
            .expect("server")
            .with_server(|s| {
                s.device
                    .advertising_data
                    .as_ref()
                    .and_then(|ad| ad.resolvable_set_identifier.clone())
            })
            .expect("staged an RSI")
    };
    let left_rsi = rsi_of(&left);
    let right_rsi = rsi_of(&right);

    // Both resolve against the one SIRK: that is "these two are a pair".
    assert!(csip::rsi_matches(&sirk, &left_rsi));
    assert!(csip::rsi_matches(&sirk, &right_rsi));
    // And they look nothing alike to anyone without it — a per-member prand is
    // the reason an RSI is not a stable tracker.
    assert_ne!(left_rsi, right_rsi, "different prand, different identifier");
    assert_ne!(
        &left_rsi[3..],
        &right_rsi[3..],
        "the random halves really do differ"
    );
    assert!(!csip::rsi_matches(&[0x00; 16], &left_rsi));
}

#[test]
fn a_rank_outside_the_set_is_rejected() {
    let err = script_error(
        r#"let server = android::BluetoothGattServer("x");
           server.add_csis([0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0], 2, 3);"#,
    );
    assert!(err.contains("rank"), "{err}");
}

#[test]
fn a_sirk_of_the_wrong_length_is_rejected() {
    let err = script_error(
        r#"let server = android::BluetoothGattServer("x");
           server.add_csis([1, 2, 3], 2, 1);"#,
    );
    assert!(err.contains("16 bytes"), "{err}");
}

// ---- Hearing Access: android::BluetoothHapClient -------------------------

const HEARING_AID_SCRIPT: &str = r#"
let server = android::BluetoothGattServer("Hearing Aid L");
server.add_has(0b10_0010, [
    #{ index: 1, name: "Universal", writable: true, available: true },
    #{ index: 2, name: "Restaurant", writable: true, available: true },
]);
"#;

#[test]
fn add_has_registers_presets_and_starts_on_the_lowest() {
    let p = peripheral(HEARING_AID_SCRIPT);
    assert_eq!(read(&p, 0x2BDA), vec![0b10_0010]); // Hearing Aid Features
    assert_eq!(read(&p, 0x2BDC), vec![1]); // Active Preset Index
    assert!(has_characteristic(&p, 0x2BDB), "Preset Control Point");
}

// `HearingAccessService::register` panics on an empty preset list. A panic in a
// binding takes the whole engine down, so the binding must turn it into a
// script error first.
#[test]
fn add_has_rejects_an_empty_preset_list_instead_of_panicking() {
    let err =
        script_error(r#"let server = android::BluetoothGattServer("x"); server.add_has(0, []);"#);
    assert!(err.contains("at least one preset"), "{err}");
}

#[test]
fn add_has_rejects_an_over_long_preset_name_instead_of_panicking() {
    let script = format!(
        r#"let server = android::BluetoothGattServer("x");
           server.add_has(0, [#{{ index: 1, name: "{}" }}]);"#,
        "x".repeat(41)
    );
    let err = script_error(&script);
    assert!(err.contains("1..=40 bytes"), "{err}");
}

// ---- Media Control -------------------------------------------------------

#[test]
fn add_gmcs_and_add_mcs_register_their_players() {
    let p = peripheral(
        r#"let server = android::BluetoothGattServer("Phone");
           server.add_gmcs("SimBLE Player", 1);"#,
    );
    assert!(has_characteristic(&p, 0x2B93), "Media Player Name");
    assert!(has_characteristic(&p, 0x2BA3), "Media State");
    assert!(has_characteristic(&p, 0x2BA4), "Media Control Point");
}

// A Media Control Point write must move the player, not land on stored bytes.
#[test]
fn a_media_control_point_write_changes_the_media_state() {
    let p = peripheral(
        r#"let server = android::BluetoothGattServer("Phone");
           server.add_gmcs("SimBLE Player", 1);"#,
    );
    // A freshly registered player is Paused (0x02), not Inactive: it has a
    // track loaded and is ready to play.
    assert_eq!(read(&p, 0x2BA3), vec![0x02], "starts Paused");
    // Opcode 0x01 = Play.
    assert_eq!(att_write(&p, 0x2BA4, &[0x01]), Ok(()));
    assert_eq!(read(&p, 0x2BA3), vec![0x01], "Playing");
    // Opcode 0x02 = Pause.
    assert_eq!(att_write(&p, 0x2BA4, &[0x02]), Ok(()));
    assert_eq!(read(&p, 0x2BA3), vec![0x02], "Paused");
}

// ---- The bindings that already existed, never covered --------------------

// `add_pacs`, `add_ascs` and `add_ras` shipped without a test of their own.
// This is the regression net the Volume bindings' neighbours were missing.
#[test]
fn the_pre_existing_bindings_still_register_their_services() {
    let p = peripheral(
        r#"let server = android::BluetoothGattServer("Sink");
           server.add_pacs(0x03, 0x00);
           server.add_ascs([0x01], []);
           server.add_ras();"#,
    );
    assert!(has_characteristic(&p, 0x2BC9), "Sink PAC");
    assert!(has_characteristic(&p, 0x2BC6), "ASE Control Point");
    assert!(has_characteristic(&p, 0x2C14), "RAS Features");
}

// ASCS Section 5 makes the ATT write itself succeed even when the individual
// ASE operations are rejected -- per-ASE outcomes ride the Control Point
// notification instead -- so the evidence that the handler ran is the ASE
// leaving Idle, not the ATT status.
#[test]
fn an_ase_control_point_write_still_drives_the_endpoint() {
    let p = peripheral(
        r#"let server = android::BluetoothGattServer("Sink");
           server.add_ascs([0x01], []);"#,
    );
    // Sink ASE 1 starts Idle (state 0x00), in [ase_id, state, ...].
    assert_eq!(read(&p, 0x2BC4)[..2], [1, 0x00]);

    // Config Codec: [opcode, num_ases, ase_id, target_latency, target_phy,
    //                codec_id(5), codec_config_len]
    let config_codec = [0x01, 1, 1, 3, 1, 0x06, 0, 0, 0, 0, 0];
    assert_eq!(att_write(&p, 0x2BC6, &config_codec), Ok(()));
    // 0x01 = Codec Configured: the write reached the state machine.
    assert_eq!(read(&p, 0x2BC4)[..2], [1, 0x01]);
}

// ---- Host-side writes must dispatch too ---------------------------------

// The page's volume buttons, the scene's `peripheral_set_value`, and anything
// else simulating an external write go through `set_characteristic_value`.
// That used `GattDatabase::set_value`, which stores bytes and bypasses any
// AttributeHandler -- so a control-point opcode was recorded and never
// executed. Every host-driven control point was inert: the Audio page's
// buttons moved nothing once the VCS state machine came from Rust instead of
// from a `tick` that polled the stored bytes.
#[test]
fn a_host_side_control_point_write_drives_the_state_machine() {
    let mut p = peripheral(VOLUME_SCRIPT);

    p.set_characteristic_value("2B7E", &[vcp::opcode::SET_ABSOLUTE_VOLUME, 0, 77])
        .expect("host write accepted");
    assert_eq!(read(&p, 0x2B7D), vec![77, 0, 1]);

    // And it is a real write, so it is validated like one.
    let stale = p.set_characteristic_value("2B7E", &[vcp::opcode::SET_ABSOLUTE_VOLUME, 0, 99]);
    assert!(
        stale.is_err(),
        "a stale change counter is rejected: {stale:?}"
    );
    assert_eq!(read(&p, 0x2B7D), vec![77, 0, 1], "and nothing moved");
}

// The other half of the contract: a characteristic with no handler still takes
// the direct path, so publishing a device's own state is unchanged.
#[test]
fn a_host_side_write_without_a_handler_still_sets_bytes_directly() {
    let mut p = peripheral(
        r#"let server = android::BluetoothGattServer("Speaker");
           server.add_vcs(128, 0);"#,
    );
    // Volume State is read/notify with no handler: the host publishes it.
    p.set_characteristic_value("2B7D", &[5, 1, 9])
        .expect("host write accepted");
    assert_eq!(read(&p, 0x2B7D), vec![5, 1, 9]);
}
