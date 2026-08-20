// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for Bluetooth 6.0 Channel Sounding (CS) and Ranging Service (RAS).

use simble::cs::compute_pbr_distance;
use simble::gatt::GattDatabase;
use simble::packets::hci::{
    HciCommand, LeCsCreateConfig, LeCsProcedureEnable, LeCsReadLocalSupportedCapabilities,
    LeCsSubeventResultHeader, cs_opcode,
};
use simble::profiles::RangingService;
use zerocopy::{FromBytes, IntoBytes, U16};

#[test]
fn test_channel_sounding_hci_packet_roundtrip() {
    // 1. Verify Read Local Capabilities command OpCode
    assert_eq!(
        LeCsReadLocalSupportedCapabilities::OP_CODE.get(),
        cs_opcode::LE_CS_READ_LOCAL_SUPPORTED_CAPABILITIES.get()
    );

    // 2. Build CS Create Config Command
    let config_cmd = LeCsCreateConfig {
        connection_handle: U16::from(0x0040),
        config_id: 1,
        create_context: 0,
        main_mode_type: 3, // RTT + PBR
        sub_mode_type: 1,
        min_main_mode_steps: 4,
        max_main_mode_steps: 16,
        main_mode_repetition: 0,
        mode_0_steps: 2,
        role: 0, // Initiator
        rtt_type: 1,
        cs_sync_phy: 1,
        channel_map: [0xFF; 10],
        channel_map_repetition: 1,
        channel_selection_type: 0,
        ch3c_shape: 0,
        ch3c_jump: 0,
        companion_signal_status: 0,
    };

    let bytes = config_cmd.as_bytes();
    let parsed = LeCsCreateConfig::ref_from_bytes(bytes).expect("zero-copy parsing");
    assert_eq!(parsed.connection_handle.get(), 0x0040);
    assert_eq!(parsed.config_id, 1);
    assert_eq!(parsed.main_mode_type, 3);
    assert_eq!(parsed.role, 0);

    // 3. Procedure Enable Command
    let enable_cmd = LeCsProcedureEnable {
        connection_handle: U16::from(0x0040),
        config_id: 1,
        enable: 1,
    };
    assert_eq!(
        LeCsProcedureEnable::OP_CODE.get(),
        cs_opcode::LE_CS_PROCEDURE_ENABLE.get()
    );
    assert_eq!(enable_cmd.as_bytes().len(), 4);
}

#[test]
fn test_channel_sounding_subevent_result_and_distance_estimation() {
    // 1. Simulate an incoming LE CS Subevent Result Header
    let subevent_header = LeCsSubeventResultHeader {
        connection_handle: U16::from(0x0040),
        config_id: 1,
        start_acl_conn_event: U16::from(100),
        procedure_counter: U16::from(1),
        frequency_compensation: U16::from(0),
        reference_power_level: -10,
        procedure_done_status: 1,
        subevent_done_status: 1,
        procedure_abort_reason: 0,
        subevent_abort_reason: 0,
        num_antenna_paths: 1,
        num_steps_reported: 8,
    };

    let header_bytes = subevent_header.as_bytes();
    let parsed_header = LeCsSubeventResultHeader::ref_from_bytes(header_bytes).unwrap();
    assert_eq!(parsed_header.num_steps_reported, 8);
    assert_eq!(parsed_header.connection_handle.get(), 0x0040);

    // 2. Estimate distance using Phase-Based Ranging (PBR) formula across 1 MHz frequency steps
    // At 3.0 meters: delta_phi = (4 * pi * 1e6 * 3.0) / 3e8 ~= 0.12566 rad
    let estimated_dist = compute_pbr_distance(1_000_000.0, 0.12566);
    assert!(
        (estimated_dist - 3.0).abs() < 0.05,
        "Estimated: {estimated_dist}"
    );
}

#[test]
fn test_ranging_service_ras_integration() {
    let mut db = GattDatabase::new();
    let ras = RangingService::register(&mut db);

    // 1. Central reads Ranging Features
    let features = db
        .read(ras.features_value_handle, 0)
        .expect("read features");
    assert_eq!(features, &[0x03, 0x00, 0x00, 0x00]); // Real-time + On-demand

    // 2. Simble Host publishes a new Ranging Measurement (5.50 meters, 98% confidence)
    let payload = ras
        .update_ranging_data(&mut db, 5.50, 0.98)
        .expect("update ranging");
    assert_eq!(payload.len(), 8);

    // 3. Read back verified
    let read_payload = db.read(ras.realtime_data_value_handle, 0).unwrap();
    let dist = f32::from_le_bytes(read_payload[0..4].try_into().unwrap());
    let conf = f32::from_le_bytes(read_payload[4..8].try_into().unwrap());
    assert!((dist - 5.50).abs() < 1e-4);
    assert!((conf - 0.98).abs() < 1e-4);
}
