// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio codecs, deliberately outside the protocol model. Simble's media
//! planes carry payloads as opaque bytes — ISO SDUs on the LE side, RTP
//! payloads on the Classic side — and a codec is only needed when something
//! wants to *listen* to them, or to put real audio on the air.

/// LC3, the LE Audio codec. Behind the optional `lc3` feature because it is
/// a ~7k-line third-party dependency; see `docs/lc3-evaluation.md`.
#[cfg(feature = "lc3")]
pub mod lc3;

/// SBC, the mandatory A2DP codec. Not feature-gated: it is written from the
/// specification with no dependencies, and A2DP without it can negotiate a
/// stream it cannot fill. See `docs/sbc-evaluation.md`.
pub mod sbc;
