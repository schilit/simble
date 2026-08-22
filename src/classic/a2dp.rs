// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Advanced Audio Distribution Profile (A2DP): codec capability structures
//! carried inside AVDTP Media Codec service capabilities
//! (`crate::classic::avdtp::MediaCodecCapabilities`), plus SDP service
//! records for the Audio Source/Sink roles.
//!
//! A2DP is mostly data over AVDTP's Set_Configuration: SBC (mandatory),
//! MPEG-2/4 AAC, and vendor-specific codecs (Opus rides the vendor-specific
//! codec type). The codec-specific information elements here encode to and
//! decode from the raw bytes AVDTP transports.
//!
//! Scope: this module is negotiation and framing. It parses and writes the
//! frame *headers* — SBC's and ADTS's — because those carry the codec
//! parameters negotiation is about, and because a sink has to be able to
//! find frame boundaries in an RTP payload.
//!
//! The codecs themselves live elsewhere, and only one of them exists:
//!
//! - **SBC**, the mandatory codec, is implemented in
//!   [`crate::audio::sbc`] — a real encoder and decoder, verified against
//!   bluez's `libsbc` in both directions. [`SbcFrame`] parses a frame's
//!   header and hands back its bytes; `SbcDecoder` turns those bytes into
//!   PCM.
//! - **AAC** is framing only. [`AacFrame`] reads and writes ADTS headers and
//!   treats the raw data blocks as opaque; simble has no AAC codec and
//!   deliberately does not plan one (`docs/sbc-evaluation.md` section 6).
//! - **Opus** and the other vendor-specific codecs are capability structures
//!   only.

use crate::classic::avdtp::{
    ADVANCED_AUDIO_DISTRIBUTION_SERVICE_UUID, AVDTP_PROTOCOL_UUID, AVDTP_PSM, DEFAULT_VERSION,
    MediaCodecCapabilities, MediaType,
};
use crate::classic::sdp::{DataElement, SdpUuid, ServiceAttribute, attribute_id};

/// Audio Source service class UUID (0x110A).
pub(crate) const AUDIO_SOURCE_SERVICE_UUID: SdpUuid = SdpUuid::Uuid16(0x110A);
/// Audio Sink service class UUID (0x110B).
pub(crate) const AUDIO_SINK_SERVICE_UUID: SdpUuid = SdpUuid::Uuid16(0x110B);

/// Media codec types (A2DP spec Table 4.1 / Assigned Numbers).
pub mod codec_type {
    /// SBC, the mandatory A2DP codec.
    pub const SBC: u8 = 0x00;
    /// MPEG-1/2 Audio (MP3).
    pub const MPEG_1_2_AUDIO: u8 = 0x01;
    /// MPEG-2/4 AAC.
    pub(crate) const MPEG_2_4_AAC: u8 = 0x02;
    /// ATRAC family.
    pub const ATRAC_FAMILY: u8 = 0x03;
    /// Vendor-specific (non-A2DP) codec.
    pub const NON_A2DP: u8 = 0xFF;
}

/// SBC capability bitmasks (A2DP spec 4.3.2, Codec Specific Information
/// Elements).
pub mod sbc {
    /// SBC sampling-frequency capability bits.
    pub mod sampling_frequency {
        /// 16 kHz.
        pub const SF_16000: u8 = 1 << 3;
        /// 32 kHz.
        pub const SF_32000: u8 = 1 << 2;
        /// 44.1 kHz.
        pub const SF_44100: u8 = 1 << 1;
        /// 48 kHz.
        pub const SF_48000: u8 = 1 << 0;
    }
    /// SBC channel-mode capability bits.
    pub mod channel_mode {
        /// Mono.
        pub const MONO: u8 = 1 << 3;
        /// Dual channel.
        pub const DUAL_CHANNEL: u8 = 1 << 2;
        /// Stereo.
        pub const STEREO: u8 = 1 << 1;
        /// Joint stereo.
        pub const JOINT_STEREO: u8 = 1 << 0;
    }
    /// SBC block-length capability bits.
    pub mod block_length {
        /// 4 blocks.
        pub const BL_4: u8 = 1 << 3;
        /// 8 blocks.
        pub const BL_8: u8 = 1 << 2;
        /// 12 blocks.
        pub const BL_12: u8 = 1 << 1;
        /// 16 blocks.
        pub const BL_16: u8 = 1 << 0;
    }
    /// SBC subband-count capability bits.
    pub mod subbands {
        /// 4 subbands.
        pub const S_4: u8 = 1 << 1;
        /// 8 subbands.
        pub const S_8: u8 = 1 << 0;
    }
    /// SBC bit-allocation-method capability bits.
    pub mod allocation_method {
        /// SNR allocation.
        pub const SNR: u8 = 1 << 1;
        /// Loudness allocation.
        pub const LOUDNESS: u8 = 1 << 0;
    }
}

/// AAC capability bitmasks (A2DP spec 4.5.2, Codec Specific Information
/// Elements).
pub mod aac {
    /// AAC object-type capability bits.
    pub mod object_type {
        /// MPEG-2 AAC LC.
        pub const MPEG_2_AAC_LC: u8 = 1 << 7;
        /// MPEG-4 AAC LC.
        pub const MPEG_4_AAC_LC: u8 = 1 << 6;
        /// MPEG-4 AAC LTP.
        pub const MPEG_4_AAC_LTP: u8 = 1 << 5;
        /// MPEG-4 AAC Scalable.
        pub const MPEG_4_AAC_SCALABLE: u8 = 1 << 4;
    }
    /// AAC sampling-frequency capability bits.
    pub mod sampling_frequency {
        /// 8 kHz.
        pub const SF_8000: u16 = 1 << 11;
        /// 11.025 kHz.
        pub const SF_11025: u16 = 1 << 10;
        /// 12 kHz.
        pub const SF_12000: u16 = 1 << 9;
        /// 16 kHz.
        pub const SF_16000: u16 = 1 << 8;
        /// 22.05 kHz.
        pub const SF_22050: u16 = 1 << 7;
        /// 24 kHz.
        pub const SF_24000: u16 = 1 << 6;
        /// 32 kHz.
        pub const SF_32000: u16 = 1 << 5;
        /// 44.1 kHz.
        pub const SF_44100: u16 = 1 << 4;
        /// 48 kHz.
        pub const SF_48000: u16 = 1 << 3;
        /// 64 kHz.
        pub const SF_64000: u16 = 1 << 2;
        /// 88.2 kHz.
        pub const SF_88200: u16 = 1 << 1;
        /// 96 kHz.
        pub const SF_96000: u16 = 1 << 0;
    }
    /// AAC channel-count capability bits.
    pub mod channels {
        /// One channel (mono).
        pub const MONO: u8 = 1 << 1;
        /// Two channels (stereo).
        pub const STEREO: u8 = 1 << 0;
    }
}

/// Opus (vendor-specific) capability bitmasks.
pub mod opus {
    /// Opus channel-mode capability bits.
    pub mod channel_mode {
        /// Mono.
        pub const MONO: u8 = 1 << 0;
        /// Stereo.
        pub const STEREO: u8 = 1 << 1;
        /// Dual mono.
        pub const DUAL_MONO: u8 = 1 << 2;
    }
    /// Opus frame-size capability bits.
    pub mod frame_size {
        /// 10 ms frames.
        pub const FS_10MS: u8 = 1 << 0;
        /// 20 ms frames.
        pub const FS_20MS: u8 = 1 << 1;
    }
    /// Opus sampling-frequency capability bits.
    pub mod sampling_frequency {
        /// 48 kHz.
        pub const SF_48000: u8 = 1 << 0;
    }
}

// ---------------------------------------------------------------------------
// Codec-specific information elements
// ---------------------------------------------------------------------------

/// SBC codec-specific information element (A2DP spec 4.3.2). Each mask
/// field holds `sbc::*` bit flags: multiple bits set express a capability,
/// exactly one bit a configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbcMediaCodecInformation {
    /// Sampling-frequency capability/config mask (`sbc::sampling_frequency`).
    pub sampling_frequency: u8,
    /// Channel-mode mask (`sbc::channel_mode`).
    pub channel_mode: u8,
    /// Block-length mask (`sbc::block_length`).
    pub block_length: u8,
    /// Subband-count mask (`sbc::subbands`).
    pub subbands: u8,
    /// Bit-allocation-method mask (`sbc::allocation_method`).
    pub allocation_method: u8,
    /// Minimum SBC bitpool value.
    pub minimum_bitpool_value: u8,
    /// Maximum SBC bitpool value.
    pub maximum_bitpool_value: u8,
}

impl SbcMediaCodecInformation {
    /// Decodes the 4-byte SBC codec information element.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        Some(Self {
            sampling_frequency: (data[0] >> 4) & 0x0F,
            channel_mode: data[0] & 0x0F,
            block_length: (data[1] >> 4) & 0x0F,
            subbands: (data[1] >> 2) & 0x03,
            allocation_method: data[1] & 0x03,
            minimum_bitpool_value: data[2],
            maximum_bitpool_value: data[3],
        })
    }

    /// Encodes to the 4-byte SBC codec information element.
    pub fn to_bytes(self) -> [u8; 4] {
        [
            (self.sampling_frequency << 4) | self.channel_mode,
            (self.block_length << 4) | (self.subbands << 2) | self.allocation_method,
            self.minimum_bitpool_value,
            self.maximum_bitpool_value,
        ]
    }

    /// Converts a sampling rate in Hz to its capability flag.
    pub fn sampling_frequency_flag(hz: u32) -> Option<u8> {
        match hz {
            16000 => Some(sbc::sampling_frequency::SF_16000),
            32000 => Some(sbc::sampling_frequency::SF_32000),
            44100 => Some(sbc::sampling_frequency::SF_44100),
            48000 => Some(sbc::sampling_frequency::SF_48000),
            _ => None,
        }
    }

    /// Intersects two capability sets for negotiation: the common bits of
    /// every mask plus the overlapping bitpool range, or `None` when the
    /// capabilities have no common operating point.
    pub fn intersect(&self, other: &Self) -> Option<Self> {
        let result = Self {
            sampling_frequency: self.sampling_frequency & other.sampling_frequency,
            channel_mode: self.channel_mode & other.channel_mode,
            block_length: self.block_length & other.block_length,
            subbands: self.subbands & other.subbands,
            allocation_method: self.allocation_method & other.allocation_method,
            minimum_bitpool_value: self.minimum_bitpool_value.max(other.minimum_bitpool_value),
            maximum_bitpool_value: self.maximum_bitpool_value.min(other.maximum_bitpool_value),
        };
        (result.sampling_frequency != 0
            && result.channel_mode != 0
            && result.block_length != 0
            && result.subbands != 0
            && result.allocation_method != 0
            && result.minimum_bitpool_value <= result.maximum_bitpool_value)
            .then_some(result)
    }
}

/// MPEG-2/4 AAC codec-specific information element (A2DP spec 4.5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AacMediaCodecInformation {
    /// Object-type mask (`aac::object_type`).
    pub object_type: u8,
    /// 12-bit `aac::sampling_frequency` mask.
    pub sampling_frequency: u16,
    /// Channel-count mask (`aac::channels`).
    pub channels: u8,
    /// Whether variable bit rate is supported.
    pub vbr: bool,
    /// 23-bit maximum bit rate; 0 means unknown.
    pub bitrate: u32,
}

impl AacMediaCodecInformation {
    /// Decodes the 6-byte AAC codec information element.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 6 {
            return None;
        }
        Some(Self {
            object_type: data[0],
            sampling_frequency: ((data[1] as u16) << 4) | ((data[2] >> 4) as u16),
            channels: (data[2] >> 2) & 0x03,
            vbr: (data[3] >> 7) & 1 != 0,
            bitrate: (((data[3] & 0x7F) as u32) << 16) | ((data[4] as u32) << 8) | data[5] as u32,
        })
    }

    /// Encodes to the 6-byte AAC codec information element.
    pub fn to_bytes(self) -> [u8; 6] {
        [
            self.object_type,
            (self.sampling_frequency >> 4) as u8,
            (((self.sampling_frequency & 0x0F) as u8) << 4) | (self.channels << 2),
            ((self.vbr as u8) << 7) | ((self.bitrate >> 16) as u8 & 0x7F),
            (self.bitrate >> 8) as u8,
            self.bitrate as u8,
        ]
    }
}

/// Vendor-specific codec information element (A2DP spec 4.7.2): a 32-bit
/// company ID, a 16-bit codec ID (both little-endian), and opaque value
/// bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorSpecificMediaCodecInformation {
    /// 32-bit vendor (company) ID, little-endian on the wire.
    pub vendor_id: u32,
    /// 16-bit vendor-assigned codec ID, little-endian on the wire.
    pub codec_id: u16,
    /// Opaque vendor codec-specific value bytes.
    pub value: Vec<u8>,
}

impl VendorSpecificMediaCodecInformation {
    /// Decodes a vendor-specific codec information element.
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 6 {
            return None;
        }
        Some(Self {
            vendor_id: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            codec_id: u16::from_le_bytes([data[4], data[5]]),
            value: data[6..].to_vec(),
        })
    }

    /// Encodes to a vendor-specific codec information element.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(6 + self.value.len());
        out.extend_from_slice(&self.vendor_id.to_le_bytes());
        out.extend_from_slice(&self.codec_id.to_le_bytes());
        out.extend_from_slice(&self.value);
        out
    }
}

/// Opus codec information, carried as a vendor-specific codec
/// ([`OpusMediaCodecInformation::VENDOR_ID`]/`CODEC_ID`) whose single value
/// byte packs channel mode, frame size, and sampling frequency flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpusMediaCodecInformation {
    /// Channel-mode mask (`opus::channel_mode`).
    pub channel_mode: u8,
    /// Frame-size mask (`opus::frame_size`).
    pub frame_size: u8,
    /// Sampling-frequency mask (`opus::sampling_frequency`).
    pub sampling_frequency: u8,
}

impl OpusMediaCodecInformation {
    /// Vendor ID under which Opus is carried.
    pub const VENDOR_ID: u32 = 0x0000_00E0;
    /// Vendor-assigned codec ID for Opus.
    pub const CODEC_ID: u16 = 0x0001;

    /// Parses the vendor-specific `value` byte (excluding vendor/codec IDs).
    pub fn parse_value(value: &[u8]) -> Option<Self> {
        let byte = *value.first()?;
        Some(Self {
            channel_mode: byte & 0x07,
            frame_size: (byte >> 3) & 0x03,
            sampling_frequency: (byte >> 7) & 0x01,
        })
    }

    /// Packs channel mode, frame size, and sampling frequency into the single value byte.
    pub fn value_byte(self) -> u8 {
        self.channel_mode | (self.frame_size << 3) | (self.sampling_frequency << 7)
    }

    /// Wraps this Opus configuration as a vendor-specific codec information element.
    pub fn to_vendor_information(self) -> VendorSpecificMediaCodecInformation {
        VendorSpecificMediaCodecInformation {
            vendor_id: Self::VENDOR_ID,
            codec_id: Self::CODEC_ID,
            value: vec![self.value_byte()],
        }
    }
}

/// A decoded media codec information element, dispatched on the AVDTP
/// media codec type byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaCodecInformation {
    /// SBC codec information.
    Sbc(SbcMediaCodecInformation),
    /// MPEG-2/4 AAC codec information.
    Aac(AacMediaCodecInformation),
    /// Opus codec information.
    Opus(OpusMediaCodecInformation),
    /// Any other vendor-specific codec information.
    Vendor(VendorSpecificMediaCodecInformation),
}

impl MediaCodecInformation {
    /// Decodes the codec information bytes for `media_codec_type`. Vendor
    /// codecs with a recognized vendor/codec ID (Opus) decode further.
    /// Unsupported codec types (MPEG-1/2 audio, ATRAC) return `None`.
    pub fn parse(media_codec_type: u8, data: &[u8]) -> Option<Self> {
        match media_codec_type {
            codec_type::SBC => SbcMediaCodecInformation::parse(data).map(Self::Sbc),
            codec_type::MPEG_2_4_AAC => AacMediaCodecInformation::parse(data).map(Self::Aac),
            codec_type::NON_A2DP => {
                let vendor = VendorSpecificMediaCodecInformation::parse(data)?;
                if vendor.vendor_id == OpusMediaCodecInformation::VENDOR_ID
                    && vendor.codec_id == OpusMediaCodecInformation::CODEC_ID
                {
                    OpusMediaCodecInformation::parse_value(&vendor.value).map(Self::Opus)
                } else {
                    Some(Self::Vendor(vendor))
                }
            }
            _ => None,
        }
    }

    /// Returns the AVDTP media codec type byte for this codec.
    pub fn codec_type(&self) -> u8 {
        match self {
            Self::Sbc(..) => codec_type::SBC,
            Self::Aac(..) => codec_type::MPEG_2_4_AAC,
            Self::Opus(..) | Self::Vendor(..) => codec_type::NON_A2DP,
        }
    }

    /// Encodes to the AVDTP media codec information bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Sbc(info) => info.to_bytes().to_vec(),
            Self::Aac(info) => info.to_bytes().to_vec(),
            Self::Opus(info) => info.to_vendor_information().to_bytes(),
            Self::Vendor(info) => info.to_bytes(),
        }
    }

    /// Wraps this codec information into an AVDTP media codec capability.
    pub fn to_capabilities(&self) -> MediaCodecCapabilities {
        MediaCodecCapabilities {
            media_type: MediaType::Audio,
            media_codec_type: self.codec_type(),
            media_codec_information: self.to_bytes(),
        }
    }
}

// ---------------------------------------------------------------------------
// SBC / AAC frame headers
// ---------------------------------------------------------------------------

/// SBC frame sync word.
pub const SBC_SYNC_WORD: u8 = 0x9C;

/// SBC frame-header sampling frequencies (SBC spec, indexed by the 2-bit
/// header field). Distinct from the A2DP capability bitmask ordering.
pub const SBC_SAMPLING_FREQUENCIES: [u32; 4] = [16000, 32000, 44100, 48000];

/// SBC frame-header mono channel mode.
pub const SBC_MONO_CHANNEL_MODE: u8 = 0x00;
/// SBC frame-header dual-channel mode.
pub const SBC_DUAL_CHANNEL_MODE: u8 = 0x01;
/// SBC frame-header stereo channel mode.
pub const SBC_STEREO_CHANNEL_MODE: u8 = 0x02;
/// SBC frame-header joint-stereo channel mode.
pub const SBC_JOINT_STEREO_CHANNEL_MODE: u8 = 0x03;

/// One SBC frame with its decoded header parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SbcFrame {
    /// Sampling frequency in Hz.
    pub sampling_frequency: u32,
    /// Number of blocks in the frame.
    pub block_count: u8,
    /// Channel mode (`SBC_*_CHANNEL_MODE`).
    pub channel_mode: u8,
    /// Bit-allocation method (0 = loudness, 1 = SNR).
    pub allocation_method: u8,
    /// Number of subbands (4 or 8).
    pub subband_count: u8,
    /// SBC bitpool value.
    pub bitpool: u8,
    /// Raw frame bytes, including the header.
    pub payload: Vec<u8>,
}

impl SbcFrame {
    /// Parses one SBC frame from the front of `data`, returning the frame
    /// and the unconsumed remainder.
    pub fn parse(data: &[u8]) -> Option<(SbcFrame, &[u8])> {
        if data.len() < 4 || data[0] != SBC_SYNC_WORD {
            return None;
        }
        let sampling_frequency = SBC_SAMPLING_FREQUENCIES[((data[1] >> 6) & 3) as usize];
        let block_count = 4 * (1 + ((data[1] >> 4) & 3));
        let channel_mode = (data[1] >> 2) & 3;
        let channels: usize = if channel_mode == SBC_MONO_CHANNEL_MODE {
            1
        } else {
            2
        };
        let allocation_method = (data[1] >> 1) & 1;
        let subband_count: u8 = if data[1] & 1 != 0 { 8 } else { 4 };
        let bitpool = data[2];

        // Frame length per the SBC codec spec's frame_length formula.
        let mut frame_length = 4 + (4 * subband_count as usize * channels) / 8;
        frame_length += if channel_mode == SBC_MONO_CHANNEL_MODE
            || channel_mode == SBC_DUAL_CHANNEL_MODE
        {
            (block_count as usize * channels * bitpool as usize).div_ceil(8)
        } else {
            let joint = (channel_mode == SBC_JOINT_STEREO_CHANNEL_MODE) as usize;
            (joint * subband_count as usize + block_count as usize * bitpool as usize).div_ceil(8)
        };

        if data.len() < frame_length {
            return None;
        }
        Some((
            SbcFrame {
                sampling_frequency,
                block_count,
                channel_mode,
                allocation_method,
                subband_count,
                bitpool,
                payload: data[..frame_length].to_vec(),
            },
            &data[frame_length..],
        ))
    }

    /// Number of PCM samples the frame decodes to.
    pub fn sample_count(&self) -> u32 {
        self.subband_count as u32 * self.block_count as u32
    }

    /// Bit rate in bits per second implied by the frame size.
    pub fn bitrate(&self) -> u32 {
        8 * ((self.payload.len() as u32 * self.sampling_frequency) / self.sample_count())
    }
}

/// AAC frame-header sampling frequencies (ADTS, indexed by the 4-bit
/// sampling frequency index field). Indices 13-15 are reserved and marked
/// with 0; [`AacFrame::parse`] rejects them rather than reporting 0 Hz.
pub const ADTS_AAC_SAMPLING_FREQUENCIES: [u32; 16] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350, 0, 0,
    0,
];

/// The ADTS sync word: twelve set bits opening every frame.
pub const ADTS_SYNC_WORD: u16 = 0xFFF;

/// Bytes in a `adts_fixed_header` plus `adts_variable_header`
/// (ISO/IEC 13818-7 6.2), before the optional CRC.
pub const ADTS_HEADER_LEN: usize = 7;

/// Bytes the header CRC adds when `protection_absent` is 0.
pub const ADTS_CRC_LEN: usize = 2;

/// Samples one AAC raw data block decodes to. Fixed for the profiles ADTS
/// can express.
pub const AAC_SAMPLES_PER_RAW_DATA_BLOCK: u32 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// AAC profile carried in the ADTS header.
pub enum AacProfile {
    /// Main profile.
    Main,
    /// Low Complexity (LC).
    Lc,
    /// Scalable Sample Rate (SSR).
    Ssr,
    /// Long Term Prediction (LTP).
    Ltp,
}

impl AacProfile {
    /// The 2-bit `profile` field value.
    pub fn to_bits(self) -> u8 {
        match self {
            Self::Main => 0,
            Self::Lc => 1,
            Self::Ssr => 2,
            Self::Ltp => 3,
        }
    }

    fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0 => Self::Main,
            1 => Self::Lc,
            2 => Self::Ssr,
            _ => Self::Ltp,
        }
    }
}

/// Which MPEG audio standard the ADTS header declares, the `ID` bit
/// (ISO/IEC 13818-7 6.2.1). A2DP's MPEG-2/4 AAC codec covers both, and the
/// bit changes nothing else about the framing — but a decoder needs it, and
/// it is the field most often left at the wrong value by a hand-built
/// header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdtsMpegVersion {
    /// MPEG-4 (`ID` = 0).
    Mpeg4,
    /// MPEG-2 (`ID` = 1).
    Mpeg2,
}

/// One AAC frame extracted from an ADTS stream, with everything the header
/// declared about it.
///
/// **Framing only.** Simble parses and builds ADTS headers; it does not
/// decode AAC. [`payload`](Self::payload) is the raw data block(s) as they
/// arrived, opaque bytes for something else to decode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AacFrame {
    /// AAC profile.
    pub profile: AacProfile,
    /// MPEG-2 or MPEG-4, from the `ID` bit.
    pub mpeg_version: AdtsMpegVersion,
    /// Sampling frequency in Hz.
    pub sampling_frequency: u32,
    /// ADTS channel configuration. Zero means the channel layout is in a
    /// `program_config_element` inside the payload rather than the header —
    /// see [`Self::has_program_config_element`].
    pub channel_configuration: u8,
    /// The header CRC, present only when `protection_absent` is 0. Simble
    /// does not verify it: the polynomial covers the raw data blocks too,
    /// which means decoding AAC, which simble does not do.
    pub crc: Option<u16>,
    /// Raw data blocks in this frame, 1 to 4
    /// (`number_of_raw_data_blocks_in_frame` + 1).
    pub raw_data_block_count: u8,
    /// Raw AAC payload, excluding the ADTS header and its CRC.
    pub payload: Vec<u8>,
}

impl AacFrame {
    /// Parses one ADTS-framed AAC frame from the front of `data`, returning
    /// the frame and the unconsumed remainder.
    ///
    /// Returns `None` for anything that is not a well-formed frame: a bad
    /// sync word, a non-zero `layer`, a reserved sampling-frequency index, a
    /// `aac_frame_length` shorter than its own header, or a buffer that ends
    /// before the frame does. A stream that has lost sync can be re-acquired
    /// with [`Self::find_sync`].
    pub fn parse(data: &[u8]) -> Option<(AacFrame, &[u8])> {
        if data.len() < ADTS_HEADER_LEN {
            return None;
        }
        let sync_word = (u16::from(data[0]) << 4) | u16::from(data[1] >> 4);
        if sync_word != ADTS_SYNC_WORD {
            return None;
        }
        let layer = (data[1] >> 1) & 0b11;
        if layer != 0 {
            return None;
        }
        let mpeg_version = if data[1] & 0b1000 != 0 {
            AdtsMpegVersion::Mpeg2
        } else {
            AdtsMpegVersion::Mpeg4
        };
        // protection_absent is *inverted*: 0 means a CRC follows the header.
        // Missing this is how the two CRC bytes end up at the front of what a
        // decoder is told is AAC payload.
        let protection_absent = data[1] & 0b1 != 0;
        let header_len = if protection_absent {
            ADTS_HEADER_LEN
        } else {
            ADTS_HEADER_LEN + ADTS_CRC_LEN
        };

        let profile = AacProfile::from_bits(data[2] >> 6);
        let frequency_index = (data[2] >> 2) & 0b1111;
        let sampling_frequency = ADTS_AAC_SAMPLING_FREQUENCIES[usize::from(frequency_index)];
        if sampling_frequency == 0 {
            // Indices 13-15 are reserved. Reporting 0 Hz would make every
            // duration and sample-rate calculation downstream divide by zero.
            return None;
        }
        let channel_configuration = ((data[2] & 0b1) << 2) | (data[3] >> 6);
        let frame_length = (usize::from(data[3] & 0b11) << 11)
            | (usize::from(data[4]) << 3)
            | usize::from(data[5] >> 5);
        if frame_length < header_len || data.len() < frame_length {
            return None;
        }
        let crc = if protection_absent {
            None
        } else {
            Some(u16::from_be_bytes([data[7], data[8]]))
        };
        let raw_data_block_count = (data[6] & 0b11) + 1;

        Some((
            AacFrame {
                profile,
                mpeg_version,
                sampling_frequency,
                channel_configuration,
                crc,
                raw_data_block_count,
                payload: data[header_len..frame_length].to_vec(),
            },
            &data[frame_length..],
        ))
    }

    /// Finds the offset of the next plausible ADTS frame in `data`, or
    /// `None`.
    ///
    /// A2DP media arrives over a lossy transport, so a sink that drops a
    /// packet has to re-acquire the stream. The sync word alone is a weak
    /// signal — 0xFFF turns up inside compressed audio — so a candidate only
    /// counts if [`Self::parse`] accepts it there.
    pub fn find_sync(data: &[u8]) -> Option<usize> {
        (0..data.len().saturating_sub(1)).find(|&offset| {
            data[offset] == 0xFF
                && data[offset + 1] & 0xF0 == 0xF0
                && Self::parse(&data[offset..]).is_some()
        })
    }

    /// PCM samples per channel this frame decodes to.
    ///
    /// A frame carries `number_of_raw_data_blocks_in_frame + 1` blocks of
    /// 1024 samples, not always one — which is why this is not a constant.
    pub fn sample_count(&self) -> u32 {
        AAC_SAMPLES_PER_RAW_DATA_BLOCK * u32::from(self.raw_data_block_count)
    }

    /// How long this frame plays for, in microseconds.
    pub fn duration_us(&self) -> u32 {
        (self.sample_count() as u64 * 1_000_000 / u64::from(self.sampling_frequency)) as u32
    }

    /// True when the channel layout is carried in a `program_config_element`
    /// inside the payload rather than in the header's
    /// `channel_configuration` field. A sink that only reads the header
    /// cannot know the channel count of such a frame.
    pub fn has_program_config_element(&self) -> bool {
        self.channel_configuration == 0
    }

    /// Encodes this frame back to ADTS: header, optional CRC, payload.
    ///
    /// The round trip is exact for anything [`Self::parse`] accepted, which
    /// is what makes an A2DP *source* possible — a scripted device can carry
    /// frames it was handed without re-deriving the header.
    pub fn to_bytes(&self) -> Option<Vec<u8>> {
        let frequency_index = ADTS_AAC_SAMPLING_FREQUENCIES
            .iter()
            .take(13)
            .position(|&f| f == self.sampling_frequency)? as u8;
        if self.channel_configuration > 7 || !(1..=4).contains(&self.raw_data_block_count) {
            return None;
        }
        let header_len = if self.crc.is_some() {
            ADTS_HEADER_LEN + ADTS_CRC_LEN
        } else {
            ADTS_HEADER_LEN
        };
        let frame_length = header_len + self.payload.len();
        // aac_frame_length is 13 bits.
        if frame_length >= 1 << 13 {
            return None;
        }

        let mut out = Vec::with_capacity(frame_length);
        out.push(0xFF);
        out.push(
            0xF0 | (u8::from(self.mpeg_version == AdtsMpegVersion::Mpeg2) << 3)
                | u8::from(self.crc.is_none()),
        );
        out.push(
            (self.profile.to_bits() << 6) | (frequency_index << 2) | (self.channel_configuration >> 2),
        );
        out.push(((self.channel_configuration & 0b11) << 6) | ((frame_length >> 11) as u8 & 0b11));
        out.push((frame_length >> 3) as u8);
        // The low 3 bits of the length, then the top 5 bits of
        // adts_buffer_fullness. 0x1F everywhere means "variable rate", which
        // is what a stored stream carries.
        out.push((((frame_length & 0b111) as u8) << 5) | 0x1F);
        out.push(0xFC | (self.raw_data_block_count - 1));
        if let Some(crc) = self.crc {
            out.extend_from_slice(&crc.to_be_bytes());
        }
        out.extend_from_slice(&self.payload);
        Some(out)
    }
}

// ---------------------------------------------------------------------------
// SDP records
// ---------------------------------------------------------------------------

fn make_audio_service_sdp_records(
    service_record_handle: u32,
    service_class: SdpUuid,
    version: (u8, u8),
) -> Vec<ServiceAttribute> {
    let version_int = ((version.0 as u16) << 8) | version.1 as u16;
    vec![
        ServiceAttribute::new(
            attribute_id::SERVICE_RECORD_HANDLE,
            DataElement::unsigned_integer_32(service_record_handle),
        ),
        ServiceAttribute::new(
            attribute_id::BROWSE_GROUP_LIST,
            DataElement::sequence(vec![DataElement::uuid(SdpUuid::SDP_PUBLIC_BROWSE_ROOT)]),
        ),
        ServiceAttribute::new(
            attribute_id::SERVICE_CLASS_ID_LIST,
            DataElement::sequence(vec![DataElement::uuid(service_class)]),
        ),
        ServiceAttribute::new(
            attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID),
                    DataElement::unsigned_integer_16(AVDTP_PSM),
                ]),
                DataElement::sequence(vec![
                    DataElement::uuid(AVDTP_PROTOCOL_UUID),
                    DataElement::unsigned_integer_16(version_int),
                ]),
            ]),
        ),
        ServiceAttribute::new(
            attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
            DataElement::sequence(vec![DataElement::sequence(vec![
                DataElement::uuid(ADVANCED_AUDIO_DISTRIBUTION_SERVICE_UUID),
                DataElement::unsigned_integer_16(version_int),
            ])]),
        ),
    ]
}

/// Builds the SDP service record for the A2DP Audio Source role.
pub fn make_audio_source_service_sdp_records(
    service_record_handle: u32,
    version: Option<(u8, u8)>,
) -> Vec<ServiceAttribute> {
    make_audio_service_sdp_records(
        service_record_handle,
        AUDIO_SOURCE_SERVICE_UUID,
        version.unwrap_or(DEFAULT_VERSION),
    )
}

/// Builds the SDP service record for the A2DP Audio Sink role.
pub fn make_audio_sink_service_sdp_records(
    service_record_handle: u32,
    version: Option<(u8, u8)>,
) -> Vec<ServiceAttribute> {
    make_audio_service_sdp_records(
        service_record_handle,
        AUDIO_SINK_SERVICE_UUID,
        version.unwrap_or(DEFAULT_VERSION),
    )
}
