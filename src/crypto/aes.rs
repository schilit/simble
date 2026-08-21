// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Self-contained AES-128 and AES-CMAC (RFC 4493 / NIST SP 800-38B) implementation.

/// Standard AES S-Box (FIPS 197)
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// Round constants
const RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

#[inline(always)]
fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 }
}

#[inline(always)]
fn mix_col(col: [u8; 4]) -> [u8; 4] {
    let [s0, s1, s2, s3] = col;
    [
        xtime(s0) ^ xtime(s1) ^ s1 ^ s2 ^ s3,
        s0 ^ xtime(s1) ^ xtime(s2) ^ s2 ^ s3,
        s0 ^ s1 ^ xtime(s2) ^ xtime(s3) ^ s3,
        xtime(s0) ^ s0 ^ s1 ^ s2 ^ xtime(s3),
    ]
}

/// Key expansion for AES-128 (10 rounds -> 11 round keys of 16 bytes each = 176 bytes).
fn expand_key(key: &[u8; 16]) -> [u8; 176] {
    let mut w = [0u8; 176];
    w[0..16].copy_from_slice(key);

    for i in 4..44 {
        let mut temp = [
            w[(i - 1) * 4],
            w[(i - 1) * 4 + 1],
            w[(i - 1) * 4 + 2],
            w[(i - 1) * 4 + 3],
        ];

        if i % 4 == 0 {
            let t = temp[0];
            temp[0] = SBOX[temp[1] as usize] ^ RCON[i / 4];
            temp[1] = SBOX[temp[2] as usize];
            temp[2] = SBOX[temp[3] as usize];
            temp[3] = SBOX[t as usize];
        }

        w[i * 4] = w[(i - 4) * 4] ^ temp[0];
        w[i * 4 + 1] = w[(i - 4) * 4 + 1] ^ temp[1];
        w[i * 4 + 2] = w[(i - 4) * 4 + 2] ^ temp[2];
        w[i * 4 + 3] = w[(i - 4) * 4 + 3] ^ temp[3];
    }

    w
}

/// Encrypts a single 16-byte block with AES-128.
pub fn aes_128_encrypt_block(key: &[u8; 16], block: &[u8; 16]) -> [u8; 16] {
    let rk = expand_key(key);
    let mut state = *block;

    // Initial Round
    for i in 0..16 {
        state[i] ^= rk[i];
    }

    // 9 Main Rounds
    for round in 1..10 {
        // SubBytes
        for b in &mut state {
            *b = SBOX[*b as usize];
        }

        // ShiftRows (4x4 column-major state)
        let s = state;
        state[1] = s[5];
        state[5] = s[9];
        state[9] = s[13];
        state[13] = s[1];

        state[2] = s[10];
        state[6] = s[14];
        state[10] = s[2];
        state[14] = s[6];

        state[3] = s[15];
        state[7] = s[3];
        state[11] = s[7];
        state[15] = s[11];

        // MixColumns
        for c in 0..4 {
            let col = [
                state[c * 4],
                state[c * 4 + 1],
                state[c * 4 + 2],
                state[c * 4 + 3],
            ];
            let mixed = mix_col(col);
            state[c * 4..c * 4 + 4].copy_from_slice(&mixed);
        }

        // AddRoundKey
        let offset = round * 16;
        for i in 0..16 {
            state[i] ^= rk[offset + i];
        }
    }

    // Final Round (No MixColumns)
    for b in &mut state {
        *b = SBOX[*b as usize];
    }

    let s = state;
    state[1] = s[5];
    state[5] = s[9];
    state[9] = s[13];
    state[13] = s[1];

    state[2] = s[10];
    state[6] = s[14];
    state[10] = s[2];
    state[14] = s[6];

    state[3] = s[15];
    state[7] = s[3];
    state[11] = s[7];
    state[15] = s[11];

    for i in 0..16 {
        state[i] ^= rk[160 + i];
    }

    state
}

/// Left shift a 16-byte big-endian block by 1 bit.
fn left_shift_128(input: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut overflow = 0u8;
    for i in (0..16).rev() {
        out[i] = (input[i] << 1) | overflow;
        overflow = (input[i] >> 7) & 1;
    }
    out
}

/// Generates CMAC subkeys K1 and K2 as specified in RFC 4493.
fn generate_subkeys(key: &[u8; 16]) -> ([u8; 16], [u8; 16]) {
    let const_rb = 0x87u8;
    let l = aes_128_encrypt_block(key, &[0u8; 16]);

    let mut k1 = left_shift_128(&l);
    if l[0] & 0x80 != 0 {
        k1[15] ^= const_rb;
    }

    let mut k2 = left_shift_128(&k1);
    if k1[0] & 0x80 != 0 {
        k2[15] ^= const_rb;
    }

    (k1, k2)
}

/// Computes AES-128 CMAC over `data` with `key` (RFC 4493 / NIST SP 800-38B).
pub fn aes_cmac(key: &[u8; 16], data: &[u8]) -> [u8; 16] {
    let (k1, k2) = generate_subkeys(key);
    let n = if data.is_empty() {
        1
    } else {
        data.len().div_ceil(16)
    };
    let flag = !data.is_empty() && data.len().is_multiple_of(16);

    let mut m_last = [0u8; 16];
    if flag {
        let last_chunk = &data[(n - 1) * 16..n * 16];
        m_last.copy_from_slice(last_chunk);
        for i in 0..16 {
            m_last[i] ^= k1[i];
        }
    } else {
        let last_chunk = if data.is_empty() {
            &[]
        } else {
            &data[(n - 1) * 16..]
        };
        m_last[..last_chunk.len()].copy_from_slice(last_chunk);
        m_last[last_chunk.len()] = 0x80;
        for i in 0..16 {
            m_last[i] ^= k2[i];
        }
    }

    let mut x = [0u8; 16];
    for i in 0..(n - 1) {
        let chunk = &data[i * 16..(i + 1) * 16];
        let mut y = [0u8; 16];
        for j in 0..16 {
            y[j] = x[j] ^ chunk[j];
        }
        x = aes_128_encrypt_block(key, &y);
    }

    let mut y = [0u8; 16];
    for j in 0..16 {
        y[j] = x[j] ^ m_last[j];
    }
    aes_128_encrypt_block(key, &y)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fips197_appendix_a1_encrypt() {
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];
        let pt = [
            0x32, 0x43, 0xf6, 0xa8, 0x88, 0x5a, 0x30, 0x8d, 0x31, 0x31, 0x98, 0xa2, 0xe0, 0x37,
            0x07, 0x34,
        ];
        let ct = aes_128_encrypt_block(&key, &pt);
        assert_eq!(
            ct,
            [
                0x39, 0x25, 0x84, 0x1d, 0x02, 0xdc, 0x09, 0xfb, 0xdc, 0x11, 0x85, 0x97, 0x19, 0x6a,
                0x0b, 0x32,
            ]
        );
    }

    #[test]
    fn test_rfc4493_aes_cmac_test_vectors() {
        let key = [
            0x2b, 0x7e, 0x15, 0x16, 0x28, 0xae, 0xd2, 0xa6, 0xab, 0xf7, 0x15, 0x88, 0x09, 0xcf,
            0x4f, 0x3c,
        ];

        // Example 1: len = 0
        let mac0 = aes_cmac(&key, &[]);
        assert_eq!(
            mac0,
            [
                0xbb, 0x1d, 0x69, 0x29, 0xe9, 0x59, 0x37, 0x28, 0x7f, 0xa3, 0x7d, 0x12, 0x9b, 0x75,
                0x67, 0x46,
            ]
        );

        // Example 2: len = 16
        let msg16 = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a,
        ];
        let mac16 = aes_cmac(&key, &msg16);
        assert_eq!(
            mac16,
            [
                0x07, 0x0a, 0x16, 0xb4, 0x6b, 0x4d, 0x41, 0x44, 0xf7, 0x9b, 0xdd, 0x9d, 0xd0, 0x4a,
                0x28, 0x7c,
            ]
        );

        // Example 3: len = 40
        let msg40 = [
            0x6b, 0xc1, 0xbe, 0xe2, 0x2e, 0x40, 0x9f, 0x96, 0xe9, 0x3d, 0x7e, 0x11, 0x73, 0x93,
            0x17, 0x2a, 0xae, 0x2d, 0x8a, 0x57, 0x1e, 0x03, 0xac, 0x9c, 0x9e, 0xb7, 0x6f, 0xac,
            0x45, 0xaf, 0x8e, 0x51, 0x30, 0xc8, 0x1c, 0x46, 0xa3, 0x5c, 0xe4, 0x11,
        ];
        let mac40 = aes_cmac(&key, &msg40);
        assert_eq!(
            mac40,
            [
                0xdf, 0xa6, 0x67, 0x47, 0xde, 0x9a, 0xe6, 0x30, 0x30, 0xca, 0x32, 0x61, 0x14, 0x97,
                0xc8, 0x27,
            ]
        );
    }
}
