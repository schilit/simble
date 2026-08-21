// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Telephony and Media Audio Profile (TMAP): Telephony and Media Audio Service
//! (TMAS, UUID 0x1855).
//!
//! A thin capability-declaration service: a single read-only 16-bit TMAP Role bitmask
//! (TMAP Section 8.1) telling peers which telephony/media roles this device implements
//! on top of the underlying CAP/BAP/VCP/MCP/CCP machinery.

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// TMAS Service and characteristic UUIDs.
pub mod tmap_uuid {
    use crate::types::Uuid;

    pub const TELEPHONY_AND_MEDIA_AUDIO_SERVICE: Uuid = Uuid::Uuid16(0x1855);
    pub const TMAP_ROLE: Uuid = Uuid::Uuid16(0x2B51);
}

/// TMAP Role bitmask (TMAP Section 8.1, 16-bit little-endian on the wire).
pub mod tmap_role {
    pub const CALL_GATEWAY: u16 = 1 << 0;
    pub const CALL_TERMINAL: u16 = 1 << 1;
    pub const UNICAST_MEDIA_SENDER: u16 = 1 << 2;
    pub const UNICAST_MEDIA_RECEIVER: u16 = 1 << 3;
    pub const BROADCAST_MEDIA_SENDER: u16 = 1 << 4;
    pub const BROADCAST_MEDIA_RECEIVER: u16 = 1 << 5;
}

/// Telephony and Media Audio Service GATT container.
#[derive(Debug, Clone)]
pub struct TelephonyAndMediaAudioService {
    pub service_handle: u16,
    pub role_value_handle: u16,
}

impl TelephonyAndMediaAudioService {
    /// Registers TMAS with `role` (a [`tmap_role`] bitmask).
    pub fn register(db: &mut GattDatabase, role: u16) -> Self {
        let service_handle = db.add_service(tmap_uuid::TELEPHONY_AND_MEDIA_AUDIO_SERVICE, true);

        let (_, role_value_handle) = db.add_characteristic(
            tmap_uuid::TMAP_ROLE,
            CharacteristicProperties(CharacteristicProperties::READ),
            role.to_le_bytes().to_vec(),
            AttributePermissions::read_only(),
        );

        Self {
            service_handle,
            role_value_handle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_role_is_published_as_16_bit_little_endian() {
        let mut db = GattDatabase::new();
        let tmas = TelephonyAndMediaAudioService::register(
            &mut db,
            tmap_role::CALL_GATEWAY | tmap_role::UNICAST_MEDIA_SENDER,
        );

        assert_eq!(db.read(tmas.role_value_handle, 0).unwrap(), &[0x05, 0x00]);
    }

    #[test]
    fn test_role_is_read_only() {
        let mut db = GattDatabase::new();
        let tmas = TelephonyAndMediaAudioService::register(&mut db, tmap_role::CALL_TERMINAL);

        assert!(db.write(tmas.role_value_handle, &[0x00, 0x00]).is_err());
    }
}
