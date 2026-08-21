// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Minimal, deliberately non-cryptographic xorshift64 PRNG shared by the
//! simulator's "random-ish" byte generators (SMP nonces/keys, WebSocket
//! handshake nonces). Simble is a protocol simulator, not a security
//! boundary, so this only needs to avoid handing out identical values across
//! rapid successive calls — not resist an attacker.

use std::time::{SystemTime, UNIX_EPOCH};

/// Fills `out` with pseudo-random bytes from a xorshift64 stream seeded from
/// the current time XOR `salt`. Callers mix per-call entropy (a counter, an
/// address) into `salt` so two calls in the same nanosecond still diverge.
pub(crate) fn fill_pseudo_random(salt: u64, out: &mut [u8]) {
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        ^ salt;
    let mut state = seed | 1;
    let mut i = 0;
    while i < out.len() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let bytes = state.to_le_bytes();
        let take = (out.len() - i).min(8);
        out[i..i + take].copy_from_slice(&bytes[..take]);
        i += take;
    }
}
