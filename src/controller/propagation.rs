// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The **radio physics** the simulated controller needs: how strong a signal
//! is when it arrives, and what phase its carrier arrives with.
//!
//! The simulated controller used to stamp every advertising report with a
//! constant `-61 dBm`, which made every RSSI-dependent demo a lie: a device
//! could be moved anywhere and the number never changed. Everything here
//! exists so that the RSSI byte in an
//! LE Advertising Report, and the phase of a Channel Sounding tone, are
//! *derived from where the two devices actually are*.
//!
//! **This model belongs to the built-in controller alone.** It generates
//! RSSI and carrier phase from device positions, which is only simble's job
//! when simble *is* the radio. On netsim the radio is netsim: positions are
//! set with `netsim move <name> <x> <y>` and reported back in
//! `netsim devices --json`, and the RSSI in an advertising report has already
//! been attenuated by netsim's own propagation. Applying this model to those
//! reports would attenuate them twice. The same holds for a USB dongle, where
//! the numbers come off real hardware.
//!
//! Estimating the other way — turning a received RSSI back into a distance,
//! as [`crate::cs::path_loss`] does — is fine against any source, netsim's
//! included: it inverts a model rather than imposing one, and deliberately
//! does not share constants with this one so it cannot mark its own homework.
//!
//! The rule is true by construction today: the only non-test consumer is
//! [`crate::controller::sim`], and the Ranging page runs in-process only.
//! It is written down here because the moment that page grows a backend
//! selector, nothing else would say so.
//!
//! Two models live here, because Bluetooth ranges two very different ways:
//!
//! * **Path loss** — the log-distance model, `PL(d) = PL(d₀) + 10·n·log₁₀(d)`,
//!   which is what an RSSI-based distance estimate inverts. It is the model,
//!   not the estimator: the estimator lives in [`crate::cs::path_loss`] and
//!   deliberately does *not* share these parameters, because a real receiver
//!   does not know them.
//! * **Carrier phase** — what Channel Sounding's Phase-Based Ranging measures.
//!   A tone at frequency `f` travelling `d` metres arrives rotated by
//!   `2π·f·d/c` radians (Core 6.0, Vol 6, Part A, Section 5).
//!
//! Nothing here is transport-aware and nothing here touches HCI; it is pure
//! arithmetic over positions, so it is testable on its own.

use crate::types::SPEED_OF_LIGHT_M_PER_S;
use std::f64::consts::PI;

/// A position on the simulated floor plan, in metres. The origin is
/// arbitrary — only the distance between two devices matters.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Position {
    /// Metres along the x axis.
    pub x: f64,
    /// Metres along the y axis.
    pub y: f64,
}

impl Position {
    /// A position at `(x, y)` metres.
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Straight-line distance to `other`, in metres.
    pub fn distance_to(self, other: Position) -> f64 {
        ((self.x - other.x).powi(2) + (self.y - other.y).powi(2)).sqrt()
    }
}

/// The lowest 2.4 GHz channel centre, in hertz: LE channel index 0 sits at
/// 2402 MHz and each index is 1 MHz above the last (Vol 6, Part B, Section
/// 1.4.1). Channel Sounding numbers its 79 tone frequencies the same way.
pub const CHANNEL_0_HZ: f64 = 2_402_000_000.0;

/// Hertz between adjacent channel indices.
pub const CHANNEL_SPACING_HZ: f64 = 1_000_000.0;

/// The number of 1 MHz channels Channel Sounding may place tones on
/// (indices 0..=78 — the same 2402–2480 MHz span as LE's data channels,
/// including the three used for advertising).
pub const CS_CHANNEL_COUNT: usize = 79;

/// Centre frequency of channel `index` in hertz.
pub fn channel_frequency_hz(index: u8) -> f64 {
    CHANNEL_0_HZ + CHANNEL_SPACING_HZ * f64::from(index)
}

/// Free-space path loss at one metre for a 2.44 GHz carrier, in dB:
/// `20·log₁₀(4π·d/λ)` works out to ≈40.2 dB. Used as the reference term
/// `PL(d₀)` of the log-distance model.
pub const FREE_SPACE_LOSS_AT_1M_DB: f64 = 40.2;

/// A receiver's thermal noise floor in dBm, for a 1 MHz Bluetooth channel with
/// a typical front end. Only the *difference* from the received power matters
/// here: it sets the SNR, which sets how noisy a phase measurement is.
pub const NOISE_FLOOR_DBM: f64 = -95.0;

/// The log-distance path-loss model the simulated radio propagates with.
///
/// `RSSI(d) = P_tx − [PL(1 m) + 10·n·log₁₀(d)] + X`, where `X` is zero-mean
/// log-normal shadowing. This is the standard model (Rappaport, *Wireless
/// Communications*, Section 4.9) and the one every RSSI ranging paper
/// inverts.
///
/// The defaults describe an ordinary indoor room, **not** free space: `n = 2.7`
/// is a typical measured office/home exponent, and 3 dB of shadowing is mild.
/// They are deliberately not the values [`crate::cs::path_loss`] assumes, so a
/// demo that inverts the model without knowing the room gets the systematic
/// error real RSSI ranging suffers.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathLossModel {
    /// The advertiser's transmit power in dBm.
    pub tx_power_dbm: f64,
    /// Path loss at the one-metre reference distance, in dB.
    pub reference_loss_db: f64,
    /// Path-loss exponent `n`: 2.0 in free space, 2.7–4.0 indoors, higher
    /// still through walls.
    pub path_loss_exponent: f64,
    /// Standard deviation of the log-normal shadowing term, in dB. This is
    /// the "why is my RSSI jumping around" term; 3–8 dB is typical indoors.
    pub shadowing_sigma_db: f64,
}

impl Default for PathLossModel {
    fn default() -> Self {
        Self {
            tx_power_dbm: 0.0,
            reference_loss_db: FREE_SPACE_LOSS_AT_1M_DB,
            path_loss_exponent: 2.7,
            shadowing_sigma_db: 3.0,
        }
    }
}

impl PathLossModel {
    /// Mean path loss over `distance_m`, in dB, with no shadowing.
    ///
    /// Clamped below one metre: the log-distance model is only defined beyond
    /// its reference distance, and without a clamp a device at 0 m would
    /// receive infinite power.
    pub fn path_loss_db(&self, distance_m: f64) -> f64 {
        let d = distance_m.max(1.0);
        self.reference_loss_db + 10.0 * self.path_loss_exponent * d.log10()
    }

    /// Received power in dBm at `distance_m`, with `shadowing_db` added — pass
    /// `0.0` for the noiseless mean.
    pub fn rssi_dbm(&self, distance_m: f64, shadowing_db: f64) -> f64 {
        self.tx_power_dbm - self.path_loss_db(distance_m) + shadowing_db
    }

    /// Signal-to-noise ratio as a linear power ratio at `distance_m`, against
    /// [`NOISE_FLOOR_DBM`]. Channel Sounding's phase noise is derived from
    /// this, which is why its accuracy degrades with distance too — just far
    /// more slowly than an RSSI estimate's does.
    pub fn snr_linear(&self, distance_m: f64) -> f64 {
        let snr_db = self.rssi_dbm(distance_m, 0.0) - NOISE_FLOOR_DBM;
        10f64.powf(snr_db / 10.0)
    }
}

/// The standard deviation, in radians, of a phase measurement made at
/// `snr_linear`.
///
/// For a tone in additive white Gaussian noise the phase estimate's variance
/// is `1/(2·SNR)` (the Cramér–Rao bound for phase). Capped at π/√3 — the
/// standard deviation of a uniform distribution over a full turn — because
/// once the SNR is that bad the measurement carries no phase information at
/// all and pretending otherwise would let a demo claim accuracy it cannot
/// have.
pub fn phase_noise_sigma_rad(snr_linear: f64) -> f64 {
    let sigma = (1.0 / (2.0 * snr_linear.max(1e-9))).sqrt();
    sigma.min(PI / 3f64.sqrt())
}

/// The phase, in radians, a carrier at `freq_hz` has accumulated over
/// `distance_m` of propagation: `2π·f·d/c`.
///
/// This is the whole basis of Phase-Based Ranging. Note it is *not* wrapped —
/// callers wrap where a real receiver would, because the wrapping is exactly
/// what makes the estimator's job non-trivial.
pub fn propagation_phase_rad(distance_m: f64, freq_hz: f64) -> f64 {
    2.0 * PI * freq_hz * distance_m / SPEED_OF_LIGHT_M_PER_S
}

/// Wraps `radians` into `(-π, π]`, the range a receiver can actually observe:
/// a phase detector reports an angle, never the number of turns behind it.
pub fn wrap_phase(radians: f64) -> f64 {
    let two_pi = 2.0 * PI;
    let wrapped = radians.rem_euclid(two_pi);
    if wrapped > PI { wrapped - two_pi } else { wrapped }
}

/// A small deterministic PRNG (xorshift64*) with a Gaussian draw.
///
/// The simulator has no `rand` dependency and must run identically on wasm32,
/// where most entropy sources panic. Determinism is also what lets a test
/// assert on a noisy estimate at all: seed the link and the noise repeats.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
    /// Box–Muller produces two normal deviates per pair of uniforms; the
    /// spare is kept rather than thrown away.
    spare_normal: Option<f64>,
}

impl Default for Rng {
    fn default() -> Self {
        Self::new(0x5EED_1234_ABCD_EF01)
    }
}

impl Rng {
    /// A generator seeded with `seed` (zero is remapped, since xorshift is
    /// absorbing at zero).
    pub fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed },
            spare_normal: None,
        }
    }

    /// The next raw 64-bit value.
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A uniform draw in `(0, 1)` — open at both ends, so `ln` is safe.
    fn next_open_unit(&mut self) -> f64 {
        // 53 significant bits, shifted off zero.
        let bits = self.next_u64() >> 11;
        (bits as f64 + 0.5) / (1u64 << 53) as f64
    }

    /// A draw from `N(0, 1)`.
    pub fn normal(&mut self) -> f64 {
        if let Some(spare) = self.spare_normal.take() {
            return spare;
        }
        let u1 = self.next_open_unit();
        let u2 = self.next_open_unit();
        let radius = (-2.0 * u1.ln()).sqrt();
        let angle = 2.0 * PI * u2;
        self.spare_normal = Some(radius * angle.sin());
        radius * angle.cos()
    }

    /// A draw from `N(0, sigma²)`.
    pub fn normal_scaled(&mut self, sigma: f64) -> f64 {
        if sigma <= 0.0 {
            return 0.0;
        }
        self.normal() * sigma
    }

    /// A uniform draw over `(-π, π]` — the distribution of an unknown local
    /// oscillator's phase when two radios independently acquire a carrier.
    pub fn uniform_phase(&mut self) -> f64 {
        (self.next_open_unit() - 0.5) * 2.0 * PI
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rssi_falls_off_with_distance_at_the_configured_exponent() {
        let model = PathLossModel {
            tx_power_dbm: 0.0,
            reference_loss_db: FREE_SPACE_LOSS_AT_1M_DB,
            path_loss_exponent: 2.0,
            shadowing_sigma_db: 0.0,
        };
        // Free space: every doubling of distance costs 6.02 dB.
        let near = model.rssi_dbm(1.0, 0.0);
        let far = model.rssi_dbm(2.0, 0.0);
        assert!((near - (-40.2)).abs() < 0.01, "1 m ≈ −40.2 dBm, got {near}");
        assert!((near - far - 6.02).abs() < 0.02, "{near} vs {far}");

        // A steeper indoor exponent costs more per doubling, which is why a
        // free-space estimator over-reports distance indoors.
        let indoor = PathLossModel {
            path_loss_exponent: 3.0,
            ..model
        };
        assert!(indoor.rssi_dbm(2.0, 0.0) < far);
    }

    #[test]
    fn test_the_model_does_not_diverge_at_zero_distance() {
        let model = PathLossModel::default();
        assert_eq!(model.path_loss_db(0.0), model.path_loss_db(1.0));
        assert!(model.rssi_dbm(0.0, 0.0).is_finite());
    }

    #[test]
    fn test_phase_advances_one_turn_per_wavelength() {
        // At 2.4 GHz a wavelength is ~12.5 cm; moving one wavelength further
        // away must rotate the carrier by exactly 2π.
        let freq = 2_400_000_000.0;
        let wavelength = SPEED_OF_LIGHT_M_PER_S / freq;
        let a = propagation_phase_rad(1.0, freq);
        let b = propagation_phase_rad(1.0 + wavelength, freq);
        assert!((b - a - 2.0 * PI).abs() < 1e-9, "{a} {b}");
    }

    #[test]
    fn test_wrapping_keeps_phase_in_the_observable_half_turn() {
        for turns in -4..=4 {
            let raw = 0.5 + 2.0 * PI * f64::from(turns);
            assert!((wrap_phase(raw) - 0.5).abs() < 1e-9);
        }
        assert!((wrap_phase(PI) - PI).abs() < 1e-9);
        assert!((wrap_phase(-PI) - PI).abs() < 1e-9, "−π aliases onto +π");
    }

    #[test]
    fn test_channel_indices_map_to_the_2_4_ghz_band() {
        assert_eq!(channel_frequency_hz(0), 2_402_000_000.0);
        assert_eq!(channel_frequency_hz(78), 2_480_000_000.0);
    }

    #[test]
    fn test_phase_noise_grows_as_the_signal_weakens() {
        let strong = phase_noise_sigma_rad(10f64.powf(3.0)); // 30 dB SNR
        let weak = phase_noise_sigma_rad(10f64.powf(0.5)); // 5 dB SNR
        assert!(strong < weak, "{strong} {weak}");
        assert!(strong < 0.05, "30 dB SNR is a good measurement: {strong}");
        // No SNR at all must not be reported as a usable measurement.
        assert!((phase_noise_sigma_rad(0.0) - PI / 3f64.sqrt()).abs() < 1e-9);
    }

    #[test]
    fn test_the_generator_is_deterministic_and_roughly_normal() {
        let first: Vec<f64> = (0..8).map(|_| Rng::new(7).normal()).collect();
        assert!(first.windows(2).all(|w| w[0] == w[1]), "same seed, same draw");

        let mut rng = Rng::new(99);
        let samples: Vec<f64> = (0..20_000).map(|_| rng.normal()).collect();
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let variance =
            samples.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / samples.len() as f64;
        assert!(mean.abs() < 0.05, "mean {mean}");
        assert!((variance - 1.0).abs() < 0.08, "variance {variance}");
    }
}
