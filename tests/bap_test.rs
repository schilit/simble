// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Basic Audio Profile (BAP) tests: codec capability/configuration LTV round-trips and
//! broadcast/basic audio announcement encode/decode.

use simble::profiles::bap::{
    self, AnnouncementType, BasicAudioAnnouncement, Bis, BroadcastAudioAnnouncement,
    CodecSpecificCapabilities, CodecSpecificConfiguration, FrameDuration, SamplingFrequency,
    Subgroup, audio_location, context_type, supported_frame_duration, supported_sampling_frequency,
};

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
    let bytes = cap.to_bytes();
    assert_eq!(CodecSpecificCapabilities::parse(&bytes), Some(cap));
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
    let bytes = config.to_bytes();
    assert_eq!(CodecSpecificConfiguration::parse(&bytes), config);
}

#[test]
fn test_codec_specific_configuration_partial_fields_round_trip() {
    // Real ASE Config Codec writes often only carry sampling frequency + frame duration.
    let config = CodecSpecificConfiguration {
        sampling_frequency: Some(SamplingFrequency::Freq48000),
        frame_duration: Some(FrameDuration::Duration7500Us),
        ..Default::default()
    };
    let bytes = config.to_bytes();
    assert_eq!(bytes.len(), 6); // two LTV entries, 3 bytes each
    assert_eq!(CodecSpecificConfiguration::parse(&bytes), config);
}

#[test]
fn test_broadcast_audio_announcement_round_trip() {
    let announcement = BroadcastAudioAnnouncement {
        broadcast_id: 123456,
    };
    let bytes = announcement.to_bytes();
    assert_eq!(bytes.len(), 3);
    assert_eq!(
        BroadcastAudioAnnouncement::parse(&bytes),
        Some(announcement)
    );
}

#[test]
fn test_basic_audio_announcement_round_trip() {
    let announcement = BasicAudioAnnouncement {
        presentation_delay: 40000,
        subgroups: vec![Subgroup {
            codec_id: bap::LC3_CODEC_ID,
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
    let bytes = announcement.to_bytes();
    assert_eq!(BasicAudioAnnouncement::parse(&bytes), Some(announcement));
}

#[test]
fn test_basic_audio_announcement_multiple_subgroups_round_trip() {
    let make_subgroup = |sampling: SamplingFrequency| Subgroup {
        codec_id: bap::LC3_CODEC_ID,
        codec_specific_configuration: CodecSpecificConfiguration {
            sampling_frequency: Some(sampling),
            frame_duration: Some(FrameDuration::Duration10000Us),
            octets_per_codec_frame: Some(80),
            ..Default::default()
        },
        metadata: Vec::new(),
        bis: vec![Bis {
            index: 0,
            codec_specific_configuration: CodecSpecificConfiguration {
                audio_channel_allocation: Some(audio_location::FRONT_CENTER),
                ..Default::default()
            },
        }],
    };
    let announcement = BasicAudioAnnouncement {
        presentation_delay: 20000,
        subgroups: vec![
            make_subgroup(SamplingFrequency::Freq16000),
            make_subgroup(SamplingFrequency::Freq48000),
        ],
    };
    let bytes = announcement.to_bytes();
    assert_eq!(BasicAudioAnnouncement::parse(&bytes), Some(announcement));
}

#[test]
fn test_sampling_frequency_hz_round_trip() {
    for hz in [8000, 16000, 24000, 48000, 96000, 192000] {
        let freq = SamplingFrequency::from_hz(hz).expect("known sampling rate");
        assert_eq!(freq.hz(), hz);
    }
    assert!(SamplingFrequency::from_hz(12345).is_none());
}

#[test]
fn test_audio_location_stereo_channel_count() {
    let stereo = audio_location::FRONT_LEFT | audio_location::FRONT_RIGHT;
    assert_eq!(audio_location::channel_count(stereo), 2);
}

#[test]
fn test_channel_counts_to_bits_round_trip() {
    let bits = bap::channel_counts_to_bits(&[1, 2]);
    assert_eq!(bap::bits_to_channel_counts(bits), vec![1, 2]);
}

#[test]
fn test_context_type_and_announcement_type_constants() {
    assert_eq!(context_type::MEDIA, 0x0004);
    assert_eq!(context_type::CONVERSATIONAL, 0x0002);
    assert_eq!(AnnouncementType::General as u8, 0x00);
    assert_eq!(AnnouncementType::Targeted as u8, 0x01);
}
