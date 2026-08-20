// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Core Bluetooth and Simble types.

pub mod error;
pub mod hci_types;

pub use error::SimbleError;
pub use hci_types::{Address, AddressType, GapDataType, LeAdvertisingEventType, Uuid};
