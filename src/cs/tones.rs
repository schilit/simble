// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Tones: the measurements a Channel Sounding subevent actually carries, and
//! how to get them out of an LE CS Subevent Result event.
//!
//! A mode-2 (Phase-Based Ranging) step reports a **Phase Correction Term** —
//! a complex sample of the tone the radio received on that channel, packed as
//! two 12-bit two's-complement values in three bytes. Everything a distance
//! estimate is built from is in there; this module is the boundary between
//! HCI bytes and arithmetic.
//!
//! Layouts are Core 6.0, Vol 4, Part E, Section 7.7.65.49 (LE CS Subevent
//! Result) and Section 7.7.65.44's mode-2 step table.

use serde::{Deserialize, Serialize};

/// One tone measurement: which channel it was received on, and the complex
/// sample the radio reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tone {
    /// Channel index (0–78), 1 MHz apart starting at 2402 MHz.
    pub channel: u8,
    /// In-phase component of the Phase Correction Term.
    pub i: i16,
    /// Quadrature component of the Phase Correction Term.
    pub q: i16,
    /// Tone Quality Indicator: 0 high, 1 medium, 2 low, 3 unavailable.
    pub quality: u8,
}

/// Tone Quality Indicator values a tone may be dropped for.
pub mod tone_quality {
    /// The tone was not measured at all.
    pub const UNAVAILABLE: u8 = 0x03;
}

impl Tone {
    /// The measured phase in radians, in `(-π, π]`.
    ///
    /// `atan2(0, 0)` is zero, which would silently pass a dead tone off as a
    /// perfectly-measured zero phase — callers should check
    /// [`Self::is_usable`] first.
    pub fn phase_rad(&self) -> f64 {
        f64::from(self.q).atan2(f64::from(self.i))
    }

    /// Magnitude of the complex sample, in the PCT's arbitrary units.
    pub fn magnitude(&self) -> f64 {
        (f64::from(self.i).powi(2) + f64::from(self.q).powi(2)).sqrt()
    }

    /// Whether this tone carries a usable phase: the radio flagged it as
    /// measured, and the sample is not at the origin.
    pub fn is_usable(&self) -> bool {
        self.quality != tone_quality::UNAVAILABLE && (self.i != 0 || self.q != 0)
    }
}

/// Unpacks a 24-bit Phase Correction Term: 12-bit signed I in bits 0–11,
/// 12-bit signed Q in bits 12–23, little-endian on the wire.
pub fn decode_pct(bytes: [u8; 3]) -> (i16, i16) {
    let packed = u32::from(bytes[0]) | u32::from(bytes[1]) << 8 | u32::from(bytes[2]) << 16;
    (sign_extend_12(packed & 0x0FFF), sign_extend_12(packed >> 12))
}

/// Sign-extends a 12-bit two's-complement value into an `i16`.
fn sign_extend_12(value: u32) -> i16 {
    let v = (value & 0x0FFF) as i16;
    if v & 0x0800 != 0 { v - 0x1000 } else { v }
}

/// One subevent's worth of results, as a host sees them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubeventResult {
    /// The connection the procedure runs on.
    pub connection_handle: u16,
    /// Which configuration produced this.
    pub config_id: u8,
    /// Identifies the procedure. Two ends' results only combine if this
    /// matches — tones from different procedures have different oscillator
    /// phases and averaging them together yields noise.
    pub procedure_counter: u16,
    /// The transmit power the peer used, in dBm, as the controller reports it.
    pub reference_power_level: i8,
    /// Antenna paths in each step.
    pub num_antenna_paths: u8,
    /// One tone per mode-2 step reported.
    pub tones: Vec<Tone>,
}

/// Byte offsets within an LE CS Subevent Result subevent body, counted from
/// the subevent code.
mod offset {
    /// The fixed header runs from the subevent code to `num_steps_reported`.
    pub const HEADER_LEN: usize = 17;
    /// Where `procedure_counter` starts.
    pub const PROCEDURE_COUNTER: usize = 6;
    /// Where `reference_power_level` sits.
    pub const REFERENCE_POWER: usize = 10;
    /// Where `num_antenna_paths` sits.
    pub const NUM_ANTENNA_PATHS: usize = 15;
    /// Where `num_steps_reported` sits.
    pub const NUM_STEPS: usize = 16;
}

/// Mode-2 step data begins with an antenna permutation index, before the
/// first tone's PCT.
const MODE_2_PERMUTATION_INDEX_LEN: usize = 1;

/// LE CS Subevent Result's subevent code.
pub const SUBEVENT_RESULT: u8 = 0x31;

/// Step mode 2: Phase-Based Ranging.
pub const STEP_MODE_PBR: u8 = 2;

/// Parses an LE CS Subevent Result out of an LE Meta event body — the bytes
/// after the event code and length, starting with the subevent code.
///
/// Returns `None` for any other subevent, or for a truncated event. Only
/// mode-2 steps produce tones; mode-0 and mode-1 steps are skipped by their
/// declared length rather than rejected, so a controller that interleaves
/// them does not break the parse.
pub fn parse_subevent_result(body: &[u8]) -> Option<SubeventResult> {
    if body.first()? != &SUBEVENT_RESULT || body.len() < offset::HEADER_LEN {
        return None;
    }
    let mut result = SubeventResult {
        connection_handle: u16::from_le_bytes([body[1], body[2]]) & 0x0FFF,
        config_id: body[3],
        procedure_counter: u16::from_le_bytes([
            body[offset::PROCEDURE_COUNTER],
            body[offset::PROCEDURE_COUNTER + 1],
        ]),
        reference_power_level: body[offset::REFERENCE_POWER] as i8,
        num_antenna_paths: body[offset::NUM_ANTENNA_PATHS],
        tones: Vec::new(),
    };

    let mut cursor = offset::HEADER_LEN;
    for _ in 0..body[offset::NUM_STEPS] {
        // step_mode(1) step_channel(1) step_data_length(1) step_data(n)
        let [mode, channel, data_len] = *body.get(cursor..cursor + 3)? else {
            return None;
        };
        let data = body.get(cursor + 3..cursor + 3 + usize::from(data_len))?;
        cursor += 3 + usize::from(data_len);
        if mode != STEP_MODE_PBR {
            continue;
        }
        // The first antenna path's tone; further paths and the extension slot
        // follow, and are not used — one path is what the simulated radio
        // reports and what a 1:1 antenna configuration provides.
        let pct = data.get(
            MODE_2_PERMUTATION_INDEX_LEN..MODE_2_PERMUTATION_INDEX_LEN + 4,
        )?;
        let (i, q) = decode_pct([pct[0], pct[1], pct[2]]);
        result.tones.push(Tone {
            channel,
            i,
            q,
            quality: pct[3],
        });
    }
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    #[test]
    fn test_a_phase_correction_term_round_trips_through_its_packing() {
        // Build a PCT the way the controller does, and read it back.
        let pack = |i: i32, q: i32| {
            let packed = (i as u32 & 0x0FFF) | ((q as u32 & 0x0FFF) << 12);
            [packed as u8, (packed >> 8) as u8, (packed >> 16) as u8]
        };
        assert_eq!(decode_pct(pack(2047, -2048)), (2047, -2048));
        assert_eq!(decode_pct(pack(-1, 1)), (-1, 1));
        assert_eq!(decode_pct(pack(0, 0)), (0, 0));
    }

    #[test]
    fn test_a_tone_reports_the_angle_of_its_sample() {
        let quarter_turn = Tone {
            channel: 0,
            i: 0,
            q: 1000,
            quality: 0,
        };
        assert!((quarter_turn.phase_rad() - PI / 2.0).abs() < 1e-9);
        assert!((quarter_turn.magnitude() - 1000.0).abs() < 1e-9);
    }

    #[test]
    fn test_an_unmeasured_tone_is_not_mistaken_for_a_zero_phase() {
        let dead = Tone {
            channel: 4,
            i: 0,
            q: 0,
            quality: tone_quality::UNAVAILABLE,
        };
        assert!(!dead.is_usable());
        // atan2(0, 0) is 0.0, which is exactly the trap is_usable exists for.
        assert_eq!(dead.phase_rad(), 0.0);
    }

    /// Builds a subevent body with `steps` of `(mode, channel, data)`.
    fn subevent_body(procedure_counter: u16, steps: &[(u8, u8, Vec<u8>)]) -> Vec<u8> {
        let mut body = vec![SUBEVENT_RESULT, 0x40, 0x00, 0x01, 0x00, 0x00];
        body.extend_from_slice(&procedure_counter.to_le_bytes());
        body.extend_from_slice(&[0xFF, 0xFF]); // frequency compensation
        body.push(0xC5_u8); // reference power level, −59 dBm
        body.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // done / abort status
        body.push(0x01); // num antenna paths
        body.push(steps.len() as u8);
        for (mode, channel, data) in steps {
            body.push(*mode);
            body.push(*channel);
            body.push(data.len() as u8);
            body.extend_from_slice(data);
        }
        body
    }

    #[test]
    fn test_parsing_pulls_one_tone_out_of_each_phase_based_step() {
        // Permutation index, then I = 2047, Q = 0, then quality "high".
        let pct = vec![0x00, 0xFF, 0x07, 0x00, 0x00];
        let body = subevent_body(
            7,
            &[
                (STEP_MODE_PBR, 0, pct.clone()),
                (STEP_MODE_PBR, 2, pct.clone()),
            ],
        );
        let parsed = parse_subevent_result(&body).expect("parsed");
        assert_eq!(parsed.connection_handle, 0x0040);
        assert_eq!(parsed.procedure_counter, 7);
        assert_eq!(parsed.reference_power_level, -59);
        assert_eq!(parsed.tones.len(), 2);
        assert_eq!(parsed.tones[1].channel, 2);
        assert_eq!(parsed.tones[0].i, 2047);
    }

    #[test]
    fn test_steps_in_other_modes_are_stepped_over_not_misread() {
        // A mode-0 step's data has nothing tone-shaped in it. Reading it as a
        // PCT would invent a measurement out of a packet-quality byte.
        let mode_0 = vec![0x01, 0x02, 0x03];
        let pbr = vec![0x00, 0x00, 0x08, 0x00, 0x00]; // I = 0, Q = 128
        let body = subevent_body(1, &[(0, 0, mode_0), (STEP_MODE_PBR, 6, pbr)]);
        let parsed = parse_subevent_result(&body).expect("parsed");
        assert_eq!(parsed.tones.len(), 1, "only the mode-2 step is a tone");
        assert_eq!(parsed.tones[0].channel, 6);
    }

    #[test]
    fn test_a_truncated_event_is_rejected_rather_than_half_parsed() {
        let pct = vec![0x00, 0xFF, 0x07, 0x00, 0x00];
        let full = subevent_body(1, &[(STEP_MODE_PBR, 0, pct)]);
        for cut in 1..full.len() {
            assert!(
                parse_subevent_result(&full[..cut]).is_none_or(|r| r.tones.is_empty()),
                "a body cut at {cut} must not yield a tone"
            );
        }
        assert!(parse_subevent_result(&[]).is_none());
        assert!(
            parse_subevent_result(&[0x30, 0x00, 0x00]).is_none(),
            "a different subevent is not a subevent result"
        );
    }
}
