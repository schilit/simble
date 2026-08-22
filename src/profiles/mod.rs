// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Standard Bluetooth SIG GATT profiles and service builders.

/// Audio Input Control Service (AICS, UUID 0x1843).
pub mod aics;
/// Apple Media Service (AMS) client and media service.
pub mod ams;
/// Apple Notification Center Service (ANCS) client and notification center.
pub mod ancs;
/// Audio Stream Control Service (ASCS, UUID 0x184E).
pub mod ascs;
pub mod ascs_client;
/// Audio Streaming for Hearing Aids (ASHA) service.
pub mod asha;
/// Basic Audio Profile (BAP) broadcast announcements and codec configuration.
pub mod bap;
/// Battery Service (BAS, UUID 0x180F).
pub(crate) mod bas;
/// Broadcast Audio Scan Service (BASS, UUID 0x184F).
pub mod bass;
/// Common Audio Service (CAS, UUID 0x1853).
pub(crate) mod cap;
/// Coordinated Set Identification Service/Profile (CSIS/CSIP).
pub mod csip;
/// Device Information Service (DIS, UUID 0x180A).
pub(crate) mod dis;
/// Generic Attribute Profile service, carrying the Service Changed characteristic.
pub(crate) mod gatt_service;
/// Gaming Audio Profile (GMAP) service.
pub mod gmap;
/// Hearing Access Service/Profile (HAS/HAP).
pub mod hap;
/// Heart Rate Service (HRS, UUID 0x180D).
pub(crate) mod hrs;
/// Media Control Service/Profile (MCS/MCP).
pub mod mcp;
/// Published Audio Capabilities Service (PACS, UUID 0x1850).
pub mod pacs;
/// Public Broadcast Profile (PBP) broadcast announcements.
pub mod pbp;
/// Ranging Service (RAS, UUID 0x185B).
pub(crate) mod ras;
/// Telephony and Media Audio Profile (TMAP) service.
pub mod tmap;
/// Volume Control Service/Profile (VCS/VCP).
pub(crate) mod vcp;
/// Volume Offset Control Service (VOCS, UUID 0x1845).
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
