// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! An ergonomic layer over `VirtualDevice`/`GattDatabase` shaped like
//! Android's `android.bluetooth.*` peripheral-side API (`BluetoothGattServer`,
//! `BluetoothGattServerCallback`, `BluetoothGattService`, and friends) — for
//! Android developers who already know that API and want the same shape when
//! scripting a virtual peripheral. This is a naming/ergonomics adapter, not a
//! reimplementation of Android's Bluetooth framework or its Binder/AIDL
//! surface: it wraps Simble's real device/GATT machinery, it doesn't
//! duplicate it.

pub mod device;
pub mod gatt_server;
pub mod gatt_service;
