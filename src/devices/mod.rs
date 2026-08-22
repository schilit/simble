// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Ready-to-use virtual BLE device models (Heart Rate Monitor, Keyboard, Mouse, Beacons).

pub mod beacon;
pub mod catalog;
pub mod heart_rate_monitor;
pub mod helpers;
pub mod keyboard;
pub mod mouse;

pub use beacon::{EddystoneUidBeacon, IBeacon};
pub use heart_rate_monitor::HeartRateMonitor;
pub use keyboard::BleKeyboard;
pub use mouse::BleMouse;
