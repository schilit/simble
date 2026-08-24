use super::*;
use crate::classic::a2dp::SBC_STEREO_CHANNEL_MODE;

/// Transient-rich test signal: hard onsets, a chirp through every
/// subband, and a silent gap. A steady sine hides codec bugs — the LC3
/// bug this module's testing method comes from measured 11.7 dB on music
/// while passing every sine-wave assertion.
pub(crate) fn transient_signal(samples: usize, rate: u32) -> Vec<i16> {
    let mut seed = 0x2545_F491u32;
    let mut rand = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f64 / f64::from(1u32 << 24) * 2.0 - 1.0
    };
    let mut chirp_phase = 0.0f64;
    (0..samples)
        .map(|i| {
            // A chirp that sweeps the whole band every 512 samples, so every
            // subband carries content within a couple of frames rather than
            // only after a second of audio.
            let sweep = (i % 512) as f64 / 512.0;
            let freq = 300.0 + sweep * (0.45 * f64::from(rate) - 300.0);
            chirp_phase += std::f64::consts::TAU * freq / f64::from(rate);
            let mut v = 0.35 * chirp_phase.sin();
            // A percussive onset every 256 samples: instant attack, fast
            // decay, broadband. This is what a codec bug shows up on.
            let env = (-((i % 256) as f64) / 40.0).exp();
            let t = i as f64 / f64::from(rate);
            v += env * (rand() * 0.5 + 0.3 * (std::f64::consts::TAU * 180.0 * t).sin());
            // A sustained chord underneath, so the low subbands are never
            // idle.
            for f in [220.0, 277.18, 329.63] {
                v += 0.07 * (std::f64::consts::TAU * f * t).sin();
            }
            // A silent gap in every 1024 samples: bit allocation has to cope
            // with a subband that carries nothing.
            if (700..760).contains(&(i % 1024)) {
                v = 0.0;
            }
            (v.clamp(-1.0, 0.999) * 30000.0) as i16
        })
        .collect()
}

/// Builds a stereo pair whose channels share structure but are not
/// identical, so the joint-stereo decision has something to decide. Two
/// copies of one channel would make `join` trivially always true.
pub(crate) fn stereo_from(mono: &[i16]) -> Vec<i16> {
    let n = mono.len();
    mono.iter()
        .enumerate()
        .flat_map(|(i, &l)| {
            let r = ((i32::from(l) + i32::from(mono[(i + 137) % n])) / 2) as i16;
            [l, r]
        })
        .collect()
}

/// Signal-to-noise ratio of `test` against `reference`, in dB.
pub(crate) fn snr_db(reference: &[i16], test: &[i16]) -> f64 {
    assert_eq!(reference.len(), test.len());
    let mut signal = 0.0f64;
    let mut noise = 0.0f64;
    for (&r, &t) in reference.iter().zip(test) {
        signal += f64::from(r) * f64::from(r);
        noise += (f64::from(r) - f64::from(t)).powi(2);
    }
    if noise == 0.0 {
        return f64::INFINITY;
    }
    10.0 * (signal / noise).log10()
}

fn encode_all(params: SbcParameters, pcm: &[i16]) -> Vec<u8> {
    let mut encoder = SbcEncoder::new(params).unwrap();
    let step = encoder.pcm_len();
    let mut out = Vec::new();
    for chunk in pcm.chunks_exact(step) {
        out.extend_from_slice(&encoder.encode(chunk).unwrap());
    }
    out
}

#[test]
fn test_frame_length_matches_the_specs_formula_for_every_mode() {
    // Cross-checked against the frame lengths libsbc reports for the same
    // configurations (docs/sbc-evaluation.md records the runs).
    let cases = [
        (SBC_MONO_CHANNEL_MODE, 8, 16, 32u8, 72usize),
        (SBC_JOINT_STEREO_CHANNEL_MODE, 8, 16, 53, 119),
        (SBC_STEREO_CHANNEL_MODE, 8, 16, 53, 118),
        (SBC_DUAL_CHANNEL_MODE, 8, 16, 32, 140),
        (SBC_MONO_CHANNEL_MODE, 4, 8, 16, 22),
    ];
    for (mode, subbands, blocks, bitpool, expected) in cases {
        let params = SbcParameters {
            sampling_frequency: 44100,
            block_count: blocks,
            channel_mode: mode,
            allocation_method: ALLOCATION_LOUDNESS,
            subband_count: subbands,
            bitpool,
        };
        assert_eq!(
            params.frame_length(),
            expected,
            "mode {mode} sb {subbands} blk {blocks} bp {bitpool}"
        );
    }
}

#[test]
fn test_an_encoded_frame_parses_back_as_the_configuration_it_was_made_from() {
    let params = SbcParameters::joint_stereo_44100(53);
    let mut encoder = SbcEncoder::new(params).unwrap();
    let pcm = transient_signal(encoder.pcm_len(), 44100);
    let frame = encoder.encode(&pcm).unwrap();

    let (parsed, rest) = SbcFrame::parse(&frame).expect("the a2dp parser accepts it");
    assert!(rest.is_empty(), "the frame is exactly frame_length bytes");
    assert_eq!(SbcParameters::from_frame(&parsed), params);
    assert_eq!(parsed.sample_count(), 128);
}

#[test]
fn test_the_header_crc_round_trips_through_the_decoder() {
    // The decoder verifies the CRC by default, so any disagreement
    // between the two sides shows up here rather than as noise.
    for mode in [
        SBC_MONO_CHANNEL_MODE,
        SBC_DUAL_CHANNEL_MODE,
        SBC_STEREO_CHANNEL_MODE,
        SBC_JOINT_STEREO_CHANNEL_MODE,
    ] {
        for subbands in [4u8, 8] {
            let params = SbcParameters {
                sampling_frequency: 44100,
                block_count: 16,
                channel_mode: mode,
                allocation_method: ALLOCATION_LOUDNESS,
                subband_count: subbands,
                bitpool: 32,
            };
            let mut encoder = SbcEncoder::new(params).unwrap();
            let pcm = transient_signal(encoder.pcm_len(), 44100);
            let frame = encoder.encode(&pcm).unwrap();
            let mut decoder = SbcDecoder::new();
            decoder
                .decode(&frame)
                .unwrap_or_else(|e| panic!("mode {mode} sb {subbands}: {e}"));
        }
    }
}

#[test]
fn test_a_corrupted_header_is_caught_by_the_crc() {
    let params = SbcParameters::joint_stereo_44100(53);
    let mut encoder = SbcEncoder::new(params).unwrap();
    let pcm = transient_signal(encoder.pcm_len(), 44100);
    let mut frame = encoder.encode(&pcm).unwrap();
    // Byte 5 is inside the scale factors, which the CRC covers.
    frame[5] ^= 0xFF;
    let mut decoder = SbcDecoder::new();
    assert!(matches!(
        decoder.decode(&frame),
        Err(SbcError::BadCrc { .. })
    ));
}

#[test]
fn test_a_truncated_frame_is_refused_rather_than_panicking() {
    let params = SbcParameters::joint_stereo_44100(53);
    let mut encoder = SbcEncoder::new(params).unwrap();
    let pcm = transient_signal(encoder.pcm_len(), 44100);
    let frame = encoder.encode(&pcm).unwrap();
    let mut decoder = SbcDecoder::new();
    for cut in [0, 1, 3, 4, 10, frame.len() - 1] {
        assert!(decoder.decode(&frame[..cut]).is_err(), "cut at {cut}");
    }
}

#[test]
fn test_garbage_never_panics_the_decoder() {
    // A sink receives whatever is on the air. Every byte pattern that
    // starts with the sync word has to come back as PCM or an error.
    let mut decoder = SbcDecoder::new();
    decoder.set_verify_crc(false);
    let mut seed = 1u32;
    for _ in 0..2000 {
        let mut frame = vec![SBC_SYNC_WORD];
        for _ in 0..200 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            frame.push((seed >> 16) as u8);
        }
        let _ = decoder.decode(&frame);
    }
}

#[test]
fn test_a_round_trip_keeps_the_signal_for_every_channel_mode() {
    // The bar is deliberately low: this catches a mode that is
    // structurally broken, not one that is merely lossy. Measured on this
    // (very dense) signal at bitpool 53: mono 35.3 dB, dual 34.8, stereo
    // 15.1, joint stereo 16.6 — stereo and joint share one bitpool across
    // both channels where mono and dual get it each, which is most of the
    // gap. Quality is graded against libsbc in `tests/sbc_interop_test.rs`.
    let rate = 44100;
    let pcm_mono = transient_signal(rate as usize / 2, rate);
    for mode in [
        SBC_MONO_CHANNEL_MODE,
        SBC_DUAL_CHANNEL_MODE,
        SBC_STEREO_CHANNEL_MODE,
        SBC_JOINT_STEREO_CHANNEL_MODE,
    ] {
        let params = SbcParameters {
            sampling_frequency: rate,
            block_count: 16,
            channel_mode: mode,
            allocation_method: ALLOCATION_LOUDNESS,
            subband_count: 8,
            bitpool: 53,
        };
        let channels = params.channels();
        let pcm: Vec<i16> = if channels == 1 {
            pcm_mono.clone()
        } else {
            stereo_from(&pcm_mono)
        };
        let frames = encode_all(params, &pcm);
        let mut decoder = SbcDecoder::new();
        let (decoded, _) = decoder.decode_all(&frames);

        // The filterbank delays by 10 blocks of subbands; line the two up
        // before comparing, and drop the first frame's start-up transient.
        let delay = params.filter_delay() * channels;
        let skip = params.pcm_len();
        let n = decoded.len() - delay - skip;
        let snr = snr_db(
            &pcm[skip..skip + n],
            &decoded[skip + delay..skip + delay + n],
        );
        assert!(snr > 12.0, "mode {mode} round trip only {snr:.1} dB");
    }
}

#[test]
fn test_a_decoder_reset_between_frames_corrupts_the_stream() {
    // The task this module was written for asserted that SBC frames are
    // independent. They are not: the synthesis filterbank spans twenty
    // blocks. This measures the difference so the claim cannot quietly
    // come back.
    let params = SbcParameters::joint_stereo_44100(53);
    let rate = 44100;
    let mono = transient_signal(rate as usize / 4, rate);
    let pcm = stereo_from(&mono);
    let frames = encode_all(params, &pcm);

    let mut continuous = SbcDecoder::new();
    let (streamed, _) = continuous.decode_all(&frames);

    let mut per_frame = Vec::new();
    let mut rest = &frames[..];
    while let Ok((_, samples, remainder)) = SbcDecoder::new().decode(rest) {
        per_frame.extend_from_slice(&samples);
        rest = remainder;
    }

    assert_eq!(streamed.len(), per_frame.len());
    let snr = snr_db(&streamed, &per_frame);
    assert!(
        snr < 30.0,
        "resetting the filterbank per frame should visibly damage the \
             stream, but only cost {snr:.1} dB — has the state been made \
             frame-local?"
    );
}

#[test]
fn test_a_higher_bitpool_is_a_better_reproduction() {
    // The property that would silently disappear if bit allocation
    // degraded: quality has to track the bits actually spent.
    let rate = 44100;
    let mono = transient_signal(rate as usize / 4, rate);
    let pcm = stereo_from(&mono);
    let mut previous = f64::NEG_INFINITY;
    for bitpool in [16u8, 26, 38, 53] {
        let params = SbcParameters::joint_stereo_44100(bitpool);
        let frames = encode_all(params, &pcm);
        let mut decoder = SbcDecoder::new();
        let (decoded, _) = decoder.decode_all(&frames);
        let delay = params.filter_delay() * 2;
        let skip = params.pcm_len();
        let n = decoded.len() - delay - skip;
        let snr = snr_db(
            &pcm[skip..skip + n],
            &decoded[skip + delay..skip + delay + n],
        );
        assert!(
            snr > previous,
            "bitpool {bitpool} gave {snr:.1} dB, no better than the \
                 previous step's {previous:.1} dB"
        );
        previous = snr;
    }
}

#[test]
fn test_unsupported_configurations_are_refused() {
    let bad = [
        SbcParameters {
            sampling_frequency: 22050,
            ..SbcParameters::joint_stereo_44100(53)
        },
        SbcParameters {
            block_count: 6,
            ..SbcParameters::joint_stereo_44100(53)
        },
        SbcParameters {
            subband_count: 6,
            ..SbcParameters::joint_stereo_44100(53)
        },
        SbcParameters::joint_stereo_44100(0),
        SbcParameters::joint_stereo_44100(251),
    ];
    for params in bad {
        assert!(
            SbcEncoder::new(params).is_err(),
            "accepted {params:?}, which SBC cannot express"
        );
    }
}

#[test]
fn test_the_encoder_rejects_a_wrong_length_pcm_buffer() {
    let mut encoder = SbcEncoder::new(SbcParameters::joint_stereo_44100(53)).unwrap();
    assert!(matches!(
        encoder.encode(&[0i16; 10]),
        Err(SbcError::WrongSampleCount { .. })
    ));
}
