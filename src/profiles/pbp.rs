// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Public Broadcast Profile (PBP).
//!
//! PBP defines no GATT service — a Public Broadcast Source just adds a Public Broadcast
//! Announcement (Service Data for UUID 0x1856) to the same extended advertisement that
//! carries BAP's Broadcast Audio Announcement (see [`crate::profiles::bap`]), declaring
//! encryption and audio-quality features plus program metadata so sinks can pick a
//! broadcast without syncing to its periodic advertising first (PBP Section 4).

/// Advertising Service Data UUID for the Public Broadcast Announcement. Not a
/// registered GATT service — it only ever appears inside AD Service Data.
pub mod pbp_uuid {
    use crate::types::Uuid;

    /// Public Broadcast Announcement Service UUID.
    pub const PUBLIC_BROADCAST_ANNOUNCEMENT_SERVICE: Uuid = Uuid::Uuid16(0x1856);
}

/// Public Broadcast Announcement Features bitmask (PBP Section 4.1).
pub mod features {
    /// Encrypted.
    pub const ENCRYPTED: u8 = 1 << 0;
    /// Standard Quality Configuration.
    pub const STANDARD_QUALITY_CONFIGURATION: u8 = 1 << 1;
    /// High Quality Configuration.
    pub const HIGH_QUALITY_CONFIGURATION: u8 = 1 << 2;
}

/// Public Broadcast Announcement AD structure payload (PBP Section 4.1): a [`features`]
/// bitmask plus program metadata. Metadata is the same opaque LTV blob shape BAP's
/// announcement structures carry (see `crate::profiles::bap` module docs for why it is
/// not decoded per-tag).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicBroadcastAnnouncement {
    /// Features.
    pub features: u8,
    /// Metadata.
    pub metadata: Vec<u8>,
}

impl PublicBroadcastAnnouncement {
    /// Serializes to the characteristic wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(2 + self.metadata.len());
        buf.push(self.features);
        buf.push(self.metadata.len() as u8);
        buf.extend_from_slice(&self.metadata);
        buf
    }

    /// Parses a value from its wire bytes.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let features = *data.first()?;
        let metadata_length = *data.get(1)? as usize;
        let metadata = data.get(2..2 + metadata_length)?.to_vec();
        Some(Self { features, metadata })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip() {
        let announcement = PublicBroadcastAnnouncement {
            features: features::ENCRYPTED | features::HIGH_QUALITY_CONFIGURATION,
            metadata: vec![0x03, 0x02, 0x04, 0x00], // Streaming_Audio_Contexts: Media
        };
        assert_eq!(
            PublicBroadcastAnnouncement::parse(&announcement.to_bytes()),
            Some(announcement)
        );
    }

    #[test]
    fn test_parse_known_bytes() {
        let announcement =
            PublicBroadcastAnnouncement::parse(&[0x02, 0x03, 0xAA, 0xBB, 0xCC]).unwrap();
        assert_eq!(
            announcement.features,
            features::STANDARD_QUALITY_CONFIGURATION
        );
        assert_eq!(announcement.metadata, vec![0xAA, 0xBB, 0xCC]);
    }

    #[test]
    fn test_parse_truncated_metadata_fails() {
        assert!(PublicBroadcastAnnouncement::parse(&[0x02, 0x04, 0xAA]).is_none());
        assert!(PublicBroadcastAnnouncement::parse(&[0x02]).is_none());
    }
}
