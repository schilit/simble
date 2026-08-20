// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Bluetooth Security Manager Protocol (SMP) cryptographic toolbox.
//!
//! Implements LE Legacy and LE Secure Connections algorithms per
//! Bluetooth Core Specification Vol 3, Part H, Section 2.2.

use crate::crypto::aes::{aes_128_encrypt_block, aes_cmac};

/// XOR two 16-byte arrays.
#[inline]
pub fn xor16(a: &[u8; 16], b: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    for i in 0..16 {
        out[i] = a[i] ^ b[i];
    }
    out
}

/// Reverse endianness of a slice into a fixed-size array.
#[inline]
fn rev<const N: usize>(slice: &[u8; N]) -> [u8; N] {
    let mut out = [0u8; N];
    for i in 0..N {
        out[i] = slice[N - 1 - i];
    }
    out
}

/// Random Address Hash function `ah` (Bluetooth Core Vol 3, Part H, Section 2.2.2).
///
/// Used for resolving Resolvable Private Addresses (RPA) against an Identity Resolving Key (IRK).
pub fn ah(k: &[u8; 16], r: &[u8; 3]) -> [u8; 3] {
    let mut r_prime = [0u8; 16];
    r_prime[0..3].copy_from_slice(r);
    let cipher = aes_128_encrypt_block(k, &r_prime);
    let mut out = [0u8; 3];
    out.copy_from_slice(&cipher[0..3]);
    out
}

/// Confirm value generation function `c1` for LE Legacy Pairing (Section 2.2.3).
pub fn c1(
    k: &[u8; 16],
    r: &[u8; 16],
    preq: &[u8; 7],
    pres: &[u8; 7],
    iat: u8,
    rat: u8,
    ia: &[u8; 6],
    ra: &[u8; 6],
) -> [u8; 16] {
    // p1 = iat || rat || preq || pres (16 bytes)
    let mut p1 = [0u8; 16];
    p1[0] = iat;
    p1[1] = rat;
    p1[2..9].copy_from_slice(preq);
    p1[9..16].copy_from_slice(pres);

    // p2 = ra || ia || padding(4) (16 bytes)
    let mut p2 = [0u8; 16];
    p2[0..6].copy_from_slice(ra);
    p2[6..12].copy_from_slice(ia);

    // e(k, e(k, r ^ p1) ^ p2)
    let res1 = aes_128_encrypt_block(k, &xor16(r, &p1));
    aes_128_encrypt_block(k, &xor16(&res1, &p2))
}

/// Key generation function `s1` for LE Legacy Pairing (Section 2.2.4).
pub fn s1(k: &[u8; 16], r1: &[u8; 16], r2: &[u8; 16]) -> [u8; 16] {
    let mut text = [0u8; 16];
    text[0..8].copy_from_slice(&r2[0..8]);
    text[8..16].copy_from_slice(&r1[0..8]);
    aes_128_encrypt_block(k, &text)
}

/// LE Secure Connections Confirm Value Generation Function `f4` (Section 2.2.6).
///
/// Uses AES-CMAC: $f4(U, V, X, Z) = \text{AES-CMAC}_X(U || V || Z)$
pub fn f4(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], z: u8) -> [u8; 16] {
    let mut msg = Vec::with_capacity(65);
    msg.extend_from_slice(&rev(u));
    msg.extend_from_slice(&rev(v));
    msg.push(z);

    let key = rev(x);
    let tag = aes_cmac(&key, &msg);
    rev(&tag)
}

/// LE Secure Connections Numeric Comparison Function `g2` (Section 2.2.9).
///
/// Generates a 6-digit user-confirmation decimal value.
pub fn g2(u: &[u8; 32], v: &[u8; 32], x: &[u8; 16], y: &[u8; 16]) -> u32 {
    let mut msg = Vec::with_capacity(80);
    msg.extend_from_slice(&rev(u));
    msg.extend_from_slice(&rev(v));
    msg.extend_from_slice(&rev(y));

    let key = rev(x);
    let tag = aes_cmac(&key, &msg);
    // Last 4 bytes of tag in big-endian modulo 1,000,000
    let val = u32::from_be_bytes(tag[12..16].try_into().unwrap());
    val % 1_000_000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ah_random_address_resolution() {
        let irk = [
            0x9B, 0x7B, 0xF2, 0x63, 0x9D, 0x89, 0x05, 0xF9, 0x69, 0x42, 0xEC, 0x4D, 0x6E, 0xD6,
            0x46, 0x41,
        ];
        let prand = [0x70, 0x81, 0x94];
        let hash = ah(&irk, &prand);
        assert_eq!(hash.len(), 3);
    }

    #[test]
    fn test_s1_key_generation() {
        let k = [0u8; 16];
        let r1 = [0x01; 16];
        let r2 = [0x02; 16];
        let ltk = s1(&k, &r1, &r2);
        assert_ne!(ltk, [0u8; 16]);
    }
}
