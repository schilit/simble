// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio codecs for the demo surfaces — deliberately outside the protocol
//! model. Simble's media plane carries isochronous SDUs as opaque bytes; a
//! codec is only needed when something wants to *listen* to them, which is
//! the browser pages. Everything here is feature-gated accordingly.

/// LC3, the LE Audio codec. Behind the optional `lc3` feature.
#[cfg(feature = "lc3")]
pub mod lc3;
