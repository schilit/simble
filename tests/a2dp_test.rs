// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Port of Bumble's a2dp_test.py test suite: the SBC/AAC/Opus codec
//! information tests and the SBC/AAC frame-header parsers. Not ported:
//! `test_self_connection` and `test_source_sink_1` (full device/controller
//! stack plus RTP media pump; the signaling half is covered synchronously
//! in avdtp_test.rs), and the packet-source/Opus-Ogg tests (RTP media
//! pipeline, out of Simble's scope). Adds new codec dispatch, capability
//! intersection, and SDP record tests with no Bumble equivalent.

use simble::classic::a2dp::{
    self, AacFrame, AacMediaCodecInformation, AacProfile, MediaCodecInformation,
    OpusMediaCodecInformation, SbcFrame, SbcMediaCodecInformation,
    VendorSpecificMediaCodecInformation, aac, codec_type, opus, sbc,
};
use simble::classic::avdtp::{self, MediaType};
use simble::classic::sdp::{SdpClient, SdpServer};

#[test]
fn test_sbc_codec_specific_information() {
    let sbc_info = SbcMediaCodecInformation::parse(&[0x3f, 0xff, 0x02, 0x35]).unwrap();
    assert_eq!(
        sbc_info.sampling_frequency,
        sbc::sampling_frequency::SF_44100 | sbc::sampling_frequency::SF_48000
    );
    assert_eq!(
        sbc_info.channel_mode,
        sbc::channel_mode::MONO
            | sbc::channel_mode::DUAL_CHANNEL
            | sbc::channel_mode::STEREO
            | sbc::channel_mode::JOINT_STEREO
    );
    assert_eq!(
        sbc_info.block_length,
        sbc::block_length::BL_4
            | sbc::block_length::BL_8
            | sbc::block_length::BL_12
            | sbc::block_length::BL_16
    );
    assert_eq!(sbc_info.subbands, sbc::subbands::S_4 | sbc::subbands::S_8);
    assert_eq!(
        sbc_info.allocation_method,
        sbc::allocation_method::SNR | sbc::allocation_method::LOUDNESS
    );
    assert_eq!(sbc_info.minimum_bitpool_value, 2);
    assert_eq!(sbc_info.maximum_bitpool_value, 53);

    let sbc_info2 = SbcMediaCodecInformation {
        sampling_frequency: sbc::sampling_frequency::SF_44100 | sbc::sampling_frequency::SF_48000,
        channel_mode: sbc::channel_mode::MONO
            | sbc::channel_mode::DUAL_CHANNEL
            | sbc::channel_mode::STEREO
            | sbc::channel_mode::JOINT_STEREO,
        block_length: sbc::block_length::BL_4
            | sbc::block_length::BL_8
            | sbc::block_length::BL_12
            | sbc::block_length::BL_16,
        subbands: sbc::subbands::S_4 | sbc::subbands::S_8,
        allocation_method: sbc::allocation_method::SNR | sbc::allocation_method::LOUDNESS,
        minimum_bitpool_value: 2,
        maximum_bitpool_value: 53,
    };
    assert_eq!(sbc_info, sbc_info2);
    assert_eq!(sbc_info2.to_bytes(), [0x3f, 0xff, 0x02, 0x35]);
}

#[test]
fn test_aac_codec_specific_information() {
    let aac_info = AacMediaCodecInformation::parse(&[0xf0, 0x01, 0x8c, 0x83, 0xe8, 0x00]).unwrap();
    assert_eq!(
        aac_info.object_type,
        aac::object_type::MPEG_2_AAC_LC
            | aac::object_type::MPEG_4_AAC_LC
            | aac::object_type::MPEG_4_AAC_LTP
            | aac::object_type::MPEG_4_AAC_SCALABLE
    );
    assert_eq!(
        aac_info.sampling_frequency,
        aac::sampling_frequency::SF_44100 | aac::sampling_frequency::SF_48000
    );
    assert_eq!(
        aac_info.channels,
        aac::channels::MONO | aac::channels::STEREO
    );
    assert!(aac_info.vbr);
    assert_eq!(aac_info.bitrate, 256000);

    let aac_info2 = AacMediaCodecInformation {
        object_type: aac::object_type::MPEG_2_AAC_LC
            | aac::object_type::MPEG_4_AAC_LC
            | aac::object_type::MPEG_4_AAC_LTP
            | aac::object_type::MPEG_4_AAC_SCALABLE,
        sampling_frequency: aac::sampling_frequency::SF_44100 | aac::sampling_frequency::SF_48000,
        channels: aac::channels::MONO | aac::channels::STEREO,
        vbr: true,
        bitrate: 256000,
    };
    assert_eq!(aac_info, aac_info2);
    assert_eq!(aac_info2.to_bytes(), [0xf0, 0x01, 0x8c, 0x83, 0xe8, 0x00]);
}

#[test]
fn test_opus_codec_specific_information() {
    let opus_info = OpusMediaCodecInformation::parse_value(&[0x92]).unwrap();
    assert_eq!(opus_info.frame_size, opus::frame_size::FS_20MS);
    assert_eq!(opus_info.channel_mode, opus::channel_mode::STEREO);
    assert_eq!(
        opus_info.sampling_frequency,
        opus::sampling_frequency::SF_48000
    );

    let opus_info2 = OpusMediaCodecInformation {
        channel_mode: opus::channel_mode::STEREO,
        frame_size: opus::frame_size::FS_20MS,
        sampling_frequency: opus::sampling_frequency::SF_48000,
    };
    assert_eq!(opus_info2, opus_info);
    assert_eq!(opus_info2.value_byte(), 0x92);

    let vendor = opus_info2.to_vendor_information();
    assert_eq!(vendor.vendor_id, OpusMediaCodecInformation::VENDOR_ID);
    assert_eq!(vendor.codec_id, OpusMediaCodecInformation::CODEC_ID);
    assert_eq!(
        vendor.to_bytes(),
        [0xe0, 0x00, 0x00, 0x00, 0x01, 0x00, 0x92]
    );
}

#[test]
fn test_sbc_parser() {
    let mut data = vec![0x9c, 0x80, 0x08, 0x00];
    data.extend_from_slice(&[0x00; 6]);

    let (frame, rest) = SbcFrame::parse(&data).unwrap();
    assert_eq!(frame.sampling_frequency, 44100);
    assert_eq!(frame.block_count, 4);
    assert_eq!(frame.channel_mode, a2dp::SBC_MONO_CHANNEL_MODE);
    assert_eq!(frame.allocation_method, 0);
    assert_eq!(frame.subband_count, 4);
    assert_eq!(frame.bitpool, 8);
    assert_eq!(frame.payload, data);
    assert!(rest.is_empty());
    assert_eq!(frame.sample_count(), 16);
    assert_eq!(frame.bitrate(), 8 * ((10 * 44100) / 16));
}

#[test]
fn test_sbc_parser_rejects_bad_sync_or_truncation() {
    assert!(SbcFrame::parse(&[0x00, 0x80, 0x08, 0x00]).is_none());
    // Valid header but a payload shorter than the computed frame length.
    assert!(SbcFrame::parse(&[0x9c, 0x80, 0x08, 0x00, 0x00]).is_none());
    assert!(SbcFrame::parse(&[]).is_none());
}

#[test]
fn test_aac_parser() {
    // 0xf0 in byte 1 means protection_absent = 0 — a two-byte CRC follows the
    // seven-byte header, so the payload starts at offset 9, not 7. This test
    // asserted a six-byte payload until `tests/adts_interop_test.rs` was
    // written; it was reading the CRC as audio.
    let mut data = vec![0xff, 0xf0, 0x10, 0x00, 0x01, 0xa0, 0x00];
    data.extend_from_slice(&[0x00; 6]);

    let (frame, rest) = AacFrame::parse(&data).unwrap();
    assert_eq!(frame.profile, AacProfile::Main);
    assert_eq!(frame.sampling_frequency, 44100);
    assert_eq!(frame.channel_configuration, 0);
    assert_eq!(frame.crc, Some(0x0000));
    assert_eq!(frame.payload, [0x00; 4]);
    assert!(rest.is_empty());
}

#[test]
fn test_aac_parser_rejects_bad_sync_or_layer() {
    // Wrong sync word.
    assert!(AacFrame::parse(&[0x00, 0xf0, 0x10, 0x00, 0x01, 0xa0, 0x00]).is_none());
    // Nonzero layer bits.
    assert!(AacFrame::parse(&[0xff, 0xf6, 0x10, 0x00, 0x01, 0xa0, 0x00]).is_none());
    assert!(AacFrame::parse(&[0xff, 0xf0]).is_none());
}

#[test]
fn test_media_codec_information_dispatch() {
    let sbc_bytes = [0x21, 0x15, 0x02, 0x35];
    let parsed = MediaCodecInformation::parse(codec_type::SBC, &sbc_bytes).unwrap();
    assert!(matches!(parsed, MediaCodecInformation::Sbc(..)));
    assert_eq!(parsed.codec_type(), codec_type::SBC);
    assert_eq!(parsed.to_bytes(), sbc_bytes);

    let opus_bytes = [0xe0, 0x00, 0x00, 0x00, 0x01, 0x00, 0x92];
    let parsed = MediaCodecInformation::parse(codec_type::NON_A2DP, &opus_bytes).unwrap();
    assert_eq!(
        parsed,
        MediaCodecInformation::Opus(OpusMediaCodecInformation {
            channel_mode: opus::channel_mode::STEREO,
            frame_size: opus::frame_size::FS_20MS,
            sampling_frequency: opus::sampling_frequency::SF_48000,
        })
    );
    assert_eq!(parsed.codec_type(), codec_type::NON_A2DP);
    assert_eq!(parsed.to_bytes(), opus_bytes);

    // An unrecognized vendor stays a raw vendor codec.
    let vendor_bytes = [0x4c, 0x00, 0x00, 0x00, 0x02, 0x00, 0xaa, 0xbb];
    let parsed = MediaCodecInformation::parse(codec_type::NON_A2DP, &vendor_bytes).unwrap();
    assert_eq!(
        parsed,
        MediaCodecInformation::Vendor(VendorSpecificMediaCodecInformation {
            vendor_id: 0x4c,
            codec_id: 2,
            value: vec![0xaa, 0xbb],
        })
    );

    // Codec types without a decoder are not silently misparsed.
    assert!(MediaCodecInformation::parse(codec_type::MPEG_1_2_AUDIO, &sbc_bytes).is_none());
    assert!(MediaCodecInformation::parse(codec_type::ATRAC_FAMILY, &sbc_bytes).is_none());

    // The AVDTP wrapper carries the raw information element.
    let capabilities = MediaCodecInformation::parse(codec_type::SBC, &sbc_bytes)
        .unwrap()
        .to_capabilities();
    assert_eq!(capabilities.media_type, MediaType::Audio);
    assert_eq!(capabilities.media_codec_type, codec_type::SBC);
    assert_eq!(capabilities.media_codec_information, sbc_bytes);
}

#[test]
fn test_sbc_capability_intersection() {
    let sink = SbcMediaCodecInformation::parse(&[0x3f, 0xff, 0x02, 0x35]).unwrap();
    let source = SbcMediaCodecInformation {
        sampling_frequency: sbc::sampling_frequency::SF_44100,
        channel_mode: sbc::channel_mode::JOINT_STEREO,
        block_length: sbc::block_length::BL_16,
        subbands: sbc::subbands::S_8,
        allocation_method: sbc::allocation_method::LOUDNESS,
        minimum_bitpool_value: 2,
        maximum_bitpool_value: 250,
    };
    // The source's single-choice configuration fits inside the sink's
    // capability masks; only the bitpool range narrows.
    let common = source.intersect(&sink).unwrap();
    assert_eq!(
        common,
        SbcMediaCodecInformation {
            maximum_bitpool_value: 53,
            ..source
        }
    );

    // Disjoint sampling frequencies have no common operating point.
    let mismatched = SbcMediaCodecInformation {
        sampling_frequency: sbc::sampling_frequency::SF_16000,
        ..source
    };
    let sink_44100 = SbcMediaCodecInformation {
        sampling_frequency: sbc::sampling_frequency::SF_44100,
        ..sink
    };
    assert!(mismatched.intersect(&sink_44100).is_none());

    // Non-overlapping bitpool ranges fail too.
    let high_bitpool = SbcMediaCodecInformation {
        minimum_bitpool_value: 60,
        maximum_bitpool_value: 80,
        ..source
    };
    assert!(high_bitpool.intersect(&sink).is_none());
}

#[test]
fn test_sampling_frequency_flag_from_hz() {
    assert_eq!(
        SbcMediaCodecInformation::sampling_frequency_flag(44100),
        Some(sbc::sampling_frequency::SF_44100)
    );
    assert_eq!(
        SbcMediaCodecInformation::sampling_frequency_flag(48000),
        Some(sbc::sampling_frequency::SF_48000)
    );
    assert_eq!(
        SbcMediaCodecInformation::sampling_frequency_flag(22050),
        None
    );
}

#[test]
fn test_sdp_records_and_avdtp_service_discovery() {
    let mut sdp_server = SdpServer::new();
    sdp_server.service_records.insert(
        0x0001_0001,
        a2dp::make_audio_sink_service_sdp_records(0x0001_0001, None),
    );

    let mut sdp_client = SdpClient::new();
    let version = avdtp::find_avdtp_service(&mut sdp_client, |request| {
        sdp_server.handle_request(request, 1024)
    })
    .unwrap();
    assert_eq!(version, Some((1, 3)));

    // A server with only a source record still advertises AVDTP.
    let mut sdp_server = SdpServer::new();
    sdp_server.service_records.insert(
        0x0001_0002,
        a2dp::make_audio_source_service_sdp_records(0x0001_0002, Some((1, 2))),
    );
    let version = avdtp::find_avdtp_service(&mut sdp_client, |request| {
        sdp_server.handle_request(request, 1024)
    })
    .unwrap();
    assert_eq!(version, Some((1, 2)));

    // No A2DP record at all: no service found.
    let mut empty_server = SdpServer::new();
    let version = avdtp::find_avdtp_service(&mut sdp_client, |request| {
        empty_server.handle_request(request, 1024)
    })
    .unwrap();
    assert_eq!(version, None);
}
