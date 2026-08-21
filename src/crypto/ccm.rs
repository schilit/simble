// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! AES-128 CCM (Counter with CBC-MAC) per RFC 3610 / NIST SP 800-38C, built on
//! the self-contained [`aes_128_encrypt_block`] primitive.
//!
//! CCM parameters are expressed the RFC 3610 way: the length-field size `L` is
//! derived from the nonce (`L = 15 - nonce_len`, valid nonces are 7..=13
//! octets) and the MIC length `M` is an even value in 4..=16. Encrypted
//! Advertising Data (Core Spec Supplement, Part A, Section 1.23.3) uses a
//! 13-octet nonce (`L = 2`) with a 4-octet MIC.

use crate::crypto::aes::aes_128_encrypt_block;

/// Computes the CBC-MAC authentication tag (RFC 3610 Section 2.2), returning
/// the full 16-byte `X_n` from which the leading `mic_len` octets are taken.
fn cbc_mac(key: &[u8; 16], nonce: &[u8], aad: &[u8], message: &[u8], mic_len: usize) -> [u8; 16] {
    let l = 15 - nonce.len();

    // B_0: Flags || Nonce || l(m), with Flags = 64*Adata + 8*M' + L' where
    // M' = (M-2)/2 and L' = L-1 (RFC 3610 Section 2.2).
    let mut b0 = [0u8; 16];
    b0[0] = (u8::from(!aad.is_empty()) << 6) | (((mic_len as u8 - 2) / 2) << 3) | (l as u8 - 1);
    b0[1..1 + nonce.len()].copy_from_slice(nonce);
    let len_bytes = (message.len() as u64).to_be_bytes();
    b0[16 - l..].copy_from_slice(&len_bytes[8 - l..]);

    let mut x = aes_128_encrypt_block(key, &b0);

    // Additional data is prefixed with l(a) encoded in 2 octets (RFC 3610
    // Section 2.2 supports longer encodings for l(a) >= 0xFF00; Bluetooth AAD
    // is always short, so only the 2-octet form is implemented).
    if !aad.is_empty() {
        assert!(
            aad.len() < 0xFF00,
            "AAD too long for 2-octet length encoding"
        );
        let mut block = [0u8; 16];
        block[..2].copy_from_slice(&(aad.len() as u16).to_be_bytes());
        let head_len = aad.len().min(14);
        block[2..2 + head_len].copy_from_slice(&aad[..head_len]);
        for (i, byte) in block.iter_mut().enumerate() {
            *byte ^= x[i];
        }
        x = aes_128_encrypt_block(key, &block);

        let mut rest = &aad[head_len..];
        while !rest.is_empty() {
            let take = rest.len().min(16);
            let mut block = x;
            for i in 0..take {
                block[i] ^= rest[i];
            }
            x = aes_128_encrypt_block(key, &block);
            rest = &rest[take..];
        }
    }

    let mut rest = message;
    while !rest.is_empty() {
        let take = rest.len().min(16);
        let mut block = x;
        for i in 0..take {
            block[i] ^= rest[i];
        }
        x = aes_128_encrypt_block(key, &block);
        rest = &rest[take..];
    }

    x
}

/// Produces the CTR keystream block `S_i = E(K, A_i)` where
/// `A_i = Flags || Nonce || i` (RFC 3610 Section 2.3).
fn keystream_block(key: &[u8; 16], nonce: &[u8], counter: u64) -> [u8; 16] {
    let l = 15 - nonce.len();
    let mut a = [0u8; 16];
    a[0] = l as u8 - 1;
    a[1..1 + nonce.len()].copy_from_slice(nonce);
    let counter_bytes = counter.to_be_bytes();
    a[16 - l..].copy_from_slice(&counter_bytes[8 - l..]);
    aes_128_encrypt_block(key, &a)
}

/// Validates the nonce/MIC parameter ranges shared by encrypt and decrypt.
fn check_parameters(nonce: &[u8], mic_len: usize) {
    assert!(
        (7..=13).contains(&nonce.len()),
        "CCM nonce must be 7..=13 octets"
    );
    assert!(
        (4..=16).contains(&mic_len) && mic_len.is_multiple_of(2),
        "CCM MIC length must be an even value in 4..=16"
    );
}

/// Encrypts `plaintext` and authenticates `aad || plaintext`, returning
/// `ciphertext || MIC` (RFC 3610 Section 2).
pub fn ccm_encrypt(
    key: &[u8; 16],
    nonce: &[u8],
    aad: &[u8],
    plaintext: &[u8],
    mic_len: usize,
) -> Vec<u8> {
    check_parameters(nonce, mic_len);

    let mut out = Vec::with_capacity(plaintext.len() + mic_len);
    out.extend_from_slice(plaintext);
    for (i, chunk) in out.chunks_mut(16).enumerate() {
        let s = keystream_block(key, nonce, i as u64 + 1);
        for (byte, k) in chunk.iter_mut().zip(s) {
            *byte ^= k;
        }
    }

    // MIC = leading M octets of (CBC-MAC XOR S_0) (RFC 3610 Section 2.3).
    let tag = cbc_mac(key, nonce, aad, plaintext, mic_len);
    let s0 = keystream_block(key, nonce, 0);
    out.extend((0..mic_len).map(|i| tag[i] ^ s0[i]));
    out
}

/// Decrypts `ciphertext || MIC` produced by [`ccm_encrypt`], returning the
/// plaintext only when the MIC authenticates (RFC 3610 Section 2.4 requires
/// discarding the message on MIC failure).
pub fn ccm_decrypt(
    key: &[u8; 16],
    nonce: &[u8],
    aad: &[u8],
    ciphertext_and_mic: &[u8],
    mic_len: usize,
) -> Option<Vec<u8>> {
    check_parameters(nonce, mic_len);
    if ciphertext_and_mic.len() < mic_len {
        return None;
    }
    let (ciphertext, mic) = ciphertext_and_mic.split_at(ciphertext_and_mic.len() - mic_len);

    let mut plaintext = ciphertext.to_vec();
    for (i, chunk) in plaintext.chunks_mut(16).enumerate() {
        let s = keystream_block(key, nonce, i as u64 + 1);
        for (byte, k) in chunk.iter_mut().zip(s) {
            *byte ^= k;
        }
    }

    let tag = cbc_mac(key, nonce, aad, &plaintext, mic_len);
    let s0 = keystream_block(key, nonce, 0);
    // Accumulated-XOR comparison avoids an early-exit timing side channel.
    let mut diff = 0u8;
    for (i, &m) in mic.iter().enumerate() {
        diff |= m ^ tag[i] ^ s0[i];
    }
    if diff != 0 {
        return None;
    }
    Some(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 3610 Section 8, Packet Vector #1: M=8, L=2, 8-octet AAD header.
    const RFC3610_KEY: [u8; 16] = [
        0xC0, 0xC1, 0xC2, 0xC3, 0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xCB, 0xCC, 0xCD, 0xCE,
        0xCF,
    ];
    const RFC3610_NONCE: [u8; 13] = [
        0x00, 0x00, 0x00, 0x03, 0x02, 0x01, 0x00, 0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5,
    ];
    const RFC3610_AAD: [u8; 8] = [0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07];
    const RFC3610_PLAINTEXT: [u8; 23] = [
        0x08, 0x09, 0x0A, 0x0B, 0x0C, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16,
        0x17, 0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E,
    ];
    const RFC3610_OUTPUT: [u8; 31] = [
        0x58, 0x8C, 0x97, 0x9A, 0x61, 0xC6, 0x63, 0xD2, 0xF0, 0x66, 0xD0, 0xC2, 0xC0, 0xF9, 0x89,
        0x80, 0x6D, 0x5F, 0x6B, 0x61, 0xDA, 0xC3, 0x84, 0x17, 0xE8, 0xD1, 0x2C, 0xFD, 0xF9, 0x26,
        0xE0,
    ];

    #[test]
    fn test_rfc3610_packet_vector_1_encrypt() {
        let out = ccm_encrypt(
            &RFC3610_KEY,
            &RFC3610_NONCE,
            &RFC3610_AAD,
            &RFC3610_PLAINTEXT,
            8,
        );
        assert_eq!(out, RFC3610_OUTPUT);
    }

    #[test]
    fn test_rfc3610_packet_vector_1_decrypt() {
        let plaintext = ccm_decrypt(
            &RFC3610_KEY,
            &RFC3610_NONCE,
            &RFC3610_AAD,
            &RFC3610_OUTPUT,
            8,
        )
        .expect("MIC must verify");
        assert_eq!(plaintext, RFC3610_PLAINTEXT);
    }

    #[test]
    fn test_tampered_ciphertext_rejected() {
        let mut tampered = RFC3610_OUTPUT;
        tampered[0] ^= 0x01;
        assert!(ccm_decrypt(&RFC3610_KEY, &RFC3610_NONCE, &RFC3610_AAD, &tampered, 8).is_none());
    }

    #[test]
    fn test_tampered_mic_rejected() {
        let mut tampered = RFC3610_OUTPUT;
        *tampered.last_mut().unwrap() ^= 0x80;
        assert!(ccm_decrypt(&RFC3610_KEY, &RFC3610_NONCE, &RFC3610_AAD, &tampered, 8).is_none());
    }

    #[test]
    fn test_wrong_aad_rejected() {
        assert!(ccm_decrypt(&RFC3610_KEY, &RFC3610_NONCE, &[0xEA], &RFC3610_OUTPUT, 8).is_none());
    }

    #[test]
    fn test_empty_plaintext_roundtrip() {
        // EAD permits empty inner payloads; the MIC alone must still round-trip.
        let out = ccm_encrypt(&RFC3610_KEY, &RFC3610_NONCE, &[0xEA], &[], 4);
        assert_eq!(out.len(), 4);
        let plaintext =
            ccm_decrypt(&RFC3610_KEY, &RFC3610_NONCE, &[0xEA], &out, 4).expect("MIC must verify");
        assert!(plaintext.is_empty());
    }
}
