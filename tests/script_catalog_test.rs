// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The Rhai surface over the device catalog, and the temporal assertion.
//!
//! Two gaps closed here, and one thing worth watching. `catalog::device` makes
//! *scripts* a consumer of the catalog resolver, which had exactly one
//! consumer per surface until now — so this file walks every entry through it,
//! which is the first time anything has asked the resolver for all of them
//! from inside the language the entries are written in.
//!
//! `assert_over` is the other half: `assert` says a thing happened,
//! `assert_over` says it stayed true. Both a passing and a failing use are
//! pinned below, because the failure *message* is the whole product — a
//! script author who monitors a heart rate and gets "assertion failed" has
//! learned nothing.

use simble::devices::catalog::{CENTRAL_EXAMPLES, EXAMPLES};
use simble::transport::wasm_ws::{SceneEngine, run_test_script};

mod common;
use common::address;

/// The error a script author sees, with the `Runtime error:` framing rhai adds.
fn failure(script: &str) -> String {
    run_test_script(script).expect_err("the script was expected to fail")
}

// ---------------------------------------------------------------------------
// catalog::device
// ---------------------------------------------------------------------------

/// Every peripheral in the catalog loads by name from a script.
///
/// The second consumer of the resolver is the interesting one: `mcp.rs` and
/// the scene loader each ask for a name a human typed, one at a time. This
/// asks for all of them, in the caller's engine, and so it is the first test
/// that would notice an entry whose script only works under one host's
/// registrations.
#[test]
fn every_catalog_peripheral_loads_from_a_script() {
    for example in EXAMPLES {
        let script = format!(
            r#"let device = catalog::device("{}");
               assert(device.name != "", "the entry named its device");"#,
            example.name
        );
        run_test_script(&script)
            .unwrap_or_else(|e| panic!("catalog::device(\"{}\") failed: {e}", example.name));
    }
}

/// `catalog::names()` is what an author reads instead of the source tree, so
/// it has to agree with the registry rather than being a second list.
#[test]
fn catalog_names_agrees_with_the_registry() {
    let script = format!(
        r#"let names = catalog::names();
           assert(names.len() == {}, "every catalog entry is listed");
           assert(names.contains("hrm"), "the peripherals are listed");
           assert(names.contains("hrm_client"), "the clients are listed too");"#,
        EXAMPLES.len() + CENTRAL_EXAMPLES.len()
    );
    run_test_script(&script).expect("catalog::names() matches the registry");
}

/// A misspelling has to come back as the name that was meant.
#[test]
fn an_unknown_name_lists_the_near_misses() {
    let message = failure(r#"let d = catalog::device("hrmm");"#);
    assert!(
        message.contains("hrmm"),
        "names what was asked for: {message}"
    );
    assert!(
        message.contains("no such catalog entry"),
        "says what went wrong: {message}"
    );
    assert!(
        message.contains("did you mean"),
        "offers a correction: {message}"
    );
    assert!(
        message.contains("hrm"),
        "and the correction is right: {message}"
    );
    assert!(
        message.contains("catalog::names()"),
        "points at the full list: {message}"
    );

    // A remembered fragment counts as a near-miss too.
    let message = failure(r#"let d = catalog::device("keyboard");"#);
    assert!(
        message.contains("hid_keyboard"),
        "a fragment finds the whole name: {message}"
    );

    // Nothing near it falls back to the whole catalog rather than shrugging.
    let message = failure(r#"let d = catalog::device("zzzzzzzzzzzz");"#);
    assert!(
        message.contains("known names:") && message.contains("thermostat"),
        "with no near-miss, list them: {message}"
    );
}

/// Asking for a client entry as if it were a device is a real mistake (the two
/// namespaces are flat and disjoint), so the error says which kind it is.
#[test]
fn asking_for_a_central_entry_says_so() {
    let message = failure(r#"let d = catalog::device("hrm_client");"#);
    assert!(
        message.contains("central") && message.contains("hrm_client"),
        "a client entry is named as one: {message}"
    );
}

/// The point of the whole seam: a scene script that loads a device by name
/// gets that device *hosted*, because what comes back is the same
/// `ScriptGattServer` the scene already scans top-level variables for.
#[test]
fn a_loaded_device_is_the_scenes_peripheral() {
    let mut scene = SceneEngine::new();
    let index = scene
        .add_peripheral(
            address(0x01),
            // `advance` forwards the scene's clock into the loaded entry's own
            // `fn tick`, which would otherwise have been dropped with the
            // scope it was compiled in.
            r#"let hrm = catalog::device("hrm");
               fn tick(server, t) { server.advance(t); }"#,
        )
        .expect("a scene script may be nothing but a catalog load");

    for i in 0..40 {
        scene.tick(i as f64 * 0.05);
    }

    let status = scene.peripheral_status_json(index).expect("status");
    assert!(
        status.contains("180D") || status.to_uppercase().contains("180D"),
        "the scene is hosting the Heart Rate service: {status}"
    );
    assert!(
        !status.contains("\"last_error\":\"") || status.contains("\"last_error\":null"),
        "the forwarded tick did not error: {status}"
    );
}

/// The loaded entry runs in the *caller's* engine, so the events it raises
/// land in the caller's queue. That is what makes `wait_for` usable against a
/// device the script did not type out itself.
#[test]
fn a_loaded_device_raises_events_the_caller_can_wait_for() {
    run_test_script(
        r#"let hrm = catalog::device("hrm");
           wait_for "service_added" {
               assert(event.uuid == uuid::HEART_RATE_SERVICE, "Heart Rate went in first");
               assert(event.status == 0, "the stack accepted it");
           }"#,
    )
    .expect("the loaded device's events reach the caller's wait_for");
}

// ---------------------------------------------------------------------------
// assert_over
// ---------------------------------------------------------------------------

/// The passing use: the HRM's own `fn tick` runs under the assertion, and the
/// reading stays inside the band on every sample.
#[test]
fn assert_over_holds_across_the_devices_own_physics() {
    run_test_script(
        r#"let hrm = catalog::device("hrm");
           assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "<", 100, 3.0);
           assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, ">", 60, 3.0);
           // The default window, and a whole number of seconds, both resolve.
           assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "<", 100);
           assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "<", 100, 1);"#,
    )
    .expect("a resting heart rate stays in the band");
}

/// The failing use, and the reason this test exists: the message is what a
/// script author gets, and it has to name the device, the characteristic, the
/// sample that broke it and when.
#[test]
fn a_broken_monitor_names_the_device_and_the_condition() {
    let message = failure(
        r#"let hrm = catalog::device("hrm");
           assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "<", 70, 3.0);"#,
    );
    assert!(
        message.contains("assert_over failed"),
        "it is a temporal assertion, not a generic panic: {message}"
    );
    assert!(
        message.contains("hrm"),
        "names the device as the author named it: {message}"
    );
    assert!(
        message.contains("byte 1") && message.contains("< 70"),
        "names the condition: {message}"
    );
    assert!(
        message.contains("t="),
        "says when it broke, which is the whole point of a window: {message}"
    );
    assert!(
        message.contains("sample"),
        "and where in the window: {message}"
    );
}

/// `assert_over` on something the device does not publish is the other common
/// mistake, and "no characteristic" alone does not say whose.
#[test]
fn monitoring_a_characteristic_the_device_lacks_names_the_device() {
    let message = failure(
        r#"let hrm = catalog::device("hrm");
           assert_over(hrm, uuid::from_u16(0x2A19), "<", 100, 0.5);"#,
    );
    assert!(
        message.contains("hrm") && message.contains("no readable characteristic"),
        "names the device that lacks it: {message}"
    );
}

/// A typo'd operator must say which operators exist rather than silently
/// passing or failing.
#[test]
fn an_unknown_operator_lists_the_operators() {
    let message = failure(
        r#"let hrm = catalog::device("hrm");
           assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "=<", 100, 0.5);"#,
    );
    assert!(
        message.contains("unknown operator") && message.contains("<="),
        "lists what it accepts: {message}"
    );
}

/// A device with no `fn tick` is steady, not an error — monitoring a constant
/// is a legitimate claim ("the battery level never dropped").
#[test]
fn a_static_device_is_monitorable() {
    run_test_script(
        r#"let battery = catalog::device("battery");
           assert_over(battery, uuid::from_u16(0x2A19), "<=", 100, 1.0, 0);"#,
    )
    .expect("a device without a tick holds its value");
}

/// The value of a passing monitor is the sample that came closest to breaking
/// it — the number that says whether it passed comfortably or by one.
#[test]
fn a_passing_monitor_reports_how_close_it_came() {
    run_test_script(
        r#"let hrm = catalog::device("hrm");
           // Under "<" the extreme is the largest sample seen.
           let worst = assert_over(hrm, uuid::HEART_RATE_MEASUREMENT, "<", 100, 3.0);
           assert(worst >= 68 && worst < 100, `the extreme was ${worst}`);"#,
    )
    .expect("the extreme comes back");
}
