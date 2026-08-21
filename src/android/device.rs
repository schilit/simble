// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `BluetoothDevice`-shaped handle to a remote peer, mirroring
//! `android.bluetooth.BluetoothDevice`. Thin wrapper over Simble's existing
//! `Address`/`AddressType` — just enough identity for the
//! `BluetoothGattServerCallback` signatures to reference.

use std::fmt;

use crate::types::{Address, AddressType};

/// A remote Bluetooth peer, mirroring `android.bluetooth.BluetoothDevice`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BluetoothDevice {
    pub address: Address,
    pub address_type: AddressType,
    pub name: Option<String>,
}

impl BluetoothDevice {
    pub fn new(address: Address, address_type: AddressType) -> Self {
        Self {
            address,
            address_type,
            name: None,
        }
    }

    pub fn with_name(address: Address, address_type: AddressType, name: impl Into<String>) -> Self {
        Self {
            address,
            address_type,
            name: Some(name.into()),
        }
    }

    /// Mirrors `BluetoothDevice.getAddress()`.
    pub fn get_address(&self) -> String {
        self.address.to_string()
    }

    /// Mirrors `BluetoothDevice.getName()`.
    pub fn get_name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

impl fmt::Display for BluetoothDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.address)
    }
}
