// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio Stream Control Service (ASCS) tests: full ASE state-machine lifecycles driven
//! through the public GATT database API, plus invalid Control Point opcode/transition
//! rejection.

use simble::gatt::GattDatabase;
use simble::profiles::ascs::{
    AseState, AudioStreamControlService, opcode, reason_code, response_code,
};
use simble::profiles::bap::LC3_CODEC_ID;

fn config_codec_pdu(ase_ids: &[u8]) -> Vec<u8> {
    let mut buf = vec![opcode::CONFIG_CODEC, ase_ids.len() as u8];
    for &ase_id in ase_ids {
        buf.push(ase_id);
        buf.push(3); // target_latency
        buf.push(1); // target_phy
        buf.extend_from_slice(&LC3_CODEC_ID);
        buf.push(0); // codec_specific_configuration length
    }
    buf
}

fn config_qos_pdu(ase_ids: &[u8]) -> Vec<u8> {
    let mut buf = vec![opcode::CONFIG_QOS, ase_ids.len() as u8];
    for &ase_id in ase_ids {
        buf.push(ase_id);
        buf.push(1); // cig_id
        buf.push(ase_id); // cis_id
        buf.extend_from_slice(&[100, 0, 0]); // sdu_interval
        buf.push(0); // framing
        buf.push(1); // phy
        buf.extend_from_slice(&100u16.to_le_bytes()); // max_sdu
        buf.push(13); // retransmission_number
        buf.extend_from_slice(&100u16.to_le_bytes()); // max_transport_latency
        buf.extend_from_slice(&[10, 0, 0]); // presentation_delay
    }
    buf
}

fn enable_pdu(ase_ids: &[u8]) -> Vec<u8> {
    let mut buf = vec![opcode::ENABLE, ase_ids.len() as u8];
    for &ase_id in ase_ids {
        buf.push(ase_id);
        buf.push(0); // metadata length
    }
    buf
}

fn id_op_pdu(op: u8, ase_ids: &[u8]) -> Vec<u8> {
    let mut buf = vec![op, ase_ids.len() as u8];
    buf.extend_from_slice(ase_ids);
    buf
}

// Port of Bumble's `le_audio_test.py::test_ascs`, minus the CIS-establishment step (Simble
// has no CIS/controller simulation): Config Codec -> Config QoS -> Enable ->
// Receiver Start Ready -> Release, observed on two Sink ASEs at once.
#[test]
fn test_ascs_two_ase_stream_lifecycle() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1, 2], &[]);

    assert_eq!(
        db.read(ascs.ase(1).unwrap().value_handle, 0).unwrap(),
        &[1, AseState::Idle as u8]
    );
    assert_eq!(
        db.read(ascs.ase(2).unwrap().value_handle, 0).unwrap(),
        &[2, AseState::Idle as u8]
    );

    let resp = ascs.write_control_point(&mut db, &config_codec_pdu(&[1, 2]));
    assert_eq!(
        resp,
        vec![
            opcode::CONFIG_CODEC,
            2,
            1,
            response_code::SUCCESS,
            reason_code::NONE,
            2,
            response_code::SUCCESS,
            reason_code::NONE,
        ]
    );
    for ase_id in [1, 2] {
        let value = db.read(ascs.ase(ase_id).unwrap().value_handle, 0).unwrap();
        assert_eq!(value[1], AseState::CodecConfigured as u8);
    }

    ascs.write_control_point(&mut db, &config_qos_pdu(&[1, 2]));
    for ase_id in [1, 2] {
        assert_eq!(ascs.ase(ase_id).unwrap().state, AseState::QosConfigured);
    }

    ascs.write_control_point(&mut db, &enable_pdu(&[1, 2]));
    for ase_id in [1, 2] {
        assert_eq!(ascs.ase(ase_id).unwrap().state, AseState::Enabling);
    }

    ascs.write_control_point(&mut db, &id_op_pdu(opcode::RECEIVER_START_READY, &[1, 2]));
    for ase_id in [1, 2] {
        assert_eq!(ascs.ase(ase_id).unwrap().state, AseState::Streaming);
    }

    let resp = ascs.write_control_point(&mut db, &id_op_pdu(opcode::RELEASE, &[1, 2]));
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(resp[6], response_code::SUCCESS);
    for ase_id in [1, 2] {
        assert_eq!(ascs.ase(ase_id).unwrap().state, AseState::Idle);
        let value = db.read(ascs.ase(ase_id).unwrap().value_handle, 0).unwrap();
        assert_eq!(value, &[ase_id, AseState::Idle as u8]);
    }
}

// Port of Bumble's `le_audio_test.py::test_ascs_enable_source_then_sink`: a Sink and a
// Source ASE progress through Config Codec/QoS together, then get Enabled independently -
// each ASE's state machine must be independent of the other's.
#[test]
fn test_ascs_sink_and_source_ase_progress_independently() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[2]);

    ascs.write_control_point(&mut db, &config_codec_pdu(&[1, 2]));
    ascs.write_control_point(&mut db, &config_qos_pdu(&[1, 2]));
    for ase_id in [1, 2] {
        assert_eq!(ascs.ase(ase_id).unwrap().state, AseState::QosConfigured);
    }

    ascs.write_control_point(&mut db, &enable_pdu(&[2]));
    assert_eq!(ascs.ase(2).unwrap().state, AseState::Enabling);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::QosConfigured);

    ascs.write_control_point(&mut db, &id_op_pdu(opcode::RECEIVER_START_READY, &[2]));
    assert_eq!(ascs.ase(2).unwrap().state, AseState::Streaming);

    ascs.write_control_point(&mut db, &enable_pdu(&[1]));
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Enabling);
    assert_eq!(ascs.ase(2).unwrap().state, AseState::Streaming);
}

#[test]
fn test_ascs_sink_disable_skips_receiver_stop_ready() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
    ascs.write_control_point(&mut db, &config_qos_pdu(&[1]));
    ascs.write_control_point(&mut db, &enable_pdu(&[1]));

    let resp = ascs.write_control_point(&mut db, &id_op_pdu(opcode::DISABLE, &[1]));
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::QosConfigured);
}

#[test]
fn test_ascs_source_disable_then_receiver_stop_ready() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[], &[2]);
    ascs.write_control_point(&mut db, &config_codec_pdu(&[2]));
    ascs.write_control_point(&mut db, &config_qos_pdu(&[2]));
    ascs.write_control_point(&mut db, &enable_pdu(&[2]));

    let resp = ascs.write_control_point(&mut db, &id_op_pdu(opcode::DISABLE, &[2]));
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(2).unwrap().state, AseState::Disabling);

    let resp = ascs.write_control_point(&mut db, &id_op_pdu(opcode::RECEIVER_STOP_READY, &[2]));
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(2).unwrap().state, AseState::QosConfigured);
}

#[test]
fn test_ascs_rejects_operations_out_of_order() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);

    // Enable before any Config Codec/QoS.
    let resp = ascs.write_control_point(&mut db, &enable_pdu(&[1]));
    assert_eq!(resp[3], response_code::INVALID_ASE_STATE_MACHINE_TRANSITION);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Idle);

    // Release before ever configuring.
    let resp = ascs.write_control_point(&mut db, &id_op_pdu(opcode::RELEASE, &[1]));
    assert_eq!(resp[3], response_code::INVALID_ASE_STATE_MACHINE_TRANSITION);

    // Config Codec, then Receiver Start Ready without Config QoS/Enable in between.
    ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
    let resp = ascs.write_control_point(&mut db, &id_op_pdu(opcode::RECEIVER_START_READY, &[1]));
    assert_eq!(resp[3], response_code::INVALID_ASE_STATE_MACHINE_TRANSITION);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::CodecConfigured);
}

#[test]
fn test_ascs_unknown_ase_id_rejected() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    let resp = ascs.write_control_point(&mut db, &config_codec_pdu(&[42]));
    assert_eq!(resp[3], response_code::INVALID_ASE_ID);
}

#[test]
fn test_ascs_unsupported_opcode_rejected() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    let resp = ascs.write_control_point(&mut db, &[0x7F, 0x00]);
    assert_eq!(resp[3], response_code::UNSUPPORTED_OPCODE);
}

#[test]
fn test_ascs_truncated_control_point_write_reports_invalid_length() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    let resp = ascs.write_control_point(&mut db, &[opcode::CONFIG_QOS, 1, 1, 2, 3]);
    assert_eq!(resp[3], response_code::INVALID_LENGTH);
}

#[test]
fn test_ascs_update_metadata_while_streaming() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
    ascs.write_control_point(&mut db, &config_qos_pdu(&[1]));
    ascs.write_control_point(&mut db, &enable_pdu(&[1]));
    ascs.write_control_point(&mut db, &id_op_pdu(opcode::RECEIVER_START_READY, &[1]));
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Streaming);

    let mut pdu = vec![opcode::UPDATE_METADATA, 1, 1, 3];
    pdu.extend_from_slice(b"eng");
    let resp = ascs.write_control_point(&mut db, &pdu);
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Streaming);
    assert_eq!(ascs.ase(1).unwrap().metadata, b"eng");
}
