// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Real P-256 ECDH for LE Secure Connections (Core Spec Vol 3, Part H,
//! Section 2.3.5.6). Public keys cross the SMP wire as little-endian X
//! then Y (the reverse of SEC1's big-endian), so this module speaks
//! **wire order** at its boundary — matching `f4`/`f5`, which take
//! wire-order inputs and reverse internally.
//!
//! Peer public keys are validated to be on the curve before use
//! (mandatory since CVE-2018-5383; Android rejects pairing with an
//! invalid point, reporting reason "Authentication Requirements").

use p256::elliptic_curve::sec1::{FromEncodedPoint, ToEncodedPoint};
use p256::{EncodedPoint, PublicKey, SecretKey};

use super::smp_crypto::rev;

/// Scalar sampling for simulated pairings: same non-cryptographic PRNG the
/// SMP session uses for nonces (simble is a protocol simulator, not a
/// security boundary), salted by a process-wide counter.
fn random_scalar_candidate() -> [u8; 32] {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0x5EED);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut out = [0u8; 32];
    crate::types::rng::fill_pseudo_random(counter.wrapping_mul(0x9E37_79B9_7F4A_7C15), &mut out);
    out
}

/// A P-256 keypair whose public half is exposed in SMP wire order.
#[derive(Clone)]
pub struct P256Keypair {
    secret: SecretKey,
    /// Public key X coordinate, little-endian (wire order).
    pub x_le: [u8; 32],
    /// Public key Y coordinate, little-endian (wire order).
    pub y_le: [u8; 32],
}

impl std::fmt::Debug for P256Keypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print the secret scalar.
        f.debug_struct("P256Keypair")
            .field("x_le", &self.x_le)
            .finish_non_exhaustive()
    }
}

impl P256Keypair {
    /// Generates a fresh keypair.
    pub fn generate() -> Self {
        loop {
            // Rejection-sample a scalar (a uniform 32-byte value exceeds
            // the group order with p ~ 2^-32).
            if let Some(keypair) = Self::from_private_be(&random_scalar_candidate()) {
                return keypair;
            }
        }
    }

    /// Builds the keypair for a big-endian private scalar (e.g. the spec's
    /// debug key, Vol 3, Part H, Section 2.3.5.6.1). `None` if the scalar
    /// is zero or not below the group order.
    pub fn from_private_be(private_be: &[u8; 32]) -> Option<Self> {
        let secret = SecretKey::from_slice(private_be).ok()?;
        let point = secret.public_key().to_encoded_point(false);
        let (x, y) = (point.x()?, point.y()?);
        Some(Self {
            secret,
            x_le: rev(<&[u8; 32]>::try_from(&x[..]).ok()?),
            y_le: rev(<&[u8; 32]>::try_from(&y[..]).ok()?),
        })
    }

    /// Computes the ECDH shared secret's X coordinate in wire order (what
    /// `f5` expects), validating the peer's point first. `None` means the
    /// peer key is not a valid point on P-256 — the caller must fail the
    /// pairing rather than proceed.
    pub fn shared_x_le(&self, peer_x_le: &[u8; 32], peer_y_le: &[u8; 32]) -> Option<[u8; 32]> {
        let encoded = EncodedPoint::from_affine_coordinates(
            &rev(peer_x_le).into(),
            &rev(peer_y_le).into(),
            false,
        );
        let peer: PublicKey = Option::from(PublicKey::from_encoded_point(&encoded))?;
        let shared = p256::ecdh::diffie_hellman(self.secret.to_nonzero_scalar(), peer.as_affine());
        let shared_be: [u8; 32] = (&shared.raw_secret_bytes()[..]).try_into().ok()?;
        Some(rev(&shared_be))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The spec's debug private key (Vol 3, Part H, 2.3.5.6.1), big-endian.
    const DEBUG_PRIVATE_BE: [u8; 32] = [
        0x3F, 0x49, 0xF6, 0xD4, 0xA3, 0xC5, 0x5F, 0x38, 0x74, 0xC9, 0xB3, 0xE3, 0xD2, 0x10, 0x3F,
        0x50, 0x4A, 0xFF, 0x60, 0x7B, 0xEB, 0x40, 0xB7, 0x99, 0x58, 0x99, 0xB8, 0xA6, 0xCD, 0x3C,
        0x1A, 0xBD,
    ];
    /// The spec's debug public key X, big-endian.
    const DEBUG_PUBLIC_X_BE: [u8; 32] = [
        0x20, 0xB0, 0x03, 0xD2, 0xF2, 0x97, 0xBE, 0x2C, 0x5E, 0x2C, 0x83, 0xA7, 0xE9, 0xF9, 0xA5,
        0xB9, 0xEF, 0xF4, 0x91, 0x11, 0xAC, 0xF4, 0xFD, 0xDB, 0xCC, 0x03, 0x01, 0x48, 0x0E, 0x35,
        0x9D, 0xE6,
    ];

    #[test]
    fn test_debug_key_derives_the_spec_public_key() {
        let keypair = P256Keypair::from_private_be(&DEBUG_PRIVATE_BE).unwrap();
        assert_eq!(keypair.x_le, rev(&DEBUG_PUBLIC_X_BE));
    }

    #[test]
    fn test_ecdh_agrees_between_two_keypairs() {
        let a = P256Keypair::generate();
        let b = P256Keypair::generate();
        let ab = a.shared_x_le(&b.x_le, &b.y_le).unwrap();
        let ba = b.shared_x_le(&a.x_le, &a.y_le).unwrap();
        assert_eq!(ab, ba);
        assert_ne!(ab, [0; 32]);
    }

    #[test]
    fn test_invalid_peer_point_is_rejected() {
        // Random 64 bytes are (with overwhelming probability) not on the
        // curve — exactly what the old stub used to send.
        let keypair = P256Keypair::generate();
        assert!(keypair.shared_x_le(&[0x42; 32], &[0x24; 32]).is_none());
    }
}
