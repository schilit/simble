// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SimBLE on a real Android device.
//!
//! **Scaffolding.** Nothing here drives a radio yet; the crate exists so that
//! a breaking change in `simble` is caught by CI on the day it lands. The
//! design is `docs/phone-as-backend.md`.
//!
//! # The shape this will take
//!
//! The thesis is that the *script* runs on the device rather than a
//! remote-control client: the same text that defines a simulated device
//! defines a real one, which is what makes the two runs comparable and the
//! measurements free of a network in the loop.
//!
//! So this crate holds the Rhai runner and a real implementation of the
//! `android::` API — the seam being one trait, with the virtual
//! implementation used everywhere else and this one used only here. Every
//! call crosses JNI, because Android exposes Bluetooth to Java and Kotlin
//! only; a thin Kotlin shim makes those calls idiomatic and forwards
//! callbacks back.
//!
//! # What a measurement taken here means
//!
//! On this device SimBLE's host stack is **not** in the path: the script
//! drives Android's `BluetoothGattServer` and Android's radio. A number from
//! a phone therefore measures Android's Bluetooth stack on real RF, where a
//! simulator number measures ours over a simulated medium. They are two
//! different stacks and not comparable like for like — see §2 of the design
//! doc, and do not let a chart imply otherwise.

/// Compiles a device script without standing anything up — the same check
/// `lint` performs over MCP.
///
/// Scaffolding, but not a stub: it exercises the scripting layer this crate
/// exists to host, so a breaking change to that layer fails CI here rather
/// than waiting for someone to open this directory.
pub fn lint(script: &str) -> Result<(), String> {
    simble::scripting::test_script::lint_script(script)
}

#[cfg(test)]
mod tests {
    /// A script that builds a device compiles; one that does not, does not.
    /// Both directions, so a `lint` that accepted everything would fail.
    #[test]
    fn lints_a_script() {
        let good = r#"
            let server = android::BluetoothGattServer("android-probe");
        "#;
        assert!(super::lint(good).is_ok(), "a real script should compile");
        assert!(
            super::lint("this is not a script(((").is_err(),
            "nonsense should not"
        );
    }
}
