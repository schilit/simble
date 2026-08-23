// Copyright 2026 Bill Schilit
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

    /// Csis Service UUID.
    pub const CSIS_SERVICE: Uuid = Uuid::Uuid16(0x1846);
    /// Set Identity Resolving Key characteristic UUID.
    pub const SET_IDENTITY_RESOLVING_KEY: Uuid = Uuid::Uuid16(0x2B84);
    /// Set Member Size characteristic UUID.
    pub const SET_MEMBER_SIZE: Uuid = Uuid::Uuid16(0x2B85);
    /// Set Member Lock characteristic UUID.
    pub const SET_MEMBER_LOCK: Uuid = Uuid::Uuid16(0x2B86);
    /// Set Member Rank characteristic UUID.
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
pub(crate) fn e(key: &[u8; 16], data: &[u8; 16]) -> [u8; 16] {
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

/// Builds the six-byte Resolvable Set Identifier a set member advertises, as the value of
/// AD type 0x2E (CSIS Section 4.9).
///
/// **The hash comes first.** CSIS defines `resolvableSetIdentifier = hash || prand` with the
/// least significant octet of the RSI being the least significant octet of *hash*, and
/// advertising data goes out least-significant-octet first — so the six octets on the air are
/// `hash[0..3]` then `prand[0..3]`, each little-endian. Writing it the other way round
/// (`prand || hash`, which reads more naturally and is what this function did first) produces
/// an RSI that resolves perfectly against our own resolver and against nothing else:
/// Zephyr's `bt_csip_set_member_generate_rsi` copies the hash to `rsi[0..3]` and the prand to
/// `rsi[3..6]`, and a phone does the same. A builder/scanner round trip cannot catch it —
/// only the byte order in the spec can.
///
/// `prand`'s two most significant bits are forced to `0b01` (CSIS Section 4.8; the same
/// masking Zephyr applies as `prand[2] &= 0x3F; prand[2] |= BIT(6)`), so an RSI can never be
/// mistaken for another address type. Because `prand` is little-endian here, its most
/// significant octet is index 2.
///
/// A coordinator resolves this by recomputing `sih` with each SIRK it holds and comparing
/// against the hash half. Until this existed, `sih` had exactly one caller and it was a test:
/// the crypto was correct but unreachable, so no advertisement ever carried a set identity.
pub fn rsi(sirk: &[u8; 16], prand: &[u8; 3]) -> [u8; 6] {
    let mut prand = *prand;
    prand[2] = (prand[2] & 0b0011_1111) | 0b0100_0000;
    let hash = sih(sirk, &prand);
    let mut out = [0u8; 6];
    out[..3].copy_from_slice(&hash);
    out[3..].copy_from_slice(&prand);
    out
}

/// Checks whether an advertised Resolvable Set Identifier belongs to the set identified by
/// `sirk` — the coordinator half of [`rsi`].
///
/// The slice pattern spells out the CSIS Section 4.9 layout — hash first, then prand — so the
/// one thing that can silently be backwards is stated in the code rather than in a comment.
pub fn rsi_matches(sirk: &[u8; 16], rsi: &[u8]) -> bool {
    match rsi {
        [h0, h1, h2, p0, p1, p2] => sih(sirk, &[*p0, *p1, *p2]) == [*h0, *h1, *h2],
        _ => false,
    }
}

/// Coordinated Set Identification Service GATT container.
#[derive(Debug, Clone)]
pub struct CoordinatedSetIdentificationService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Attribute handle of the Sirk.
    pub sirk_handle: u16,
    /// Value attribute handle of the Sirk characteristic.
    pub sirk_value_handle: u16,
    /// Attribute handle of the Size.
    pub size_handle: u16,
    /// Value attribute handle of the Size characteristic.
    pub size_value_handle: u16,
    /// Attribute handle of the Lock.
    pub lock_handle: u16,
    /// Value attribute handle of the Lock characteristic.
    pub lock_value_handle: u16,
    /// Attribute handle of the Rank.
    pub rank_handle: u16,
    /// Value attribute handle of the Rank characteristic.
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
