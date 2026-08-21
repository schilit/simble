// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Common Audio Profile (CAP): Common Audio Service (CAS, UUID 0x1853).
//!
//! CAS carries no characteristics of its own — its whole purpose is marking a device as
//! a CAP Acceptor and, when the device belongs to a coordinated set (a pair of earbuds,
//! a set of hearing aids), pointing at the set membership via a GATT Include of the
//! Coordinated Set Identification Service (CAP Section 4.2).

use crate::gatt::GattDatabase;
use crate::profiles::csip::{CoordinatedSetIdentificationService, csip_uuid};

/// CAS Service UUIDs.
pub mod cap_uuid {
    use crate::types::Uuid;

    /// Common Audio Service UUID.
    pub const COMMON_AUDIO_SERVICE: Uuid = Uuid::Uuid16(0x1853);
}

/// Common Audio Service GATT container.
#[derive(Debug, Clone)]
pub struct CommonAudioService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Handle of the Include declaration referencing CSIS; `None` when the device is
    /// not a coordinated-set member (the CSIS include is optional per CAP Section 4.2).
    pub csis_include_handle: Option<u16>,
}

impl CommonAudioService {
    /// Registers CAS including an already-registered CSIS instance.
    pub fn register(db: &mut GattDatabase, csis: &CoordinatedSetIdentificationService) -> Self {
        let service_handle = db.add_service(cap_uuid::COMMON_AUDIO_SERVICE, true);
        let csis_include_handle = db.add_include(
            csis.service_handle,
            // CSIS's group ends at its last characteristic value (Set Member Rank).
            csis.rank_value_handle,
            Some(csip_uuid::CSIS_SERVICE),
        );

        Self {
            service_handle,
            csis_include_handle: Some(csis_include_handle),
        }
    }

    /// Registers CAS for a standalone (non-coordinated-set) device: no CSIS include.
    pub fn register_standalone(db: &mut GattDatabase) -> Self {
        Self {
            service_handle: db.add_service(cap_uuid::COMMON_AUDIO_SERVICE, true),
            csis_include_handle: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gatt::service_uuid;
    use crate::types::Uuid;

    #[test]
    fn test_cas_includes_csis() {
        let mut db = GattDatabase::new();
        let csis = CoordinatedSetIdentificationService::register(&mut db, [0xAA; 16], 2, 1);
        let cas = CommonAudioService::register(&mut db, &csis);

        let include_handle = cas.csis_include_handle.unwrap();
        let includes = db.read_by_type(
            cas.service_handle,
            include_handle,
            Uuid::from_u16(service_uuid::INCLUDE),
        );
        assert_eq!(includes.len(), 1);

        // Include value: CSIS service handle + end group handle + 16-bit CSIS UUID.
        let mut expected = Vec::new();
        expected.extend_from_slice(&csis.service_handle.to_le_bytes());
        expected.extend_from_slice(&csis.rank_value_handle.to_le_bytes());
        expected.extend_from_slice(&0x1846u16.to_le_bytes());
        assert_eq!(includes[0].1, expected.as_slice());
    }

    #[test]
    fn test_standalone_cas_has_no_include() {
        let mut db = GattDatabase::new();
        let cas = CommonAudioService::register_standalone(&mut db);

        assert!(cas.csis_include_handle.is_none());
        let includes = db.read_by_type(1, 0xFFFF, Uuid::from_u16(service_uuid::INCLUDE));
        assert!(includes.is_empty());
    }
}
