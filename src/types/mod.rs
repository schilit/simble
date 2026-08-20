// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Core Bluetooth and Simble types.

pub mod error;
pub mod hci_types;

pub use error::SimbleError;
pub use hci_types::{Address, AddressType, GapDataType, LeAdvertisingEventType, Uuid};
