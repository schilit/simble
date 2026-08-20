// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Bluetooth 6.0 Channel Sounding procedures, configuration, and ranging algorithms.

pub mod procedures;

pub use procedures::{
    CsConfig, CsDistanceEstimate, CsMainMode, CsRole, CsStepResult, compute_pbr_distance,
};
