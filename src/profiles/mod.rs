// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Standard Bluetooth SIG GATT profiles and service builders.

pub mod bas;
pub mod csip;
pub mod dis;
pub mod gatt_service;
pub mod hrs;
pub mod pacs;
pub mod ras;
pub mod vcp;

pub use bas::BatteryService;
pub use csip::{CoordinatedSetIdentificationService, csip_uuid};
pub use dis::DeviceInformationService;
pub use gatt_service::{GenericAttributeProfileService, gatt_char_uuid};
pub use hrs::{BodySensorLocation, HeartRateService};
pub use pacs::{PublishedAudioCapabilitiesService, audio_location, pacs_uuid};
pub use ras::{RangingService, ras_uuid};
pub use vcp::{VolumeControlService, vcp_uuid};
