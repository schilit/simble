// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! REST / JSON API definitions and data transfer objects.

pub mod dto;

pub use dto::{
    CreateDeviceRequest, CreateDeviceResponse, DeviceEvent, DeviceRole, GattCharacteristicConfig,
    GattServiceConfig, SetAdvertisingRequest, UpdateCharacteristicRequest,
};
