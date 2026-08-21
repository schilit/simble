// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Standard Bluetooth SIG GATT profiles and service builders.

pub mod aics;
pub mod ams;
pub mod ancs;
pub mod ascs;
pub mod asha;
pub mod bap;
pub mod bas;
pub mod bass;
pub mod cap;
pub mod csip;
pub mod dis;
pub mod gatt_service;
pub mod gmap;
pub mod hap;
pub mod hrs;
pub mod mcp;
pub mod pacs;
pub mod pbp;
pub mod ras;
pub mod tmap;
pub mod vcp;
pub mod vocs;

pub use aics::{AudioInputControlService, aics_uuid};
pub use ams::{AmsClient, MediaService as AmsMediaService, ams_uuid};
pub use ancs::{AncsClient, NotificationCenterService, ancs_uuid};
pub use ascs::{AudioStreamControlService, ascs_uuid};
pub use asha::{AshaService, asha_uuid};
pub use bap::{
    BasicAudioAnnouncement, BroadcastAudioAnnouncement, CodecSpecificCapabilities,
    CodecSpecificConfiguration, bap_uuid,
};
pub use bas::BatteryService;
pub use bass::{BroadcastAudioScanService, bass_uuid};
pub use cap::{CommonAudioService, cap_uuid};
pub use csip::{CoordinatedSetIdentificationService, csip_uuid};
pub use dis::DeviceInformationService;
pub use gatt_service::{GenericAttributeProfileService, gatt_char_uuid};
pub use gmap::{GamingAudioService, gmap_uuid};
pub use hap::{HearingAccessService, hap_uuid};
pub use hrs::{BodySensorLocation, HeartRateService};
pub use mcp::{MediaControlService, mcp_uuid};
pub use pacs::{PublishedAudioCapabilitiesService, audio_location, pacs_uuid};
pub use pbp::{PublicBroadcastAnnouncement, pbp_uuid};
pub use ras::{RangingService, ras_uuid};
pub use tmap::{TelephonyAndMediaAudioService, tmap_uuid};
pub use vcp::{VolumeControlService, vcp_uuid};
pub use vocs::{VolumeOffsetControlService, vocs_uuid};
