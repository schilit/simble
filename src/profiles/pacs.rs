// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Published Audio Capabilities Service (PACS, UUID 0x1850).
//!
//! Exposes audio capabilities (LC3 codec records, sample rates, channel allocations)
//! for LE Audio unicast (BAP) and broadcast (Auracast) audio endpoints.

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// PACS Service UUIDs.
pub mod pacs_uuid {
    use crate::types::Uuid;

    pub const PACS_SERVICE: Uuid = Uuid::Uuid16(0x1850);
    pub const SINK_PAC: Uuid = Uuid::Uuid16(0x2BC9);
    pub const SINK_AUDIO_LOCATIONS: Uuid = Uuid::Uuid16(0x2BCA);
    pub const SOURCE_PAC: Uuid = Uuid::Uuid16(0x2BCB);
    pub const SOURCE_AUDIO_LOCATIONS: Uuid = Uuid::Uuid16(0x2BCC);
    pub const AVAILABLE_AUDIO_CONTEXTS: Uuid = Uuid::Uuid16(0x2BCD);
    pub const SUPPORTED_AUDIO_CONTEXTS: Uuid = Uuid::Uuid16(0x2BCE);
}

/// Standard Audio Location bitmask flags (BAP Section 4.3) — the complete
/// set lives in [`crate::profiles::bap`], re-exported here for PACS callers.
pub use crate::profiles::bap::audio_location;

/// Published Audio Capabilities Service GATT container.
#[derive(Debug, Clone)]
pub struct PublishedAudioCapabilitiesService {
    pub service_handle: u16,
    pub sink_pac_value_handle: u16,
    pub sink_locations_value_handle: u16,
    pub source_pac_value_handle: u16,
    pub source_locations_value_handle: u16,
    pub available_contexts_value_handle: u16,
    pub supported_contexts_value_handle: u16,
}

impl PublishedAudioCapabilitiesService {
    /// Registers PACS into a GATT database with standard LC3 Sink & Source capabilities.
    pub fn register(db: &mut GattDatabase, sink_location: u32, source_location: u32) -> Self {
        let service_handle = db.add_service(pacs_uuid::PACS_SERVICE, true);

        // 1. Sink PAC (0x2BC9) - LC3 audio codec record (Codec ID 0x06 = LC3)
        // 1 record: Codec ID (5 bytes: 0x06 LC3), Cap length, Caps, Metadata length, Metadata
        let pac_record = vec![
            0x01, // 1 PAC Record
            0x06, 0x00, 0x00, 0x00, 0x00, // Coding format: LC3 (0x06)
            0x04, // Codec Specific Caps length (4 bytes)
            0x03, 0x01, 0x14, 0x00, // Sampling frequency: 16 kHz (bit 2) + 48 kHz (bit 4)
            0x00, // Metadata length: 0
        ];
        let (_, sink_pac_value_handle) = db.add_characteristic(
            pacs_uuid::SINK_PAC,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            pac_record.clone(),
            AttributePermissions::default(),
        );

        // 2. Sink Audio Locations (0x2BCA) - Read | Notify
        let (_, sink_locations_value_handle) = db.add_characteristic(
            pacs_uuid::SINK_AUDIO_LOCATIONS,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            sink_location.to_le_bytes().to_vec(),
            AttributePermissions::default(),
        );

        // 3. Source PAC (0x2BCB)
        let (_, source_pac_value_handle) = db.add_characteristic(
            pacs_uuid::SOURCE_PAC,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            pac_record,
            AttributePermissions::default(),
        );

        // 4. Source Audio Locations (0x2BCC)
        let (_, source_locations_value_handle) = db.add_characteristic(
            pacs_uuid::SOURCE_AUDIO_LOCATIONS,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            source_location.to_le_bytes().to_vec(),
            AttributePermissions::default(),
        );

        // 5. Available Audio Contexts (0x2BCD) - Context bits: Media (0x0002), Conversational (0x0004)
        let (_, available_contexts_value_handle) = db.add_characteristic(
            pacs_uuid::AVAILABLE_AUDIO_CONTEXTS,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![0x06, 0x00, 0x06, 0x00], // Sink contexts (2B) + Source contexts (2B)
            AttributePermissions::default(),
        );

        // 6. Supported Audio Contexts (0x2BCE)
        let (_, supported_contexts_value_handle) = db.add_characteristic(
            pacs_uuid::SUPPORTED_AUDIO_CONTEXTS,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![0x07, 0x00, 0x07, 0x00],
            AttributePermissions::default(),
        );

        Self {
            service_handle,
            sink_pac_value_handle,
            sink_locations_value_handle,
            source_pac_value_handle,
            source_locations_value_handle,
            available_contexts_value_handle,
            supported_contexts_value_handle,
        }
    }
}
