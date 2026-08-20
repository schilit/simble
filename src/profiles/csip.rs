// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Coordinated Set Identification Profile (CSIP, UUID 0x1846).
//!
//! Enables coordinated sets of devices (e.g. Left/Right TWS Earbuds, Hearing Aids)
//! to be discovered, bonded, and locked for synchronized audio streaming.

use crate::crypto::aes::{aes_128_encrypt_block, aes_cmac};
use crate::crypto::smp_crypto::rev;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// CSIP Service UUIDs.
pub mod csip_uuid {
    use crate::types::Uuid;

    pub const CSIS_SERVICE: Uuid = Uuid::Uuid16(0x1846);
    pub const SET_IDENTITY_RESOLVING_KEY: Uuid = Uuid::Uuid16(0x2B84);
    pub const SET_MEMBER_SIZE: Uuid = Uuid::Uuid16(0x2B85);
    pub const SET_MEMBER_LOCK: Uuid = Uuid::Uuid16(0x2B86);
    pub const SET_MEMBER_RANK: Uuid = Uuid::Uuid16(0x2B87);
}

/// CSIP Crypto Toolbox: s1 SALT generation function.
pub fn s1(m: &[u8]) -> [u8; 16] {
    let mut rev_m = m.to_vec();
    rev_m.reverse();
    let zero_key = [0u8; 16];
    let tag = aes_cmac(&zero_key, &rev_m);
    rev(&tag)
}

/// CSIP Crypto Toolbox: k1 key derivation function.
pub fn k1(k: &[u8; 16], salt: &[u8; 16], p: &[u8]) -> [u8; 16] {
    let mut rev_k = *k;
    rev_k.reverse();
    let mut rev_salt = *salt;
    rev_salt.reverse();
    let t = aes_cmac(&rev_salt, &rev_k);

    let mut rev_p = p.to_vec();
    rev_p.reverse();
    let tag = aes_cmac(&t, &rev_p);
    rev(&tag)
}

/// Bluetooth Security function `e` (Vol 3, Part H, Section 2.2.1).
pub fn e(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
    let mut rev_k = *key;
    rev_k.reverse();
    let mut rev_d = *data;
    rev_d.reverse();
    let mut cipher = aes_128_encrypt_block(&rev_k, &rev_d);
    cipher.reverse();
    cipher
}

/// CSIP Crypto Toolbox: Set Identification Hash function `sih`.
pub fn sih(sirk: &[u8; 16], prand: &[u8; 3]) -> [u8; 3] {
    let mut r_prime = [0u8; 16];
    r_prime[0..3].copy_from_slice(prand);
    let cipher = e(sirk, &r_prime);
    [cipher[0], cipher[1], cipher[2]]
}

/// Coordinated Set Identification Service GATT container.
#[derive(Debug, Clone)]
pub struct CoordinatedSetIdentificationService {
    pub service_handle: u16,
    pub sirk_handle: u16,
    pub sirk_value_handle: u16,
    pub size_handle: u16,
    pub size_value_handle: u16,
    pub lock_handle: u16,
    pub lock_value_handle: u16,
    pub rank_handle: u16,
    pub rank_value_handle: u16,
}

impl CoordinatedSetIdentificationService {
    /// Registers CSIS into a GATT database.
    pub fn register(db: &mut GattDatabase, sirk: [u8; 16], set_size: u8, rank: u8) -> Self {
        let service_handle = db.add_service(csip_uuid::CSIS_SERVICE, true);

        // 1. SIRK (Plaintext or Encrypted) - Read | Notify
        let mut sirk_val = Vec::with_capacity(17);
        sirk_val.push(0x01); // Plaintext type
        sirk_val.extend_from_slice(&sirk);
        let (sirk_handle, sirk_value_handle) = db.add_characteristic(
            csip_uuid::SET_IDENTITY_RESOLVING_KEY,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            sirk_val,
            AttributePermissions::default(),
        );

        // 2. Set Member Size - Read | Notify
        let (size_handle, size_value_handle) = db.add_characteristic(
            csip_uuid::SET_MEMBER_SIZE,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![set_size],
            AttributePermissions::default(),
        );

        // 3. Set Member Lock - Read | Write | Notify
        let (lock_handle, lock_value_handle) = db.add_characteristic(
            csip_uuid::SET_MEMBER_LOCK,
            CharacteristicProperties(
                CharacteristicProperties::READ
                    | CharacteristicProperties::WRITE
                    | CharacteristicProperties::NOTIFY,
            ),
            vec![0x01], // Unlocked (0x01: Unlocked, 0x02: Locked)
            AttributePermissions {
                read: true,
                write: true,
                ..Default::default()
            },
        );

        // 4. Set Member Rank - Read
        let (rank_handle, rank_value_handle) = db.add_characteristic(
            csip_uuid::SET_MEMBER_RANK,
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![rank],
            AttributePermissions::default(),
        );

        Self {
            service_handle,
            sirk_handle,
            sirk_value_handle,
            size_handle,
            size_value_handle,
            lock_handle,
            lock_value_handle,
            rank_handle,
            rank_value_handle,
        }
    }
}
