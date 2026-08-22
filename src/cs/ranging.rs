// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Turning Channel Sounding tones into a distance.
//!
//! # The measurement
//!
//! A tone at frequency `f` that has travelled `d` metres arrives with its
//! carrier rotated by `2π·f·d/c`. Sample the same path at several
//! frequencies and the phase rotates *linearly with frequency*, at a rate
//! proportional to distance. Fit that slope and you have the range. This is
//! Phase-Based Ranging (Core 6.0, Vol 6, Part A), and it is why Channel
//! Sounding is accurate where RSSI is not: it measures a geometric quantity,
//! not a power that anything in the room can attenuate.
//!
//! # Why one radio's measurements are useless on their own
//!
//! Each radio adds its own local oscillator's phase to what it measures, and
//! that phase is **redrawn on every frequency hop** — a synthesizer that
//! re-locks comes back wherever it likes. So one end's tones are, across
//! frequency, uniform noise; there is no slope in them to fit.
//!
//! The two ends' offsets are equal and opposite (`+Δθ` at one end, `−Δθ` at
//! the other, because each measures the *difference* between the two
//! oscillators). Adding the two measurements at the same frequency cancels
//! `Δθ` exactly and leaves `2·(2π·f·d/c)`.
//!
//! That is the entire reason the Channel Sounding Profile defines a Ranging
//! Service: the reflector's tones must be carried, over GATT, to the
//! initiator's host, because neither controller can compute a distance alone.
//! [`combine`] is where they meet.
//!
//! # What bounds it
//!
//! Phase is only ever observed modulo a turn, so the fit has to unwrap the
//! sequence, and unwrapping is only correct while the true rotation between
//! adjacent tones stays under half a turn. That caps the unambiguous range at
//! `c / (4·Δf)` — 37.5 m for tones 2 MHz apart. Past it the estimate does not
//! degrade gracefully; it aliases to a shorter distance.

use crate::controller::propagation::{channel_frequency_hz, wrap_phase};
use crate::cs::tones::Tone;
use crate::types::SPEED_OF_LIGHT_M_PER_S;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// A distance recovered from a set of tones, with what it cost to get it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PbrEstimate {
    /// The estimated distance in metres.
    pub distance_m: f64,
    /// One standard error of that estimate, in metres, propagated from the
    /// scatter of the tones about the fitted line. This is the honest measure
    /// of how much to trust the number.
    pub std_error_m: f64,
    /// How many tone pairs the fit used.
    pub tones_used: usize,
    /// Frequency span the tones covered, in hertz. Precision improves in
    /// proportion to it.
    pub bandwidth_hz: f64,
    /// The largest distance this tone spacing can measure without aliasing.
    /// An estimate near or past this is not to be believed.
    pub unambiguous_range_m: f64,
    /// Root-mean-square scatter of the tones about the fitted line, in
    /// radians — the raw goodness of fit.
    pub residual_rad: f64,
}

impl PbrEstimate {
    /// Whether the estimate sits inside the range this tone plan can measure.
    ///
    /// This is a *necessary* condition, not a sufficient one. An aliased
    /// measurement folds back **into** the window, so a true 50 m separation
    /// measured with 2 MHz tones reports something under 37.5 m and passes
    /// this check while being completely wrong. Phase-Based Ranging cannot
    /// detect its own wrap; that is what a coarse round-trip-time measurement
    /// is for, and Simble's radio does not model one. Treat this as "the tone
    /// plan does not obviously rule the answer out", and keep the plan's
    /// range wider than anything being measured.
    pub fn is_unambiguous(&self) -> bool {
        self.distance_m < self.unambiguous_range_m
    }
}

/// A tone measured by both ends of a procedure at the same frequency.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CombinedTone {
    /// Channel index the pair was measured on.
    pub channel: u8,
    /// The sum of the two ends' phases, wrapped into `(-π, π]`. Free of the
    /// oscillator offset, and equal to `2·(2π·f·d/c)` modulo a turn.
    pub phase_rad: f64,
}

/// Pairs the initiator's tones with the reflector's by channel and sums each
/// pair's phase, cancelling the oscillator offset.
///
/// Tones present at only one end are dropped: a sum needs both halves, and a
/// lone measurement contributes nothing but its offset. Unusable tones (the
/// controller flagged them, or the sample is at the origin) are dropped for
/// the same reason.
pub fn combine(local: &[Tone], remote: &[Tone]) -> Vec<CombinedTone> {
    let mut combined = Vec::new();
    for tone in local.iter().filter(|t| t.is_usable()) {
        let Some(peer) = remote
            .iter()
            .find(|t| t.channel == tone.channel && t.is_usable())
        else {
            continue;
        };
        combined.push(CombinedTone {
            channel: tone.channel,
            phase_rad: wrap_phase(tone.phase_rad() + peer.phase_rad()),
        });
    }
    combined.sort_by_key(|t| t.channel);
    combined.dedup_by_key(|t| t.channel);
    combined
}

/// Estimates the distance the tones travelled.
///
/// Returns `None` with fewer than three tones — two points define a line
/// exactly, so a fit through them has no residual and would report perfect
/// confidence in a number that could be anything.
pub fn estimate(tones: &[CombinedTone]) -> Option<PbrEstimate> {
    if tones.len() < 3 {
        return None;
    }
    let mut sorted = tones.to_vec();
    sorted.sort_by_key(|t| t.channel);
    let unwrapped = unwrap_sequence(&sorted);

    let frequencies: Vec<f64> = sorted
        .iter()
        .map(|t| channel_frequency_hz(t.channel))
        .collect();
    let fit = least_squares_slope(&frequencies, &unwrapped)?;

    // The combined phase advances at 4π·d/c per hertz — twice the one-way
    // rate, because the sum counts the path once for each direction.
    let scale = SPEED_OF_LIGHT_M_PER_S / (4.0 * PI);
    let spacing_hz = widest_gap(&frequencies);
    Some(PbrEstimate {
        // A negative slope means the fit found phase running backwards with
        // frequency, which no propagation can produce; at very short range
        // noise does it, and zero is the honest floor.
        distance_m: (fit.slope * scale).max(0.0),
        std_error_m: fit.slope_std_error * scale,
        tones_used: sorted.len(),
        bandwidth_hz: frequencies.last()? - frequencies.first()?,
        unambiguous_range_m: SPEED_OF_LIGHT_M_PER_S / (4.0 * spacing_hz),
        residual_rad: fit.residual_rms,
    })
}

/// Unwraps a sequence of combined phases into the continuous ramp the fit is
/// actually made against.
///
/// Each tone's phase is only known modulo a turn, so this accumulates the
/// *smallest* step consistent with the previous tone. Correct while the true
/// step stays under half a turn — which is exactly what bounds the
/// unambiguous range.
///
/// Public because it is the only honest way to draw the measurement: the
/// wrapped phases look like scatter at any real distance, and the straight
/// line only appears here. `tones` must already be sorted by channel.
pub fn unwrap_sequence(tones: &[CombinedTone]) -> Vec<f64> {
    let Some(first) = tones.first() else {
        return Vec::new();
    };
    let mut unwrapped = Vec::with_capacity(tones.len());
    let mut running = first.phase_rad;
    unwrapped.push(running);
    for pair in tones.windows(2) {
        running += wrap_phase(pair[1].phase_rad - pair[0].phase_rad);
        unwrapped.push(running);
    }
    unwrapped
}

/// The whole pipeline: pair up two ends' tones and fit a distance.
pub fn estimate_from_tones(local: &[Tone], remote: &[Tone]) -> Option<PbrEstimate> {
    estimate(&combine(local, remote))
}

/// The **widest** gap between adjacent frequencies.
///
/// Unwrapping is only correct while the true phase step across every gap
/// stays under half a turn, so it is the worst gap that bounds the whole
/// measurement — one hole in the tone plan (channels the controller could not
/// use, tones the peer did not report) shrinks the unambiguous range to what
/// that hole alone would allow. Taking the *narrowest* gap here would have
/// let a plan with a hole in it claim a range it cannot deliver.
fn widest_gap(frequencies: &[f64]) -> f64 {
    frequencies
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|gap| *gap > 0.0)
        .fold(0.0, f64::max)
}

/// An ordinary-least-squares line fit, with the slope's standard error.
struct LineFit {
    /// Radians per hertz.
    slope: f64,
    /// One standard error of `slope`, from the residual scatter.
    slope_std_error: f64,
    /// RMS residual, in radians.
    residual_rms: f64,
}

/// Fits `y = a + b·x` and reports `b` with its uncertainty.
///
/// The standard error is the textbook `s / √Σ(x−x̄)²` with
/// `s² = Σr²/(n−2)`: it is what makes the estimate's own error bar fall out
/// of the measurement rather than being asserted.
fn least_squares_slope(x: &[f64], y: &[f64]) -> Option<LineFit> {
    let n = x.len();
    if n < 3 || y.len() != n {
        return None;
    }
    let count = n as f64;
    let mean_x = x.iter().sum::<f64>() / count;
    let mean_y = y.iter().sum::<f64>() / count;
    let sxx: f64 = x.iter().map(|v| (v - mean_x).powi(2)).sum();
    if sxx <= 0.0 {
        return None; // every tone on one frequency: no slope to find
    }
    let sxy: f64 = x
        .iter()
        .zip(y)
        .map(|(a, b)| (a - mean_x) * (b - mean_y))
        .sum();
    let slope = sxy / sxx;
    let intercept = mean_y - slope * mean_x;

    let residual_sum_squares: f64 = x
        .iter()
        .zip(y)
        .map(|(a, b)| (b - (intercept + slope * a)).powi(2))
        .sum();
    let variance = residual_sum_squares / (count - 2.0);
    Some(LineFit {
        slope,
        slope_std_error: (variance / sxx).sqrt(),
        residual_rms: (residual_sum_squares / count).sqrt(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::controller::propagation::{Rng, propagation_phase_rad};

    /// The tones one end measures at `distance_m`, with an oscillator offset
    /// redrawn per hop and `sigma` radians of measurement noise — the same
    /// construction the simulated radio uses.
    fn measured_pair(distance_m: f64, sigma: f64, seed: u64) -> (Vec<Tone>, Vec<Tone>) {
        let mut rng = Rng::new(seed);
        let (mut local, mut remote) = (Vec::new(), Vec::new());
        for step in 0..19u8 {
            let channel = step * 2;
            let phase = propagation_phase_rad(distance_m, channel_frequency_hz(channel));
            let offset = rng.uniform_phase();
            let quantize = |p: f64| Tone {
                channel,
                i: (p.cos() * 2047.0).round() as i16,
                q: (p.sin() * 2047.0).round() as i16,
                quality: 0,
            };
            local.push(quantize(wrap_phase(
                phase + offset + rng.normal_scaled(sigma),
            )));
            remote.push(quantize(wrap_phase(
                phase - offset + rng.normal_scaled(sigma),
            )));
        }
        (local, remote)
    }

    #[test]
    fn test_a_noiseless_pair_of_ends_recovers_the_distance() {
        for truth in [0.5, 1.0, 5.0, 12.5, 30.0] {
            let (local, remote) = measured_pair(truth, 0.0, 1);
            let estimate = estimate_from_tones(&local, &remote).expect("an estimate");
            assert!(
                (estimate.distance_m - truth).abs() < 0.05,
                "{truth} m estimated as {}",
                estimate.distance_m
            );
            assert_eq!(estimate.tones_used, 19);
            assert!(estimate.is_unambiguous());
        }
    }

    #[test]
    fn test_one_end_alone_cannot_recover_anything() {
        // The point of the Ranging Service, as a test: fit the initiator's
        // own tones against a set of zero-phase stand-ins for the reflector's
        // and the answer is noise, because the per-hop oscillator offset is
        // still in there.
        let (local, _) = measured_pair(8.0, 0.0, 3);
        let zeroed: Vec<Tone> = local
            .iter()
            .map(|t| Tone {
                i: 2047,
                q: 0,
                ..*t
            })
            .collect();
        let alone = estimate_from_tones(&local, &zeroed).expect("a fit, of nothing");
        assert!(
            (alone.distance_m - 8.0).abs() > 1.0,
            "one end alone must not land near the truth, got {}",
            alone.distance_m
        );
        assert!(
            alone.residual_rad > 1.0,
            "and the fit must look as bad as it is: {}",
            alone.residual_rad
        );
    }

    #[test]
    fn test_noise_widens_the_error_bar_it_reports() {
        let (clean_local, clean_remote) = measured_pair(10.0, 0.0, 5);
        let clean = estimate_from_tones(&clean_local, &clean_remote).unwrap();
        let (noisy_local, noisy_remote) = measured_pair(10.0, 0.3, 5);
        let noisy = estimate_from_tones(&noisy_local, &noisy_remote).unwrap();
        assert!(
            noisy.std_error_m > clean.std_error_m,
            "clean {} vs noisy {}",
            clean.std_error_m,
            noisy.std_error_m
        );
        // Even at 0.3 rad per tone the estimate stays close: 19 tones over
        // 36 MHz is a lot of averaging. This is the accuracy claim.
        assert!(
            (noisy.distance_m - 10.0).abs() < 1.0,
            "estimated {}",
            noisy.distance_m
        );
    }

    #[test]
    fn test_beyond_the_unambiguous_range_the_estimate_aliases() {
        // 2 MHz spacing measures to 37.5 m. At 50 m the phase between
        // neighbouring tones rotates past half a turn and unwrapping picks
        // the wrong branch, so the estimate folds back rather than reading
        // high. A demo must not present that as a large-distance reading.
        let (local, remote) = measured_pair(50.0, 0.0, 9);
        let estimate = estimate_from_tones(&local, &remote).unwrap();
        assert!(
            estimate.distance_m < estimate.unambiguous_range_m,
            "aliased to {} inside the {} m window",
            estimate.distance_m,
            estimate.unambiguous_range_m
        );
        assert!((estimate.unambiguous_range_m - 37.47).abs() < 0.1);
    }

    #[test]
    fn test_unwrapping_turns_the_wrapped_phases_into_a_straight_line() {
        // The wrapped sequence looks like scatter — this is why a chart of it
        // teaches nothing, and why the estimator's own unwrapped sequence is
        // what a reader needs to see.
        let (local, remote) = measured_pair(14.0, 0.0, 21);
        let combined = combine(&local, &remote);
        let unwrapped = unwrap_sequence(&combined);
        assert_eq!(unwrapped.len(), combined.len());

        let steps: Vec<f64> = unwrapped.windows(2).map(|w| w[1] - w[0]).collect();
        let mean = steps.iter().sum::<f64>() / steps.len() as f64;
        // Evenly spaced to within the 12-bit quantization of the Phase
        // Correction Terms themselves — about a milliradian.
        assert!(
            steps.iter().all(|s| (s - mean).abs() < 1e-3),
            "a noiseless measurement unwraps to evenly spaced steps: {steps:?}"
        );
        // The step is 4π·d·Δf/c: 1.17 rad at 14 m with 2 MHz tones.
        assert!((mean - 1.174).abs() < 0.01, "step {mean}");
        assert!(unwrap_sequence(&[]).is_empty());
    }

    #[test]
    fn test_too_few_tones_is_refused_rather_than_fitted() {
        let (local, remote) = measured_pair(4.0, 0.0, 11);
        assert!(estimate_from_tones(&local[..2], &remote[..2]).is_none());
        assert!(estimate_from_tones(&[], &[]).is_none());
        assert!(estimate_from_tones(&local[..3], &remote[..3]).is_some());
    }

    #[test]
    fn test_only_tones_both_ends_measured_are_combined() {
        let (local, remote) = measured_pair(6.0, 0.0, 13);
        // The reflector reported nothing above channel 24.
        let partial: Vec<Tone> = remote.iter().copied().filter(|t| t.channel <= 24).collect();
        let combined = combine(&local, &partial);
        assert_eq!(combined.len(), partial.len());
        assert!(combined.iter().all(|t| t.channel <= 24));
        let estimate = estimate(&combined).unwrap();
        assert!((estimate.distance_m - 6.0).abs() < 0.1);
        // Fewer tones over a narrower span is a less precise measurement, and
        // the estimate must say so rather than reporting the same confidence.
        let full = estimate_from_tones(&local, &remote).unwrap();
        assert!(estimate.bandwidth_hz < full.bandwidth_hz);
    }

    #[test]
    fn test_a_hole_in_the_tone_plan_shortens_the_unambiguous_range() {
        // Unwrapping has to step across the hole in one jump, so the widest
        // gap — not the nominal spacing — is what bounds the measurement.
        // Reporting the nominal 37.5 m here would be a claim the tone plan
        // cannot back.
        let (local, remote) = measured_pair(6.0, 0.0, 13);
        let holed: Vec<Tone> = remote
            .iter()
            .copied()
            .filter(|t| t.channel < 12 || t.channel > 24)
            .collect();
        let estimate = estimate_from_tones(&local, &holed).unwrap();
        assert!(
            estimate.unambiguous_range_m < 6.0,
            "a 16 MHz hole bounds this at {} m",
            estimate.unambiguous_range_m
        );
        // And the consequence is real: past the bound the answer is simply
        // wrong. It folds inward rather than reading high, which is why the
        // reported bound is the only warning available — `is_unambiguous`
        // cannot catch this, and says so.
        assert!(
            (estimate.distance_m - 6.0).abs() > 1.0,
            "an aliased fit landed at {} m",
            estimate.distance_m
        );
        assert!(estimate.is_unambiguous(), "the alias folds inside the window");
    }

    #[test]
    fn test_tones_the_controller_flagged_unusable_are_dropped() {
        let (mut local, remote) = measured_pair(3.0, 0.0, 17);
        local[0].quality = crate::cs::tones::tone_quality::UNAVAILABLE;
        local[1].i = 0;
        local[1].q = 0;
        let combined = combine(&local, &remote);
        assert_eq!(combined.len(), 17);
        assert!((estimate(&combined).unwrap().distance_m - 3.0).abs() < 0.1);
    }
}
