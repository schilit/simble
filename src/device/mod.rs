// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Virtual Bluetooth Device state machine and packet processor.

pub mod att_server;
pub mod connection;
pub mod observer;
pub mod virtual_device;

pub use connection::{ConnectionState, PrepareWriteChunk};
pub use observer::AttServerObserver;
pub use virtual_device::VirtualDevice;
