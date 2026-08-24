// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! **Channel Sounding measured by real silicon**, run through simble's own
//! decoder and arithmetic.
//!
//! `docs/test-strategy.md` names `profiles/ras.rs`, `cs/*` and
//! `device/channel_sounding.rs` as the largest body of load-bearing code in
//! this repository with no foreign oracle at all: neither Bumble nor Zephyr
//! mainline implements RAS, and the physics had no reference either — the
//! path-loss test inverts simble's own model, and every CS test has simble
//! generating the tones it later reads back.
//!
//! This file is the first oracle that code has ever had. The bytes in
//! `third_party/waves/cs_capture_excerpt.txt` came off two Nordic nRF54L15-DK
//! boards running a real Bluetooth 6.0 Channel Sounding procedure, dumped
//! verbatim from the HCI LE CS Subevent Result buffer by the firmware in
//! <https://github.com/skig/waves>. No hardware is needed to run this test and
//! nothing here fetches anything: the excerpt is vendored, MIT, attributed in
//! its own header and beside its licence.
//!
//! # What these bytes can and cannot witness
//!
//! **They witness the step records.** Step mode, step channel, step data
//! length and the mode-2 step body are laid out identically in the HCI event
//! and in the Ranging Service's Ranging Data, so real HCI steps exercise
//! `RangingData::parse` for real. Corroborated independently by
//! `third_party/rootcanal-rs/.../packets/hci_packets.pdl`, whose `CsStepData`
//! is `step_mode : 8, step_channel : 8, _size_(step_data) : 8, step_data`.
//!
//! **They do not witness the RAS headers.** The capture is a controller-side
//! HCI dump; the boards never ran RAS. The four-octet Ranging Header and
//! eight-octet Subevent Header that `RangingData` wraps around these steps —
//! including the 12-bit counter and 4-bit configuration id that were once
//! masked three bits wide — remain checked only against simble itself.
//!
//! **There is no ground truth distance.** Neither log records how far apart
//! the boards were and upstream does not state it. So the distance assertions
//! below compare simble's arithmetic against *another implementation's*
//! arithmetic on the same samples. That catches a wrong constant, a wrong
//! frequency map, a missing factor of two or an inverted convention. It
//! cannot certify either implementation against a tape measure, and nothing
//! here claims it does.
//!
//! # The reference implementation
//!
//! `waves`' phase-slope estimator (`toolset/processing/cs_phase_slope.py`)
//! does what `cs::ranging` does — sum the two ends' phases per channel,
//! unwrap across frequency, least-squares fit the slope — with three
//! differences worth naming, because they set the tolerance:
//!
//! 1. It averages every tone entry in a step whose extension slot is not
//!    marked *not expected*; simble reads the first antenna path only.
//! 2. It unwraps raw phase sums, which live in `(-2π, 2π)`, correcting by at
//!    most one turn per step; simble wraps each sum into `(-π, π]` first.
//! 3. It fits `sum/2` and scales by `c/2π`; simble fits the sum and scales by
//!    `c/4π`. Algebraically the same.

use simble::cs::ranging::{combine, estimate};
use simble::cs::tones::Tone;
use simble::profiles::ras::RangingData;

// ---------------------------------------------------------------------------
// The capture.
// ---------------------------------------------------------------------------

/// The vendored excerpt: procedures 0 and 1 from each end of one real
/// procedure, verbatim from `tests/ini.txt` and `tests/ref.txt` upstream.
const CAPTURE: &str = include_str!("../third_party/waves/cs_capture_excerpt.txt");

/// One `I: CS Subevent result received:` block: the header fields the
/// firmware printed, and the step bytes it dumped.
#[derive(Debug)]
struct CapturedSubevent {
    role: String,
    procedure_counter: u16,
    reference_power_level: i8,
    num_antenna_paths: u8,
    num_steps_reported: u8,
    /// `result->step_data_buf`, verbatim — the *Steps* portion of the HCI
    /// event and nothing else.
    steps: Vec<u8>,
}

/// Splits the excerpt into its four blocks. Deliberately dumb: it reads the
/// log text as the firmware printed it so that the vendored file stays
/// diffable line-for-line against upstream.
fn captured() -> Vec<CapturedSubevent> {
    let mut out: Vec<CapturedSubevent> = Vec::new();
    let mut role = String::new();
    let mut in_hex = false;
    for line in CAPTURE.lines() {
        if let Some(rest) = line.strip_prefix("=== ") {
            role = rest
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .to_string();
            in_hex = false;
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        if line.starts_with("I: CS Subevent result received:") {
            out.push(CapturedSubevent {
                role: role.clone(),
                procedure_counter: 0,
                reference_power_level: 0,
                num_antenna_paths: 0,
                num_steps_reported: 0,
                steps: Vec::new(),
            });
            in_hex = false;
            continue;
        }
        let Some(current) = out.last_mut() else {
            continue;
        };
        if line.starts_with("I: Raw step data:") {
            in_hex = true;
            continue;
        }
        if let Some(field) = line.strip_prefix("I:  - ") {
            in_hex = false;
            let (name, value) = field.split_once(": ").expect("a `name: value` field");
            let number = value
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .parse::<i64>();
            match (name, number) {
                ("Procedure counter", Ok(v)) => current.procedure_counter = v as u16,
                ("Reference power level", Ok(v)) => current.reference_power_level = v as i8,
                ("Num antenna paths", Ok(v)) => current.num_antenna_paths = v as u8,
                ("Num steps reported", Ok(v)) => current.num_steps_reported = v as u8,
                _ => {}
            }
            continue;
        }
        if in_hex {
            let hex = line.trim();
            assert!(
                hex.len() % 2 == 0 && hex.chars().all(|c| c.is_ascii_hexdigit()),
                "a raw-step-data line must be whole hex octets: {hex:?}"
            );
            current.steps.extend(
                (0..hex.len())
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex octet")),
            );
        }
    }
    out
}

/// The one block for `role` at `procedure_counter`.
fn subevent(role: &str, procedure_counter: u16) -> CapturedSubevent {
    captured()
        .into_iter()
        .find(|s| s.role == role && s.procedure_counter == procedure_counter)
        .unwrap_or_else(|| panic!("{role} procedure {procedure_counter} is in the excerpt"))
}

// ---------------------------------------------------------------------------
// The reference tool's published answers for the two vendored procedures.
// ---------------------------------------------------------------------------

/// `waves`' phase-slope distance for procedures 0 and 1, in metres, from
/// running its own `calculate_distance_from_phase_slope` over the same two
/// pairs of blocks. Its MUSIC estimator puts both at 1.173 m; its IFFT peak
/// lands in bin 0 and reports 0.00 m, which is an artefact of that estimator's
/// resolution rather than a third opinion, so it is not used here.
const REFERENCE_DISTANCE_M: [f64; 2] = [0.984_799, 0.982_111];

/// How far simble's answer may sit from the reference tool's.
///
/// Measured, not chosen: across **all 62** paired procedures in the full
/// upstream capture — not just the two vendored here — the largest
/// disagreement between the two implementations is **6.3 mm**, and for these
/// two it is 3.3 mm and 5.3 mm. Twenty millimetres is about three times the
/// worst case in the whole capture, which leaves headroom for the three
/// algorithmic differences listed in the module header (extension-slot
/// averaging above all) and for nothing else.
///
/// What it catches and what it does not, stated plainly. Two percent of the
/// answer at this range is enough to catch every *structural* error, because
/// those are not small: dropping the factor of two between the summed phase
/// and the one-way path doubles the answer; getting the channel spacing wrong
/// by a factor halves it; failing to unwrap scatters it entirely. It will not
/// catch a sub-percent constant — rounding `c` to 3 × 10⁸ moves this
/// measurement by 0.7 mm — and it is not meant to. Note also which way the
/// comparison runs: this tolerance is **far tighter than the measurement's own
/// precision**, since the fit's standard error on these procedures is 54 mm.
/// Two implementations agreeing to 3 mm on a number either could be 50 mm
/// wrong about is a statement about the arithmetic, not about the distance.
const TOLERANCE_M: f64 = 0.020;

// ---------------------------------------------------------------------------
// The step records.
// ---------------------------------------------------------------------------

/// Every field the firmware printed, recovered from the octets it dumped.
///
/// The step buffer lengths are the load-bearing part. 888 initiator bytes is
/// `3 × (3 + 5) + 72 × (3 + 9)` and 882 reflector bytes is
/// `3 × (3 + 3) + 72 × (3 + 9)`; both come out exact only if the step record
/// really is mode/channel/length/data, if mode-0 data is 5 octets on the
/// initiator and 3 on the reflector, and if mode-2 data is 9. Nothing here is
/// a round trip — simble did not produce a byte of it.
#[test]
fn test_the_capture_walks_as_step_records_with_no_bytes_left_over() {
    let blocks = captured();
    assert_eq!(blocks.len(), 4, "two procedures from each of two roles");

    for block in &blocks {
        let mut cursor = 0usize;
        let mut modes = std::collections::BTreeMap::<(u8, u8), usize>::new();
        let mut steps = 0usize;
        while cursor < block.steps.len() {
            let [mode, channel, len] = block.steps[cursor..cursor + 3] else {
                unreachable!()
            };
            assert!(mode <= 3, "step mode {mode} is not a Channel Sounding mode");
            assert!(channel <= 78, "channel {channel} is off the LE band");
            *modes.entry((mode, len)).or_default() += 1;
            cursor += 3 + usize::from(len);
            steps += 1;
        }
        assert_eq!(
            cursor,
            block.steps.len(),
            "{} procedure {}: the declared lengths must consume the buffer exactly",
            block.role,
            block.procedure_counter
        );
        assert_eq!(usize::from(block.num_steps_reported), steps);
        assert_eq!(steps, 75);
        assert_eq!(block.num_antenna_paths, 1);

        let mode_0_len = if block.role == "INITIATOR" { 5 } else { 3 };
        assert_eq!(
            modes,
            [((0, mode_0_len), 3), ((2, 9), 72)].into_iter().collect(),
            "{}: 3 mode-0 steps of {mode_0_len} octets and 72 mode-2 steps of 9",
            block.role
        );
    }
}

/// Real mode-2 steps declare **9** octets, not 5.
///
/// Core 6.0 Vol 4 Part E §7.7.65.44 gives mode-2 step data as an antenna
/// permutation index followed by a `Tone_PCT` and `Tone_Quality_Indicator`
/// for each of `Num_Antenna_Paths + 1` entries — the `+ 1` being the tone
/// extension slot, which is always present. With one antenna path that is
/// `1 + 2 × 4 = 9`.
///
/// `RangingData::to_bytes` wrote `1 + 4 = 5`, omitting the extension slot.
/// The round-trip test in `ras.rs` could never see it, because
/// `RangingData::parse` reads only the first tone entry and so accepts either
/// length. The reference tool's parser does not: it computes
/// `k = (len − 1) / 4` and rejects anything outside 2–5 antenna paths, so a
/// 5-octet step from simble is dropped as malformed. All 9 072 mode-2 steps
/// in the full upstream capture declare 9.
///
/// simble's *simulated controller* had this right all along —
/// `controller::sim::pbr_step` loops `0..=NUM_ANTENNA_PATHS` and cites the
/// same section — so the two encoders in this repository disagreed with each
/// other, and only the HCI one agreed with silicon.
#[test]
fn test_a_mode_2_step_carries_one_tone_entry_per_path_plus_the_extension_slot() {
    let real = subevent("INITIATOR", 0);
    // Three mode-0 steps of 3 + 5 octets precede the first mode-2 one.
    const FIRST_MODE_2: usize = 3 * (3 + 5);
    assert_eq!(
        real.steps[FIRST_MODE_2], 2,
        "step 3 is the first mode-2 one"
    );
    assert_eq!(real.steps[FIRST_MODE_2 + 2], 9, "and declares 9 octets");

    // simble's own encoder, on a single-path measurement, must now agree.
    let ours = RangingData {
        ranging_counter: 0,
        config_id: 0,
        selected_tx_power: 0,
        antenna_paths_mask: 0x01,
        reference_power_level: -16,
        tones: vec![Tone {
            channel: 5,
            i: -46,
            q: 77,
            quality: 0,
        }],
    };
    let encoded = ours.to_bytes();
    let step = &encoded[encoded.len() - 12..];
    assert_eq!(step[0], 2, "step mode 2");
    assert_eq!(step[1], 5, "step channel");
    assert_eq!(
        step[2], 9,
        "step data length must match what real silicon reports for one path"
    );
    // And the first tone entry still round-trips, so nothing regressed.
    assert_eq!(
        RangingData::parse(&encoded).expect("parsed").tones,
        ours.tones
    );
}

/// simble's Ranging Data decoder, pointed at real step records.
///
/// The 12 header octets are simble's own (the capture has no RAS traffic to
/// take them from — see the module header); every octet after them is the
/// controller's. The expected I/Q values are the reference tool's independent
/// parse of the same bytes, not simble's.
#[test]
fn test_the_ranging_data_decoder_reads_real_step_records() {
    let real = subevent("INITIATOR", 0);
    let parsed = RangingData::parse(&ras_body(&real)).expect("real steps parse");

    assert_eq!(parsed.ranging_counter, 0);
    assert_eq!(parsed.reference_power_level, -16);
    assert_eq!(parsed.tones.len(), 72, "72 mode-2 steps, 3 mode-0 skipped");

    // The first mode-2 step: channel 5, PCT octets d2 df 04 -> I = -46,
    // Q = 77, tone quality indicator 0 (high).
    assert_eq!(
        parsed.tones[0],
        Tone {
            channel: 5,
            i: -46,
            q: 77,
            quality: 0
        }
    );
    // The second: channel 58, PCT 71 40 f6 -> I = 113, Q = -156.
    assert_eq!(
        parsed.tones[1],
        Tone {
            channel: 58,
            i: 113,
            q: -156,
            quality: 0
        }
    );

    // The tone quality indicator's low nibble is the quality; a real
    // controller never reported "unavailable" (3) on the first antenna path
    // anywhere in the capture, so `Tone::is_usable`'s rejection of 3 is still
    // unwitnessed. What is witnessed is that all three of 0, 1 and 2 occur.
    let qualities: std::collections::BTreeSet<u8> = captured()
        .iter()
        .flat_map(|b| RangingData::parse(&ras_body(b)).expect("parsed").tones)
        .map(|t| t.quality)
        .collect();
    assert!(qualities.contains(&0), "high-quality tones are present");
    assert!(
        qualities.iter().all(|q| *q <= 2),
        "unavailable (3) never appears on path 0: {qualities:?}"
    );
}

/// The controller reports steps in **hop order**, not sorted by channel.
///
/// simble's simulated radio emits channels 0, 2, 4 … 36 in ascending order, so
/// nothing in-tree ever exercised the sort inside `combine`. Real steps arrive
/// 5, 58, 13, 76, 11, 73 … — the channel-selection algorithm's pseudo-random
/// sequence. If `combine` stopped sorting, every simulated test would still
/// pass and every real measurement would be fitted against a scrambled
/// frequency axis.
#[test]
fn test_real_steps_arrive_in_hop_order_and_must_be_sorted_before_fitting() {
    let tones = tones_of("INITIATOR", 0);
    let as_reported: Vec<u8> = tones.iter().map(|t| t.channel).collect();
    assert_eq!(&as_reported[..6], &[5, 58, 13, 76, 11, 73]);
    assert!(
        as_reported.windows(2).any(|w| w[0] > w[1]),
        "the capture is not already sorted"
    );

    let combined = combine(&tones, &tones_of("REFLECTOR", 0));
    assert_eq!(combined.len(), 72);
    assert!(
        combined.windows(2).all(|w| w[0].channel < w[1].channel),
        "combine must hand the fit a monotonic frequency axis"
    );
    // 1 MHz spacing over channels 2..=76, with three channels spent on the
    // mode-0 steps, so the widest gap is 4 MHz rather than the nominal 1.
    assert_eq!(combined.first().unwrap().channel, 2);
    assert_eq!(combined.last().unwrap().channel, 76);
}

// ---------------------------------------------------------------------------
// The arithmetic.
// ---------------------------------------------------------------------------

/// **A second divergence, in the HCI event around these steps.** simble's
/// LE CS Subevent Result header is two octets longer than the specification's,
/// so a real controller's event does not parse.
///
/// Core 6.0 Vol 4 Part E §7.7.65.49 packs `Procedure_Done_Status` and
/// `Subevent_Done_Status` into the two nibbles of **one** octet, and the
/// procedure and subevent abort reasons into the two nibbles of **one** more.
/// `third_party/rootcanal-rs/third_party/rootcanal/packets/hci_packets.pdl`
/// says so in as many words:
///
/// ```text
/// packet LeCsSubeventResult : LeMetaEvent (subevent_code = LE_CS_SUBEVENT_RESULT) {
///   connection_handle : 12,   _reserved_ : 4,   config_id : 8,
///   start_acl_conn_event_counter : 16,   procedure_counter : 16,
///   frequency_compensation : 16,   reference_power_level : 8,
///   procedure_done_status : 4,   subevent_done_status : 4,
///   abort_reason : 8,   num_antenna_paths : 8,   _count_(steps) : 8,
///   steps : CsStepData[],
/// }
/// ```
///
/// That is 15 octets from the subevent code to the end of the step count.
/// `packets::hci::LeCsSubeventResultHeader` gives each of the four status and
/// abort fields a whole octet, so simble's is 17, and
/// `cs::tones::parse_subevent_result` reads `num_antenna_paths` and
/// `num_steps_reported` from the two octets *after* where they end — which in
/// a real event are the step mode and step channel of step 0.
/// `controller::sim` emits the same 17-octet layout, so the two agree with
/// each other and every simulated test passes.
///
/// This asserts the consequence rather than pinning a constant: a
/// specification-shaped event carrying these real steps is mis-parsed today.
/// **When the header is fixed, this test fails — delete it then.** The fix is
/// three coordinated edits (`packets/hci.rs`'s struct, `cs/tones.rs`'s
/// `offset` module, and `controller/sim.rs`'s construction of the header) and
/// so is outside what this file changed.
#[test]
fn test_a_spec_shaped_subevent_result_is_misparsed_by_two_octets() {
    let real = subevent("INITIATOR", 0);

    // The event as the PDL above defines it: 15 octets, then the real steps.
    let mut spec_shaped = vec![0x31, 0x40, 0x00, 0x00, 0x00, 0x00];
    spec_shaped.extend_from_slice(&real.procedure_counter.to_le_bytes());
    spec_shaped.extend_from_slice(&[0xFF, 0xFF]); // frequency compensation
    spec_shaped.push(real.reference_power_level as u8);
    spec_shaped.push(0x00); // procedure done | subevent done, one octet
    spec_shaped.push(0x00); // procedure abort | subevent abort, one octet
    spec_shaped.push(real.num_antenna_paths);
    spec_shaped.push(real.num_steps_reported);
    assert_eq!(spec_shaped.len(), 15, "the PDL's header is 15 octets");
    spec_shaped.extend_from_slice(&real.steps);

    let parsed = simble::cs::tones::parse_subevent_result(&spec_shaped)
        .expect("the fields before the drift still line up");

    // Everything up to and including the reference power level is at the same
    // offset either way, so those come out right and hide the problem.
    assert_eq!(parsed.procedure_counter, 0);
    assert_eq!(parsed.reference_power_level, -16);

    // Everything after it is read two octets late. `num_antenna_paths` picks
    // up step 0's mode (0, a mode-0 step) instead of 1, and
    // `num_steps_reported` picks up step 0's channel (11) instead of 75.
    assert_eq!(
        parsed.num_antenna_paths, 0,
        "reads step 0's step_mode as the antenna path count"
    );
    assert_ne!(
        parsed.tones.len(),
        72,
        "and cannot recover the 72 mode-2 steps that are really there"
    );

    // The same steps behind simble's own 17-octet header parse perfectly,
    // which is exactly why nothing in-tree ever noticed.
    let mut simble_shaped = spec_shaped[..15].to_vec();
    simble_shaped.splice(13..13, [0x00, 0x00]); // re-split the packed nibbles
    simble_shaped.extend_from_slice(&real.steps);
    let ours = simble::cs::tones::parse_subevent_result(&simble_shaped).expect("parsed");
    assert_eq!(ours.num_antenna_paths, 1);
    assert_eq!(ours.tones.len(), 72);
    assert_eq!(ours.tones[0].channel, 5);
}

/// **The divergence.** Real silicon's phase runs the other way, and simble
/// reads a metre of real separation as zero.
///
/// A tone delayed by `τ` arrives rotated by `−2πfτ`: its phase *decreases*
/// with frequency. Both ends' Phase Correction Terms in this capture do
/// exactly that, so the sum of the two ends' phases has a negative slope
/// against frequency, and `waves` recovers a positive distance with
/// `−slope · c / 2π`.
///
/// simble's simulated radio uses the opposite sign —
/// `controller::propagation::propagation_phase_rad` returns `+2πfd/c` — and
/// `cs::ranging::estimate` matches it with `+slope · c / 4π`, then clamps a
/// negative slope to zero on the grounds that "no propagation can produce"
/// one. The two halves agree with each other, which is why all 1 402 tests
/// pass, and they agree with nothing else: fed the real thing, `estimate`
/// returns **0.0 m for all 62 procedures** in the upstream capture, silently,
/// with a plausible-looking standard error beside it.
///
/// This is pinned rather than fixed because the fix is not in this file's
/// remit: it has to flip `propagation_phase_rad` and `estimate` together, or
/// every simulated ranging test inverts. Both magnitudes are asserted so the
/// pin cannot be satisfied by an estimator that has simply stopped working.
#[test]
fn test_real_silicon_phase_slopes_downward_and_simble_reads_it_as_zero_metres() {
    for procedure in 0u16..2 {
        let combined = combine(
            &tones_of("INITIATOR", procedure),
            &tones_of("REFLECTOR", procedure),
        );
        let estimate = estimate(&combined).expect("72 tones is enough to fit");

        assert_eq!(
            estimate.distance_m, 0.0,
            "procedure {procedure}: real tones currently estimate to exactly zero"
        );
        assert_eq!(estimate.tones_used, 72);
        assert!(
            (estimate.bandwidth_hz - 74.0e6).abs() < 1.0,
            "channels 2..=76 is 74 MHz, got {}",
            estimate.bandwidth_hz
        );

        // The magnitude the clamp threw away is the right answer, and the
        // conjugate transform below recovers it. `std_error_m` survives the
        // clamp, so a caller sees 0.0 ± 0.05 m: a confident zero.
        assert!(
            estimate.std_error_m > 0.04 && estimate.std_error_m < 0.07,
            "procedure {procedure}: std error {} m",
            estimate.std_error_m
        );
    }
}

/// With the sign convention reconciled, simble's arithmetic and the reference
/// tool's agree on real samples to within [`TOLERANCE_M`].
///
/// Conjugating both ends' samples — negating Q, which is `e^{-jθ} ↦ e^{jθ}`
/// — converts the capture from the hardware's convention into the one
/// `cs::ranging` was written for. It is a single sign flip applied uniformly
/// to the input, not a fitted correction, and it is the whole of the
/// difference between the two implementations: what comes out is the
/// reference tool's number to a few millimetres.
///
/// This is the assertion the module exists for. `estimate` has never before
/// been handed a sample it did not generate.
#[test]
fn test_pbr_distance_from_real_samples_matches_the_reference_implementation() {
    for (procedure, reference) in REFERENCE_DISTANCE_M.iter().enumerate() {
        let procedure = procedure as u16;
        let local = conjugate(&tones_of("INITIATOR", procedure));
        let remote = conjugate(&tones_of("REFLECTOR", procedure));

        let estimate =
            simble::cs::ranging::estimate_from_tones(&local, &remote).expect("an estimate");

        assert!(
            (estimate.distance_m - reference).abs() < TOLERANCE_M,
            "procedure {procedure}: simble {} m vs waves {reference} m, \
             a difference of {} mm (tolerance {} mm)",
            estimate.distance_m,
            (estimate.distance_m - reference).abs() * 1000.0,
            TOLERANCE_M * 1000.0
        );

        // The fit's own bookkeeping, on a real measurement rather than a
        // noiseless synthetic one. 0.41 rad of residual scatter across 72
        // tones is what a metre of indoor multipath looks like; the simulated
        // radio, which has no multipath model at all, produces ~0.
        assert!(
            estimate.residual_rad > 0.3 && estimate.residual_rad < 0.6,
            "procedure {procedure}: residual {} rad",
            estimate.residual_rad
        );
        // The 4 MHz hole left by the mode-0 steps, not the nominal 1 MHz
        // spacing, is what bounds this measurement — 18.7 m, not 74.9 m.
        assert!(
            (estimate.unambiguous_range_m - 18.737).abs() < 0.01,
            "unambiguous range {} m",
            estimate.unambiguous_range_m
        );
        assert!(estimate.is_unambiguous());
    }
}

/// Two ends measured a metre apart are useless on their own — on real data,
/// not just in the simulator.
///
/// `cs/ranging.rs` opens with the claim that one radio's tones are, across
/// frequency, uniform noise, because the local oscillator's phase is redrawn
/// on every hop; it is the entire reason RAS exists. Until now that claim was
/// only ever tested against a simulator built to satisfy it. Here it is
/// against silicon: fit the initiator's real tones alone and the answer is
/// nowhere near the reference distance, with a residual that gives it away.
#[test]
fn test_one_real_end_alone_recovers_nothing() {
    let local = conjugate(&tones_of("INITIATOR", 0));
    let flat: Vec<Tone> = local
        .iter()
        .map(|t| Tone {
            i: 2047,
            q: 0,
            ..*t
        })
        .collect();
    let alone = simble::cs::ranging::estimate_from_tones(&local, &flat).expect("a fit, of noise");

    assert!(
        (alone.distance_m - REFERENCE_DISTANCE_M[0]).abs() > 0.5,
        "one end alone landed at {} m, near the truth by luck",
        alone.distance_m
    );
    assert!(
        alone.residual_rad > 1.0,
        "and the fit must look as bad as it is: {} rad",
        alone.residual_rad
    );
}

// ---------------------------------------------------------------------------
// Helpers.
// ---------------------------------------------------------------------------

/// Wraps real step records in a Ranging Data body.
///
/// The ranging and subevent headers are simble's own layout — the capture has
/// no RAS traffic — carrying the values the firmware printed. The steps are
/// the controller's, untouched.
fn ras_body(block: &CapturedSubevent) -> Vec<u8> {
    let mut body = Vec::with_capacity(12 + block.steps.len());
    body.extend_from_slice(&block.procedure_counter.to_le_bytes()); // counter | config id 0
    body.push(0x00); // selected TX power, 0 dBm per the log preamble
    body.push(0x01); // antenna paths mask: one path
    body.extend_from_slice(&block.procedure_counter.to_le_bytes()); // start ACL conn event
    body.extend_from_slice(&[0xFF, 0xFF]); // frequency compensation: unavailable
    body.push(0x00); // ranging done / subevent done
    body.push(0x00); // ranging abort / subevent abort
    body.push(block.reference_power_level as u8);
    body.push(block.num_steps_reported);
    body.extend_from_slice(&block.steps);
    body
}

/// The tones one captured block reports, in the order the controller reported
/// them.
fn tones_of(role: &str, procedure_counter: u16) -> Vec<Tone> {
    RangingData::parse(&ras_body(&subevent(role, procedure_counter)))
        .expect("real steps parse")
        .tones
}

/// Conjugates each sample, flipping the sense in which phase advances with
/// frequency. See
/// `test_pbr_distance_from_real_samples_matches_the_reference_implementation`.
fn conjugate(tones: &[Tone]) -> Vec<Tone> {
    tones.iter().map(|t| Tone { q: -t.q, ..*t }).collect()
}
