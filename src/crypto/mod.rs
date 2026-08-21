// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Cryptographic primitives for Bluetooth Security & Database Hash computation.

pub mod aes;
pub mod ccm;
pub mod smp_crypto;

pub use aes::{aes_128_encrypt_block, aes_cmac};
pub use ccm::{ccm_decrypt, ccm_encrypt};
pub use smp_crypto::{ah, c1, f4, f5, f6, g2, h6, h7, s1, xor16};
