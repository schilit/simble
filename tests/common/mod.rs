// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Scaffolding shared by the integration tests in `tests/`.
//!
//! # Why this is a subdirectory
//!
//! Cargo's autotest discovery takes `tests/*.rs` and `tests/*/main.rs`. A
//! `tests/common/mod.rs` matches neither, so it is **not** a test target: it is
//! compiled only into the binaries that say `mod common;`. That is the whole
//! difference between this file and the `tests/mod.rs` that was deleted for
//! re-running 35 files as a 52nd binary (`docs/test-strategy.md`). Verified,
//! not assumed: a `#[test]` planted in this file appears in no binary's
//! `--list`, and `cargo test` builds the same 62 test binaries with it present
//! as without.
//!
//! # What belongs here
//!
//! **Scenes and fixtures, never assertions.** A helper that builds a situation
//! is good; a helper that performs the check a test is named for is not — it
//! hides the subject of the test from its own body and tempts the next author
//! to assert nothing at all. So `run_until` *returns* whether it got there and
//! leaves the `assert!` (and the message explaining what never happened) at the
//! call site, and nothing in this file calls `assert!`.
//!
//! Nor does anything here duplicate the library. H4 command packets are built
//! by [`simble::device::host::command`], which is public API; tests call that
//! directly rather than growing a third copy of four `push`es.

// This module is compiled into each test binary that declares it, and no
// binary uses all of it. Without this, every unused helper is `dead_code` in
// every binary that does not happen to call it.
#![allow(dead_code)]

use simble::types::Address;

/// A distinct public address per device in a scene: `AA:BB:CC:00:00:<last>`.
///
/// Built from bytes rather than parsed from text. Both forms were in use and
/// both produce the same [`Address`] — `FromStr` writes the leftmost text byte
/// into `bytes[5]`, which is where `from_be_bytes` puts index 0 — but a fixture
/// that goes through the parser makes every test using it depend on the parser
/// being right, and does so invisibly. `FromStr` has its own tests; a device
/// identity does not need to re-run them.
///
/// A test that is *about* address parsing must therefore not use this.
pub fn address(last: u8) -> Address {
    Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x00, 0x00, last])
}

/// One HCI Command Complete event, H4-framed, with one command slot free.
///
/// `params` is the whole return-parameter block including the status byte, so
/// a plain success is `&[0x00]`.
pub fn command_complete(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
    // Type adapter over the crate's builder: integration tests name opcodes as
    // byte pairs, the packet carries a u16. `cargo test` turns on the
    // `testing` feature via the self-dev-dependency, so `test_support` is
    // reachable from here.
    simble::test_support::command_complete(u16::from_le_bytes(opcode), params)
}

/// A set of devices on one simulated medium that can be advanced a step at a
/// time.
///
/// Implementing [`tick`](Scene::tick) buys [`run_until`](Scene::run_until),
/// which is the loop every end-to-end test in `tests/` was writing for itself.
/// What `tick` means is the scene's own business — which hosts get pumped, and
/// whether the medium moves before or after them — and that is exactly the part
/// that differs between scenes and must not be shared.
pub trait Scene {
    /// Advances the scene one step.
    fn tick(&mut self);

    /// Ticks until `done` holds, or gives up after `ticks`.
    ///
    /// Returns whether it finished, so the caller can say "it never got there"
    /// rather than assert on whatever half-state a fixed tick count left
    /// behind. The predicate is `FnMut` so it may accumulate.
    ///
    /// `false` means **the budget ran out**, not that the condition is false —
    /// nothing is checked before the first tick, so a zero budget fails even a
    /// condition that already holds. Hence `#[must_use]`: a dropped `false` is
    /// a test that carried on from a state it never reached. Three call sites
    /// were dropping it when this was seven copies. Say `let _ =` when the
    /// point really is just to let a fixed number of ticks pass.
    #[must_use]
    fn run_until<F>(&mut self, ticks: usize, mut done: F) -> bool
    where
        Self: Sized,
        F: FnMut(&Self) -> bool,
    {
        for _ in 0..ticks {
            self.tick();
            if done(self) {
                return true;
            }
        }
        false
    }
}
