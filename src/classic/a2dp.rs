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
//! Scope: capability/configuration negotiation only. RTP packetization and
//! media container parsing (Bumble's `SbcPacketSource`/`AacPacketSource`/
//! `OpusParser` Ogg pipeline) belong to the audio path, which Simble does
//! not simulate; only the SBC and ADTS frame-header parsers are ported
//! since they carry the codec parameters negotiation is about.

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
pub(crate) const SBC_SYNC_WORD: u8 = 0x9C;

/// SBC frame-header sampling frequencies (SBC spec, indexed by the 2-bit
/// header field). Distinct from the A2DP capability bitmask ordering.
pub(crate) const SBC_SAMPLING_FREQUENCIES: [u32; 4] = [16000, 32000, 44100, 48000];

/// SBC frame-header mono channel mode.
pub const SBC_MONO_CHANNEL_MODE: u8 = 0x00;
/// SBC frame-header dual-channel mode.
pub(crate) const SBC_DUAL_CHANNEL_MODE: u8 = 0x01;
/// SBC frame-header stereo channel mode.
pub const SBC_STEREO_CHANNEL_MODE: u8 = 0x02;
/// SBC frame-header joint-stereo channel mode.
pub(crate) const SBC_JOINT_STEREO_CHANNEL_MODE: u8 = 0x03;

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
/// sampling frequency index field; 0 marks reserved entries).
pub(crate) const ADTS_AAC_SAMPLING_FREQUENCIES: [u32; 16] = [
    96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350, 0, 0,
    0,
];

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

/// One AAC frame extracted from an ADTS stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AacFrame {
    /// AAC profile.
    pub profile: AacProfile,
    /// Sampling frequency in Hz.
    pub sampling_frequency: u32,
    /// ADTS channel configuration.
    pub channel_configuration: u8,
    /// Raw AAC payload, excluding the ADTS header.
    pub payload: Vec<u8>,
}

impl AacFrame {
    /// Parses one ADTS-framed AAC frame from the front of `data`, returning
    /// the frame (payload excludes the 7-byte ADTS header) and the
    /// unconsumed remainder.
    pub fn parse(data: &[u8]) -> Option<(AacFrame, &[u8])> {
        if data.len() < 7 {
            return None;
        }
        let sync_word = ((data[0] as u16) << 4) | ((data[1] >> 4) as u16);
        if sync_word != 0xFFF {
            return None;
        }
        let layer = (data[1] >> 1) & 0b11;
        if layer != 0 {
            return None;
        }
        let profile = match (data[2] >> 6) & 0b11 {
            0 => AacProfile::Main,
            1 => AacProfile::Lc,
            2 => AacProfile::Ssr,
            _ => AacProfile::Ltp,
        };
        let sampling_frequency = ADTS_AAC_SAMPLING_FREQUENCIES[((data[2] >> 2) & 0b1111) as usize];
        let channel_configuration = ((data[2] & 0b1) << 2) | (data[3] >> 6);
        let frame_length = (((data[3] & 0b11) as usize) << 11)
            | ((data[4] as usize) << 3)
            | (data[5] >> 5) as usize;
        if frame_length < 7 || data.len() < frame_length {
            return None;
        }
        Some((
            AacFrame {
                profile,
                sampling_frequency,
                channel_configuration,
                payload: data[7..frame_length].to_vec(),
            },
            &data[frame_length..],
        ))
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
