// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Encrypted Advertising Data (EAD), introduced in Core Spec 5.4.
//!
//! An Encrypted Data AD structure (AD type 0x31, Core Spec Supplement, Part A,
//! Section 1.23) wraps other AD structures in AES-CCM:
//!
//! ```text
//! Length | 0x31 | Randomizer (5) | Encrypted AD payload | MIC (4)
//! ```
//!
//! The session key and IV come from the peer's Encrypted Data Key Material
//! characteristic (UUID 0x2B88, Core Spec Vol 3, Part C, Section 12.6). The
//! CCM nonce is `Randomizer || IV` (13 octets, so length field size L = 2),
//! the MIC is 4 octets, and the additional authenticated data is the single
//! octet 0xEA (Supplement Part A, Section 1.23.3).

use crate::crypto::ccm::{ccm_decrypt, ccm_encrypt};
use serde::{Deserialize, Serialize};

/// AD type for an Encrypted Data structure (Assigned Numbers, Common Data Types).
pub const ENCRYPTED_ADVERTISING_DATA_AD_TYPE: u8 = 0x31;

/// GATT characteristic UUID for Encrypted Data Key Material (Assigned Numbers).
pub const KEY_MATERIAL_CHARACTERISTIC_UUID: u16 = 0x2B88;

/// Randomizer field size in octets (Supplement Part A, Section 1.23.2).
pub const RANDOMIZER_SIZE: usize = 5;

/// MIC size in octets (Supplement Part A, Section 1.23.3).
pub const MIC_SIZE: usize = 4;

/// CCM additional authenticated data: the fixed octet 0xEA (Supplement Part A,
/// Section 1.23.3).
const AAD: [u8; 1] = [0xEA];

/// Session key + IV pair shared via the Encrypted Data Key Material
/// characteristic (UUID 0x2B88).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyMaterial {
    /// 16-octet AES-CCM session key.
    pub session_key: [u8; 16],
    /// 8-octet initialization vector, forming the low half of the CCM nonce.
    pub iv: [u8; 8],
}

impl KeyMaterial {
    /// Characteristic value length: Session Key (16) followed by IV (8).
    pub const LENGTH: usize = 24;

    /// Serializes as the 0x2B88 characteristic value: Session Key octets
    /// followed by IV octets, in over-the-air (least-significant-octet-first)
    /// order per the GATT Specification Supplement.
    // `&self` rather than `self` on a `Copy` type is what clippy objects to,
    // and changing it is a signature change to settle separately -- this task
    // moves visibility, not shapes. Reported only because `gap` is
    // `pub(crate)` without `testing`; `avoid-breaking-exported-api` hid it
    // while the module was unconditionally `pub`.
    #[allow(clippy::wrong_self_convention)]
    pub fn to_bytes(&self) -> [u8; Self::LENGTH] {
        let mut out = [0u8; Self::LENGTH];
        out[..16].copy_from_slice(&self.session_key);
        out[16..].copy_from_slice(&self.iv);
        out
    }

    /// Parses a 0x2B88 characteristic value; `None` unless exactly 24 octets.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != Self::LENGTH {
            return None;
        }
        let mut session_key = [0u8; 16];
        session_key.copy_from_slice(&bytes[..16]);
        let mut iv = [0u8; 8];
        iv.copy_from_slice(&bytes[16..]);
        Some(Self { session_key, iv })
    }
}

/// Builds the 13-octet CCM nonce `Randomizer || IV` (Supplement Part A,
/// Section 1.23.3).
fn nonce(randomizer: &[u8; RANDOMIZER_SIZE], iv: &[u8; 8]) -> [u8; 13] {
    let mut n = [0u8; 13];
    n[..RANDOMIZER_SIZE].copy_from_slice(randomizer);
    n[RANDOMIZER_SIZE..].copy_from_slice(iv);
    n
}

/// Encrypts `plaintext_ad` (one or more complete AD structures) into a full
/// Encrypted Data AD structure: `Length || 0x31 || Randomizer || Payload || MIC`.
///
/// The Randomizer is caller-supplied because the spec requires a fresh random
/// value per advertising event (Supplement Part A, Section 1.23.2) and key
/// generation policy belongs to the caller, not this codec.
pub fn encrypt_ad(
    key_material: &KeyMaterial,
    randomizer: &[u8; RANDOMIZER_SIZE],
    plaintext_ad: &[u8],
) -> Vec<u8> {
    let n = nonce(randomizer, &key_material.iv);
    let ciphertext_and_mic =
        ccm_encrypt(&key_material.session_key, &n, &AAD, plaintext_ad, MIC_SIZE);

    let value_len = 1 + RANDOMIZER_SIZE + ciphertext_and_mic.len();
    let mut out = Vec::with_capacity(1 + value_len);
    // AD structure Length covers the AD type octet plus the value.
    out.push(value_len as u8);
    out.push(ENCRYPTED_ADVERTISING_DATA_AD_TYPE);
    out.extend_from_slice(randomizer);
    out.extend_from_slice(&ciphertext_and_mic);
    out
}

/// Decrypts the value of an Encrypted Data AD structure (the octets after the
/// Length and 0x31 type octets: `Randomizer || Payload || MIC`), returning the
/// wrapped AD structures, or `None` if the structure is malformed or the MIC
/// does not authenticate under `key_material`.
pub fn decrypt_ad(key_material: &KeyMaterial, ad_payload: &[u8]) -> Option<Vec<u8>> {
    if ad_payload.len() < RANDOMIZER_SIZE + MIC_SIZE {
        return None;
    }
    let mut randomizer = [0u8; RANDOMIZER_SIZE];
    randomizer.copy_from_slice(&ad_payload[..RANDOMIZER_SIZE]);
    let n = nonce(&randomizer, &key_material.iv);
    ccm_decrypt(
        &key_material.session_key,
        &n,
        &AAD,
        &ad_payload[RANDOMIZER_SIZE..],
        MIC_SIZE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // No external EAD vector is asserted here: Bumble stops at the AD-structure
    // framing (no EAD crypto to port) and the Core Spec Supplement's worked
    // example could not be reproduced verbatim from memory. The CCM core is
    // pinned to RFC 3610 vectors in `crypto::ccm`; these tests pin the EAD
    // framing and nonce/AAD construction via round-trip and negative cases.

    fn test_key_material() -> KeyMaterial {
        KeyMaterial {
            session_key: [
                0x57, 0x83, 0xD5, 0x21, 0x56, 0xAD, 0x6F, 0x0E, 0x63, 0x88, 0x27, 0x4E, 0xC6, 0x70,
                0x2E, 0xE0,
            ],
            iv: [0x6E, 0x9F, 0x4A, 0x12, 0x70, 0x15, 0x05, 0xA9],
        }
    }

    const RANDOMIZER: [u8; RANDOMIZER_SIZE] = [0x18, 0xE1, 0x57, 0xCA, 0xDE];

    // A complete inner AD structure: Complete Local Name "Short Mini-Bus".
    const PLAINTEXT_AD: &[u8] = &[
        0x0F, 0x09, 0x53, 0x68, 0x6F, 0x72, 0x74, 0x20, 0x4D, 0x69, 0x6E, 0x69, 0x2D, 0x42, 0x75,
        0x73,
    ];

    #[test]
    fn test_encrypt_ad_structure_framing() {
        let ad = encrypt_ad(&test_key_material(), &RANDOMIZER, PLAINTEXT_AD);
        assert_eq!(
            ad.len(),
            2 + RANDOMIZER_SIZE + PLAINTEXT_AD.len() + MIC_SIZE
        );
        assert_eq!(ad[0] as usize, ad.len() - 1);
        assert_eq!(ad[1], ENCRYPTED_ADVERTISING_DATA_AD_TYPE);
        assert_eq!(&ad[2..7], &RANDOMIZER);
        // CCM's CTR mode must actually mask the payload.
        assert_ne!(&ad[7..7 + PLAINTEXT_AD.len()], PLAINTEXT_AD);
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let km = test_key_material();
        let ad = encrypt_ad(&km, &RANDOMIZER, PLAINTEXT_AD);
        let decrypted = decrypt_ad(&km, &ad[2..]).expect("MIC must verify");
        assert_eq!(decrypted, PLAINTEXT_AD);
    }

    #[test]
    fn test_wrong_key_rejected() {
        let km = test_key_material();
        let ad = encrypt_ad(&km, &RANDOMIZER, PLAINTEXT_AD);
        let mut wrong = km;
        wrong.session_key[0] ^= 0x01;
        assert!(decrypt_ad(&wrong, &ad[2..]).is_none());
    }

    #[test]
    fn test_wrong_iv_rejected() {
        let km = test_key_material();
        let ad = encrypt_ad(&km, &RANDOMIZER, PLAINTEXT_AD);
        let mut wrong = km;
        wrong.iv[7] ^= 0x01;
        assert!(decrypt_ad(&wrong, &ad[2..]).is_none());
    }

    #[test]
    fn test_tampered_payload_and_mic_rejected() {
        let km = test_key_material();
        let ad = encrypt_ad(&km, &RANDOMIZER, PLAINTEXT_AD);

        let mut tampered = ad.clone();
        tampered[8] ^= 0x40;
        assert!(decrypt_ad(&km, &tampered[2..]).is_none());

        let mut tampered = ad.clone();
        *tampered.last_mut().unwrap() ^= 0x01;
        assert!(decrypt_ad(&km, &tampered[2..]).is_none());

        // Tampering the Randomizer changes the nonce, so the MIC must fail too.
        let mut tampered = ad;
        tampered[2] ^= 0x01;
        assert!(decrypt_ad(&km, &tampered[2..]).is_none());
    }

    #[test]
    fn test_truncated_payload_rejected() {
        let km = test_key_material();
        assert!(decrypt_ad(&km, &[0u8; RANDOMIZER_SIZE + MIC_SIZE - 1]).is_none());
    }

    #[test]
    fn test_key_material_serialization_roundtrip() {
        let km = test_key_material();
        let bytes = km.to_bytes();
        assert_eq!(bytes.len(), KeyMaterial::LENGTH);
        assert_eq!(&bytes[..16], &km.session_key);
        assert_eq!(&bytes[16..], &km.iv);
        assert_eq!(KeyMaterial::from_bytes(&bytes), Some(km));
        assert_eq!(KeyMaterial::from_bytes(&bytes[..23]), None);
    }
}
