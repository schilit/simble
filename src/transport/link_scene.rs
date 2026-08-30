// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The `link` controller: SimBLE's deterministic in-process world as a [`Scene`].
//!
//! It wraps [`SceneEngine`] — the shared-`Link` medium that `crate::mcp`'s "self"
//! mode already runs on — so a v1 `run` can drive scripted devices with no netsim
//! or USB hardware: the tick-driven, fully reproducible path. Unlike the live
//! backends there is no wire to pump — `SceneEngine::tick` routes across the
//! medium itself — and its clock is absolute, so this tracks elapsed time and
//! advances it by each `tick` delta.

use super::Scene;
use crate::scene::SceneEngine;
use crate::types::Address;

/// The deterministic self/`link` scene: scripted devices on the in-process
/// `Link`, driven entirely by `tick`.
pub struct LinkScene {
    engine: SceneEngine,
    /// Absolute script-clock seconds, advanced by each `tick` — `SceneEngine`
    /// takes an absolute time, while the [`Scene`] contract is advance-by-delta.
    elapsed: f64,
}

impl LinkScene {
    /// A fresh deterministic scene with no devices and its clock at zero.
    pub fn new() -> Self {
        Self {
            engine: SceneEngine::new(),
            elapsed: 0.0,
        }
    }
}

impl Default for LinkScene {
    fn default() -> Self {
        Self::new()
    }
}

impl Scene for LinkScene {
    fn name(&self) -> &'static str {
        "link"
    }
    fn add_peripheral(&mut self, address: Address, script: &str) -> Result<usize, String> {
        self.engine.add_peripheral(address, script)
    }
    // `SceneEngine::tick` routes across the shared medium itself, so there is no
    // separate wire to pump.
    fn pump(&mut self) {}
    fn tick(&mut self, advance_us: u64) -> Option<u64> {
        self.elapsed += advance_us as f64 / 1_000_000.0;
        self.engine.tick(self.elapsed);
        self.next_deadline_us()
    }
    fn now_us(&self) -> u64 {
        (self.elapsed * 1_000_000.0).round() as u64
    }
    fn device_count(&self) -> usize {
        self.engine.device_count()
    }
    fn peripheral_status_json(&self, index: usize) -> Option<String> {
        self.engine.peripheral_status_json(index)
    }
    fn next_deadline_us(&self) -> Option<u64> {
        self.engine
            .next_deadline()
            .map(|s| (s * 1_000_000.0).round() as u64)
    }
    fn stop(&mut self, index: usize) -> Result<(), String> {
        self.engine.stop(index)
    }
    fn device_stopped(&self, index: usize) -> bool {
        self.engine.device_stopped(index)
    }
    fn send(&mut self, index: usize, event: &str, data_json: &str) -> Result<(), String> {
        self.engine.send(index, event, data_json)
    }
}
