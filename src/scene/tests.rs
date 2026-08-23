// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Format tests: the shape of the file, the round trip, and every validation
//! error a hand-written scene is likely to hit. Hosting is tested in
//! [`super::runner`].

use super::*;
use crate::smp::PairingKey;

/// The smallest complete scene, used wherever the topology is not the point.
const MINIMAL: &str = r#"{ "version": 1, "devices": [ { "id": "hr", "device": "hrm" } ] }"#;

fn parse(json: &str) -> Scene {
    Scene::from_json(json).unwrap_or_else(|e| panic!("should parse: {e}\n{json}"))
}

fn error(json: &str) -> String {
    match Scene::from_json(json) {
        Err(e) => e.to_string(),
        Ok(scene) => match scene.resolve() {
            Err(e) => e.to_string(),
            Ok(_) => panic!("should have been rejected:\n{json}"),
        },
    }
}

// --- the shape --------------------------------------------------------------

#[test]
fn a_scene_needs_only_a_version_and_one_device() {
    let scene = parse(MINIMAL);
    assert_eq!(scene.version, VERSION);
    assert_eq!(scene.controller, Controller::InProcess);
    assert_eq!(scene.devices[0].role, Role::Peripheral);
    assert!(scene.devices[0].address.is_none());
}

#[test]
fn the_starting_example_from_the_format_doc_parses_and_resolves() {
    // The scene in docs/scene-format.md, kept here so the doc's headline
    // example cannot rot: LE Audio topology, a catalog sink and a client
    // role wired to it by id.
    let scene = parse(
        r#"{
             "version": 1,
             "name": "LE Audio unicast",
             "description": "A source streaming LC3 to a sink over a real CIS",
             "controller": "netsim",
             "devices": [
               { "id": "sink", "role": "peripheral", "name": "web-speaker",
                 "address": "CC:1E:57:00:00:06", "device": "volume",
                 "config": { "sample_rate_hz": 16000, "octets_per_frame": 40 } },
               { "id": "source", "role": "audio_source", "name": "web-source",
                 "address": "CC:1E:57:00:00:07", "target": "sink" }
             ]
           }"#,
    );
    let resolved = scene.resolve().unwrap();
    assert_eq!(resolved.controller, Controller::Netsim);
    assert_eq!(resolved.devices[0].address.to_string(), "CC:1E:57:00:00:06");
    assert_eq!(
        resolved.devices[1].target.as_ref().map(|(id, _)| id.as_str()),
        Some("sink")
    );
    // The peer was resolved to the *address* the sink actually holds.
    assert_eq!(
        resolved.devices[1].target.as_ref().unwrap().1,
        resolved.devices[0].address
    );
}

#[test]
fn a_catalog_name_resolves_to_the_catalog_script_and_an_inline_script_pins_its_own() {
    let resolved = parse(MINIMAL).resolve().unwrap();
    assert_eq!(
        resolved.devices[0].script.as_deref(),
        crate::devices::catalog::script("hrm")
    );

    let pinned = parse(
        r#"{ "version": 1, "devices": [
             { "id": "x", "script": "let server = android::BluetoothGattServer(\"Pinned\");" } ] }"#,
    )
    .resolve()
    .unwrap();
    assert!(pinned.devices[0].script.as_deref().unwrap().contains("Pinned"));
}

#[test]
fn an_omitted_address_becomes_a_deterministic_scene_address() {
    let resolved = parse(
        r#"{ "version": 1, "devices": [
             { "id": "a", "device": "battery" }, { "id": "b", "device": "hrm" } ] }"#,
    )
    .resolve()
    .unwrap();
    assert_eq!(resolved.devices[0].address, auto_address(1));
    assert_eq!(resolved.devices[1].address, auto_address(2));
}

#[test]
fn an_auto_assigned_address_steps_around_one_a_later_device_claimed_explicitly() {
    // Claiming happens before assignment, so file order cannot silently
    // produce two devices at one address.
    let resolved = parse(&format!(
        r#"{{ "version": 1, "devices": [
             {{ "id": "auto", "device": "battery" }},
             {{ "id": "fixed", "device": "hrm", "address": "{}" }} ] }}"#,
        auto_address(1)
    ))
    .resolve()
    .unwrap();
    assert_eq!(resolved.devices[0].address, auto_address(2));
    assert_eq!(resolved.devices[1].address, auto_address(1));
}

// --- the round trip ---------------------------------------------------------

#[test]
fn parsing_serializing_and_parsing_again_yields_the_same_scene() {
    let full = r#"{
      "version": 1,
      "name": "round trip",
      "description": "every field the format has",
      "controller": "netsim",
      "devices": [
        { "id": "sink", "role": "peripheral", "name": "web-speaker",
          "address": "CC:1E:57:00:00:06", "device": "volume",
          "config": { "sample_rate_hz": 16000 } },
        { "id": "phone", "role": "central", "target": "sink" }
      ],
      "bonds": [
        { "between": ["sink", "phone"],
          "security": { "keys": { "ltk": { "value": "000102030405060708090a0b0c0d0e0f" } },
                        "secure_connections": true, "authenticated": true, "key_size": 16 },
          "cccds": [ { "handle": 12, "value": 1 } ],
          "known_by": ["sink"],
          "sides": { "sink": { "cccds": [ { "handle": 12, "value": 2 } ] } } }
      ]
    }"#;
    let once = parse(full);
    let twice = parse(&once.to_json());
    assert_eq!(once, twice, "serializing must not lose or invent anything");
    assert_eq!(once.to_json(), twice.to_json());
}

#[test]
fn a_minimal_scene_round_trips_without_gaining_noise() {
    let once = parse(MINIMAL);
    let text = once.to_json();
    assert_eq!(parse(&text), once);
    // Absent optional fields stay absent, so a diff of a saved scene shows
    // what changed rather than a wall of nulls.
    assert!(!text.contains("null"), "{text}");
    assert!(!text.contains("bonds"), "{text}");
    assert!(!text.contains("config"), "{text}");
}

#[test]
fn every_committed_example_scene_parses_resolves_and_round_trips() {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/catalog/scenes");
    let mut count = 0;
    for entry in std::fs::read_dir(dir).expect("catalog/scenes should exist") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let scene = Scene::from_json(&text)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        scene
            .resolve()
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        assert_eq!(
            Scene::from_json(&scene.to_json()).unwrap(),
            scene,
            "{} does not round-trip",
            path.display()
        );
        count += 1;
    }
    assert!(count >= 3, "expected several example scenes, found {count}");
}

// --- validation -------------------------------------------------------------

#[test]
fn a_future_version_is_refused_rather_than_half_understood() {
    let message = error(r#"{ "version": 99, "devices": [ { "id": "a", "device": "hrm" } ] }"#);
    assert!(message.contains("version 99"), "{message}");
}

#[test]
fn a_misspelt_field_is_an_error_not_a_silently_ignored_one() {
    // "adress" would otherwise put the device on the air with the wrong
    // identity, and SMP would fail with no clue why.
    let message = error(
        r#"{ "version": 1, "devices": [ { "id": "a", "device": "hrm",
             "adress": "AA:BB:CC:00:00:01" } ] }"#,
    );
    assert!(message.contains("adress"), "{message}");
}

#[test]
fn a_duplicate_device_id_names_the_id() {
    let message = error(
        r#"{ "version": 1, "devices": [
             { "id": "a", "device": "hrm" }, { "id": "a", "device": "battery" } ] }"#,
    );
    assert!(message.contains("\"a\"") && message.contains("duplicate"), "{message}");
}

#[test]
fn a_malformed_address_says_so_with_the_offending_text() {
    let message = error(
        r#"{ "version": 1, "devices": [ { "id": "a", "device": "hrm", "address": "not-a-mac" } ] }"#,
    );
    assert!(message.contains("not-a-mac"), "{message}");
}

#[test]
fn two_devices_at_one_address_is_an_error() {
    let message = error(
        r#"{ "version": 1, "devices": [
             { "id": "a", "device": "hrm", "address": "AA:BB:CC:00:00:01" },
             { "id": "b", "device": "battery", "address": "AA:BB:CC:00:00:01" } ] }"#,
    );
    assert!(message.contains("already used"), "{message}");
}

#[test]
fn an_unknown_catalog_device_lists_the_catalog() {
    let message = error(
        r#"{ "version": 1, "devices": [ { "id": "a", "device": "le_audio_sink" } ] }"#,
    );
    assert!(message.contains("le_audio_sink"), "{message}");
    assert!(message.contains("hrm"), "the catalog should be listed: {message}");
}

#[test]
fn an_unknown_role_lists_the_known_roles() {
    let message = error(
        r#"{ "version": 1, "devices": [ { "id": "a", "role": "toaster", "device": "hrm" } ] }"#,
    );
    assert!(message.contains("toaster"), "{message}");
    assert!(message.contains("audio_source"), "{message}");
}

#[test]
fn an_unknown_controller_lists_the_known_controllers() {
    let message = error(
        r#"{ "version": 1, "controller": "rootcanal",
             "devices": [ { "id": "a", "device": "hrm" } ] }"#,
    );
    assert!(message.contains("rootcanal") && message.contains("netsim"), "{message}");
}

#[test]
fn a_dangling_target_names_the_devices_that_do_exist() {
    let message = error(
        r#"{ "version": 1, "devices": [
             { "id": "hr", "device": "hrm" },
             { "id": "phone", "role": "central", "target": "speaker" } ] }"#,
    );
    assert!(message.contains("speaker"), "{message}");
    assert!(message.contains("hr"), "the message should list real ids: {message}");
}

#[test]
fn a_device_cannot_target_itself() {
    let message = error(
        r#"{ "version": 1, "devices": [
             { "id": "loop", "role": "central", "target": "loop" } ] }"#,
    );
    assert!(message.contains("targets itself"), "{message}");
}

#[test]
fn a_client_role_without_a_target_is_refused() {
    let message =
        error(r#"{ "version": 1, "devices": [ { "id": "phone", "role": "central" } ] }"#);
    assert!(message.contains("needs a \"target\""), "{message}");
}

#[test]
fn a_peripheral_cannot_have_a_target_because_it_connects_to_nothing() {
    let message = error(
        r#"{ "version": 1, "devices": [
             { "id": "a", "device": "hrm" },
             { "id": "b", "device": "battery", "target": "a" } ] }"#,
    );
    assert!(message.contains("cannot have a \"target\""), "{message}");
}

#[test]
fn naming_both_a_catalog_device_and_an_inline_script_is_ambiguous_and_refused() {
    let message = error(
        r#"{ "version": 1, "devices": [
             { "id": "a", "device": "hrm", "script": "let s = 1;" } ] }"#,
    );
    assert!(message.contains("not both"), "{message}");
}

#[test]
fn a_peripheral_with_neither_a_device_nor_a_script_is_refused() {
    let message = error(r#"{ "version": 1, "devices": [ { "id": "a" } ] }"#);
    assert!(message.contains("catalog name"), "{message}");
}

#[test]
fn an_id_that_would_not_survive_a_netsim_url_is_refused() {
    let message = error(
        r#"{ "version": 1, "devices": [ { "id": "my device", "device": "hrm" } ] }"#,
    );
    assert!(message.contains("ids may use"), "{message}");
}

#[test]
fn a_scene_with_no_devices_is_refused() {
    let message = error(r#"{ "version": 1, "devices": [] }"#);
    assert!(message.contains("at least one"), "{message}");
}

// --- bonds ------------------------------------------------------------------

/// A scene with one bond between `sink` and `phone`, with `body` spliced into
/// the bond object.
fn bonded(body: &str) -> String {
    format!(
        r#"{{ "version": 1,
              "devices": [ {{ "id": "sink", "device": "hrm" }},
                           {{ "id": "phone", "role": "central", "target": "sink" }} ],
              "bonds": [ {{ "between": ["sink", "phone"],
                            "security": {{ "keys": {{ "ltk": {{ "value": "{}" }} }},
                                           "secure_connections": true }}{body} }} ] }}"#,
        "0f0e0d0c0b0a09080706050403020100"
    )
}

#[test]
fn a_bond_is_symmetric_by_default_and_each_side_is_keyed_by_the_other_address() {
    let resolved = parse(&bonded("")).resolve().unwrap();
    let sink = &resolved.devices[0];
    let phone = &resolved.devices[1];

    assert!(sink.bonds.is_bonded(phone.address), "sink remembers phone");
    assert!(phone.bonds.is_bonded(sink.address), "phone remembers sink");
    // Keyed by the *peer*, never by itself.
    assert!(!sink.bonds.is_bonded(sink.address));

    let security = sink.bonds.load_security(phone.address).unwrap();
    assert!(security.secure_connections);
    assert_eq!(
        security.keys.ltk,
        Some(PairingKey::new([
            0x0f, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a, 0x09, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02,
            0x01, 0x00
        ]))
    );
}

#[test]
fn an_omitted_key_size_becomes_the_maximum_and_is_written_back_explicitly() {
    let scene = parse(&bonded(""));
    assert_eq!(scene.bonds[0].security.key_size, 16);
    assert!(scene.to_json().contains("\"key_size\": 16"));
}

#[test]
fn known_by_expresses_one_side_having_forgotten_the_bond() {
    // The interesting failure mode: the peer offers an LTK the device no
    // longer has. The format has to be able to say that.
    let resolved = parse(&bonded(r#", "known_by": ["phone"]"#))
        .resolve()
        .unwrap();
    let sink = &resolved.devices[0];
    let phone = &resolved.devices[1];
    assert!(phone.bonds.is_bonded(sink.address), "phone still remembers");
    assert!(!sink.bonds.is_bonded(phone.address), "the sink forgot");
}

#[test]
fn a_side_override_replaces_only_that_sides_material() {
    // An IRK belongs to a device, so the record a peer holds carries the
    // peer's IRK — the case a single shared block cannot express.
    let resolved = parse(&bonded(
        r#", "sides": { "phone": { "security": {
              "keys": { "ltk": { "value": "00112233445566778899aabbccddeeff" },
                        "irk": { "value": "ffeeddccbbaa99887766554433221100" } },
              "secure_connections": true, "key_size": 16 } } }"#,
    ))
    .resolve()
    .unwrap();
    let sink = &resolved.devices[0];
    let phone = &resolved.devices[1];

    assert!(phone.bonds.load_security(sink.address).unwrap().keys.irk.is_some());
    assert!(
        sink.bonds.load_security(phone.address).unwrap().keys.irk.is_none(),
        "the shared block is untouched by the other side's override"
    );
}

#[test]
fn cccd_subscriptions_are_restored_per_side() {
    let resolved = parse(&bonded(
        r#", "sides": { "sink": { "cccds": [ { "handle": 12, "value": 1 } ] } }"#,
    ))
    .resolve()
    .unwrap();
    let sink = &resolved.devices[0];
    let phone = &resolved.devices[1];
    assert_eq!(sink.bonds.load_cccds(phone.address), vec![(12, 1)]);
    assert!(phone.bonds.load_cccds(sink.address).is_empty());
}

#[test]
fn a_bond_naming_a_device_that_does_not_exist_is_refused() {
    let message = error(
        r#"{ "version": 1, "devices": [ { "id": "sink", "device": "hrm" } ],
             "bonds": [ { "between": ["sink", "ghost"],
                          "security": { "keys": { "ltk": { "value": "00000000000000000000000000000000" } } } } ] }"#,
    );
    assert!(message.contains("ghost"), "{message}");
}

#[test]
fn a_bond_without_a_long_term_key_cannot_start_encryption_and_is_refused() {
    let message = error(
        r#"{ "version": 1, "devices": [ { "id": "sink", "device": "hrm" },
                                        { "id": "phone", "role": "central", "target": "sink" } ],
             "bonds": [ { "between": ["sink", "phone"], "security": { "authenticated": true } } ] }"#,
    );
    assert!(message.contains("no long-term key"), "{message}");
}

#[test]
fn a_key_size_outside_the_spec_range_is_refused() {
    let message = error(&bonded(r#", "sides": { "sink": { "security": {
        "keys": { "ltk": { "value": "00000000000000000000000000000000" } }, "key_size": 32 } } }"#));
    assert!(message.contains("7..=16"), "{message}");
}

#[test]
fn a_zero_cccd_value_records_nothing_and_is_refused_as_a_mistake() {
    let message = error(&bonded(r#", "cccds": [ { "handle": 12, "value": 0 } ]"#));
    assert!(message.contains("records nothing"), "{message}");
}

#[test]
fn the_same_pair_cannot_be_bonded_twice_in_one_scene() {
    let message = error(
        r#"{ "version": 1,
             "devices": [ { "id": "sink", "device": "hrm" },
                          { "id": "phone", "role": "central", "target": "sink" } ],
             "bonds": [ { "between": ["sink", "phone"],
                          "security": { "keys": { "ltk": { "value": "00000000000000000000000000000000" } } } },
                        { "between": ["phone", "sink"],
                          "security": { "keys": { "ltk": { "value": "11111111111111111111111111111111" } } } } ] }"#,
    );
    assert!(message.contains("declared twice"), "{message}");
}

#[test]
fn known_by_naming_nobody_is_a_bond_that_means_nothing() {
    let message = error(&bonded(r#", "known_by": []"#));
    assert!(message.contains("delete it"), "{message}");
}

#[test]
fn a_bond_key_record_reads_the_same_json_a_bumble_keystore_writes() {
    // The reason bonds embed `PairingKeys` verbatim rather than a
    // scene-private shape: key material lifted out of a Bumble keystore
    // pastes straight in.
    let scene = parse(
        r#"{ "version": 1,
             "devices": [ { "id": "sink", "device": "hrm" },
                          { "id": "phone", "role": "central", "target": "sink" } ],
             "bonds": [ { "between": ["sink", "phone"], "security": { "keys": {
                 "address_type": 1,
                 "ltk": { "value": "000102030405060708090a0b0c0d0e0f", "authenticated": true },
                 "ltk_central": { "value": "0102030405060708090a0b0c0d0e0f00",
                                  "ediv": 4660, "rand": "0001020304050607" },
                 "irk": { "value": "0f0e0d0c0b0a09080706050403020100" },
                 "csrk": { "value": "00000000000000000000000000000001" } } } } ] }"#,
    );
    let keys = &scene.bonds[0].security.keys;
    assert_eq!(keys.address_type, Some(1));
    assert!(keys.ltk.as_ref().unwrap().authenticated);
    assert_eq!(keys.ltk_central.as_ref().unwrap().ediv, Some(0x1234));
    assert_eq!(
        keys.ltk_central.as_ref().unwrap().rand,
        Some([0, 1, 2, 3, 4, 5, 6, 7])
    );
    assert!(keys.csrk.is_some());
    scene.resolve().unwrap();
}
