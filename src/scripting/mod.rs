// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Rhai scripting engine integration — scenario scripts and scripted
//! devices (see the "Simble API Explorer" spec). This module owns the bare
//! `rhai::Engine` lifecycle; the `android::*`/`corebluetooth::*` host
//! bindings that make scripts actually drive a `VirtualDevice` are separate,
//! larger work layered on top of this, not yet built.

use rhai::Engine;

/// Constructs a fresh Rhai engine with Simble's default settings (no host
/// bindings registered yet — see the module doc).
pub fn new_engine() -> Engine {
    Engine::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_engine_evaluates_a_script() {
        let engine = new_engine();
        let result: i64 = engine.eval("40 + 2").expect("valid script");
        assert_eq!(result, 42);
    }
}
