// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Estimating distance from RSSI — the method Channel Sounding replaces.
//!
//! Invert the log-distance path-loss model and you get a distance out of a
//! single number every Bluetooth receiver already reports:
//!
//! ```text
//!   d̂ = 10 ^ ((P_tx − PL(1 m) − RSSI) / (10·n))
//! ```
//!
//! It needs no extra hardware, no procedure, and no cooperation from the
//! peer, which is why every proximity product since iBeacon has shipped it.
//! It is also, in an ordinary room, wrong by tens of percent, for three
//! reasons this module makes explicit rather than hiding:
//!
//! 1. **The transmit power is a guess.** A tag can advertise its calibrated
//!    1-metre RSSI (the `TX Power Level` AD type, or iBeacon's measured
//!    power), but plenty do not, and the ones that do are calibrated in free
//!    space, not on a wrist or in a pocket. Every dB of error here is a
//!    *multiplicative* error in the distance.
//! 2. **The path-loss exponent is a guess.** The estimator has to assume one;
//!    the room has whatever it has. Assuming free space (`n = 2`) in a room
//!    that behaves like `n = 2.7` makes far-away things look very far away.
//! 3. **Shadowing and multipath are unmodellable.** Reflections add and
//!    cancel; a few dB of fading is a large fraction of a distance estimate,
//!    and it does not average away quickly because it is correlated in space.
//!
//! The estimator here deliberately does **not** share its parameters with
//! [`crate::controller::propagation::PathLossModel`], the model the simulated
//! radio actually propagates through. A receiver does not get to know the
//! room it is standing in, and a demo where the estimator is handed the
//! truth's own constants would show an accuracy no real device has.

use serde::{Deserialize, Serialize};

/// The parameters an RSSI-based estimator *assumes*.
///
/// Defaults are what a proximity app ships with when it knows nothing: a
/// 0 dBm transmitter, free-space loss at one metre, and a free-space
/// exponent. Every one of them is a guess.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RssiRangingParams {
    /// Assumed RSSI, in dBm, at the one-metre reference distance. This is the
    /// single calibration constant beacon formats carry.
    pub reference_rssi_dbm: f64,
    /// Assumed path-loss exponent.
    pub path_loss_exponent: f64,
}

impl Default for RssiRangingParams {
    fn default() -> Self {
        Self {
            reference_rssi_dbm: -40.2,
            path_loss_exponent: 2.0,
        }
    }
}

impl RssiRangingParams {
    /// Estimates distance in metres from one RSSI sample.
    ///
    /// Clamped at the reference distance: a sample stronger than the
    /// one-metre calibration would give a sub-metre answer that the model has
    /// no validity below, and a demo showing 0.2 m there would be reporting
    /// the model's breakdown as precision.
    pub fn distance_m(&self, rssi_dbm: f64) -> f64 {
        let loss_beyond_reference = self.reference_rssi_dbm - rssi_dbm;
        10f64
            .powf(loss_beyond_reference / (10.0 * self.path_loss_exponent))
            .max(1.0)
    }
}

/// A short window of RSSI samples and the distance they imply.
///
/// Real proximity implementations all smooth: a single advertising report's
/// RSSI swings several dB, and a raw per-report distance is unusable. The
/// window is what makes the RSSI half of a ranging demo a fair fight rather
/// than a straw man — and even smoothed, the systematic error remains,
/// because averaging fixes noise and not a wrong exponent.
#[derive(Debug, Clone)]
pub struct RssiRanger {
    params: RssiRangingParams,
    window: std::collections::VecDeque<i8>,
    capacity: usize,
}

impl RssiRanger {
    /// A ranger that averages the last `capacity` samples.
    pub fn new(params: RssiRangingParams, capacity: usize) -> Self {
        Self {
            params,
            window: std::collections::VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
        }
    }

    /// The assumptions this ranger is working from.
    pub fn params(&self) -> RssiRangingParams {
        self.params
    }

    /// Replaces the assumptions, keeping the samples — this is what a page's
    /// "assume a different exponent" control drives, and re-deriving the
    /// estimate from samples already collected is what shows how much of the
    /// error was the assumption rather than the radio.
    pub fn set_params(&mut self, params: RssiRangingParams) {
        self.params = params;
    }

    /// Records one advertising report's RSSI.
    pub fn push(&mut self, rssi_dbm: i8) {
        if self.window.len() == self.capacity {
            self.window.pop_front();
        }
        self.window.push_back(rssi_dbm);
    }

    /// The most recent sample, if any.
    pub fn latest_rssi_dbm(&self) -> Option<i8> {
        self.window.back().copied()
    }

    /// How many samples are in the window.
    pub fn sample_count(&self) -> usize {
        self.window.len()
    }

    /// The mean RSSI over the window.
    ///
    /// Averaging in dB — that is, geometrically in power — rather than in
    /// linear power is what every implementation does and what the
    /// log-distance model wants, since the model is linear in dB.
    pub fn mean_rssi_dbm(&self) -> Option<f64> {
        if self.window.is_empty() {
            return None;
        }
        let sum: i32 = self.window.iter().map(|s| i32::from(*s)).sum();
        Some(f64::from(sum) / self.window.len() as f64)
    }

    /// Standard deviation of the window, in dB — the visible jitter.
    pub fn rssi_std_dev_db(&self) -> Option<f64> {
        let mean = self.mean_rssi_dbm()?;
        if self.window.len() < 2 {
            return Some(0.0);
        }
        let variance = self
            .window
            .iter()
            .map(|s| (f64::from(*s) - mean).powi(2))
            .sum::<f64>()
            / (self.window.len() - 1) as f64;
        Some(variance.sqrt())
    }

    /// The smoothed distance estimate in metres.
    pub fn distance_m(&self) -> Option<f64> {
        Some(self.params.distance_m(self.mean_rssi_dbm()?))
    }

    /// The distance the single most recent sample implies — the unsmoothed
    /// number, kept so a demo can show what smoothing is hiding.
    pub fn instantaneous_distance_m(&self) -> Option<f64> {
        Some(self.params.distance_m(f64::from(self.latest_rssi_dbm()?)))
    }

    /// Forgets every sample (on disconnect, or when the scene is reset).
    pub fn clear(&mut self) {
        self.window.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::propagation::PathLossModel;

    #[test]
    fn test_inverting_the_model_it_was_generated_with_is_exact() {
        let truth = PathLossModel {
            tx_power_dbm: 0.0,
            reference_loss_db: 40.2,
            path_loss_exponent: 2.0,
            shadowing_sigma_db: 0.0,
        };
        let estimator = RssiRangingParams {
            reference_rssi_dbm: -40.2,
            path_loss_exponent: 2.0,
        };
        for distance in [1.0, 2.0, 7.5, 20.0] {
            let rssi = truth.rssi_dbm(distance, 0.0);
            assert!(
                (estimator.distance_m(rssi) - distance).abs() < 0.01,
                "{distance} m round-tripped as {}",
                estimator.distance_m(rssi)
            );
        }
    }

    #[test]
    fn test_a_wrong_exponent_biases_the_estimate_far_away() {
        // The room absorbs more than free space; an estimator that assumes
        // free space reads the extra loss as extra distance. This is the
        // systematic half of RSSI's error and no amount of averaging removes
        // it.
        let room = PathLossModel {
            path_loss_exponent: 2.7,
            shadowing_sigma_db: 0.0,
            ..PathLossModel::default()
        };
        let free_space = RssiRangingParams::default();
        let estimated = free_space.distance_m(room.rssi_dbm(10.0, 0.0));
        assert!(estimated > 20.0, "10 m read as {estimated} m");
    }

    #[test]
    fn test_a_wrong_reference_power_scales_every_estimate() {
        // 6 dB of calibration error is a factor of two in distance at n = 2.
        let optimistic = RssiRangingParams {
            reference_rssi_dbm: -34.2,
            ..RssiRangingParams::default()
        };
        let honest = RssiRangingParams::default();
        let rssi = -60.0;
        let ratio = optimistic.distance_m(rssi) / honest.distance_m(rssi);
        assert!((ratio - 2.0).abs() < 0.01, "ratio {ratio}");
    }

    #[test]
    fn test_the_model_is_not_extrapolated_below_its_reference_distance() {
        let params = RssiRangingParams::default();
        assert_eq!(params.distance_m(-10.0), 1.0);
        assert_eq!(params.distance_m(0.0), 1.0);
    }

    #[test]
    fn test_averaging_removes_the_jitter_but_not_the_bias() {
        let room = PathLossModel {
            path_loss_exponent: 2.0,
            shadowing_sigma_db: 6.0,
            ..PathLossModel::default()
        };
        let mut rng = crate::controller::propagation::Rng::new(42);
        let mut ranger = RssiRanger::new(RssiRangingParams::default(), 32);
        for _ in 0..32 {
            let shadowing = rng.normal_scaled(room.shadowing_sigma_db);
            ranger.push(room.rssi_dbm(8.0, shadowing).round() as i8);
        }
        let smoothed = ranger.distance_m().unwrap();
        let instant = ranger.instantaneous_distance_m().unwrap();
        assert!(ranger.rssi_std_dev_db().unwrap() > 3.0, "the jitter is real");
        assert!(
            (smoothed - 8.0).abs() < (instant - 8.0).abs().max(0.5),
            "smoothed {smoothed} should beat the single sample {instant}"
        );
    }

    #[test]
    fn test_an_empty_window_has_no_opinion() {
        let ranger = RssiRanger::new(RssiRangingParams::default(), 8);
        assert!(ranger.distance_m().is_none());
        assert!(ranger.mean_rssi_dbm().is_none());
        assert_eq!(ranger.sample_count(), 0);
    }

    #[test]
    fn test_the_window_keeps_only_its_capacity() {
        let mut ranger = RssiRanger::new(RssiRangingParams::default(), 3);
        for rssi in [-50, -60, -70, -80] {
            ranger.push(rssi);
        }
        assert_eq!(ranger.sample_count(), 3);
        assert_eq!(ranger.mean_rssi_dbm(), Some(-70.0));
        assert_eq!(ranger.latest_rssi_dbm(), Some(-80));
    }

    #[test]
    fn test_changing_the_assumption_re_derives_from_the_same_samples() {
        let mut ranger = RssiRanger::new(RssiRangingParams::default(), 8);
        ranger.push(-70);
        let free_space = ranger.distance_m().unwrap();
        ranger.set_params(RssiRangingParams {
            path_loss_exponent: 2.7,
            ..RssiRangingParams::default()
        });
        let indoor = ranger.distance_m().unwrap();
        assert_eq!(ranger.sample_count(), 1, "the samples are untouched");
        assert!(indoor < free_space, "{indoor} vs {free_space}");
    }
}
