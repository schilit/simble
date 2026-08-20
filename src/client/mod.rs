// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! GATT Client role implementation.

pub mod gatt_client;

pub use gatt_client::{
    DiscoveredCharacteristic, DiscoveredDescriptor, DiscoveredService, GattClient,
};
