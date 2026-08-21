// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Virtual Bluetooth Device state machine and packet processor.

pub(crate) mod att_server;
pub(crate) mod bond_store;
pub(crate) mod connection;
pub(crate) mod observer;
pub(crate) mod virtual_device;

pub use bond_store::{BondSecurity, BondStore, MemoryBondStore};
pub use connection::{ConnectionRole, ConnectionState, PrepareWriteChunk};
pub use observer::{AttServerObserver, SubscriptionReason};
pub use virtual_device::VirtualDevice;
