// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SBC, the mandatory A2DP codec — encoder and decoder.
//!
//! A2DP negotiation (`crate::classic::a2dp`) and RTP packetization
//! (`crate::classic::rtp`) were complete long before anything could produce
//! or consume a byte of audio, which is the same shape of gap LE Audio had
//! when ASCS/PACS existed but no CIS carried LC3. This module closes it: PCM
//! in, valid SBC frames out, and back.
//!
//! **Written from the specification, not ported.** Every SBC implementation
//! in wide use — bluez's `libsbc`, FFmpeg's `sbcenc`/`sbcdec`, and the crates
//! that translate them — is LGPL, and simble is Apache-2.0. The algorithm is
//! fully described in the A2DP specification's Appendix B (the SBC codec
//! specification), sections 12.3 through 12.6, so it is implemented from
//! there. The two prototype filter coefficient tables below are the numeric
//! constants the specification tabulates. See `docs/sbc-evaluation.md`.
//!
//! **Frames are not independent.** The polyphase filterbank carries state
//! across frames: the analysis side holds 10 taps per subband of past input,
//! the synthesis side 20 per subband of past output. A decoder handed frame
//! *n* with a freshly zeroed filter produces a wrong first block, so a
//! stream must be decoded with one long-lived
//! [`SbcDecoder`](crate::audio::sbc::SbcDecoder). This is measured, not
//! assumed — see `test_a_decoder_reset_between_frames_corrupts_the_stream`.
//!
//! The implementation is `f64` throughout, where the reference
//! implementations are fixed point. Output therefore differs from theirs by
//! rounding, not by design; `tests/sbc_interop_test.rs` pins how far.

use crate::classic::a2dp::{
    SBC_DUAL_CHANNEL_MODE, SBC_JOINT_STEREO_CHANNEL_MODE, SBC_MONO_CHANNEL_MODE,
    SBC_SAMPLING_FREQUENCIES, SBC_SYNC_WORD, SbcFrame,
};

/// The most subbands SBC defines (A2DP spec 4.3.2: 4 or 8).
pub const MAX_SUBBANDS: usize = 8;
/// The most blocks SBC defines (4, 8, 12 or 16).
pub const MAX_BLOCKS: usize = 16;
/// SBC carries at most two channels.
pub const MAX_CHANNELS: usize = 2;

/// Bit-allocation method: loudness (0), the psychoacoustically weighted one.
pub const ALLOCATION_LOUDNESS: u8 = 0;
/// Bit-allocation method: SNR (1), which weights every subband equally.
pub const ALLOCATION_SNR: u8 = 1;

/// The polyphase filterbank spans ten blocks of `subbands` past samples.
const FILTER_TAPS: usize = 10;

/// Scale factors are a 4-bit field, so the largest exponent is 15.
const MAX_SCALE_FACTOR: u8 = 15;

/// SBC's 4-subband prototype filter, the 40 coefficients the codec
/// specification tabulates (SBC spec 12.8, `proto_4_40`). The sign pattern
/// in the second half is part of the tabulated window, not a transcription
/// slip.
const PROTO_4_40: [f64; 40] = [
    0.00000000E+00,
    5.36548976E-04,
    1.49188357E-03,
    2.73370904E-03,
    3.83720193E-03,
    3.89205149E-03,
    1.86581691E-03,
    -3.06012286E-03,
    1.09137620E-02,
    2.04385087E-02,
    2.88757392E-02,
    3.21939290E-02,
    2.58767811E-02,
    6.13245186E-03,
    -2.88217274E-02,
    -7.76463494E-02,
    1.35593274E-01,
    1.94987841E-01,
    2.46636662E-01,
    2.81828203E-01,
    2.94315332E-01,
    2.81828203E-01,
    2.46636662E-01,
    1.94987841E-01,
    -1.35593274E-01,
    -7.76463494E-02,
    -2.88217274E-02,
    6.13245186E-03,
    2.58767811E-02,
    3.21939290E-02,
    2.88757392E-02,
    2.04385087E-02,
    -1.09137620E-02,
    -3.06012286E-03,
    1.86581691E-03,
    3.89205149E-03,
    3.83720193E-03,
    2.73370904E-03,
    1.49188357E-03,
    5.36548976E-04,
];

/// SBC's 8-subband prototype filter, the 80 coefficients the codec
/// specification tabulates (SBC spec 12.8, `proto_8_80`).
const PROTO_8_80: [f64; 80] = [
    0.00000000E+00,
    1.56575398E-04,
    3.43256425E-04,
    5.54620202E-04,
    8.23919506E-04,
    1.13992507E-03,
    1.47640169E-03,
    1.78371725E-03,
    2.01182542E-03,
    2.10371989E-03,
    1.99454554E-03,
    1.61656283E-03,
    9.02154502E-04,
    -1.78805361E-04,
    -1.64973098E-03,
    -3.49717454E-03,
    5.65949473E-03,
    8.02941163E-03,
    1.04584443E-02,
    1.27472335E-02,
    1.46525263E-02,
    1.59045603E-02,
    1.62208471E-02,
    1.53184106E-02,
    1.29371806E-02,
    8.85757540E-03,
    2.92408442E-03,
    -4.91578024E-03,
    -1.46404076E-02,
    -2.61098752E-02,
    -3.90751381E-02,
    -5.31873032E-02,
    6.79989431E-02,
    8.29847578E-02,
    9.75753918E-02,
    1.11196689E-01,
    1.23264548E-01,
    1.33264415E-01,
    1.40753505E-01,
    1.45389847E-01,
    1.46955068E-01,
    1.45389847E-01,
    1.40753505E-01,
    1.33264415E-01,
    1.23264548E-01,
    1.11196689E-01,
    9.75753918E-02,
    8.29847578E-02,
    -6.79989431E-02,
    -5.31873032E-02,
    -3.90751381E-02,
    -2.61098752E-02,
    -1.46404076E-02,
    -4.91578024E-03,
    2.92408442E-03,
    8.85757540E-03,
    1.29371806E-02,
    1.53184106E-02,
    1.62208471E-02,
    1.59045603E-02,
    1.46525263E-02,
    1.27472335E-02,
    1.04584443E-02,
    8.02941163E-03,
    -5.65949473E-03,
    -3.49717454E-03,
    -1.64973098E-03,
    -1.78805361E-04,
    9.02154502E-04,
    1.61656283E-03,
    1.99454554E-03,
    2.10371989E-03,
    2.01182542E-03,
    1.78371725E-03,
    1.47640169E-03,
    1.13992507E-03,
    8.23919506E-04,
    5.54620202E-04,
    3.43256425E-04,
    1.56575398E-04,
];

/// Loudness offsets for 4 subbands, indexed by sampling-frequency index then
/// subband (SBC spec 12.6.3, `offset4`).
const LOUDNESS_OFFSET_4: [[i32; 4]; 4] = [
    [-1, 0, 0, 0],
    [-2, 0, 0, 1],
    [-2, 0, 0, 1],
    [-2, 0, 0, 1],
];

/// Loudness offsets for 8 subbands (SBC spec 12.6.3, `offset8`).
const LOUDNESS_OFFSET_8: [[i32; 8]; 4] = [
    [-2, 0, 0, 0, 0, 0, 0, 1],
    [-3, 0, 0, 0, 0, 0, 1, 2],
    [-4, 0, 0, 0, 0, 0, 1, 2],
    [-4, 0, 0, 0, 0, 0, 1, 2],
];

/// Why a frame could not be encoded or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SbcError {
    /// A field of the requested configuration is not one SBC defines.
    UnsupportedConfig(&'static str),
    /// The input ran out before a whole frame was there.
    Truncated,
    /// The frame did not start with SBC's sync word.
    BadSyncWord,
    /// The frame header's CRC did not match its contents.
    BadCrc {
        /// The CRC the frame carries.
        found: u8,
        /// The CRC its header contents imply.
        expected: u8,
    },
    /// The caller handed the encoder a PCM buffer of the wrong length.
    WrongSampleCount {
        /// What the configuration requires.
        expected: usize,
        /// What arrived.
        got: usize,
    },
}

impl std::fmt::Display for SbcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedConfig(what) => write!(f, "unsupported SBC configuration: {what}"),
            Self::Truncated => write!(f, "SBC frame truncated"),
            Self::BadSyncWord => write!(f, "not an SBC frame (bad sync word)"),
            Self::BadCrc { found, expected } => {
                write!(f, "SBC header CRC {found:#04x}, expected {expected:#04x}")
            }
            Self::WrongSampleCount { expected, got } => {
                write!(f, "expected {expected} PCM samples, got {got}")
            }
        }
    }
}

impl std::error::Error for SbcError {}

/// Everything the frame header carries about how a frame is coded.
///
/// These are the same six fields [`SbcFrame`] exposes after parsing; keeping
/// them in their own type lets an encoder be configured before any frame
/// exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbcParameters {
    /// Sampling frequency in Hz: 16000, 32000, 44100 or 48000.
    pub sampling_frequency: u32,
    /// Blocks per frame: 4, 8, 12 or 16.
    pub block_count: u8,
    /// Channel mode (`crate::classic::a2dp::SBC_*_CHANNEL_MODE`).
    pub channel_mode: u8,
    /// [`ALLOCATION_LOUDNESS`] or [`ALLOCATION_SNR`].
    pub allocation_method: u8,
    /// Subbands per block: 4 or 8.
    pub subband_count: u8,
    /// Bits shared out across the subbands of one block.
    pub bitpool: u8,
}

impl SbcParameters {
    /// The configuration A2DP sources overwhelmingly settle on: 44.1 kHz
    /// joint stereo, 8 subbands, 16 blocks, loudness allocation. `bitpool`
    /// is the one knob left, and 53 is the usual "high quality" value.
    pub fn joint_stereo_44100(bitpool: u8) -> Self {
        Self {
            sampling_frequency: 44100,
            block_count: 16,
            channel_mode: SBC_JOINT_STEREO_CHANNEL_MODE,
            allocation_method: ALLOCATION_LOUDNESS,
            subband_count: 8,
            bitpool,
        }
    }

    /// Reads the parameters back off a parsed frame.
    pub fn from_frame(frame: &SbcFrame) -> Self {
        Self {
            sampling_frequency: frame.sampling_frequency,
            block_count: frame.block_count,
            channel_mode: frame.channel_mode,
            allocation_method: frame.allocation_method,
            subband_count: frame.subband_count,
            bitpool: frame.bitpool,
        }
    }

    /// Rejects anything the frame header cannot express, so the encoder can
    /// index its tables without bounds checks later.
    pub fn validate(&self) -> Result<(), SbcError> {
        self.frequency_index()
            .ok_or(SbcError::UnsupportedConfig("sampling frequency"))?;
        if !matches!(self.block_count, 4 | 8 | 12 | 16) {
            return Err(SbcError::UnsupportedConfig("block count"));
        }
        if self.channel_mode > SBC_JOINT_STEREO_CHANNEL_MODE {
            return Err(SbcError::UnsupportedConfig("channel mode"));
        }
        if self.allocation_method > ALLOCATION_SNR {
            return Err(SbcError::UnsupportedConfig("allocation method"));
        }
        if !matches!(self.subband_count, 4 | 8) {
            return Err(SbcError::UnsupportedConfig("subband count"));
        }
        // Below 2 the allocator cannot give any subband a usable width, and
        // the header field caps the top end.
        if self.bitpool < 2 || self.bitpool > 250 {
            return Err(SbcError::UnsupportedConfig("bitpool"));
        }
        Ok(())
    }

    /// The 2-bit sampling-frequency index the header carries.
    pub fn frequency_index(&self) -> Option<usize> {
        SBC_SAMPLING_FREQUENCIES
            .iter()
            .position(|&f| f == self.sampling_frequency)
    }

    /// 1 for mono, 2 otherwise.
    pub fn channels(&self) -> usize {
        if self.channel_mode == SBC_MONO_CHANNEL_MODE {
            1
        } else {
            2
        }
    }

    /// PCM samples per channel in one frame.
    pub fn samples_per_channel(&self) -> usize {
        self.block_count as usize * self.subband_count as usize
    }

    /// Total interleaved PCM samples one frame carries.
    pub fn pcm_len(&self) -> usize {
        self.samples_per_channel() * self.channels()
    }

    /// How many bytes on the wire one frame occupies (SBC spec 12.9,
    /// `frame_length`).
    pub fn frame_length(&self) -> usize {
        let channels = self.channels();
        let subbands = self.subband_count as usize;
        let blocks = self.block_count as usize;
        let bitpool = self.bitpool as usize;
        let mut len = 4 + (4 * subbands * channels) / 8;
        len += if self.channel_mode == SBC_MONO_CHANNEL_MODE
            || self.channel_mode == SBC_DUAL_CHANNEL_MODE
        {
            (blocks * channels * bitpool).div_ceil(8)
        } else {
            let joint = usize::from(self.channel_mode == SBC_JOINT_STEREO_CHANNEL_MODE);
            (joint * subbands + blocks * bitpool).div_ceil(8)
        };
        len
    }

    /// The delay, in samples per channel, between a sample entering an
    /// encoder and the same sample leaving a decoder.
    ///
    /// The polyphase filterbank spans ten blocks on the analysis side and
    /// twenty on the synthesis side (SBC spec 12.4.3), so a round trip is not
    /// sample-aligned: `9 * subbands + 1`. Anything comparing input PCM with
    /// output PCM — a test, a latency budget, a lip-sync correction — needs
    /// this. The value was confirmed by scanning for the lag that maximises
    /// SNR through a real round trip, for both subband counts.
    pub fn filter_delay(&self) -> usize {
        9 * self.subband_count as usize + 1
    }

    /// The bit rate this configuration implies, in bits per second.
    pub fn bitrate(&self) -> u32 {
        (8 * self.frame_length() as u32 * self.sampling_frequency) / self.samples_per_channel() as u32
    }

    /// True when the header carries a joint-stereo `join` field.
    fn is_joint(&self) -> bool {
        self.channel_mode == SBC_JOINT_STEREO_CHANNEL_MODE
    }
}

// ---------------------------------------------------------------------------
// Header CRC
// ---------------------------------------------------------------------------

/// The 8-bit CRC over the frame header (SBC spec 12.3): polynomial
/// x^8 + x^4 + x^3 + x^2 + 1, seeded with 0x0F.
///
/// It covers the two bytes after the sync word, then — skipping the CRC byte
/// itself — the joint-stereo field if present and every scale factor. Bit
/// counts, not byte counts: with 4 subbands the joint field is a nibble, so
/// the covered region ends mid-byte.
struct Crc8(u8);

impl Crc8 {
    fn new() -> Self {
        Self(0x0F)
    }

    fn push_bit(&mut self, bit: u8) {
        let feedback = (self.0 >> 7) ^ (bit & 1);
        self.0 = self.0.wrapping_shl(1);
        if feedback != 0 {
            self.0 ^= 0x1D;
        }
    }

    /// Feeds `bit_len` bits starting at absolute bit offset `bit_offset`,
    /// MSB-first within each byte.
    fn push_bits(&mut self, data: &[u8], bit_offset: usize, bit_len: usize) {
        for i in bit_offset..bit_offset + bit_len {
            let byte = data[i / 8];
            self.push_bit((byte >> (7 - (i % 8))) & 1);
        }
    }
}

/// Computes the header CRC of a frame whose header fields are already
/// written into `frame`. The CRC byte itself (index 3) is skipped.
fn header_crc(frame: &[u8], params: &SbcParameters) -> u8 {
    let mut crc = Crc8::new();
    // Bytes 1 and 2: the coding parameters and the bitpool.
    crc.push_bits(frame, 8, 16);
    let joint_bits = if params.is_joint() {
        params.subband_count as usize
    } else {
        0
    };
    let scale_factor_bits = 4 * params.channels() * params.subband_count as usize;
    crc.push_bits(frame, 32, joint_bits + scale_factor_bits);
    crc.0
}

// ---------------------------------------------------------------------------
// Bit I/O
// ---------------------------------------------------------------------------

/// MSB-first bit writer over a caller-owned buffer.
struct BitWriter {
    data: Vec<u8>,
    bit: usize,
}

impl BitWriter {
    fn with_capacity(bytes: usize) -> Self {
        Self {
            data: vec![0; bytes],
            bit: 0,
        }
    }

    fn write(&mut self, value: u32, bits: usize) {
        for i in (0..bits).rev() {
            let b = ((value >> i) & 1) as u8;
            if b != 0 {
                self.data[self.bit / 8] |= 1 << (7 - (self.bit % 8));
            }
            self.bit += 1;
        }
    }
}

/// MSB-first bit reader.
struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit: 0 }
    }

    fn seek_bits(&mut self, bit: usize) {
        self.bit = bit;
    }

    fn read(&mut self, bits: usize) -> u32 {
        let mut value = 0u32;
        for _ in 0..bits {
            let byte = self.data[self.bit / 8];
            value = (value << 1) | u32::from((byte >> (7 - (self.bit % 8))) & 1);
            self.bit += 1;
        }
        value
    }
}

// ---------------------------------------------------------------------------
// Polyphase filterbanks (SBC spec 12.4.3)
// ---------------------------------------------------------------------------

/// The cosine matrices and prototype window for one subband count, computed
/// once so the per-block loops are plain multiply-accumulates.
struct FilterTables {
    subbands: usize,
    /// Analysis matrix, `[k][i] = cos((k + 0.5)(i - M/2) pi / M)`.
    analysis: Vec<f64>,
    /// Synthesis matrix, `[k][i] = cos((i + 0.5)(k + M/2) pi / M)`.
    synthesis: Vec<f64>,
    /// The prototype filter, used directly for analysis.
    proto: &'static [f64],
    /// The synthesis window, `-M` times the prototype (SBC spec 12.4.3.2).
    window: Vec<f64>,
}

impl FilterTables {
    fn new(subbands: usize) -> Self {
        let m = subbands;
        let proto: &'static [f64] = if m == 4 { &PROTO_4_40 } else { &PROTO_8_80 };
        let pi_over_m = std::f64::consts::PI / m as f64;
        let half = m as f64 / 2.0;

        let mut analysis = vec![0.0; m * 2 * m];
        for k in 0..m {
            for i in 0..2 * m {
                analysis[k * 2 * m + i] = ((k as f64 + 0.5) * (i as f64 - half) * pi_over_m).cos();
            }
        }

        let mut synthesis = vec![0.0; 2 * m * m];
        for k in 0..2 * m {
            for i in 0..m {
                synthesis[k * m + i] = ((i as f64 + 0.5) * (k as f64 + half) * pi_over_m).cos();
            }
        }

        let window = proto.iter().map(|c| -(m as f64) * c).collect();

        Self {
            subbands: m,
            analysis,
            synthesis,
            proto,
            window,
        }
    }
}

/// Analysis filterbank state: `10 * subbands` past input samples per channel.
struct AnalysisBank {
    x: [[f64; FILTER_TAPS * MAX_SUBBANDS]; MAX_CHANNELS],
}

impl AnalysisBank {
    fn new() -> Self {
        Self {
            x: [[0.0; FILTER_TAPS * MAX_SUBBANDS]; MAX_CHANNELS],
        }
    }

    /// Pushes `subbands` new PCM samples for one channel and returns the
    /// subband samples for that block (SBC spec 12.4.3.1).
    fn analyze(&mut self, tables: &FilterTables, channel: usize, input: &[f64], out: &mut [f64]) {
        let m = tables.subbands;
        let taps = FILTER_TAPS * m;
        let x = &mut self.x[channel];

        x.copy_within(0..taps - m, m);
        for (i, &sample) in input.iter().enumerate().take(m) {
            x[m - 1 - i] = sample;
        }

        // Windowing, then fold the 10M-tap window down to 2M partial sums.
        let mut y = [0.0f64; 2 * MAX_SUBBANDS];
        for (i, slot) in y.iter_mut().enumerate().take(2 * m) {
            let mut acc = 0.0;
            for j in 0..FILTER_TAPS / 2 {
                let n = i + 2 * m * j;
                acc += tables.proto[n] * x[n];
            }
            *slot = acc;
        }

        for (k, slot) in out.iter_mut().enumerate().take(m) {
            let row = &tables.analysis[k * 2 * m..k * 2 * m + 2 * m];
            *slot = row.iter().zip(&y[..2 * m]).map(|(a, b)| a * b).sum();
        }
    }
}

/// Synthesis filterbank state: `20 * subbands` past values per channel.
struct SynthesisBank {
    v: [[f64; 2 * FILTER_TAPS * MAX_SUBBANDS]; MAX_CHANNELS],
}

impl SynthesisBank {
    fn new() -> Self {
        Self {
            v: [[0.0; 2 * FILTER_TAPS * MAX_SUBBANDS]; MAX_CHANNELS],
        }
    }

    /// Turns one block of subband samples back into `subbands` PCM samples
    /// (SBC spec 12.4.3.2).
    fn synthesize(&mut self, tables: &FilterTables, channel: usize, s: &[f64], out: &mut [f64]) {
        let m = tables.subbands;
        let taps = 2 * FILTER_TAPS * m;
        let v = &mut self.v[channel];

        v.copy_within(0..taps - 2 * m, 2 * m);
        for (k, slot) in v.iter_mut().enumerate().take(2 * m) {
            let row = &tables.synthesis[k * m..k * m + m];
            *slot = row.iter().zip(&s[..m]).map(|(a, b)| a * b).sum();
        }

        // Gather the two halves of every other 4M window into a 10M vector,
        // apply the synthesis window, then fold down to M output samples.
        let mut u = [0.0f64; FILTER_TAPS * MAX_SUBBANDS];
        for i in 0..FILTER_TAPS / 2 {
            for j in 0..m {
                u[i * 2 * m + j] = v[i * 4 * m + j];
                u[i * 2 * m + m + j] = v[i * 4 * m + 3 * m + j];
            }
        }

        for (j, slot) in out.iter_mut().enumerate().take(m) {
            let mut acc = 0.0;
            for i in 0..FILTER_TAPS {
                acc += tables.window[j + m * i] * u[j + m * i];
            }
            *slot = acc;
        }
    }
}

// ---------------------------------------------------------------------------
// Scale factors and bit allocation (SBC spec 12.6)
// ---------------------------------------------------------------------------

/// The exponent that brings `max_abs` inside the quantizer's `±2^(sf+1)`
/// range. Capped at 15 because the field is four bits wide.
fn scale_factor_for(max_abs: f64) -> u8 {
    let mut sf = 0u8;
    while sf < MAX_SCALE_FACTOR && f64::from(1u32 << (sf + 1)) <= max_abs {
        sf += 1;
    }
    sf
}

/// Turns scale factors into per-subband bit widths (SBC spec 12.6.3).
///
/// Mono and dual-channel share the bitpool per channel; stereo and joint
/// stereo share one bitpool across both, which is why the two arms differ by
/// more than a loop bound.
fn allocate_bits(
    params: &SbcParameters,
    scale_factors: &[[u8; MAX_SUBBANDS]; MAX_CHANNELS],
) -> [[u8; MAX_SUBBANDS]; MAX_CHANNELS] {
    let m = params.subband_count as usize;
    let freq = params.frequency_index().expect("validated");
    let mut bits = [[0u8; MAX_SUBBANDS]; MAX_CHANNELS];

    let bitneed_of = |ch: usize, sb: usize| -> i32 {
        let sf = i32::from(scale_factors[ch][sb]);
        if params.allocation_method == ALLOCATION_SNR {
            return sf;
        }
        if sf == 0 {
            return -5;
        }
        let offset = if m == 4 {
            LOUDNESS_OFFSET_4[freq][sb]
        } else {
            LOUDNESS_OFFSET_8[freq][sb]
        };
        let loudness = sf - offset;
        if loudness > 0 { loudness / 2 } else { loudness }
    };

    // Mono and dual-channel give each channel its own bitpool, so the
    // allocator runs once per channel over M subbands. Stereo and joint
    // stereo share one bitpool across both channels, so it runs once over 2M.
    // That is a real difference in the specification, not a loop bound.
    let groups: Vec<Vec<usize>> = if matches!(
        params.channel_mode,
        SBC_MONO_CHANNEL_MODE | SBC_DUAL_CHANNEL_MODE
    ) {
        (0..params.channels()).map(|ch| vec![ch]).collect()
    } else {
        vec![vec![0, 1]]
    };

    for group in &groups {
        let mut bitneed = [[0i32; MAX_SUBBANDS]; MAX_CHANNELS];
        let mut max_bitneed = i32::MIN;
        for &ch in group {
            for (sb, need) in bitneed[ch].iter_mut().enumerate().take(m) {
                *need = bitneed_of(ch, sb);
                max_bitneed = max_bitneed.max(*need);
            }
        }

        let bitpool = i32::from(params.bitpool);
        let mut bitcount = 0i32;
        let mut slicecount = 0i32;
        let mut bitslice = max_bitneed + 1;
        loop {
            bitslice -= 1;
            bitcount += slicecount;
            slicecount = 0;
            for &ch in group {
                for &need in bitneed[ch].iter().take(m) {
                    if need > bitslice + 1 && need < bitslice + 16 {
                        slicecount += 1;
                    } else if need == bitslice + 1 {
                        slicecount += 2;
                    }
                }
            }
            if bitcount + slicecount >= bitpool {
                break;
            }
            // The loop is guaranteed to terminate for any bitpool the header
            // can express, but a corrupt frame must not hang a sink: once
            // every bitneed is far below the slice, slicecount is pinned at
            // zero and no further progress is possible.
            if bitslice < max_bitneed - 32 {
                break;
            }
        }

        for &ch in group {
            for sb in 0..m {
                bits[ch][sb] = if bitneed[ch][sb] < bitslice + 2 {
                    0
                } else {
                    (bitneed[ch][sb] - bitslice).min(16) as u8
                };
            }
        }

        // Hand out what the slice left over, one bit at a time, in the order
        // the spec fixes: subband-major, channel alternating.
        let order: Vec<(usize, usize)> = (0..m)
            .flat_map(|sb| group.iter().map(move |&ch| (ch, sb)))
            .collect();

        let mut i = 0;
        while bitcount < bitpool && i < order.len() {
            let (ch, sb) = order[i];
            if bits[ch][sb] >= 2 && bits[ch][sb] < 16 {
                bits[ch][sb] += 1;
                bitcount += 1;
            } else if bitneed[ch][sb] == bitslice + 1 && bitpool > bitcount + 1 {
                bits[ch][sb] = 2;
                bitcount += 2;
            }
            i += 1;
        }

        let mut i = 0;
        while bitcount < bitpool && i < order.len() {
            let (ch, sb) = order[i];
            if bits[ch][sb] < 16 {
                bits[ch][sb] += 1;
                bitcount += 1;
            }
            i += 1;
        }
    }

    bits
}

// ---------------------------------------------------------------------------
// Encoder
// ---------------------------------------------------------------------------

/// An SBC encoder. One instance per stream: the analysis filterbank's state
/// crosses frame boundaries, so encoding two streams through one encoder
/// leaks each into the other.
pub struct SbcEncoder {
    params: SbcParameters,
    tables: FilterTables,
    bank: AnalysisBank,
}

impl SbcEncoder {
    /// Creates an encoder for `params`, rejecting a configuration SBC cannot
    /// express.
    pub fn new(params: SbcParameters) -> Result<Self, SbcError> {
        params.validate()?;
        Ok(Self {
            tables: FilterTables::new(params.subband_count as usize),
            bank: AnalysisBank::new(),
            params,
        })
    }

    /// The configuration this encoder was built for.
    pub fn parameters(&self) -> &SbcParameters {
        &self.params
    }

    /// Interleaved PCM samples one call to [`Self::encode`] consumes.
    pub fn pcm_len(&self) -> usize {
        self.params.pcm_len()
    }

    /// Bytes each encoded frame occupies.
    pub fn frame_length(&self) -> usize {
        self.params.frame_length()
    }

    /// Encodes exactly one frame from interleaved 16-bit PCM.
    pub fn encode(&mut self, pcm: &[i16]) -> Result<Vec<u8>, SbcError> {
        let expected = self.pcm_len();
        if pcm.len() != expected {
            return Err(SbcError::WrongSampleCount {
                expected,
                got: pcm.len(),
            });
        }

        let m = self.params.subband_count as usize;
        let blocks = self.params.block_count as usize;
        let channels = self.params.channels();

        // 1. Analysis filterbank.
        let mut sb = vec![[[0.0f64; MAX_SUBBANDS]; MAX_CHANNELS]; blocks];
        let mut input = [0.0f64; MAX_SUBBANDS];
        for (blk, block) in sb.iter_mut().enumerate() {
            for ch in 0..channels {
                for i in 0..m {
                    input[i] = f64::from(pcm[(blk * m + i) * channels + ch]);
                }
                let mut out = [0.0f64; MAX_SUBBANDS];
                self.bank.analyze(&self.tables, ch, &input[..m], &mut out);
                block[ch] = out;
            }
        }

        // 2. Joint-stereo decision, per subband (SBC spec 12.6.1). The
        //    highest subband never joins — its bit is the RFA.
        let mut join = [false; MAX_SUBBANDS];
        if self.params.is_joint() {
            for (sbnd, joined) in join.iter_mut().enumerate().take(m.saturating_sub(1)) {
                let (mut max_l, mut max_r, mut max_m, mut max_s) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                for block in sb.iter() {
                    let (l, r) = (block[0][sbnd], block[1][sbnd]);
                    max_l = max_l.max(l.abs());
                    max_r = max_r.max(r.abs());
                    max_m = max_m.max(((l + r) / 2.0).abs());
                    max_s = max_s.max(((l - r) / 2.0).abs());
                }
                let plain = u32::from(scale_factor_for(max_l)) + u32::from(scale_factor_for(max_r));
                let joint = u32::from(scale_factor_for(max_m)) + u32::from(scale_factor_for(max_s));
                if joint < plain {
                    *joined = true;
                    for block in sb.iter_mut() {
                        let (l, r) = (block[0][sbnd], block[1][sbnd]);
                        block[0][sbnd] = (l + r) / 2.0;
                        block[1][sbnd] = (l - r) / 2.0;
                    }
                }
            }
        }

        // 3. Scale factors.
        let mut scale_factors = [[0u8; MAX_SUBBANDS]; MAX_CHANNELS];
        for (ch, factors) in scale_factors.iter_mut().enumerate().take(channels) {
            for (sbnd, factor) in factors.iter_mut().enumerate().take(m) {
                let max_abs = sb.iter().map(|b| b[ch][sbnd].abs()).fold(0.0f64, f64::max);
                *factor = scale_factor_for(max_abs);
            }
        }

        // 4. Bit allocation.
        let bits = allocate_bits(&self.params, &scale_factors);

        // 5. Pack.
        let frame_length = self.frame_length();
        let mut writer = BitWriter::with_capacity(frame_length);
        writer.write(u32::from(SBC_SYNC_WORD), 8);
        writer.write(self.params.frequency_index().expect("validated") as u32, 2);
        writer.write(u32::from(self.params.block_count / 4 - 1), 2);
        writer.write(u32::from(self.params.channel_mode), 2);
        writer.write(u32::from(self.params.allocation_method), 1);
        writer.write(u32::from(self.params.subband_count == 8), 1);
        writer.write(u32::from(self.params.bitpool), 8);
        writer.write(0, 8); // CRC placeholder, filled once the header is whole.

        if self.params.is_joint() {
            for joined in join.iter().take(m - 1) {
                writer.write(u32::from(*joined), 1);
            }
            writer.write(0, 1); // RFA
        }
        for factors in scale_factors.iter().take(channels) {
            for &factor in factors.iter().take(m) {
                writer.write(u32::from(factor), 4);
            }
        }

        for block in sb.iter() {
            for ch in 0..channels {
                for sbnd in 0..m {
                    let width = bits[ch][sbnd];
                    if width == 0 {
                        continue;
                    }
                    let levels = f64::from((1u32 << width) - 1);
                    let scale = f64::from(1u32 << (scale_factors[ch][sbnd] + 1));
                    let normalized = block[ch][sbnd] / scale;
                    let quantized = ((normalized + 1.0) * levels / 2.0).floor();
                    let clamped = quantized.clamp(0.0, levels) as u32;
                    writer.write(clamped, width as usize);
                }
            }
        }

        let mut frame = writer.data;
        frame[3] = header_crc(&frame, &self.params);
        Ok(frame)
    }
}

// ---------------------------------------------------------------------------
// Decoder
// ---------------------------------------------------------------------------

/// An SBC decoder. One instance per stream, for the same reason as
/// [`SbcEncoder`]: the synthesis filterbank remembers the last twenty blocks.
///
/// It re-reads the configuration from every frame header, so a source that
/// changes bitpool mid-stream — which A2DP sources do under congestion — is
/// followed without the caller doing anything.
pub struct SbcDecoder {
    bank: SynthesisBank,
    tables: Option<FilterTables>,
    verify_crc: bool,
}

impl Default for SbcDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SbcDecoder {
    /// Creates a decoder with a zeroed filterbank.
    pub fn new() -> Self {
        Self {
            bank: SynthesisBank::new(),
            tables: None,
            verify_crc: true,
        }
    }

    /// Whether to reject frames whose header CRC does not match. On by
    /// default; a caller feeding a lossy capture may prefer to hear the
    /// damage.
    pub fn set_verify_crc(&mut self, verify: bool) {
        self.verify_crc = verify;
    }

    /// Decodes one frame from the front of `data`, returning the PCM and the
    /// unconsumed remainder.
    ///
    /// PCM comes back interleaved, `parameters.pcm_len()` samples long.
    pub fn decode<'a>(
        &mut self,
        data: &'a [u8],
    ) -> Result<(SbcParameters, Vec<i16>, &'a [u8]), SbcError> {
        if data.len() < 4 {
            return Err(SbcError::Truncated);
        }
        if data[0] != SBC_SYNC_WORD {
            return Err(SbcError::BadSyncWord);
        }
        let params = SbcParameters {
            sampling_frequency: SBC_SAMPLING_FREQUENCIES[usize::from((data[1] >> 6) & 3)],
            block_count: 4 * (1 + ((data[1] >> 4) & 3)),
            channel_mode: (data[1] >> 2) & 3,
            allocation_method: (data[1] >> 1) & 1,
            subband_count: if data[1] & 1 != 0 { 8 } else { 4 },
            bitpool: data[2],
        };
        params.validate()?;

        let frame_length = params.frame_length();
        if data.len() < frame_length {
            return Err(SbcError::Truncated);
        }
        let frame = &data[..frame_length];

        if self.verify_crc {
            let expected = header_crc(frame, &params);
            if frame[3] != expected {
                return Err(SbcError::BadCrc {
                    found: frame[3],
                    expected,
                });
            }
        }

        let m = params.subband_count as usize;
        let blocks = params.block_count as usize;
        let channels = params.channels();

        // The tables depend only on the subband count, so they survive a
        // bitpool change and are rebuilt only on a 4 <-> 8 switch.
        if self.tables.as_ref().map(|t| t.subbands) != Some(m) {
            self.tables = Some(FilterTables::new(m));
        }
        let tables = self.tables.as_ref().expect("just set");

        let mut reader = BitReader::new(frame);
        reader.seek_bits(32);

        let mut join = [false; MAX_SUBBANDS];
        if params.is_joint() {
            for joined in join.iter_mut().take(m - 1) {
                *joined = reader.read(1) != 0;
            }
            reader.read(1); // RFA
        }

        let mut scale_factors = [[0u8; MAX_SUBBANDS]; MAX_CHANNELS];
        for factors in scale_factors.iter_mut().take(channels) {
            for factor in factors.iter_mut().take(m) {
                *factor = reader.read(4) as u8;
            }
        }

        let bits = allocate_bits(&params, &scale_factors);

        let mut pcm = vec![0i16; params.pcm_len()];
        let mut sb = [[0.0f64; MAX_SUBBANDS]; MAX_CHANNELS];
        let mut out = [0.0f64; MAX_SUBBANDS];
        for blk in 0..blocks {
            for ch in 0..channels {
                for sbnd in 0..m {
                    let width = bits[ch][sbnd];
                    sb[ch][sbnd] = if width == 0 {
                        0.0
                    } else {
                        let levels = f64::from((1u32 << width) - 1);
                        let raw = f64::from(reader.read(width as usize));
                        let scale = f64::from(1u32 << (scale_factors[ch][sbnd] + 1));
                        ((raw * 2.0 + 1.0) / levels - 1.0) * scale
                    };
                }
            }

            // Undo joint stereo before synthesis: the transmitted channels
            // are mid and side wherever join is set.
            if params.is_joint() {
                for (sbnd, &joined) in join.iter().enumerate().take(m) {
                    if joined {
                        let (mid, side) = (sb[0][sbnd], sb[1][sbnd]);
                        sb[0][sbnd] = mid + side;
                        sb[1][sbnd] = mid - side;
                    }
                }
            }

            for ch in 0..channels {
                self.bank.synthesize(tables, ch, &sb[ch][..m], &mut out);
                for (i, &value) in out.iter().enumerate().take(m) {
                    pcm[(blk * m + i) * channels + ch] =
                        value.round().clamp(-32768.0, 32767.0) as i16;
                }
            }
        }

        Ok((params, pcm, &data[frame_length..]))
    }

    /// Decodes every whole frame in `data`, stopping at the first byte that
    /// does not begin one. Returns the PCM and how many bytes were consumed.
    pub fn decode_all(&mut self, data: &[u8]) -> (Vec<i16>, usize) {
        let mut pcm = Vec::new();
        let mut rest = data;
        while let Ok((_, samples, remainder)) = self.decode(rest) {
            pcm.extend_from_slice(&samples);
            rest = remainder;
        }
        (pcm, data.len() - rest.len())
    }
}

#[cfg(test)]
mod tests {
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
            let snr = snr_db(&pcm[skip..skip + n], &decoded[skip + delay..skip + delay + n]);
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
            let snr = snr_db(&pcm[skip..skip + n], &decoded[skip + delay..skip + delay + n]);
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
}
