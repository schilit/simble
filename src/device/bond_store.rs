// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Bonded-peer record store, following NimBLE's `ble_store` pattern: typed
//! record kinds (peer security material, per-peer CCCD subscription state)
//! keyed by peer identity address, behind a trait so alternative backends
//! (e.g. a JSON-file store) can slot in without touching the device layer.
//!
//! Relationship to [`crate::smp::KeyStore`]: `KeyStore` is the
//! Bumble-`JsonKeyStore`-compatible *persistence format* — string-named
//! peers, JSON hex serialization. `BondStore` is the *runtime* store the
//! device consults on (re)connection, so it is keyed by [`Address`] and
//! carries state the Bumble format has no fields for (Secure Connections
//! flag, negotiated key size, CCCD subscriptions). Rather than duplicating
//! the key material schema, [`BondSecurity`] embeds the same
//! [`PairingKeys`] record `KeyStore` stores, so a future file-backed
//! `BondStore` can round-trip its key material through `KeyStore` directly.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::smp::PairingKeys;
use crate::types::Address;

/// Security material recorded for one bonded peer, mirroring NimBLE's
/// `ble_store_value_sec`: the distributed/derived keys plus the metadata
/// needed to judge whether the bond satisfies a service's security
/// requirements on reconnection.
///
/// Serializable so a bond can be *declared* rather than only earned: a JSON
/// scene file (`crate::scene`) states a pre-existing bond with this exact
/// shape, embedding the same [`PairingKeys`] record `KeyStore` persists, so
/// key material can move between a Bumble keystore, a scene fixture and this
/// runtime store without a translation layer to drift.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BondSecurity {
    /// LTK/EDIV/RAND, IRK, CSRK — the same record shape [`crate::smp::KeyStore`]
    /// persists, so the two stores never disagree on key schema.
    #[serde(default)]
    pub keys: PairingKeys,
    /// Whether the keys came from an LE Secure Connections pairing (a single
    /// `f5`-derived LTK) rather than LE Legacy key distribution.
    #[serde(default)]
    pub secure_connections: bool,
    /// Whether the pairing used a MITM-protected association model.
    #[serde(default)]
    pub authenticated: bool,
    /// Negotiated encryption key size in bytes (7..=16, Core Spec Vol 3,
    /// Part H, Section 2.3.4).
    #[serde(default)]
    pub key_size: u8,
}

/// Store of per-peer bonding records, keyed by peer identity address.
///
/// Requires `Send + Sync` for the same reason `AttServerObserver`
/// (`crate::device::observer`) does: `VirtualDevice` holds one behind
/// `Option<Box<dyn BondStore>>` and itself ends up behind `Arc<Mutex<_>>`
/// in `service::manager`.
pub trait BondStore: std::fmt::Debug + Send + Sync {
    /// Inserts or replaces the security record for `peer`.
    fn store_security(&mut self, peer: Address, security: BondSecurity);

    /// Loads the security record for `peer`, if bonded.
    fn load_security(&self, peer: Address) -> Option<BondSecurity>;

    /// Records `peer`'s CCCD subscription state for `cccd_handle`. A zero
    /// `value` deletes the record — an unsubscribed CCCD and an absent one
    /// are indistinguishable on restore, so nothing is kept (NimBLE's
    /// `ble_store` likewise deletes CCCD records on unsubscribe).
    fn store_cccd(&mut self, peer: Address, cccd_handle: u16, value: u16);

    /// Loads all recorded (CCCD handle, value) subscriptions for `peer`.
    fn load_cccds(&self, peer: Address) -> Vec<(u16, u16)>;

    /// Deletes every record for `peer`, returning whether any existed.
    fn delete(&mut self, peer: Address) -> bool;

    /// Lists every peer with at least one record.
    fn peers(&self) -> Vec<Address>;

    /// Whether a security record exists for `peer` (i.e. the peer is bonded;
    /// CCCD records alone don't constitute a bond).
    fn is_bonded(&self, peer: Address) -> bool {
        self.load_security(peer).is_some()
    }
}

/// All record kinds for one peer, grouped so a single map lookup finds
/// everything and `delete` can't leave orphaned sub-records.
#[derive(Debug, Clone, Default)]
struct BondRecord {
    security: Option<BondSecurity>,
    cccds: BTreeMap<u16, u16>,
}

/// Default in-memory [`BondStore`] backend.
#[derive(Debug, Clone, Default)]
pub struct MemoryBondStore {
    bonds: HashMap<Address, BondRecord>,
}

impl MemoryBondStore {
    /// Creates an empty in-memory bond store.
    pub fn new() -> Self {
        Self::default()
    }
}

impl BondStore for MemoryBondStore {
    fn store_security(&mut self, peer: Address, security: BondSecurity) {
        self.bonds.entry(peer).or_default().security = Some(security);
    }

    fn load_security(&self, peer: Address) -> Option<BondSecurity> {
        self.bonds.get(&peer)?.security.clone()
    }

    fn store_cccd(&mut self, peer: Address, cccd_handle: u16, value: u16) {
        if value == 0 {
            if let Some(record) = self.bonds.get_mut(&peer) {
                record.cccds.remove(&cccd_handle);
            }
        } else {
            self.bonds
                .entry(peer)
                .or_default()
                .cccds
                .insert(cccd_handle, value);
        }
    }

    fn load_cccds(&self, peer: Address) -> Vec<(u16, u16)> {
        self.bonds
            .get(&peer)
            .map(|record| record.cccds.iter().map(|(&h, &v)| (h, v)).collect())
            .unwrap_or_default()
    }

    fn delete(&mut self, peer: Address) -> bool {
        self.bonds.remove(&peer).is_some()
    }

    fn peers(&self) -> Vec<Address> {
        self.bonds.keys().copied().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::smp::PairingKey;

    fn addr(last: u8) -> Address {
        Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, last])
    }

    #[test]
    fn test_memory_bond_store_security_crud() {
        let mut store = MemoryBondStore::new();
        let peer = addr(0x01);
        assert!(!store.is_bonded(peer));
        assert!(store.load_security(peer).is_none());
        assert!(store.peers().is_empty());

        let mut security = BondSecurity {
            secure_connections: true,
            authenticated: false,
            key_size: 16,
            ..BondSecurity::default()
        };
        security.keys.ltk = Some(PairingKey::new([0xAB; 16]));
        store.store_security(peer, security.clone());

        assert!(store.is_bonded(peer));
        assert_eq!(store.load_security(peer), Some(security));
        assert_eq!(store.peers(), vec![peer]);

        assert!(store.delete(peer));
        assert!(!store.is_bonded(peer));
        assert!(!store.delete(peer));
    }

    #[test]
    fn test_memory_bond_store_cccd_records() {
        let mut store = MemoryBondStore::new();
        let peer = addr(0x02);

        store.store_cccd(peer, 0x0004, 0x0001);
        store.store_cccd(peer, 0x0007, 0x0002);
        assert_eq!(
            store.load_cccds(peer),
            vec![(0x0004, 0x0001), (0x0007, 0x0002)]
        );

        // Unsubscribing deletes the record rather than storing a zero.
        store.store_cccd(peer, 0x0004, 0x0000);
        assert_eq!(store.load_cccds(peer), vec![(0x0007, 0x0002)]);

        // CCCD records alone don't make the peer "bonded".
        assert!(!store.is_bonded(peer));

        assert!(store.delete(peer));
        assert!(store.load_cccds(peer).is_empty());
    }
}
