// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Volume Control Service (VCS, UUID 0x1844).
//!
//! Provides volume and mute controls for LE Audio streaming sinks and headsets.

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// VCS Service UUIDs.
pub mod vcp_uuid {
    use crate::types::Uuid;

    pub const VOLUME_CONTROL_SERVICE: Uuid = Uuid::Uuid16(0x1844);
    pub const VOLUME_STATE: Uuid = Uuid::Uuid16(0x2B7D);
    pub const VOLUME_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2B7E);
    pub const VOLUME_FLAGS: Uuid = Uuid::Uuid16(0x2B7F);
}

/// Volume Control Service GATT container.
#[derive(Debug, Clone)]
pub struct VolumeControlService {
    pub service_handle: u16,
    pub volume_state_value_handle: u16,
    pub control_point_value_handle: u16,
    pub volume_flags_value_handle: u16,
}

impl VolumeControlService {
    /// Registers VCS into a GATT database.
    pub fn register(db: &mut GattDatabase, initial_volume: u8, initial_mute: u8) -> Self {
        let service_handle = db.add_service(vcp_uuid::VOLUME_CONTROL_SERVICE, true);

        // 1. Volume State (0x2B7D) - Read | Notify
        // Payload: Volume Setting (1B), Mute (1B: 0=Not Muted, 1=Muted), Change Counter (1B)
        let state_val = vec![initial_volume, initial_mute, 0x00];
        let (_, volume_state_value_handle) = db.add_characteristic(
            vcp_uuid::VOLUME_STATE,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            state_val,
            AttributePermissions::default(),
        );

        // 2. Volume Control Point (0x2B7E) - Write
        let (_, control_point_value_handle) = db.add_characteristic(
            vcp_uuid::VOLUME_CONTROL_POINT,
            CharacteristicProperties(CharacteristicProperties::WRITE),
            vec![],
            AttributePermissions::write_only(),
        );

        // 3. Volume Flags (0x2B7F) - Read | Notify
        let (_, volume_flags_value_handle) = db.add_characteristic(
            vcp_uuid::VOLUME_FLAGS,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![0x00], // Volume Setting Persisted = 0
            AttributePermissions::default(),
        );

        Self {
            service_handle,
            volume_state_value_handle,
            control_point_value_handle,
            volume_flags_value_handle,
        }
    }
}
