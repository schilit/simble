// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! LC3 decoding for the demo pages — **not** part of simble's protocol
//! model.
//!
//! Simble treats an isochronous SDU as opaque bytes: the media plane is real
//! (framing, sequence numbers, a CIS carrying them) while what rides inside
//! is the codec's business. This module exists only so a browser page can
//! *play* what a scripted sink received, which is why it sits behind the
//! optional `lc3` feature and nothing in the core library or the `simble
//! mcp` binary references it.
//!
//! It wraps `lc3-codec`, a pure-Rust implementation chosen because it is the
//! only one that builds for `wasm32-unknown-unknown` (see
//! `docs/lc3-evaluation.md`). That crate's tests assert against golden
//! output its author captured rather than ETSI/SIG reference vectors, so
//! treat successful decoding as "the demo makes sound", never as a
//! conformance claim.

use lc3_codec::common::config::{FrameDuration, SamplingFrequency};
use lc3_codec::decoder::lc3_decoder::Lc3Decoder;
use lc3_codec::encoder::lc3_encoder::Lc3Encoder;

/// A single-channel LC3 decoder configured for one stream.
///
/// The decoder is created once and kept for the life of the stream. That is
/// not an optimization: LC3 is an MDCT codec whose decoder carries overlap,
/// long-term post-filter, and packet-loss state from each frame into the
/// next. Rebuilding it per frame — which this type used to do, because
/// `Lc3Decoder` borrows its working buffers — zeroes that state and leaves a
/// discontinuity at every frame boundary, which at 10 ms frames is 100 seams
/// a second of audible scratchiness.
pub struct Lc3Stream {
    // Field order is load-bearing: `decoder` borrows the two buffers below
    // and Rust drops fields in declaration order, so it must be dropped
    // first.
    decoder: Lc3Decoder<'static>,
    _scaler_buf: Box<[f32]>,
    _complex_buf: Box<[lc3_codec::common::complex::Complex]>,
    samples_per_frame: usize,
}

/// Why a frame could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lc3Error {
    /// The sampling frequency or frame duration is not one LC3 defines.
    UnsupportedConfig,
    /// The decoder rejected the frame (truncated, corrupt, or encoded with
    /// different parameters than this stream was configured for).
    BadFrame(String),
}

impl std::fmt::Display for Lc3Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedConfig => write!(f, "unsupported LC3 configuration"),
            Self::BadFrame(why) => write!(f, "LC3 frame rejected: {why}"),
        }
    }
}

impl std::error::Error for Lc3Error {}

/// Maps a sampling rate in Hz onto LC3's enumeration.
fn sampling_frequency(hz: u32) -> Option<SamplingFrequency> {
    Some(match hz {
        8_000 => SamplingFrequency::Hz8000,
        16_000 => SamplingFrequency::Hz16000,
        24_000 => SamplingFrequency::Hz24000,
        32_000 => SamplingFrequency::Hz32000,
        44_100 => SamplingFrequency::Hz44100,
        48_000 => SamplingFrequency::Hz48000,
        _ => return None,
    })
}

/// Maps a frame duration in microseconds onto LC3's enumeration. LC3 defines
/// exactly two: 7.5 ms and 10 ms.
fn frame_duration(micros: u32) -> Option<FrameDuration> {
    Some(match micros {
        7_500 => FrameDuration::SevenPointFiveMs,
        10_000 => FrameDuration::TenMs,
        _ => return None,
    })
}

impl Lc3Stream {
    /// Creates a mono decoder for `sample_rate_hz` and `frame_duration_us`,
    /// which must match the codec configuration the ASE was set up with —
    /// a sink decoding with different parameters than the source encoded
    /// gets noise, not an error.
    pub fn new(sample_rate_hz: u32, frame_duration_us: u32) -> Result<Self, Lc3Error> {
        let frequency = sampling_frequency(sample_rate_hz).ok_or(Lc3Error::UnsupportedConfig)?;
        let duration = frame_duration(frame_duration_us).ok_or(Lc3Error::UnsupportedConfig)?;
        let (scaler_len, complex_len) =
            Lc3Decoder::calc_working_buffer_lengths(1, duration, frequency);
        let mut scaler_buf = vec![0.0f32; scaler_len].into_boxed_slice();
        let mut complex_buf =
            vec![lc3_codec::common::complex::Complex::default(); complex_len].into_boxed_slice();

        // SAFETY: the decoder borrows these buffers for as long as it lives.
        // Both are heap allocations owned by the struct being built, so their
        // addresses are stable even when the struct itself is moved, and the
        // field order above guarantees the decoder is dropped before them.
        // Nothing else ever aliases them: they are private and only reachable
        // through the decoder.
        let (scaler_ref, complex_ref) = unsafe {
            (
                std::slice::from_raw_parts_mut(scaler_buf.as_mut_ptr(), scaler_buf.len()),
                std::slice::from_raw_parts_mut(complex_buf.as_mut_ptr(), complex_buf.len()),
            )
        };
        let decoder = Lc3Decoder::new(1, duration, frequency, scaler_ref, complex_ref);

        Ok(Self {
            decoder,
            _scaler_buf: scaler_buf,
            _complex_buf: complex_buf,
            samples_per_frame: (sample_rate_hz as usize * frame_duration_us as usize) / 1_000_000,
        })
    }

    /// How many PCM samples one decoded frame yields.
    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    /// Decodes one LC3 frame into 16-bit PCM.
    ///
    /// Frames must be fed in the order they were encoded: the decoder is
    /// stateful, so a skipped or reordered frame degrades the frames after
    /// it as well as itself.
    pub fn decode(&mut self, frame: &[u8]) -> Result<Vec<i16>, Lc3Error> {
        let mut samples = vec![0i16; self.samples_per_frame];
        self.decoder
            .decode_frame(16, 0, frame, &mut samples)
            .map_err(|e| Lc3Error::BadFrame(format!("{e:?}")))?;
        Ok(samples)
    }
}

/// A single-channel LC3 encoder, the mirror of [`Lc3Stream`].
///
/// The demo pages need this so a scripted *source* can put real LC3 frames
/// on the air rather than PCM — which makes the media plane an honest round
/// trip end to end. A sink receiving from Android needs only the decoder.
pub struct Lc3Encode {
    // Kept alive across frames for the same reason as [`Lc3Stream`]: the
    // encoder carries MDCT and post-filter state between frames.
    encoder: Lc3Encoder<'static>,
    _integer_buf: Box<[i16]>,
    _scaler_buf: Box<[f32]>,
    _complex_buf: Box<[lc3_codec::common::complex::Complex]>,
    samples_per_frame: usize,
}

impl Lc3Encode {
    /// Creates a mono encoder for `sample_rate_hz` and `frame_duration_us`.
    pub fn new(sample_rate_hz: u32, frame_duration_us: u32) -> Result<Self, Lc3Error> {
        let frequency = sampling_frequency(sample_rate_hz).ok_or(Lc3Error::UnsupportedConfig)?;
        let duration = frame_duration(frame_duration_us).ok_or(Lc3Error::UnsupportedConfig)?;
        let (integer_len, scaler_len, complex_len) =
            Lc3Encoder::calc_working_buffer_lengths(1, duration, frequency);
        let mut integer_buf = vec![0i16; integer_len].into_boxed_slice();
        let mut scaler_buf = vec![0.0f32; scaler_len].into_boxed_slice();
        let mut complex_buf =
            vec![lc3_codec::common::complex::Complex::default(); complex_len].into_boxed_slice();

        // SAFETY: as in `Lc3Stream::new` — heap buffers owned by this struct,
        // dropped after the encoder that borrows them, never aliased.
        let (integer_ref, scaler_ref, complex_ref) = unsafe {
            (
                std::slice::from_raw_parts_mut(integer_buf.as_mut_ptr(), integer_buf.len()),
                std::slice::from_raw_parts_mut(scaler_buf.as_mut_ptr(), scaler_buf.len()),
                std::slice::from_raw_parts_mut(complex_buf.as_mut_ptr(), complex_buf.len()),
            )
        };
        let encoder = Lc3Encoder::new(
            1,
            duration,
            frequency,
            integer_ref,
            scaler_ref,
            complex_ref,
        );

        Ok(Self {
            encoder,
            _integer_buf: integer_buf,
            _scaler_buf: scaler_buf,
            _complex_buf: complex_buf,
            samples_per_frame: (sample_rate_hz as usize * frame_duration_us as usize) / 1_000_000,
        })
    }

    /// How many PCM samples one frame consumes.
    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    /// Encodes one frame of PCM into `output_bytes` of LC3.
    ///
    /// `output_bytes` is the codec frame size the stream was configured
    /// with — the ASE's Octets_Per_Codec_Frame — not a buffer capacity: LC3
    /// is a constant-bitrate codec and the decoder must be told the same
    /// number.
    pub fn encode(&mut self, samples: &[i16], output_bytes: usize) -> Result<Vec<u8>, Lc3Error> {
        if samples.len() != self.samples_per_frame {
            return Err(Lc3Error::BadFrame(format!(
                "expected {} samples per frame, got {}",
                self.samples_per_frame,
                samples.len()
            )));
        }
        let mut out = vec![0u8; output_bytes];
        self.encoder
            .encode_frame(0, samples, &mut out)
            .map_err(|e| Lc3Error::BadFrame(format!("{e:?}")))?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_reports_the_frame_size_its_config_implies() {
        // 16 kHz at 10 ms is what simble's PAC record advertises.
        let stream = Lc3Stream::new(16_000, 10_000).unwrap();
        assert_eq!(stream.samples_per_frame(), 160);

        let stream = Lc3Stream::new(48_000, 10_000).unwrap();
        assert_eq!(stream.samples_per_frame(), 480);

        let stream = Lc3Stream::new(16_000, 7_500).unwrap();
        assert_eq!(stream.samples_per_frame(), 120);
    }

    #[test]
    fn test_unsupported_configurations_are_refused() {
        // LC3 defines two frame durations and a fixed set of rates; anything
        // else is a configuration error, not something to decode badly.
        assert_eq!(
            Lc3Stream::new(22_050, 10_000).err(),
            Some(Lc3Error::UnsupportedConfig)
        );
        assert_eq!(
            Lc3Stream::new(16_000, 20_000).err(),
            Some(Lc3Error::UnsupportedConfig)
        );
    }

    /// The round trip that matters: PCM in, LC3 frame on the wire, PCM out.
    /// LC3 is lossy, so this checks the signal survives — same length, and
    /// energy in the same ballpark — not sample equality.
    #[test]
    fn test_pcm_survives_an_encode_decode_round_trip() {
        let (rate, duration_us, frame_bytes) = (16_000, 10_000, 40);
        let mut encoder = Lc3Encode::new(rate, duration_us).unwrap();
        let mut decoder = Lc3Stream::new(rate, duration_us).unwrap();
        assert_eq!(encoder.samples_per_frame(), decoder.samples_per_frame());

        // A 440 Hz tone at half scale.
        let samples: Vec<i16> = (0..encoder.samples_per_frame())
            .map(|i| {
                let t = i as f32 / rate as f32;
                ((t * 440.0 * std::f32::consts::TAU).sin() * 16_000.0) as i16
            })
            .collect();

        let frame = encoder.encode(&samples, frame_bytes).unwrap();
        assert_eq!(frame.len(), frame_bytes, "LC3 is constant bitrate");
        assert!(frame.iter().any(|&b| b != 0), "the frame carries content");

        let decoded = decoder.decode(&frame).unwrap();
        assert_eq!(decoded.len(), samples.len());
        let energy = |s: &[i16]| s.iter().map(|&v| (v as f64).abs()).sum::<f64>() / s.len() as f64;
        let (before, after) = (energy(&samples), energy(&decoded));
        assert!(
            after > before * 0.3,
            "a lossy codec still has to carry the signal: {before:.0} in, {after:.0} out"
        );
    }

    /// Frames encoded by Google's liblc3 (via its `lc3py` binding) — the
    /// very implementation Android ships — from a 440 Hz tone at 16 kHz,
    /// 10 ms, 40 octets per frame, which is exactly the codec configuration
    /// simble's PAC record advertises.
    ///
    /// Everything else in this file checks simble against itself. These
    /// check it against the encoder a real phone would use, which is the
    /// only way to know the decoder would understand a phone at all.
    #[rustfmt::skip]
    const LIBLC3_440HZ_FRAMES: [[u8; 40]; 5] = [
  // frame 0
  [0xA7, 0xC9, 0xFD, 0xAC, 0x49, 0x85, 0x6D, 0xDA,
   0xCD, 0xFF, 0xCA, 0xCF, 0xCA, 0x7B, 0xF3, 0x0A,
   0x97, 0xB6, 0x8A, 0x12, 0x5A, 0x44, 0xA8, 0xEC,
   0x60, 0x84, 0xE5, 0xAD, 0x7D, 0x91, 0x01, 0xA9,
   0x9D, 0x39, 0xCB, 0x3D, 0x88, 0xAF, 0x74, 0x45],
  // frame 1
  [0xA9, 0x32, 0xE0, 0x62, 0x7F, 0x5A, 0xF6, 0xCD,
   0x19, 0x9B, 0xAA, 0x19, 0xBF, 0xC6, 0x4B, 0xB7,
   0x93, 0x48, 0x7A, 0xCF, 0x2C, 0x70, 0x1F, 0xB6,
   0x35, 0xBA, 0x18, 0xD1, 0x88, 0x4E, 0x75, 0xF9,
   0xA1, 0x7A, 0xB7, 0xA0, 0x41, 0xAF, 0x4A, 0x21],
  // frame 2
  [0xA9, 0x69, 0xAB, 0xAF, 0x5E, 0xA5, 0x91, 0x26,
   0x6A, 0x88, 0x24, 0xAE, 0x15, 0x6E, 0x55, 0x6A,
   0x87, 0x13, 0xC9, 0x1C, 0x60, 0x4A, 0x05, 0x78,
   0xCB, 0xA8, 0x60, 0xB1, 0x77, 0xB7, 0xD4, 0xF1,
   0xA6, 0x37, 0xFC, 0x3D, 0x01, 0xAF, 0x4A, 0x21],
  // frame 3
  [0x7E, 0x18, 0xAE, 0x1B, 0x15, 0x1C, 0xA9, 0x8E,
   0x76, 0x16, 0x1E, 0x27, 0x34, 0x73, 0x8B, 0xF5,
   0xD1, 0x39, 0xF4, 0xC7, 0x0D, 0x26, 0x92, 0xA0,
   0x38, 0x10, 0xCA, 0x28, 0x79, 0x13, 0x45, 0x39,
   0xA3, 0x7E, 0xB2, 0xDD, 0x41, 0xAF, 0x52, 0x1F],
  // frame 4
  [0xFE, 0x48, 0x6F, 0x5F, 0x2B, 0x5E, 0x3F, 0x03,
   0xA9, 0x8C, 0xC4, 0x17, 0xF9, 0x10, 0x95, 0x5F,
   0x83, 0xFD, 0x03, 0x22, 0xDC, 0xB8, 0x6D, 0xCF,
   0xB7, 0x90, 0x74, 0xCC, 0xB3, 0xC0, 0xE3, 0xF9,
   0xA2, 0x36, 0x7E, 0xA9, 0x01, 0xAD, 0x50, 0x25],
    ];

    /// **The interop test that matters**: foreign frames must come back as
    /// the tone that went in.
    #[test]
    fn test_frames_from_googles_liblc3_decode_to_the_original_tone() {
        let mut decoder = Lc3Stream::new(16_000, 10_000).unwrap();
        let mut pcm = Vec::new();
        for frame in &LIBLC3_440HZ_FRAMES {
            pcm.extend(
                decoder
                    .decode(frame)
                    .expect("a frame from liblc3 must decode"),
            );
        }
        assert_eq!(pcm.len(), 5 * 160, "five 10 ms frames at 16 kHz");

        // The tone should come back with real amplitude, not silence or noise.
        let peak = pcm.iter().map(|&s| (s as i32).abs()).max().unwrap();
        assert!(
            (8_000..=20_000).contains(&peak),
            "expected roughly the source amplitude (16000), got peak {peak}"
        );

        // 440 Hz over 50 ms is ~22 cycles, so ~44 zero crossings. Counting
        // them is a cheap way to prove it is the *right* tone rather than
        // merely loud: a wrong sample rate or a garbled frame lands far off.
        let crossings = pcm
            .windows(2)
            .filter(|w| (w[0] < 0) != (w[1] < 0))
            .count();
        assert!(
            (36..=52).contains(&crossings),
            "expected ~44 zero crossings for 440 Hz over 50 ms, got {crossings}"
        );
    }

    /// The decoder carries MDCT overlap, long-term post-filter, and
    /// packet-loss state from each frame into the next. It was once rebuilt
    /// per call — because `Lc3Decoder` borrows its working buffers — which
    /// zeroed that state and left a step discontinuity at every frame
    /// boundary: 100 a second at 10 ms frames, and audible as a scratchy,
    /// static-y stream.
    ///
    /// The tone test above passed throughout. A steady sine is nearly
    /// stationary, so losing the overlap barely moves its peak or its zero
    /// crossings; the damage was at the seams, so that is where this looks.
    #[test]
    fn test_decoder_state_carries_across_frame_boundaries() {
        let mut decoder = Lc3Stream::new(16_000, 10_000).unwrap();
        let mut pcm = Vec::new();
        for frame in &LIBLC3_440HZ_FRAMES {
            pcm.extend(decoder.decode(frame).expect("frame must decode"));
        }

        // The largest sample-to-sample step at a frame boundary, against the
        // largest step anywhere inside a frame. For continuous audio the two
        // are the same order of magnitude; a decoder that has forgotten its
        // state jumps at the seam.
        let step = |i: usize| (pcm[i] as i32 - pcm[i - 1] as i32).abs();
        let at_seams = (1..LIBLC3_440HZ_FRAMES.len())
            .map(|f| step(f * 160))
            .max()
            .unwrap();
        let inside = (1..pcm.len())
            .filter(|i| i % 160 != 0)
            .map(step)
            .max()
            .unwrap();

        assert!(
            at_seams <= inside * 2,
            "frame boundaries are discontinuous: the largest step at a seam \
             is {at_seams} against {inside} inside a frame, so the decoder is \
             losing its inter-frame state"
        );
    }

    #[test]
    fn test_encoder_rejects_a_wrong_length_frame() {
        let mut encoder = Lc3Encode::new(16_000, 10_000).unwrap();
        assert!(encoder.encode(&[0i16; 100], 40).is_err());
    }

    #[test]
    fn test_a_corrupt_frame_is_rejected_rather_than_panicking() {
        // A sink receives whatever is on the air; garbage must come back as
        // an error the page can show, not take the device down.
        let mut stream = Lc3Stream::new(16_000, 10_000).unwrap();
        let result = stream.decode(&[0xAB; 40]);
        // Either it decodes to something meaningless or it errors — both are
        // acceptable; panicking is not, and that is what this pins.
        if let Ok(samples) = result {
            assert_eq!(samples.len(), 160);
        }
    }
}
