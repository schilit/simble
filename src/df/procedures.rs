// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Host-side Direction Finding state tracking and angle-of-arrival/departure
//! estimation from IQ sample data (the angular counterpart to
//! `cs::procedures`'s distance ranging).

use serde::{Deserialize, Serialize};

/// Constant Tone Extension type (Core Spec Vol 4, Part E, Section 7.8.80).
// The shared `AngleOf` prefix is the specification's own naming -- these are
// Angle of Arrival and Angle of Departure, and shortening them to `Arrival` /
// `DepartureOneUs` would cost a reader the ability to match the identifier
// against Section 7.8.80. clippy only reports this at all because `df` is
// `pub(crate)` without the `testing` feature; while it was unconditionally
// `pub`, `avoid-breaking-exported-api` suppressed it.
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CteType {
    /// Angle of Arrival CTE.
    AngleOfArrival = 0,
    /// Angle of Departure CTE with 1us switching/sampling slots.
    AngleOfDepartureOneUs = 1,
    /// Angle of Departure CTE with 2us switching/sampling slots.
    AngleOfDepartureTwoUs = 2,
}

/// Antenna switching/sampling slot duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SlotDuration {
    /// 1-microsecond slots.
    OneMicrosecond = 1,
    /// 2-microsecond slots.
    TwoMicroseconds = 2,
}

/// Direction Finding configuration for a connection or advertising set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CteConfig {
    /// The CTE type to transmit or receive.
    pub cte_type: CteType,
    /// Antenna switching/sampling slot duration.
    pub slot_duration: SlotDuration,
    /// CTE length in units of 8 microseconds.
    pub cte_length: u8, // Units of 8us.
    /// Antenna IDs listed in switching order.
    pub switching_pattern: Vec<u8>, // Antenna IDs, in switch order.
}

impl Default for CteConfig {
    fn default() -> Self {
        Self {
            cte_type: CteType::AngleOfArrival,
            slot_duration: SlotDuration::OneMicrosecond,
            cte_length: 20,
            switching_pattern: vec![0, 1, 2, 3],
        }
    }
}

/// A single baseband IQ sample, as reported in an IQ report HCI event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IqSample {
    /// In-phase component.
    pub i: i8,
    /// Quadrature component.
    pub q: i8,
}

impl IqSample {
    /// Creates a sample from its in-phase and quadrature components.
    pub fn new(i: i8, q: i8) -> Self {
        Self { i, q }
    }

    /// Phase of this sample in radians, in `(-pi, pi]`.
    pub fn phase_radians(&self) -> f64 {
        (self.q as f64).atan2(self.i as f64)
    }
}

/// Angle-of-arrival estimation outcome from a switched-antenna IQ capture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AoaEstimate {
    /// Estimated angle of arrival in degrees.
    pub estimated_angle_degrees: f64,
    /// Mean adjacent-antenna phase difference in radians.
    pub mean_phase_difference_radians: f64,
    /// Number of antennas contributing to the estimate.
    pub num_antennas: usize,
    /// Number of adjacent antenna pairs averaged.
    pub num_antenna_pairs: usize,
}

fn wrap_to_pi(angle: f64) -> f64 {
    let two_pi = std::f64::consts::TAU;
    let mut wrapped = angle % two_pi;
    if wrapped > std::f64::consts::PI {
        wrapped -= two_pi;
    } else if wrapped < -std::f64::consts::PI {
        wrapped += two_pi;
    }
    wrapped
}

/// Estimates angle-of-arrival from IQ samples captured across a uniform
/// linear antenna array, using two-element phase interferometry:
///
/// $\theta = \arcsin\left(\frac{\lambda \cdot \Delta\phi}{2\pi d}\right)$
///
/// where `d` is `antenna_spacing_meters`, `lambda` is the carrier
/// wavelength, and `Delta phi` is the phase difference measured between
/// adjacent antennas in the switching pattern. This is the standard
/// two-element AoA interferometer formula used in BLE 5.1+ direction finding
/// application notes (e.g. Nordic AN-1029, Silicon Labs AN1296).
///
/// `samples_per_antenna[k]` holds the IQ samples captured while antenna `k`
/// (in switching-pattern order) was selected; per-antenna samples are
/// averaged as a complex vector (sum I, sum Q, then `atan2`) before taking
/// phase differences, which is more robust to sample noise than averaging
/// per-sample phases directly (avoids phase-wrap cancellation errors).
/// Adjacent-pair phase differences are wrapped into `(-pi, pi]` and averaged
/// across the whole array before the arcsin step.
///
/// Simplifications, documented so this isn't mistaken for a spec-certified
/// implementation: assumes a uniform linear array with known, fixed
/// `antenna_spacing_meters`; does not correct for multipath, phase
/// ambiguity beyond `+/- pi` (which aliases at spacings > lambda/2), I/Q
/// gain/phase imbalance, or reference-period phase rotation from CTE
/// switching artifacts.
///
/// Returns `None` if fewer than two antennas are provided, any antenna has
/// no samples, or `antenna_spacing_meters` / `frequency_hz` are non-positive.
pub fn estimate_aoa(
    samples_per_antenna: &[Vec<IqSample>],
    antenna_spacing_meters: f64,
    frequency_hz: f64,
) -> Option<AoaEstimate> {
    const SPEED_OF_LIGHT: f64 = crate::types::SPEED_OF_LIGHT_M_PER_S;

    if samples_per_antenna.len() < 2 || antenna_spacing_meters <= 0.0 || frequency_hz <= 0.0 {
        return None;
    }
    if samples_per_antenna.iter().any(|samples| samples.is_empty()) {
        return None;
    }

    let mean_phases: Vec<f64> = samples_per_antenna
        .iter()
        .map(|samples| {
            let (sum_i, sum_q) = samples
                .iter()
                .fold((0.0, 0.0), |(si, sq), s| (si + s.i as f64, sq + s.q as f64));
            sum_q.atan2(sum_i)
        })
        .collect();

    let phase_diffs: Vec<f64> = mean_phases
        .windows(2)
        .map(|pair| wrap_to_pi(pair[1] - pair[0]))
        .collect();
    let num_pairs = phase_diffs.len();
    let mean_delta_phi = phase_diffs.iter().sum::<f64>() / num_pairs as f64;

    let wavelength = SPEED_OF_LIGHT / frequency_hz;
    let sin_theta = (mean_delta_phi * wavelength
        / (2.0 * std::f64::consts::PI * antenna_spacing_meters))
        .clamp(-1.0, 1.0);
    let angle_rad = sin_theta.asin();

    Some(AoaEstimate {
        estimated_angle_degrees: angle_rad.to_degrees(),
        mean_phase_difference_radians: mean_delta_phi,
        num_antennas: samples_per_antenna.len(),
        num_antenna_pairs: num_pairs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cte_config_defaults_and_serde() {
        let config = CteConfig::default();
        assert_eq!(config.cte_type, CteType::AngleOfArrival);
        assert_eq!(config.switching_pattern.len(), 4);

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("AngleOfArrival"));
    }

    #[test]
    fn test_estimate_aoa_broadside() {
        let samples = vec![
            vec![IqSample::new(100, 0)],
            vec![IqSample::new(100, 0)],
            vec![IqSample::new(100, 0)],
        ];
        let estimate = estimate_aoa(&samples, 0.05, 2.4e9).expect("estimate");
        assert!(estimate.estimated_angle_degrees.abs() < 0.01);
    }

    #[test]
    fn test_estimate_aoa_insufficient_antennas() {
        let samples = vec![vec![IqSample::new(100, 0)]];
        assert!(estimate_aoa(&samples, 0.05, 2.4e9).is_none());
    }

    #[test]
    fn test_estimate_aoa_empty_samples() {
        let samples = vec![vec![IqSample::new(100, 0)], vec![]];
        assert!(estimate_aoa(&samples, 0.05, 2.4e9).is_none());
    }
}
