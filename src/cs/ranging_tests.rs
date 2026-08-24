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
    assert!(
        estimate.is_unambiguous(),
        "the alias folds inside the window"
    );
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
