// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Gaming Audio Profile (GMAP): Gaming Audio Service (GMAS, UUID 0x1858).
//!
//! A thin capability-declaration service on top of BAP: one read-only GMAP Role bitmask
//! plus one features characteristic per supported role (GMAP Sections 3.1-3.5). Feature
//! characteristics for roles absent from the role bitmask are omitted from the database
//! entirely, since GMAP conditions each on its role bit.

use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// GMAS Service and characteristic UUIDs.
pub mod gmap_uuid {
    use crate::types::Uuid;

    pub const GAMING_AUDIO_SERVICE: Uuid = Uuid::Uuid16(0x1858);
    pub const GMAP_ROLE: Uuid = Uuid::Uuid16(0x2C00);
    pub const UGG_FEATURES: Uuid = Uuid::Uuid16(0x2C01);
    pub const UGT_FEATURES: Uuid = Uuid::Uuid16(0x2C02);
    pub const BGS_FEATURES: Uuid = Uuid::Uuid16(0x2C03);
    pub const BGR_FEATURES: Uuid = Uuid::Uuid16(0x2C04);
}

/// GMAP Role bitmask (GMAP Section 3.1).
pub mod gmap_role {
    pub const UNICAST_GAME_GATEWAY: u8 = 1 << 0;
    pub const UNICAST_GAME_TERMINAL: u8 = 1 << 1;
    pub const BROADCAST_GAME_SENDER: u8 = 1 << 2;
    pub const BROADCAST_GAME_RECEIVER: u8 = 1 << 3;
}

/// UGG Features bitmask (GMAP Section 3.2).
pub mod ugg_features {
    pub const MULTIPLEX: u8 = 1 << 0;
    pub const SOURCE_96_KBPS: u8 = 1 << 1;
    pub const MULTISINK: u8 = 1 << 2;
}

/// UGT Features bitmask (GMAP Section 3.3).
pub mod ugt_features {
    pub const SOURCE: u8 = 1 << 0;
    pub const SOURCE_80_KBPS: u8 = 1 << 1;
    pub const SINK: u8 = 1 << 2;
    pub const SINK_64_KBPS: u8 = 1 << 3;
    pub const MULTIPLEX: u8 = 1 << 4;
    pub const MULTISINK: u8 = 1 << 5;
    pub const MULTISOURCE: u8 = 1 << 6;
}

/// BGS Features bitmask (GMAP Section 3.4).
pub mod bgs_features {
    pub const BGS_96_KBPS: u8 = 1 << 0;
}

/// BGR Features bitmask (GMAP Section 3.5).
pub mod bgr_features {
    pub const MULTISINK: u8 = 1 << 0;
    pub const MULTIPLEX: u8 = 1 << 1;
}

/// Gaming Audio Service GATT container. Feature value handles are `None` for roles the
/// registered role bitmask doesn't include.
#[derive(Debug, Clone)]
pub struct GamingAudioService {
    pub service_handle: u16,
    pub gmap_role_value_handle: u16,
    pub ugg_features_value_handle: Option<u16>,
    pub ugt_features_value_handle: Option<u16>,
    pub bgs_features_value_handle: Option<u16>,
    pub bgr_features_value_handle: Option<u16>,
}

impl GamingAudioService {
    /// Registers GMAS with `role` (a [`gmap_role`] bitmask) and per-role feature
    /// bitmasks; a feature value is only published when its role bit is set.
    pub fn register(
        db: &mut GattDatabase,
        role: u8,
        ugg_features: u8,
        ugt_features: u8,
        bgs_features: u8,
        bgr_features: u8,
    ) -> Self {
        let service_handle = db.add_service(gmap_uuid::GAMING_AUDIO_SERVICE, true);

        let add_read_only = |db: &mut GattDatabase, uuid, value: u8| {
            let (_, value_handle) = db.add_characteristic(
                uuid,
                CharacteristicProperties(CharacteristicProperties::READ),
                vec![value],
                AttributePermissions::read_only(),
            );
            value_handle
        };

        let gmap_role_value_handle = add_read_only(db, gmap_uuid::GMAP_ROLE, role);
        let ugg_features_value_handle = (role & gmap_role::UNICAST_GAME_GATEWAY != 0)
            .then(|| add_read_only(db, gmap_uuid::UGG_FEATURES, ugg_features));
        let ugt_features_value_handle = (role & gmap_role::UNICAST_GAME_TERMINAL != 0)
            .then(|| add_read_only(db, gmap_uuid::UGT_FEATURES, ugt_features));
        let bgs_features_value_handle = (role & gmap_role::BROADCAST_GAME_SENDER != 0)
            .then(|| add_read_only(db, gmap_uuid::BGS_FEATURES, bgs_features));
        let bgr_features_value_handle = (role & gmap_role::BROADCAST_GAME_RECEIVER != 0)
            .then(|| add_read_only(db, gmap_uuid::BGR_FEATURES, bgr_features));

        Self {
            service_handle,
            gmap_role_value_handle,
            ugg_features_value_handle,
            ugt_features_value_handle,
            bgs_features_value_handle,
            bgr_features_value_handle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_roles_publish_all_features() {
        let mut db = GattDatabase::new();
        let gmas = GamingAudioService::register(
            &mut db,
            gmap_role::UNICAST_GAME_GATEWAY
                | gmap_role::UNICAST_GAME_TERMINAL
                | gmap_role::BROADCAST_GAME_SENDER
                | gmap_role::BROADCAST_GAME_RECEIVER,
            ugg_features::MULTISINK,
            ugt_features::SOURCE,
            bgs_features::BGS_96_KBPS,
            bgr_features::MULTISINK,
        );

        assert_eq!(db.read(gmas.gmap_role_value_handle, 0).unwrap(), &[0b1111]);
        assert_eq!(
            db.read(gmas.ugg_features_value_handle.unwrap(), 0).unwrap(),
            &[ugg_features::MULTISINK]
        );
        assert_eq!(
            db.read(gmas.ugt_features_value_handle.unwrap(), 0).unwrap(),
            &[ugt_features::SOURCE]
        );
        assert_eq!(
            db.read(gmas.bgs_features_value_handle.unwrap(), 0).unwrap(),
            &[bgs_features::BGS_96_KBPS]
        );
        assert_eq!(
            db.read(gmas.bgr_features_value_handle.unwrap(), 0).unwrap(),
            &[bgr_features::MULTISINK]
        );
    }

    #[test]
    fn test_absent_roles_omit_their_features_characteristics() {
        let mut db = GattDatabase::new();
        let gmas = GamingAudioService::register(
            &mut db,
            gmap_role::UNICAST_GAME_TERMINAL,
            ugg_features::MULTISINK,
            ugt_features::SINK,
            0,
            0,
        );

        assert!(gmas.ugg_features_value_handle.is_none());
        assert!(gmas.bgs_features_value_handle.is_none());
        assert!(gmas.bgr_features_value_handle.is_none());
        assert_eq!(
            db.read(gmas.ugt_features_value_handle.unwrap(), 0).unwrap(),
            &[ugt_features::SINK]
        );
    }
}
