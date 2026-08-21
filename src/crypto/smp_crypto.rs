// Copyright 2026 Bill Schilit
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
pub fn rev<const N: usize>(slice: &[u8; N]) -> [u8; N] {
    let mut out = [0u8; N];
    for i in 0..N {
        out[i] = slice[N - 1 - i];
    }
    out
}

/// Security function `e` (Section 2.2.1): AES-128 encryption of a 128-bit
/// block. The spec defines `e` over big-endian values, but SMP's own values
/// (keys, nonces, addresses) are all little-endian on the wire, so `e`
/// reverses key/plaintext going in and the ciphertext coming out around the
/// raw big-endian AES-128 primitive.
fn e(k: &[u8; 16], x: &[u8; 16]) -> [u8; 16] {
    rev(&aes_128_encrypt_block(&rev(k), &rev(x)))
}

/// Random Address Hash function `ah` (Bluetooth Core Vol 3, Part H, Section 2.2.2).
///
/// Used for resolving Resolvable Private Addresses (RPA) against an Identity Resolving Key (IRK).
pub fn ah(k: &[u8; 16], r: &[u8; 3]) -> [u8; 3] {
    let mut r_prime = [0u8; 16];
    r_prime[0..3].copy_from_slice(r);
    let cipher = e(k, &r_prime);
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
    let res1 = e(k, &xor16(r, &p1));
    e(k, &xor16(&res1, &p2))
}

/// Key generation function `s1` for LE Legacy Pairing (Section 2.2.4).
pub fn s1(k: &[u8; 16], r1: &[u8; 16], r2: &[u8; 16]) -> [u8; 16] {
    let mut text = [0u8; 16];
    text[0..8].copy_from_slice(&r2[0..8]);
    text[8..16].copy_from_slice(&r1[0..8]);
    e(k, &text)
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

/// LE Secure Connections Key Generation Function `f5` (Section 2.2.7).
///
/// Returns `(MacKey, LTK)`.
pub fn f5(
    w: &[u8; 32],
    n1: &[u8; 16],
    n2: &[u8; 16],
    a1: &[u8; 7],
    a2: &[u8; 7],
) -> ([u8; 16], [u8; 16]) {
    let salt: [u8; 16] = [
        0x6C, 0x88, 0x83, 0x91, 0xAA, 0xF5, 0xA5, 0x38, 0x60, 0x37, 0x0B, 0xDB, 0x5A, 0x60, 0x83,
        0xBE,
    ];
    let t = aes_cmac(&salt, &rev(w));
    let key_id = [0x62, 0x74, 0x6C, 0x65];

    let mut msg = Vec::with_capacity(1 + 4 + 16 + 16 + 7 + 7 + 2);
    msg.push(0);
    msg.extend_from_slice(&key_id);
    msg.extend_from_slice(&rev(n1));
    msg.extend_from_slice(&rev(n2));
    msg.extend_from_slice(&rev(a1));
    msg.extend_from_slice(&rev(a2));
    msg.extend_from_slice(&[1, 0]);
    let mac_key = rev(&aes_cmac(&t, &msg));

    msg[0] = 1;
    let ltk = rev(&aes_cmac(&t, &msg));

    (mac_key, ltk)
}

/// LE Secure Connections Check Value Generation Function `f6` (Section 2.2.8).
pub fn f6(
    mac_key: &[u8; 16],
    n1: &[u8; 16],
    n2: &[u8; 16],
    r: &[u8; 16],
    io_cap: &[u8; 3],
    a1: &[u8; 7],
    a2: &[u8; 7],
) -> [u8; 16] {
    let mut msg = Vec::with_capacity(16 + 16 + 16 + 3 + 7 + 7);
    msg.extend_from_slice(&rev(n1));
    msg.extend_from_slice(&rev(n2));
    msg.extend_from_slice(&rev(r));
    msg.extend_from_slice(&rev(io_cap));
    msg.extend_from_slice(&rev(a1));
    msg.extend_from_slice(&rev(a2));

    let key = rev(mac_key);
    rev(&aes_cmac(&key, &msg))
}

/// Link Key conversion function `h6` (Section 2.2.10), used for cross-transport key derivation.
pub fn h6(w: &[u8; 16], key_id: &[u8; 4]) -> [u8; 16] {
    rev(&aes_cmac(&rev(w), key_id))
}

/// Link Key conversion function `h7` (Section 2.2.11), used for cross-transport key derivation.
pub fn h7(salt: &[u8; 16], w: &[u8; 16]) -> [u8; 16] {
    rev(&aes_cmac(salt, &rev(w)))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vectors below are the little-endian (wire order) byte values from
    // Bluetooth Core Spec Vol 3, Part H worked examples, exactly as consumed
    // by Bumble's `smp_test.py` (which reverses the spec's big-endian hex
    // strings before passing them to its crypto functions).

    #[test]
    fn test_ah_random_address_resolution() {
        let irk = [
            0x9B, 0x7D, 0x39, 0x0A, 0xA6, 0x10, 0x10, 0x34, 0x05, 0xAD, 0xC8, 0x57, 0xA3, 0x34,
            0x02, 0xEC,
        ];
        let prand = [0x94, 0x81, 0x70];
        let expected = [0xAA, 0xFB, 0x0D];
        assert_eq!(ah(&irk, &prand), expected);
    }

    #[test]
    fn test_s1_key_generation() {
        let k = [0u8; 16];
        let r1 = [
            0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E,
            0x0F, 0x00,
        ];
        let r2 = [
            0x00, 0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA, 0x99, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03,
            0x02, 0x01,
        ];
        let expected = [
            0x62, 0xA0, 0x6D, 0x79, 0xAE, 0x16, 0x42, 0x5B, 0x9B, 0xF4, 0xB0, 0xE8, 0xF0, 0xE1,
            0x1F, 0x9A,
        ];
        assert_eq!(s1(&k, &r1, &r2), expected);
    }

    #[test]
    fn test_c1_confirm_value_generation() {
        let k = [0u8; 16];
        let r = [
            0xE0, 0x2E, 0x70, 0xC6, 0x4E, 0x27, 0x88, 0x63, 0x0E, 0x6F, 0xAD, 0x56, 0x21, 0xD5,
            0x83, 0x57,
        ];
        let pres = [0x02, 0x03, 0x00, 0x00, 0x08, 0x00, 0x05];
        let preq = [0x01, 0x01, 0x00, 0x00, 0x10, 0x07, 0x07];
        let iat = 1;
        let ia = [0xA6, 0xA5, 0xA4, 0xA3, 0xA2, 0xA1];
        let rat = 0;
        let ra = [0xB6, 0xB5, 0xB4, 0xB3, 0xB2, 0xB1];
        let expected = [
            0x86, 0x3B, 0xF1, 0xBE, 0xC5, 0x4D, 0xA7, 0xD2, 0xEA, 0x88, 0x89, 0x87, 0xEF, 0x3F,
            0x1E, 0x1E,
        ];
        assert_eq!(c1(&k, &r, &preq, &pres, iat, rat, &ia, &ra), expected);
    }

    #[test]
    fn test_f4_confirm_value_generation() {
        let u = [
            0xE6, 0x9D, 0x35, 0x0E, 0x48, 0x01, 0x03, 0xCC, 0xDB, 0xFD, 0xF4, 0xAC, 0x11, 0x91,
            0xF4, 0xEF, 0xB9, 0xA5, 0xF9, 0xE9, 0xA7, 0x83, 0x2C, 0x5E, 0x2C, 0xBE, 0x97, 0xF2,
            0xD2, 0x03, 0xB0, 0x20,
        ];
        let v = [
            0xFD, 0xC5, 0x7F, 0xF4, 0x49, 0xDD, 0x4F, 0x6B, 0xFB, 0x7C, 0x9D, 0xF1, 0xC2, 0x9A,
            0xCB, 0x59, 0x2A, 0xE7, 0xD4, 0xEE, 0xFB, 0xFC, 0x0A, 0x90, 0x9A, 0xBB, 0xF6, 0x32,
            0x3D, 0x8B, 0x18, 0x55,
        ];
        let x = [
            0xAB, 0xAE, 0x2B, 0x71, 0xEC, 0xB2, 0xFF, 0xFF, 0x3E, 0x73, 0x77, 0xD1, 0x54, 0x84,
            0xCB, 0xD5,
        ];
        let expected = [
            0x2D, 0x87, 0x74, 0xA9, 0xBE, 0xA1, 0xED, 0xF1, 0x1C, 0xBD, 0xA9, 0x07, 0xF1, 0x16,
            0xC9, 0xF2,
        ];
        assert_eq!(f4(&u, &v, &x, 0), expected);
    }

    #[test]
    fn test_g2_numeric_comparison_value_generation() {
        let u = [
            0xE6, 0x9D, 0x35, 0x0E, 0x48, 0x01, 0x03, 0xCC, 0xDB, 0xFD, 0xF4, 0xAC, 0x11, 0x91,
            0xF4, 0xEF, 0xB9, 0xA5, 0xF9, 0xE9, 0xA7, 0x83, 0x2C, 0x5E, 0x2C, 0xBE, 0x97, 0xF2,
            0xD2, 0x03, 0xB0, 0x20,
        ];
        let v = [
            0xFD, 0xC5, 0x7F, 0xF4, 0x49, 0xDD, 0x4F, 0x6B, 0xFB, 0x7C, 0x9D, 0xF1, 0xC2, 0x9A,
            0xCB, 0x59, 0x2A, 0xE7, 0xD4, 0xEE, 0xFB, 0xFC, 0x0A, 0x90, 0x9A, 0xBB, 0xF6, 0x32,
            0x3D, 0x8B, 0x18, 0x55,
        ];
        let x = [
            0xAB, 0xAE, 0x2B, 0x71, 0xEC, 0xB2, 0xFF, 0xFF, 0x3E, 0x73, 0x77, 0xD1, 0x54, 0x84,
            0xCB, 0xD5,
        ];
        let y = [
            0xCF, 0xC4, 0x3D, 0xFF, 0xF7, 0x83, 0x65, 0x21, 0x6E, 0x5F, 0xA7, 0x25, 0xCC, 0xE7,
            0xE8, 0xA6,
        ];
        // Simble's `g2` already applies the `% 1_000_000` truncation that
        // Bumble's callers do separately after invoking its (untruncated) `g2`.
        assert_eq!(g2(&u, &v, &x, &y), 0x2F9ED5BA % 1_000_000);
    }

    #[test]
    fn test_f5_key_generation() {
        let w = [
            0x98, 0xA6, 0xBF, 0x73, 0xF3, 0x34, 0x8D, 0x86, 0xF1, 0x66, 0xF8, 0xB4, 0x13, 0x6B,
            0x79, 0x99, 0x9B, 0x7D, 0x39, 0x0A, 0xA6, 0x10, 0x10, 0x34, 0x05, 0xAD, 0xC8, 0x57,
            0xA3, 0x34, 0x02, 0xEC,
        ];
        let n1 = [
            0xAB, 0xAE, 0x2B, 0x71, 0xEC, 0xB2, 0xFF, 0xFF, 0x3E, 0x73, 0x77, 0xD1, 0x54, 0x84,
            0xCB, 0xD5,
        ];
        let n2 = [
            0xCF, 0xC4, 0x3D, 0xFF, 0xF7, 0x83, 0x65, 0x21, 0x6E, 0x5F, 0xA7, 0x25, 0xCC, 0xE7,
            0xE8, 0xA6,
        ];
        let a1 = [0xCE, 0xBF, 0x37, 0x37, 0x12, 0x56, 0x00];
        let a2 = [0xC1, 0xCF, 0x2D, 0x70, 0x13, 0xA7, 0x00];
        let (mac_key, ltk) = f5(&w, &n1, &n2, &a1, &a2);
        assert_eq!(
            mac_key,
            [
                0x20, 0x6E, 0x63, 0xCE, 0x20, 0x6A, 0x3F, 0xFD, 0x02, 0x4A, 0x08, 0xA1, 0x76, 0xF1,
                0x65, 0x29,
            ]
        );
        assert_eq!(
            ltk,
            [
                0x38, 0x0A, 0x75, 0x94, 0xB5, 0x22, 0x05, 0x98, 0x23, 0xCD, 0xD7, 0x69, 0x11, 0x79,
                0x86, 0x69,
            ]
        );
    }

    #[test]
    fn test_f6_check_value_generation() {
        let n1 = [
            0xAB, 0xAE, 0x2B, 0x71, 0xEC, 0xB2, 0xFF, 0xFF, 0x3E, 0x73, 0x77, 0xD1, 0x54, 0x84,
            0xCB, 0xD5,
        ];
        let n2 = [
            0xCF, 0xC4, 0x3D, 0xFF, 0xF7, 0x83, 0x65, 0x21, 0x6E, 0x5F, 0xA7, 0x25, 0xCC, 0xE7,
            0xE8, 0xA6,
        ];
        let mac_key = [
            0x20, 0x6E, 0x63, 0xCE, 0x20, 0x6A, 0x3F, 0xFD, 0x02, 0x4A, 0x08, 0xA1, 0x76, 0xF1,
            0x65, 0x29,
        ];
        let r = [
            0xC8, 0x0F, 0x2D, 0x0C, 0xD2, 0x42, 0xDA, 0x08, 0x54, 0xBB, 0x53, 0xB4, 0x3B, 0x34,
            0xA3, 0x12,
        ];
        let io_cap = [0x02, 0x01, 0x01];
        let a1 = [0xCE, 0xBF, 0x37, 0x37, 0x12, 0x56, 0x00];
        let a2 = [0xC1, 0xCF, 0x2D, 0x70, 0x13, 0xA7, 0x00];
        let expected = [
            0x61, 0x8F, 0x95, 0xDA, 0x09, 0x0B, 0x6C, 0xD2, 0xC5, 0xE8, 0xD0, 0x9C, 0x98, 0x73,
            0xC4, 0xE3,
        ];
        assert_eq!(f6(&mac_key, &n1, &n2, &r, &io_cap, &a1, &a2), expected);
    }

    #[test]
    fn test_h6_link_key_conversion() {
        let w = [
            0x9B, 0x7D, 0x39, 0x0A, 0xA6, 0x10, 0x10, 0x34, 0x05, 0xAD, 0xC8, 0x57, 0xA3, 0x34,
            0x02, 0xEC,
        ];
        let key_id = *b"lebr";
        let expected = [
            0x99, 0x63, 0xB1, 0x80, 0xE2, 0xA9, 0xD3, 0xE8, 0x1C, 0xC9, 0x6D, 0xE7, 0x02, 0xE1,
            0x9A, 0x2D,
        ];
        assert_eq!(h6(&w, &key_id), expected);
    }

    #[test]
    fn test_h7_link_key_conversion() {
        let w = [
            0x9B, 0x7D, 0x39, 0x0A, 0xA6, 0x10, 0x10, 0x34, 0x05, 0xAD, 0xC8, 0x57, 0xA3, 0x34,
            0x02, 0xEC,
        ];
        let salt: [u8; 16] = [
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x74, 0x6D,
            0x70, 0x31,
        ];
        let expected = [
            0x11, 0x70, 0xA5, 0x75, 0x2A, 0x8C, 0x99, 0xD2, 0xEC, 0xC0, 0xA3, 0xC6, 0x97, 0x35,
            0x17, 0xFB,
        ];
        assert_eq!(h7(&salt, &w), expected);
    }
}
