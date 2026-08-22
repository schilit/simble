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
/// The working buffers are owned here because `Lc3Decoder` borrows them for
/// its lifetime; a caller only sees frames in and PCM out.
pub struct Lc3Stream {
    scaler_buf: Vec<f32>,
    complex_buf: Vec<lc3_codec::common::complex::Complex>,
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
        Ok(Self {
            scaler_buf: vec![0.0; scaler_len],
            complex_buf: vec![lc3_codec::common::complex::Complex::default(); complex_len],
            samples_per_frame: (sample_rate_hz as usize * frame_duration_us as usize) / 1_000_000,
        })
    }

    /// How many PCM samples one decoded frame yields.
    pub fn samples_per_frame(&self) -> usize {
        self.samples_per_frame
    }

    /// Decodes one LC3 frame into 16-bit PCM.
    ///
    /// The decoder is rebuilt per call because it borrows the working
    /// buffers for its lifetime; that costs a little setup and keeps the
    /// borrow structure simple, which is the right trade for demo playback
    /// at one frame per 10 ms.
    pub fn decode(
        &mut self,
        frame: &[u8],
        sample_rate_hz: u32,
        frame_duration_us: u32,
    ) -> Result<Vec<i16>, Lc3Error> {
        let frequency = sampling_frequency(sample_rate_hz).ok_or(Lc3Error::UnsupportedConfig)?;
        let duration = frame_duration(frame_duration_us).ok_or(Lc3Error::UnsupportedConfig)?;
        let mut samples = vec![0i16; self.samples_per_frame];
        let mut decoder = Lc3Decoder::new(
            1,
            duration,
            frequency,
            &mut self.scaler_buf,
            &mut self.complex_buf,
        );
        decoder
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
    integer_buf: Vec<i16>,
    scaler_buf: Vec<f32>,
    complex_buf: Vec<lc3_codec::common::complex::Complex>,
    samples_per_frame: usize,
    frequency: SamplingFrequency,
    duration: FrameDuration,
}

impl Lc3Encode {
    /// Creates a mono encoder for `sample_rate_hz` and `frame_duration_us`.
    pub fn new(sample_rate_hz: u32, frame_duration_us: u32) -> Result<Self, Lc3Error> {
        let frequency = sampling_frequency(sample_rate_hz).ok_or(Lc3Error::UnsupportedConfig)?;
        let duration = frame_duration(frame_duration_us).ok_or(Lc3Error::UnsupportedConfig)?;
        let (integer_len, scaler_len, complex_len) =
            Lc3Encoder::calc_working_buffer_lengths(1, duration, frequency);
        Ok(Self {
            integer_buf: vec![0; integer_len],
            scaler_buf: vec![0.0; scaler_len],
            complex_buf: vec![lc3_codec::common::complex::Complex::default(); complex_len],
            samples_per_frame: (sample_rate_hz as usize * frame_duration_us as usize) / 1_000_000,
            frequency,
            duration,
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
        let mut encoder = Lc3Encoder::new(
            1,
            self.duration,
            self.frequency,
            &mut self.integer_buf,
            &mut self.scaler_buf,
            &mut self.complex_buf,
        );
        encoder
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

        let decoded = decoder.decode(&frame, rate, duration_us).unwrap();
        assert_eq!(decoded.len(), samples.len());
        let energy = |s: &[i16]| s.iter().map(|&v| (v as f64).abs()).sum::<f64>() / s.len() as f64;
        let (before, after) = (energy(&samples), energy(&decoded));
        assert!(
            after > before * 0.3,
            "a lossy codec still has to carry the signal: {before:.0} in, {after:.0} out"
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
        let result = stream.decode(&[0xAB; 40], 16_000, 10_000);
        // Either it decodes to something meaningless or it errors — both are
        // acceptable; panicking is not, and that is what this pins.
        if let Ok(samples) = result {
            assert_eq!(samples.len(), 160);
        }
    }
}
