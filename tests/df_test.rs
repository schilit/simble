// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Direction Finding (AoA/AoD) tests, written from the Bluetooth Core Spec.

use simble::df::packets::{
    LeConnectionIqReportEventHeader, LeSetConnectionCteReceiveParametersHeader, df_opcode,
    df_subevent_code,
};
use simble::df::procedures::{IqSample, estimate_aoa};
use simble::packets::HciCommand;
use zerocopy::IntoBytes;

#[test]
fn test_connection_cte_receive_parameters_command_roundtrip() {
    assert_eq!(
        LeSetConnectionCteReceiveParametersHeader::OP_CODE.get(),
        df_opcode::LE_SET_CONNECTION_CTE_RECEIVE_PARAMETERS.get()
    );

    let antenna_ids = [0u8, 1, 2, 3, 4];
    let bytes = LeSetConnectionCteReceiveParametersHeader::serialize(0x0055, 1, 1, &antenna_ids);

    let (header, parsed_antennas) =
        LeSetConnectionCteReceiveParametersHeader::parse(&bytes).expect("valid parse");
    assert_eq!(header.connection_handle.get(), 0x0055);
    assert_eq!(header.sampling_enable, 1);
    assert_eq!(header.slot_durations, 1);
    assert_eq!(header.switching_pattern_length, antenna_ids.len() as u8);
    assert_eq!(parsed_antennas, &antenna_ids[..]);
}

#[test]
fn test_connection_cte_receive_parameters_rejects_truncated_antenna_ids() {
    let mut bytes = LeSetConnectionCteReceiveParametersHeader::serialize(0x0055, 1, 1, &[0, 1, 2]);
    bytes.truncate(bytes.len() - 1);
    assert!(LeSetConnectionCteReceiveParametersHeader::parse(&bytes).is_none());
}

#[test]
fn test_connection_iq_report_event_roundtrip_with_iq_trailer() {
    use zerocopy::byteorder::{I16, LittleEndian, U16};

    let header = LeConnectionIqReportEventHeader {
        connection_handle: U16::<LittleEndian>::from_bytes(0x0040u16.to_le_bytes()),
        rx_phy: 1,
        data_channel_index: 12,
        rssi: I16::<LittleEndian>::from_bytes((-450i16).to_le_bytes()),
        rssi_antenna_id: 0,
        cte_type: 0, // AoA
        slot_durations: 1,
        packet_status: 0,
        connection_event_counter: U16::<LittleEndian>::from_bytes(100u16.to_le_bytes()),
        sample_count: 3,
    };

    let iq_bytes: [i8; 6] = [10, -5, 20, -15, 30, -25];
    let mut bytes = Vec::new();
    bytes.extend_from_slice(header.as_bytes());
    for sample in iq_bytes {
        bytes.push(sample as u8);
    }

    let (parsed_header, iq_samples) =
        LeConnectionIqReportEventHeader::parse(&bytes).expect("valid parse");
    assert_eq!(parsed_header.connection_handle.get(), 0x0040);
    assert_eq!(parsed_header.rx_phy, 1);
    assert_eq!(parsed_header.data_channel_index, 12);
    assert_eq!(parsed_header.rssi.get(), -450);
    assert_eq!(parsed_header.cte_type, 0);
    assert_eq!(parsed_header.sample_count, 3);
    assert_eq!(iq_samples, &iq_bytes[..]);

    assert_eq!(
        df_subevent_code::LE_CONNECTION_IQ_REPORT,
        0x16,
        "LE Connection IQ Report subevent code per Core Spec 7.7.65.21"
    );
}

#[test]
fn test_connection_iq_report_event_rejects_short_iq_trailer() {
    use zerocopy::byteorder::{I16, LittleEndian, U16};

    let header = LeConnectionIqReportEventHeader {
        connection_handle: U16::<LittleEndian>::from_bytes(0x0040u16.to_le_bytes()),
        rx_phy: 1,
        data_channel_index: 12,
        rssi: I16::<LittleEndian>::from_bytes((-450i16).to_le_bytes()),
        rssi_antenna_id: 0,
        cte_type: 0,
        slot_durations: 1,
        packet_status: 0,
        connection_event_counter: U16::<LittleEndian>::from_bytes(100u16.to_le_bytes()),
        sample_count: 3, // Claims 3 samples (6 bytes) but only 2 are provided below.
    };

    let mut bytes = Vec::new();
    bytes.extend_from_slice(header.as_bytes());
    bytes.extend_from_slice(&[10u8, 20]);

    assert!(LeConnectionIqReportEventHeader::parse(&bytes).is_none());
}

/// Feeds IQ samples whose phase difference corresponds, by construction, to
/// a known 20-degree angle of arrival, then verifies `estimate_aoa` recovers
/// it (accounting for `i8` amplitude quantization).
#[test]
fn test_estimate_aoa_matches_known_angle_by_construction() {
    let antenna_spacing_meters = 0.03; // < half-wavelength at 2.4 GHz, avoids spatial aliasing.
    let frequency_hz = 2.4e9_f64;
    let wavelength = 299_792_458.0_f64 / frequency_hz;
    let target_deg = 20.0_f64;
    let delta_phi =
        2.0 * std::f64::consts::PI * antenna_spacing_meters * target_deg.to_radians().sin()
            / wavelength;

    let amplitude = 100.0_f64;
    let antenna0 = IqSample::new(amplitude.round() as i8, 0);
    let antenna1 = IqSample::new(
        (amplitude * delta_phi.cos()).round() as i8,
        (amplitude * delta_phi.sin()).round() as i8,
    );

    let samples = vec![vec![antenna0], vec![antenna1]];
    let estimate = estimate_aoa(&samples, antenna_spacing_meters, frequency_hz).expect("estimate");

    assert_eq!(estimate.num_antennas, 2);
    assert_eq!(estimate.num_antenna_pairs, 1);
    assert!(
        (estimate.estimated_angle_degrees - target_deg).abs() < 1.0,
        "estimated {} vs target {target_deg}",
        estimate.estimated_angle_degrees
    );
}

#[test]
fn test_estimate_aoa_zero_phase_difference_is_broadside() {
    let samples = vec![
        vec![IqSample::new(100, 0), IqSample::new(100, 0)],
        vec![IqSample::new(100, 0), IqSample::new(100, 0)],
        vec![IqSample::new(100, 0), IqSample::new(100, 0)],
    ];
    let estimate = estimate_aoa(&samples, 0.05, 2.4e9).expect("estimate");
    assert!(estimate.estimated_angle_degrees.abs() < 0.01);
    assert_eq!(estimate.num_antennas, 3);
    assert_eq!(estimate.num_antenna_pairs, 2);
}

#[test]
fn test_estimate_aoa_insufficient_antennas_returns_none() {
    let single_antenna = vec![vec![IqSample::new(100, 0)]];
    assert!(estimate_aoa(&single_antenna, 0.05, 2.4e9).is_none());

    let no_antennas: Vec<Vec<IqSample>> = vec![];
    assert!(estimate_aoa(&no_antennas, 0.05, 2.4e9).is_none());
}

#[test]
fn test_estimate_aoa_rejects_invalid_parameters() {
    let samples = vec![vec![IqSample::new(100, 0)], vec![IqSample::new(0, 100)]];
    assert!(estimate_aoa(&samples, 0.0, 2.4e9).is_none());
    assert!(estimate_aoa(&samples, 0.05, 0.0).is_none());

    let with_empty_antenna = vec![vec![IqSample::new(100, 0)], vec![]];
    assert!(estimate_aoa(&with_empty_antenna, 0.05, 2.4e9).is_none());
}
