// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Core Bluetooth and Simble types.

pub mod error;
pub mod hci_types;
pub(crate) mod rng;

pub use error::SimbleError;
pub use hci_types::{Address, AddressType, Uuid};

/// Speed of light in vacuum, meters per second — shared by the RF
/// distance/angle estimators (Direction Finding, Channel Sounding).
pub const SPEED_OF_LIGHT_M_PER_S: f64 = 299_792_458.0;
