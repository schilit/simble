// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Rhai scripting engine integration — scenario scripts and scripted
//! devices (see the "Simble API Explorer" spec, §03/§05/§05a/§06).
//!
//! What a script gets:
//! - `android::*` — the Android-shaped peripheral API (`BluetoothGattServer`,
//!   `BluetoothGattService`, `BluetoothGattCharacteristic`,
//!   `BluetoothGattDescriptor`) plus its `PROPERTY_*`/`PERMISSION_*`/
//!   `GATT_*`/`SERVICE_TYPE_*` constants, all backed by the real
//!   `crate::android` layer over `VirtualDevice` (see [`bindings`]).
//! - `uuid::*` and `audio::location::*`/`audio::context::*` — a constants
//!   dictionary referencing the Rust consts that already exist, never
//!   re-declared values (spec §05a; see [`constants`]).
//! - `assert(condition, message)` — the test primitive (spec §06).
//! - `wait_for "event" { ... }` — custom syntax over the event queue.
//!
//! Callback model: the spec sketches assigning Rhai closures to
//! `server.callback.on_*`. That design is structurally blocked here:
//! `BluetoothGattServerCallback` requires `Send`, and this build of `rhai`
//! (no `sync` feature) has non-`Send` `FnPtr`/`Engine`, so a Rhai closure
//! can never live inside the observer bridge. We ship the spec's sanctioned
//! fallback instead — an event queue: the observer records plain-data
//! events, and scripts drain them with `take_events()` /
//! `server.take_events()` or match them with `wait_for` (synchronous
//! semantics, which fits Simble's synchronous ATT dispatch).

pub mod bindings;
pub mod constants;

pub use bindings::{ScriptEvent, ScriptGattServer, SessionEvents};

use rhai::Engine;

/// Constructs a fresh Rhai engine with Simble's bindings and sandboxing
/// defaults registered. Each engine owns one session event queue shared by
/// every `android::BluetoothGattServer` the engine's scripts create.
pub fn new_engine() -> Engine {
    let mut engine = Engine::new();

    // Sandboxing (spec §05 "What this costs, honestly"): Rhai registers no
    // filesystem/network functions by default, so nothing needs revoking —
    // the remaining risk is runaway scripts, bounded here. 10M operations is
    // generous for scenario scripts but still terminates an infinite loop.
    engine.set_max_operations(10_000_000);
    engine.set_max_call_levels(64);

    let events = SessionEvents::default();
    bindings::register(&mut engine, events);
    constants::register(&mut engine);

    engine
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
