// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! An **Android-shaped API** for building virtual peripherals — the peripheral
//! side of `android.bluetooth.*`, so Android developers can script a virtual
//! device with the API they already know.
//!
//! The building blocks:
//!
//! - [`BluetoothGattServer`](gatt_server::BluetoothGattServer) — the device
//!   itself: add services and notify subscribers.
//! - [`BluetoothGattServerCallback`](gatt_server::BluetoothGattServerCallback) —
//!   connection, read, and write events from the remote central.
//! - [`BluetoothGattService`](gatt_service::BluetoothGattService),
//!   [`BluetoothGattCharacteristic`](gatt_service::BluetoothGattCharacteristic),
//!   and [`BluetoothGattDescriptor`](gatt_service::BluetoothGattDescriptor) — the
//!   GATT hierarchy.
//! - [`BluetoothDevice`](device::BluetoothDevice) — the remote central handed to
//!   a callback.
//!
//! It is a **naming and ergonomics adapter, not a reimplementation** of the
//! Android framework — there is no Binder/AIDL surface. Every type is a thin
//! wrapper over Simble's real device and GATT machinery rather than a duplicate
//! of it.

pub mod device;
pub mod gatt_server;
pub mod gatt_service;
