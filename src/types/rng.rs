// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Minimal, deliberately non-cryptographic xorshift64 PRNG shared by the
//! simulator's "random-ish" byte generators (SMP nonces/keys, WebSocket
//! handshake nonces). Simble is a protocol simulator, not a security
//! boundary, so this only needs to avoid handing out identical values across
//! rapid successive calls — not resist an attacker.

#[cfg(not(target_arch = "wasm32"))]
use std::time::{SystemTime, UNIX_EPOCH};

/// A clock reading to seed the stream with.
///
/// `SystemTime::now()` **panics** on `wasm32-unknown-unknown` ("time not
/// implemented on this platform") — it is not a `Result`, so the caller's
/// `unwrap_or` never sees it. That panic poisoned every later call into the
/// same wasm object ("recursive use of an object..."), which is what stopped
/// a browser-hosted device from pairing. The browser's own clock works fine.
fn clock_seed() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x9E37_79B9_7F4A_7C15)
    }
    #[cfg(target_arch = "wasm32")]
    {
        // Milliseconds only, so mix in a counter to keep successive calls
        // inside the same millisecond distinct.
        use std::sync::atomic::{AtomicU64, Ordering};
        static TICKS: AtomicU64 = AtomicU64::new(0);
        let millis = js_sys::Date::now() as u64;
        millis
            .wrapping_mul(1_000_000)
            .wrapping_add(TICKS.fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed))
    }
}

/// Fills `out` with pseudo-random bytes from a xorshift64 stream seeded from
/// the current time XOR `salt`. Callers mix per-call entropy (a counter, an
/// address) into `salt` so two calls in the same nanosecond still diverge.
pub(crate) fn fill_pseudo_random(salt: u64, out: &mut [u8]) {
    let seed = clock_seed() ^ salt;
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
