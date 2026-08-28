// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Every catalog script actually runs.
//!
//! The catalog is what the MCP `example` tool hands an agent, what a scene's
//! `"device": "..."` resolves against, and what three web pages fetch through
//! `catalog_script`. A broken entry is therefore a broken first impression,
//! delivered to whoever asked.
//!
//! It was almost entirely unchecked. `every_catalog_name_is_unique_and_resolves
//! _to_its_own_script` compares a name to a string and asserts the summary is
//! non-empty; it never evaluates anything. The only entries that were ever
//! *executed* were the four peripherals named as a `ClientExample`'s peer, by
//! `central_script_test.rs` — leaving **15 of 19 peripherals** able to carry a
//! syntax error or a renamed binding all the way to an agent.
//!
//! These tests are self-contained: both ends are simble's, so they prove the
//! scripts parse, build a database, and survive being ticked. They are not
//! evidence about the wire — `tests/interop/` is.

use simble::devices::catalog::{CENTRAL_EXAMPLES, EXAMPLES};
use simble::scene::SceneEngine;
use simble::scripting::test_script::run_test_script;

mod common;
use common::address;

/// Every peripheral in the catalog builds a device and survives being run.
///
/// Ticking matters as much as constructing: several entries define `fn tick`
/// and do their real work there, so a script that builds cleanly can still
/// fail on its first update. `last_error` in the status JSON is where a tick
/// failure surfaces.
#[test]
fn every_catalog_peripheral_builds_and_ticks() {
    for example in EXAMPLES {
        let mut scene = SceneEngine::new();
        let index = scene
            .add_peripheral(address(0x01), example.script)
            .unwrap_or_else(|e| panic!("{}: script rejected: {e}", example.name));

        for i in 0..40 {
            scene.tick(i as f64 * 0.05);
        }

        let status = scene
            .peripheral_status_json(index)
            .unwrap_or_else(|| panic!("{}: no status", example.name));
        assert!(
            !status.contains("\"last_error\":\"") || status.contains("\"last_error\":null"),
            "{} reported a tick error: {status}",
            example.name,
        );
        // A device that advertises nothing is a device nobody can find. Every
        // catalog entry is meant to be a working example, so each must put at
        // least one service in its database.
        //
        // Checking for the KEY was not enough, and this test passed happily
        // while every device built from Rust profile registrars reported
        // `"services": []` -- the registrars write into the GattDatabase and
        // never reach the script's own service list, so the array was empty
        // for exactly the devices most worth checking. Assert it is populated.
        assert!(
            !status.contains("\"services\":[]"),
            "{} reports an empty service list: {status}",
            example.name,
        );
        assert!(
            status.contains("\"services\""),
            "{} exposes no services: {status}",
            example.name,
        );
    }
}

/// Every script in `catalog/tests/` gets the verdict its filename promises.
///
/// These are the shipped *tests* rather than the shipped devices: the browser
/// Testing page loads three of them, and `.github/workflows/ci.yml` runs the
/// whole directory through the `simble` CLI, keying on the `.pass`/`.fail`
/// suffix. That was the only thing running them — so `cargo test` was green
/// while a `catalog/tests/` entry was broken, and a contributor with no CI run
/// in front of them had no way to find out. The walk above covers
/// `catalog/devices/` because those are `include_str!`'d into `EXAMPLES`;
/// nothing covered this directory, which is a directory of files whose whole
/// job is to be run.
///
/// A `.fail.rhai` that stops failing is the more interesting break of the two:
/// it means the assertion it was written to demonstrate no longer catches
/// anything.
#[test]
fn every_catalog_test_script_gets_the_verdict_its_name_promises() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("catalog/tests");
    let mut scripts: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.expect("readable directory entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == "rhai"))
        .collect();
    scripts.sort();
    assert!(
        !scripts.is_empty(),
        "{}: no scripts — the walk is passing vacuously",
        dir.display()
    );

    for path in scripts {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let source = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        let outcome = run_test_script(&source);
        if name.contains(".fail.") {
            outcome.err().unwrap_or_else(|| {
                panic!("{name} is named .fail but passed — it demonstrates nothing")
            });
        } else if let Err(e) = outcome {
            panic!("{name} is named .pass but failed: {e}");
        }
    }
}

/// The catalog's own cross-references resolve.
///
/// A `ClientExample` names the peripheral it was written against. If that name
/// is wrong the pairing silently tests nothing, because the central connects
/// to a device that was never added.
#[test]
fn every_central_example_names_a_peripheral_in_the_catalog() {
    for example in CENTRAL_EXAMPLES {
        assert!(
            EXAMPLES.iter().any(|p| p.name == example.peer),
            "{} names peer {:?}, which is not in the catalog",
            example.name,
            example.peer,
        );
    }
}
