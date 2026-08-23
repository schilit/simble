// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Direction Finding (AoA/AoD) tests, written from the Bluetooth Core Spec.

use simble::df::packets::{
    LeConnectionCteRequestEnable, LeConnectionIqReportEventHeader,
    LeConnectionlessIqReportEventHeader, LeCteRequestFailedEvent, LeReadAntennaInformationResponse,
    LeSetConnectionCteReceiveParametersHeader, LeSetConnectionCteTransmitParametersHeader,
    LeSetConnectionlessCteTransmitParametersHeader, LeSetConnectionlessIqSamplingEnableHeader,
    df_opcode, df_subevent_code,
};
use simble::df::procedures::{IqSample, estimate_aoa};
use simble::packets::HciCommand;
use zerocopy::{FromBytes, IntoBytes};

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
        "LE Connection IQ Report subevent code per Core Spec 7.7.65.22"
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

// ---------------------------------------------------------------------------
// Foreign-byte parse vectors
//
// The structs in `df::packets` are `#[repr(C)]` zerocopy views with no
// serializer of their own, so nothing in-tree produces bytes for most of them
// and a round trip through `as_bytes` would only prove the layout agrees with
// itself. A field at the wrong offset stays invisible until real bytes arrive.
//
// Every vector below is written out octet by octet from the Core Spec's own
// parameter tables, with the section cited per vector, and every field is
// asserted — including the ones a length-only check would skip. Bumble and
// Zephyr mainline are the usual oracles for this crate; bumble leaves all
// three IQ events as `_payload_, // placeholder (unimplemented)`, so these
// layouts were cross-checked against Zephyr's `hci_types.h` instead.
// ---------------------------------------------------------------------------

/// Core Spec Vol 4, Part E, §7.8.80 — LE Set Connectionless CTE Transmit
/// Parameters. Advertising_Handle(1), CTE_Length(1), CTE_Type(1),
/// CTE_Count(1), Switching_Pattern_Length(1), then Antenna_IDs[i](1 each).
#[test]
fn test_connectionless_cte_transmit_parameters_parses_spec_bytes() {
    #[rustfmt::skip]
    let bytes: [u8; 9] = [
        0x07,                   // [0]   Advertising_Handle = 7
        0x14,                   // [1]   CTE_Length = 20 (x 8us = 160us)
        0x00,                   // [2]   CTE_Type = 0 (AoA)
        0x03,                   // [3]   CTE_Count = 3
        0x04,                   // [4]   Switching_Pattern_Length = 4
        0x01, 0x02, 0x03, 0x04, // [5..] Antenna_IDs
    ];

    let (header, antenna_ids) =
        LeSetConnectionlessCteTransmitParametersHeader::parse(&bytes).expect("valid parse");
    assert_eq!(header.advertising_handle, 0x07);
    assert_eq!(header.cte_length, 0x14);
    assert_eq!(header.cte_type, 0x00);
    assert_eq!(header.cte_count, 0x03);
    assert_eq!(header.switching_pattern_length, 4);
    assert_eq!(antenna_ids, &[0x01, 0x02, 0x03, 0x04]);

    // The five fixed octets are all u8, so the tail starts at exactly 5.
    assert_eq!(
        size_of::<LeSetConnectionlessCteTransmitParametersHeader>(),
        5
    );

    // A pattern one octet short of what the header declares is not a command.
    assert!(LeSetConnectionlessCteTransmitParametersHeader::parse(&bytes[..8]).is_none());
}

/// Core Spec Vol 4, Part E, §7.8.82 — LE Set Connectionless IQ Sampling
/// Enable. Sync_Handle(2), Sampling_Enable(1), Slot_Durations(1),
/// Max_Sampled_CTEs(1), Switching_Pattern_Length(1), then Antenna_IDs[i].
///
/// This struct used to stop after Max_Sampled_CTEs, describing a 5-octet
/// command where the spec defines a variable-length one of at least 6. Against
/// the vector below the old struct would have consumed octet [5] — the
/// switching pattern length — as the end of the fixed header and reported no
/// antenna pattern at all. Verified against Zephyr's
/// `bt_hci_cp_le_set_cl_cte_sampling_enable`, which carries both fields.
#[test]
fn test_connectionless_iq_sampling_enable_parses_spec_bytes() {
    #[rustfmt::skip]
    let bytes: [u8; 9] = [
        0x34, 0x02,             // [0..2] Sync_Handle = 0x0234 (little endian)
        0x01,                   // [2]    Sampling_Enable = 1 (enabled)
        0x02,                   // [3]    Slot_Durations = 2 (2us slots)
        0x05,                   // [4]    Max_Sampled_CTEs = 5
        0x03,                   // [5]    Switching_Pattern_Length = 3
        0x00, 0x01, 0x02,       // [6..]  Antenna_IDs
    ];

    let (header, antenna_ids) =
        LeSetConnectionlessIqSamplingEnableHeader::parse(&bytes).expect("valid parse");
    assert_eq!(header.sync_handle.get(), 0x0234);
    assert_eq!(header.iq_sampling_enable, 1);
    assert_eq!(header.slot_durations, 2);
    assert_eq!(header.max_sampled_ctes, 5);
    assert_eq!(
        header.switching_pattern_length, 3,
        "Switching_Pattern_Length is a real parameter of 7.8.82, not part of the tail"
    );
    assert_eq!(antenna_ids, &[0x00, 0x01, 0x02]);

    assert_eq!(
        size_of::<LeSetConnectionlessIqSamplingEnableHeader>(),
        6,
        "2-octet Sync_Handle plus four single-octet parameters"
    );
    assert_eq!(
        LeSetConnectionlessIqSamplingEnableHeader::OP_CODE.get(),
        0x2053
    );

    // Round-tripping the serializer must reproduce the same wire octets.
    assert_eq!(
        LeSetConnectionlessIqSamplingEnableHeader::serialize(0x0234, 1, 2, 5, &[0x00, 0x01, 0x02]),
        bytes
    );
}

/// Core Spec Vol 4, Part E, §7.8.84 — LE Set Connection CTE Transmit
/// Parameters. Connection_Handle(2), CTE_Types(1), Switching_Pattern_Length(1),
/// then Antenna_IDs[i].
#[test]
fn test_connection_cte_transmit_parameters_parses_spec_bytes() {
    #[rustfmt::skip]
    let bytes: [u8; 6] = [
        0x0E, 0x01,             // [0..2] Connection_Handle = 0x010E (little endian)
        0x06,                   // [2]    CTE_Types = bit1|bit2 (AoD 1us + AoD 2us)
        0x02,                   // [3]    Switching_Pattern_Length = 2
        0x0A, 0x0B,             // [4..]  Antenna_IDs
    ];

    let (header, antenna_ids) =
        LeSetConnectionCteTransmitParametersHeader::parse(&bytes).expect("valid parse");
    assert_eq!(
        header.connection_handle.get(),
        0x010E,
        "a byte-swapped handle would read 0x0E01 here"
    );
    assert_eq!(header.cte_types, 0b0000_0110);
    assert_eq!(header.switching_pattern_length, 2);
    assert_eq!(antenna_ids, &[0x0A, 0x0B]);
    assert_eq!(size_of::<LeSetConnectionCteTransmitParametersHeader>(), 4);
}

/// Core Spec Vol 4, Part E, §7.8.85 — LE Connection CTE Request Enable.
/// Connection_Handle(2), Enable(1), CTE_Request_Interval(2),
/// Requested_CTE_Length(1), Requested_CTE_Type(1).
///
/// The interesting one for layout: CTE_Request_Interval is a 16-bit field
/// sitting at odd offset 3, so any padding the compiler inserted to align it
/// would shift every field behind it.
#[test]
fn test_connection_cte_request_enable_parses_spec_bytes() {
    #[rustfmt::skip]
    let bytes: [u8; 7] = [
        0x0C, 0x00,             // [0..2] Connection_Handle = 0x000C
        0x01,                   // [2]    Enable = 1
        0xD0, 0x07,             // [3..5] CTE_Request_Interval = 2000 connection events
        0x10,                   // [5]    Requested_CTE_Length = 16 (x 8us = 128us)
        0x01,                   // [6]    Requested_CTE_Type = 1 (AoD 1us)
    ];

    let cmd = LeConnectionCteRequestEnable::read_from_bytes(&bytes).expect("valid parse");
    assert_eq!(cmd.connection_handle.get(), 0x000C);
    assert_eq!(cmd.enable, 1);
    assert_eq!(
        cmd.cte_request_interval.get(),
        2000,
        "a padding octet before this field would read 0x1007 instead"
    );
    assert_eq!(cmd.requested_cte_length, 0x10);
    assert_eq!(cmd.requested_cte_type, 0x01);
    assert_eq!(
        size_of::<LeConnectionCteRequestEnable>(),
        7,
        "unaligned repr(C): no padding around the 16-bit field at offset 3"
    );
    assert_eq!(LeConnectionCteRequestEnable::OP_CODE.get(), 0x2056);
}

/// Core Spec Vol 4, Part E, §7.8.87 — LE Read Antenna Information, return
/// parameters of the Command Complete. Status(1),
/// Supported_Switching_Sampling_Rates(1), Num_Antennae(1),
/// Max_Switching_Pattern_Length(1), Max_CTE_Length(1).
#[test]
fn test_read_antenna_information_response_parses_spec_bytes() {
    #[rustfmt::skip]
    let bytes: [u8; 5] = [
        0x00,                   // [0] Status = Success
        0x0F,                   // [1] Supported_Switching_Sampling_Rates: all four bits
        0x04,                   // [2] Num_Antennae = 4
        0x08,                   // [3] Max_Switching_Pattern_Length = 8
        0x14,                   // [4] Max_CTE_Length = 20 (x 8us = 160us)
    ];

    let rp = LeReadAntennaInformationResponse::read_from_bytes(&bytes).expect("valid parse");
    assert_eq!(rp.status, 0x00);
    assert_eq!(rp.supported_switching_sampling_rates, 0x0F);
    assert_eq!(rp.num_antennae, 4);
    assert_eq!(rp.max_switching_pattern_length, 8);
    assert_eq!(rp.max_cte_length, 0x14);
    assert_eq!(size_of::<LeReadAntennaInformationResponse>(), 5);
}

/// Core Spec Vol 4, Part E, §7.7.65.21 — LE Connectionless IQ Report,
/// LE Meta subevent 0x15. Sync_Handle(2), Channel_Index(1), RSSI(2, signed,
/// 0.1 dBm), RSSI_Antenna_ID(1), CTE_Type(1), Slot_Durations(1),
/// Packet_Status(1), Periodic_Event_Counter(2), Sample_Count(1), then the
/// samples.
///
/// Sample ordering is I_Sample[0], Q_Sample[0], I_Sample[1], Q_Sample[1], …
/// per §5.4.4's rule for interleaving co-indexed arrayed parameters — matching
/// Zephyr's `struct bt_hci_le_iq_sample { int8_t i; int8_t q; }`.
///
/// Nothing in this crate had ever called this parser.
#[test]
fn test_connectionless_iq_report_parses_spec_bytes() {
    assert_eq!(df_subevent_code::LE_CONNECTIONLESS_IQ_REPORT, 0x15);

    #[rustfmt::skip]
    let bytes: [u8; 20] = [
        0x0D, 0x00,             // [0..2]   Sync_Handle = 0x000D
        0x24,                   // [2]      Channel_Index = 36
        0x9C, 0xFE,             // [3..5]   RSSI = -356 => -35.6 dBm
        0x02,                   // [5]      RSSI_Antenna_ID = 2
        0x00,                   // [6]      CTE_Type = 0 (AoA)
        0x01,                   // [7]      Slot_Durations = 1 (1us slots)
        0x00,                   // [8]      Packet_Status = 0 (CRC correct)
        0xE8, 0x03,             // [9..11]  Periodic_Event_Counter = 1000
        0x04,                   // [11]     Sample_Count = 4
        0x0A, 0xF6,             // [12..]   I0 = 10,  Q0 = -10
        0x14, 0xEC,             //          I1 = 20,  Q1 = -20
        0x1E, 0xE2,             //          I2 = 30,  Q2 = -30
        0x28, 0xD8,             //          I3 = 40,  Q3 = -40
    ];

    let (header, iq) =
        LeConnectionlessIqReportEventHeader::parse(&bytes).expect("valid parse of spec bytes");
    assert_eq!(header.sync_handle.get(), 0x000D);
    assert_eq!(header.channel_index, 36);
    assert_eq!(
        header.rssi.get(),
        -356,
        "RSSI is signed and little endian; a wrong offset here reads 0xFE24 or 0x029C"
    );
    assert_eq!(header.rssi_antenna_id, 2);
    assert_eq!(header.cte_type, 0);
    assert_eq!(header.slot_durations, 1);
    assert_eq!(header.packet_status, 0);
    assert_eq!(header.periodic_event_counter.get(), 1000);
    assert_eq!(header.sample_count, 4);
    assert_eq!(iq, &[10, -10, 20, -20, 30, -30, 40, -40]);

    assert_eq!(
        size_of::<LeConnectionlessIqReportEventHeader>(),
        12,
        "three 16-bit fields and six octets; the samples begin at offset 12"
    );

    // Sample_Count says 4 pairs (8 octets); 7 is a malformed event.
    assert!(LeConnectionlessIqReportEventHeader::parse(&bytes[..19]).is_none());
    // A header cut short is not a report either.
    assert!(LeConnectionlessIqReportEventHeader::parse(&bytes[..11]).is_none());
}

/// Core Spec Vol 4, Part E, §7.7.65.22 — LE Connection IQ Report, LE Meta
/// subevent 0x16. Connection_Handle(2), RX_PHY(1), Data_Channel_Index(1),
/// RSSI(2, signed), RSSI_Antenna_ID(1), CTE_Type(1), Slot_Durations(1),
/// Packet_Status(1), Connection_Event_Counter(2), Sample_Count(1), samples.
///
/// The existing round-trip test for this event builds its bytes with
/// `as_bytes`, so it agrees with the struct by construction whatever the
/// offsets are. These are hand-written spec octets instead.
#[test]
fn test_connection_iq_report_parses_spec_bytes() {
    assert_eq!(df_subevent_code::LE_CONNECTION_IQ_REPORT, 0x16);

    #[rustfmt::skip]
    let bytes: [u8; 19] = [
        0x40, 0x00,             // [0..2]   Connection_Handle = 0x0040
        0x02,                   // [2]      RX_PHY = 2 (LE 2M)
        0x0C,                   // [3]      Data_Channel_Index = 12
        0x3A, 0xFF,             // [4..6]   RSSI = -198 => -19.8 dBm
        0x01,                   // [6]      RSSI_Antenna_ID = 1
        0x02,                   // [7]      CTE_Type = 2 (AoD 2us)
        0x02,                   // [8]      Slot_Durations = 2 (2us slots)
        0x01,                   // [9]      Packet_Status = 1 (CRC incorrect)
        0x39, 0x30,             // [10..12] Connection_Event_Counter = 12345
        0x03,                   // [12]     Sample_Count = 3
        0x64, 0x00,             // [13..]   I0 = 100,  Q0 = 0
        0x00, 0x64,             //          I1 = 0,    Q1 = 100
        0x9C, 0x00,             //          I2 = -100, Q2 = 0
    ];

    let (header, iq) =
        LeConnectionIqReportEventHeader::parse(&bytes).expect("valid parse of spec bytes");
    assert_eq!(header.connection_handle.get(), 0x0040);
    assert_eq!(header.rx_phy, 2);
    assert_eq!(
        header.data_channel_index, 12,
        "RX_PHY precedes Data_Channel_Index; swapping them reads 2 here"
    );
    assert_eq!(header.rssi.get(), -198);
    assert_eq!(header.rssi_antenna_id, 1);
    assert_eq!(header.cte_type, 2);
    assert_eq!(header.slot_durations, 2);
    assert_eq!(header.packet_status, 1);
    assert_eq!(header.connection_event_counter.get(), 12345);
    assert_eq!(header.sample_count, 3);
    assert_eq!(iq, &[100, 0, 0, 100, -100, 0]);

    assert_eq!(
        size_of::<LeConnectionIqReportEventHeader>(),
        13,
        "one octet wider than the connectionless report: it carries RX_PHY too"
    );
}

/// Core Spec Vol 4, Part E, §7.7.65.23 — LE CTE Request Failed, LE Meta
/// subevent 0x17. Status(1), Connection_Handle(2).
///
/// Status 0x00 does not mean success here: it means the peer answered
/// LL_CTE_RSP without a CTE. Any other value is a Controller error code.
#[test]
fn test_cte_request_failed_parses_spec_bytes() {
    assert_eq!(df_subevent_code::LE_CTE_REQUEST_FAILED, 0x17);

    #[rustfmt::skip]
    let rejected: [u8; 3] = [
        0x1A,                   // [0]    Status = 0x1A (Unsupported Remote Feature)
        0x0C, 0x00,             // [1..3] Connection_Handle = 0x000C
    ];
    let evt = LeCteRequestFailedEvent::read_from_bytes(&rejected).expect("valid parse");
    assert_eq!(evt.status, 0x1A);
    assert_eq!(
        evt.connection_handle.get(),
        0x000C,
        "the handle follows the status octet; reading it from offset 0 gives 0x0C1A"
    );

    #[rustfmt::skip]
    let no_cte: [u8; 3] = [
        0x00,                   // [0]    Status = 0x00 (LL_CTE_RSP without a CTE)
        0xFF, 0x0E,             // [1..3] Connection_Handle = 0x0EFF (largest valid handle)
    ];
    let evt = LeCteRequestFailedEvent::read_from_bytes(&no_cte).expect("valid parse");
    assert_eq!(evt.status, 0x00);
    assert_eq!(evt.connection_handle.get(), 0x0EFF);

    assert_eq!(size_of::<LeCteRequestFailedEvent>(), 3);
    assert!(LeCteRequestFailedEvent::read_from_bytes(&no_cte[..2]).is_err());
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
