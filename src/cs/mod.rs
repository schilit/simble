// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Bluetooth 6.0 Channel Sounding: measuring how far away a peer is.
//!
//! Two ways of answering that question live here, deliberately side by side,
//! because the contrast is the point:
//!
//! * [`path_loss`] — invert a path-loss model on the RSSI every advertising
//!   report already carries. Free, universal, and wrong by tens of percent.
//! * [`ranging`] — fit the slope of carrier phase against frequency across a
//!   set of Channel Sounding tones. Needs a real procedure and the peer's
//!   cooperation, and is accurate to centimetres.
//!
//! [`tones`] is the boundary between HCI bytes and that arithmetic, and
//! [`procedures`] holds the configuration types a host sets up a procedure
//! with.
//!
//! Everything here is transport-free: measurements in, distances out. The
//! measurements themselves are produced by a radio — the simulated one lives
//! in [`crate::controller::sim`] — and carried to the initiator's host over
//! HCI and, for the reflector's half, over the Ranging Service
//! ([`crate::profiles::ras`]).

pub mod path_loss;
pub mod procedures;
pub mod ranging;
pub mod tones;

pub use path_loss::{RssiRanger, RssiRangingParams};
pub use procedures::{
    CsConfig, CsDistanceEstimate, CsMainMode, CsRole, CsStepResult, compute_pbr_distance,
};
pub use ranging::{CombinedTone, PbrEstimate, combine, estimate, estimate_from_tones};
pub use tones::{SubeventResult, Tone, decode_pct, parse_subevent_result};
