// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! GATT (Generic Attribute Profile) Server Database and handle management.

pub mod database;

pub use database::{
    Attribute, AttributePermissions, CharacteristicProperties, GattDatabase, desc_uuid,
    service_uuid,
};
