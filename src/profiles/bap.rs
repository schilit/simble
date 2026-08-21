// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Basic Audio Profile (BAP).
//!
//! BAP defines no GATT service of its own — its surface is the codec capability/
//! configuration LTV structures and broadcast/unicast announcement payloads that PACS
//! (`crate::profiles::pacs`) advertises and ASCS's (`crate::profiles::ascs`) ASE Control
//! Point opcodes carry. This module holds those config/data structures plus the LTV
//! encode/decode helpers, not a `register()`-style service.
//!
//! Metadata LTV entries (Bluetooth Assigned Numbers 6.12.6) are carried as opaque
//! `Vec<u8>` blobs rather than a typed `Entry`/`Tag` decode layer — no ASE transition or
//! announcement round-trip below needs per-tag decoding, so the extra type isn't earned.

/// Advertising Service Data UUIDs used by BAP's broadcast announcements. Neither is a
/// registered GATT service — both only ever appear inside AD Service Data.
pub mod bap_uuid {
    use crate::types::Uuid;

    pub const BASIC_AUDIO_ANNOUNCEMENT_SERVICE: Uuid = Uuid::Uuid16(0x1851);
    pub const BROADCAST_AUDIO_ANNOUNCEMENT_SERVICE: Uuid = Uuid::Uuid16(0x1852);
}

/// Coding Format 0x06 (LC3), Company ID 0x0000, Vendor Codec ID 0x0000 — HCI `Coding_Format`
/// wire shape (Core Spec Vol 4, Part E, 7.8.19).
pub const LC3_CODEC_ID: [u8; 5] = [0x06, 0x00, 0x00, 0x00, 0x00];

/// Bluetooth Assigned Numbers, Section 6.12.1 - Audio Location bitmask (32-bit).
pub mod audio_location {
    // fmt: off
    pub const NOT_ALLOWED: u32 = 0x0000_0000;
    pub const FRONT_LEFT: u32 = 0x0000_0001;
    pub const FRONT_RIGHT: u32 = 0x0000_0002;
    pub const FRONT_CENTER: u32 = 0x0000_0004;
    pub const LOW_FREQUENCY_EFFECTS_1: u32 = 0x0000_0008;
    pub const BACK_LEFT: u32 = 0x0000_0010;
    pub const BACK_RIGHT: u32 = 0x0000_0020;
    pub const FRONT_LEFT_OF_CENTER: u32 = 0x0000_0040;
    pub const FRONT_RIGHT_OF_CENTER: u32 = 0x0000_0080;
    pub const BACK_CENTER: u32 = 0x0000_0100;
    pub const LOW_FREQUENCY_EFFECTS_2: u32 = 0x0000_0200;
    pub const SIDE_LEFT: u32 = 0x0000_0400;
    pub const SIDE_RIGHT: u32 = 0x0000_0800;
    pub const TOP_FRONT_LEFT: u32 = 0x0000_1000;
    pub const TOP_FRONT_RIGHT: u32 = 0x0000_2000;
    pub const TOP_FRONT_CENTER: u32 = 0x0000_4000;
    pub const TOP_CENTER: u32 = 0x0000_8000;
    pub const TOP_BACK_LEFT: u32 = 0x0001_0000;
    pub const TOP_BACK_RIGHT: u32 = 0x0002_0000;
    pub const TOP_SIDE_LEFT: u32 = 0x0004_0000;
    pub const TOP_SIDE_RIGHT: u32 = 0x0008_0000;
    pub const TOP_BACK_CENTER: u32 = 0x0010_0000;
    pub const BOTTOM_FRONT_CENTER: u32 = 0x0020_0000;
    pub const BOTTOM_FRONT_LEFT: u32 = 0x0040_0000;
    pub const BOTTOM_FRONT_RIGHT: u32 = 0x0080_0000;
    pub const FRONT_LEFT_WIDE: u32 = 0x0100_0000;
    pub const FRONT_RIGHT_WIDE: u32 = 0x0200_0000;
    pub const LEFT_SURROUND: u32 = 0x0400_0000;
    pub const RIGHT_SURROUND: u32 = 0x0800_0000;
    // fmt: on

    pub const STEREO: u32 = FRONT_LEFT | FRONT_RIGHT;

    pub fn channel_count(locations: u32) -> u32 {
        locations.count_ones()
    }
}

/// Bluetooth Assigned Numbers, Section 6.12.3 - Context Type bitmask (16-bit).
pub mod context_type {
    // fmt: off
    pub const PROHIBITED: u16 = 0x0000;
    pub const UNSPECIFIED: u16 = 0x0001;
    pub const CONVERSATIONAL: u16 = 0x0002;
    pub const MEDIA: u16 = 0x0004;
    pub const GAME: u16 = 0x0008;
    pub const INSTRUCTIONAL: u16 = 0x0010;
    pub const VOICE_ASSISTANTS: u16 = 0x0020;
    pub const LIVE: u16 = 0x0040;
    pub const SOUND_EFFECTS: u16 = 0x0080;
    pub const NOTIFICATIONS: u16 = 0x0100;
    pub const RINGTONE: u16 = 0x0200;
    pub const ALERTS: u16 = 0x0400;
    pub const EMERGENCY_ALARM: u16 = 0x0800;
    // fmt: on
}

/// Bluetooth Assigned Numbers, Section 6.12.5.1 - Sampling Frequency.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum SamplingFrequency {
    Freq8000 = 0x01,
    Freq11025 = 0x02,
    Freq16000 = 0x03,
    Freq22050 = 0x04,
    Freq24000 = 0x05,
    Freq32000 = 0x06,
    Freq44100 = 0x07,
    Freq48000 = 0x08,
    Freq88200 = 0x09,
    Freq96000 = 0x0A,
    Freq176400 = 0x0B,
    Freq192000 = 0x0C,
    Freq384000 = 0x0D,
}

impl SamplingFrequency {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::Freq8000,
            0x02 => Self::Freq11025,
            0x03 => Self::Freq16000,
            0x04 => Self::Freq22050,
            0x05 => Self::Freq24000,
            0x06 => Self::Freq32000,
            0x07 => Self::Freq44100,
            0x08 => Self::Freq48000,
            0x09 => Self::Freq88200,
            0x0A => Self::Freq96000,
            0x0B => Self::Freq176400,
            0x0C => Self::Freq192000,
            0x0D => Self::Freq384000,
            _ => return None,
        })
    }

    pub fn from_hz(frequency: u32) -> Option<Self> {
        Some(match frequency {
            8000 => Self::Freq8000,
            11025 => Self::Freq11025,
            16000 => Self::Freq16000,
            22050 => Self::Freq22050,
            24000 => Self::Freq24000,
            32000 => Self::Freq32000,
            44100 => Self::Freq44100,
            48000 => Self::Freq48000,
            88200 => Self::Freq88200,
            96000 => Self::Freq96000,
            176400 => Self::Freq176400,
            192000 => Self::Freq192000,
            384000 => Self::Freq384000,
            _ => return None,
        })
    }

    pub fn hz(self) -> u32 {
        match self {
            Self::Freq8000 => 8000,
            Self::Freq11025 => 11025,
            Self::Freq16000 => 16000,
            Self::Freq22050 => 22050,
            Self::Freq24000 => 24000,
            Self::Freq32000 => 32000,
            Self::Freq44100 => 44100,
            Self::Freq48000 => 48000,
            Self::Freq88200 => 88200,
            Self::Freq96000 => 96000,
            Self::Freq176400 => 176400,
            Self::Freq192000 => 192000,
            Self::Freq384000 => 384000,
        }
    }
}

/// Bluetooth Assigned Numbers, Section 6.12.4.1 - Supported Sampling Frequency bitmask,
/// bit `N` = `SamplingFrequency` value `N + 1`.
pub mod supported_sampling_frequency {
    // fmt: off
    pub const FREQ_8000: u16 = 1 << 0;
    pub const FREQ_11025: u16 = 1 << 1;
    pub const FREQ_16000: u16 = 1 << 2;
    pub const FREQ_22050: u16 = 1 << 3;
    pub const FREQ_24000: u16 = 1 << 4;
    pub const FREQ_32000: u16 = 1 << 5;
    pub const FREQ_44100: u16 = 1 << 6;
    pub const FREQ_48000: u16 = 1 << 7;
    pub const FREQ_88200: u16 = 1 << 8;
    pub const FREQ_96000: u16 = 1 << 9;
    pub const FREQ_176400: u16 = 1 << 10;
    pub const FREQ_192000: u16 = 1 << 11;
    pub const FREQ_384000: u16 = 1 << 12;
    // fmt: on
}

/// Bluetooth Assigned Numbers, Section 6.12.5.2 - Frame Duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameDuration {
    Duration7500Us = 0x00,
    Duration10000Us = 0x01,
}

impl FrameDuration {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x00 => Self::Duration7500Us,
            0x01 => Self::Duration10000Us,
            _ => return None,
        })
    }

    pub fn us(self) -> u32 {
        match self {
            Self::Duration7500Us => 7500,
            Self::Duration10000Us => 10000,
        }
    }
}

/// Bluetooth Assigned Numbers, Section 6.12.4.2 - Frame Duration bitmask.
pub mod supported_frame_duration {
    pub const DURATION_7500_US_SUPPORTED: u8 = 0b0001;
    pub const DURATION_10000_US_SUPPORTED: u8 = 0b0010;
}

/// Basic Audio Profile, 3.5.3 - Additional ASCS requirements.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AnnouncementType {
    General = 0x00,
    Targeted = 0x01,
}

pub(crate) fn read_u24_le(data: &[u8]) -> u32 {
    data[0] as u32 | (data[1] as u32) << 8 | (data[2] as u32) << 16
}

pub(crate) fn write_u24_le(value: u32) -> [u8; 3] {
    [value as u8, (value >> 8) as u8, (value >> 16) as u8]
}

/// A single Length-Type-Value entry as used by LTV-structured codec capability/
/// configuration data (Bluetooth Assigned Numbers, Section 6.12.4/6.12.5).
fn ltv_entries(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut entries = Vec::new();
    let mut offset = 0;
    while offset + 1 < data.len() {
        let length = data[offset] as usize;
        let tag = data[offset + 1];
        let value_end = (offset + 1 + length).min(data.len());
        entries.push((tag, &data[offset + 2..value_end]));
        offset += 1 + length;
    }
    entries
}

fn le_uint(data: &[u8]) -> u32 {
    data.iter()
        .take(4)
        .enumerate()
        .fold(0u32, |acc, (i, &b)| acc | ((b as u32) << (8 * i)))
}

/// Bluetooth Assigned Numbers, 6.12.4 - Codec Specific Capabilities LTV Structures.
pub mod codec_capability_type {
    pub const SAMPLING_FREQUENCY: u8 = 0x01;
    pub const FRAME_DURATION: u8 = 0x02;
    pub const AUDIO_CHANNEL_COUNT: u8 = 0x03;
    pub const OCTETS_PER_FRAME: u8 = 0x04;
    pub const CODEC_FRAMES_PER_SDU: u8 = 0x05;
}

/// Basic Audio Profile, 4.3.1 - Codec_Specific_Capabilities LTV requirements.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodecSpecificCapabilities {
    pub supported_sampling_frequencies: u16,
    pub supported_frame_durations: u8,
    pub supported_audio_channel_counts: Vec<u8>,
    pub min_octets_per_codec_frame: u16,
    pub max_octets_per_codec_frame: u16,
    pub supported_max_codec_frames_per_sdu: u8,
}

pub fn bits_to_channel_counts(data: u8) -> Vec<u8> {
    (0..8u8)
        .filter(|bit| data & (1 << bit) != 0)
        .map(|bit| bit + 1)
        .collect()
}

pub fn channel_counts_to_bits(counts: &[u8]) -> u8 {
    counts
        .iter()
        .fold(0u8, |acc, &count| acc | (1 << (count - 1)))
}

impl CodecSpecificCapabilities {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.push(3);
        buf.push(codec_capability_type::SAMPLING_FREQUENCY);
        buf.extend_from_slice(&self.supported_sampling_frequencies.to_le_bytes());
        buf.push(2);
        buf.push(codec_capability_type::FRAME_DURATION);
        buf.push(self.supported_frame_durations);
        buf.push(2);
        buf.push(codec_capability_type::AUDIO_CHANNEL_COUNT);
        buf.push(channel_counts_to_bits(&self.supported_audio_channel_counts));
        buf.push(5);
        buf.push(codec_capability_type::OCTETS_PER_FRAME);
        buf.extend_from_slice(&self.min_octets_per_codec_frame.to_le_bytes());
        buf.extend_from_slice(&self.max_octets_per_codec_frame.to_le_bytes());
        buf.push(2);
        buf.push(codec_capability_type::CODEC_FRAMES_PER_SDU);
        buf.push(self.supported_max_codec_frames_per_sdu);
        buf
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        let mut supported_sampling_frequencies = None;
        let mut supported_frame_durations = None;
        let mut supported_audio_channel_counts = vec![1];
        let mut min_octets_per_codec_frame = None;
        let mut max_octets_per_codec_frame = None;
        let mut supported_max_codec_frames_per_sdu = 1;

        for (tag, value) in ltv_entries(data) {
            match tag {
                codec_capability_type::SAMPLING_FREQUENCY => {
                    supported_sampling_frequencies = Some(le_uint(value) as u16);
                }
                codec_capability_type::FRAME_DURATION => {
                    supported_frame_durations = Some(le_uint(value) as u8);
                }
                codec_capability_type::AUDIO_CHANNEL_COUNT => {
                    supported_audio_channel_counts = bits_to_channel_counts(le_uint(value) as u8);
                }
                codec_capability_type::OCTETS_PER_FRAME => {
                    min_octets_per_codec_frame = Some((le_uint(value) & 0xFFFF) as u16);
                    max_octets_per_codec_frame = Some((le_uint(value) >> 16) as u16);
                }
                codec_capability_type::CODEC_FRAMES_PER_SDU => {
                    supported_max_codec_frames_per_sdu = le_uint(value) as u8;
                }
                _ => {}
            }
        }

        Some(Self {
            supported_sampling_frequencies: supported_sampling_frequencies?,
            supported_frame_durations: supported_frame_durations?,
            supported_audio_channel_counts,
            min_octets_per_codec_frame: min_octets_per_codec_frame?,
            max_octets_per_codec_frame: max_octets_per_codec_frame?,
            supported_max_codec_frames_per_sdu,
        })
    }
}

/// Bluetooth Assigned Numbers, 6.12.5 - Codec Specific Configuration LTV Structures.
pub mod codec_config_type {
    pub const SAMPLING_FREQUENCY: u8 = 0x01;
    pub const FRAME_DURATION: u8 = 0x02;
    pub const AUDIO_CHANNEL_ALLOCATION: u8 = 0x03;
    pub const OCTETS_PER_FRAME: u8 = 0x04;
    pub const CODEC_FRAMES_PER_SDU: u8 = 0x05;
}

/// Basic Audio Profile, 4.3.2 - Codec_Specific_Configuration LTV requirements. Every field
/// is optional per spec: only the tags actually negotiated are present on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CodecSpecificConfiguration {
    pub sampling_frequency: Option<SamplingFrequency>,
    pub frame_duration: Option<FrameDuration>,
    pub audio_channel_allocation: Option<u32>,
    pub octets_per_codec_frame: Option<u16>,
    pub codec_frames_per_sdu: Option<u8>,
}

impl CodecSpecificConfiguration {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        if let Some(v) = self.sampling_frequency {
            buf.extend_from_slice(&[2, codec_config_type::SAMPLING_FREQUENCY, v as u8]);
        }
        if let Some(v) = self.frame_duration {
            buf.extend_from_slice(&[2, codec_config_type::FRAME_DURATION, v as u8]);
        }
        if let Some(v) = self.audio_channel_allocation {
            buf.push(5);
            buf.push(codec_config_type::AUDIO_CHANNEL_ALLOCATION);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = self.octets_per_codec_frame {
            buf.push(3);
            buf.push(codec_config_type::OCTETS_PER_FRAME);
            buf.extend_from_slice(&v.to_le_bytes());
        }
        if let Some(v) = self.codec_frames_per_sdu {
            buf.extend_from_slice(&[2, codec_config_type::CODEC_FRAMES_PER_SDU, v]);
        }
        buf
    }

    pub fn parse(data: &[u8]) -> Self {
        let mut config = Self::default();
        for (tag, value) in ltv_entries(data) {
            match tag {
                codec_config_type::SAMPLING_FREQUENCY => {
                    config.sampling_frequency = SamplingFrequency::from_u8(le_uint(value) as u8);
                }
                codec_config_type::FRAME_DURATION => {
                    config.frame_duration = FrameDuration::from_u8(le_uint(value) as u8);
                }
                codec_config_type::AUDIO_CHANNEL_ALLOCATION => {
                    config.audio_channel_allocation = Some(le_uint(value));
                }
                codec_config_type::OCTETS_PER_FRAME => {
                    config.octets_per_codec_frame = Some(le_uint(value) as u16);
                }
                codec_config_type::CODEC_FRAMES_PER_SDU => {
                    config.codec_frames_per_sdu = Some(le_uint(value) as u8);
                }
                _ => {}
            }
        }
        config
    }
}

/// Broadcast Audio Announcement AD structure (BAP 3.7.2.1): carries only the 24-bit
/// Broadcast_ID used to identify a broadcast source before synchronizing to its PA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BroadcastAudioAnnouncement {
    pub broadcast_id: u32,
}

impl BroadcastAudioAnnouncement {
    pub fn to_bytes(&self) -> [u8; 3] {
        write_u24_le(self.broadcast_id)
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 3 {
            return None;
        }
        Some(Self {
            broadcast_id: read_u24_le(data),
        })
    }
}

/// One Broadcast Isochronous Stream entry within a `BasicAudioAnnouncement` subgroup
/// (BAP 3.7.2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bis {
    pub index: u8,
    pub codec_specific_configuration: CodecSpecificConfiguration,
}

impl Bis {
    pub fn to_bytes(&self) -> Vec<u8> {
        let config_bytes = self.codec_specific_configuration.to_bytes();
        let mut buf = Vec::with_capacity(2 + config_bytes.len());
        buf.push(self.index);
        buf.push(config_bytes.len() as u8);
        buf.extend_from_slice(&config_bytes);
        buf
    }
}

/// One subgroup of BISes sharing a codec configuration within a `BasicAudioAnnouncement`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subgroup {
    pub codec_id: [u8; 5],
    pub codec_specific_configuration: CodecSpecificConfiguration,
    pub metadata: Vec<u8>,
    pub bis: Vec<Bis>,
}

impl Subgroup {
    pub fn to_bytes(&self) -> Vec<u8> {
        let config_bytes = self.codec_specific_configuration.to_bytes();
        let mut buf = Vec::new();
        buf.push(self.bis.len() as u8);
        buf.extend_from_slice(&self.codec_id);
        buf.push(config_bytes.len() as u8);
        buf.extend_from_slice(&config_bytes);
        buf.push(self.metadata.len() as u8);
        buf.extend_from_slice(&self.metadata);
        for bis in &self.bis {
            buf.extend_from_slice(&bis.to_bytes());
        }
        buf
    }
}

/// Basic Audio Announcement AD structure (BAP 3.7.2.2): the periodic-advertising payload
/// describing a broadcast source's subgroups, codec configuration, and BISes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BasicAudioAnnouncement {
    pub presentation_delay: u32,
    pub subgroups: Vec<Subgroup>,
}

impl BasicAudioAnnouncement {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&write_u24_le(self.presentation_delay));
        buf.push(self.subgroups.len() as u8);
        for subgroup in &self.subgroups {
            buf.extend_from_slice(&subgroup.to_bytes());
        }
        buf
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        let presentation_delay = read_u24_le(data);
        let num_subgroups = data[3];
        let mut offset = 4;
        let mut subgroups = Vec::with_capacity(num_subgroups as usize);

        for _ in 0..num_subgroups {
            let num_bis = *data.get(offset)?;
            offset += 1;
            let codec_id: [u8; 5] = data.get(offset..offset + 5)?.try_into().ok()?;
            offset += 5;
            let config_len = *data.get(offset)? as usize;
            offset += 1;
            let config_bytes = data.get(offset..offset + config_len)?;
            offset += config_len;
            let metadata_len = *data.get(offset)? as usize;
            offset += 1;
            let metadata = data.get(offset..offset + metadata_len)?.to_vec();
            offset += metadata_len;

            let mut bis = Vec::with_capacity(num_bis as usize);
            for _ in 0..num_bis {
                let index = *data.get(offset)?;
                offset += 1;
                let bis_config_len = *data.get(offset)? as usize;
                offset += 1;
                let bis_config_bytes = data.get(offset..offset + bis_config_len)?;
                offset += bis_config_len;
                bis.push(Bis {
                    index,
                    codec_specific_configuration: CodecSpecificConfiguration::parse(
                        bis_config_bytes,
                    ),
                });
            }

            subgroups.push(Subgroup {
                codec_id,
                codec_specific_configuration: CodecSpecificConfiguration::parse(config_bytes),
                metadata,
                bis,
            });
        }

        Some(Self {
            presentation_delay,
            subgroups,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_codec_specific_capabilities_round_trip() {
        let cap = CodecSpecificCapabilities {
            supported_sampling_frequencies: supported_sampling_frequency::FREQ_16000,
            supported_frame_durations: supported_frame_duration::DURATION_10000_US_SUPPORTED,
            supported_audio_channel_counts: vec![1],
            min_octets_per_codec_frame: 40,
            max_octets_per_codec_frame: 40,
            supported_max_codec_frames_per_sdu: 1,
        };
        assert_eq!(CodecSpecificCapabilities::parse(&cap.to_bytes()), Some(cap));
    }

    #[test]
    fn test_codec_specific_configuration_round_trip() {
        let config = CodecSpecificConfiguration {
            sampling_frequency: Some(SamplingFrequency::Freq16000),
            frame_duration: Some(FrameDuration::Duration10000Us),
            audio_channel_allocation: Some(audio_location::FRONT_LEFT),
            octets_per_codec_frame: Some(60),
            codec_frames_per_sdu: Some(1),
        };
        assert_eq!(
            CodecSpecificConfiguration::parse(&config.to_bytes()),
            config
        );
    }

    #[test]
    fn test_broadcast_audio_announcement_round_trip() {
        let announcement = BroadcastAudioAnnouncement {
            broadcast_id: 123456,
        };
        assert_eq!(
            BroadcastAudioAnnouncement::parse(&announcement.to_bytes()),
            Some(announcement)
        );
    }

    #[test]
    fn test_basic_audio_announcement_round_trip() {
        let announcement = BasicAudioAnnouncement {
            presentation_delay: 40000,
            subgroups: vec![Subgroup {
                codec_id: LC3_CODEC_ID,
                codec_specific_configuration: CodecSpecificConfiguration {
                    sampling_frequency: Some(SamplingFrequency::Freq48000),
                    frame_duration: Some(FrameDuration::Duration10000Us),
                    octets_per_codec_frame: Some(100),
                    ..Default::default()
                },
                metadata: b"eng".to_vec(),
                bis: vec![
                    Bis {
                        index: 0,
                        codec_specific_configuration: CodecSpecificConfiguration {
                            audio_channel_allocation: Some(audio_location::FRONT_LEFT),
                            ..Default::default()
                        },
                    },
                    Bis {
                        index: 1,
                        codec_specific_configuration: CodecSpecificConfiguration {
                            audio_channel_allocation: Some(audio_location::FRONT_RIGHT),
                            ..Default::default()
                        },
                    },
                ],
            }],
        };
        assert_eq!(
            BasicAudioAnnouncement::parse(&announcement.to_bytes()),
            Some(announcement)
        );
    }

    #[test]
    fn test_channel_count_bit_round_trip() {
        assert_eq!(
            bits_to_channel_counts(channel_counts_to_bits(&[1, 2])),
            vec![1, 2]
        );
    }
}
