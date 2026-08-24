use super::*;

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

#[test]
fn test_ase_starts_idle() {
    let mut db = GattDatabase::new();
    let ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Idle);
    assert_eq!(
        db.read(ascs.ase(1).unwrap().value_handle, 0).unwrap(),
        &[1, 0]
    );
}

#[test]
fn test_full_valid_transition_sequence() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);

    let resp = ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
    assert_eq!(
        resp,
        vec![
            opcode::CONFIG_CODEC,
            1,
            1,
            response_code::SUCCESS,
            reason_code::NONE
        ]
    );
    assert_eq!(ascs.ase(1).unwrap().state, AseState::CodecConfigured);

    let qos_pdu = {
        let mut buf = vec![opcode::CONFIG_QOS, 1, 1, 1, 2];
        buf.extend_from_slice(&[10, 0, 0]); // sdu_interval
        buf.push(0); // framing
        buf.push(1); // phy
        buf.extend_from_slice(&40u16.to_le_bytes());
        buf.push(5); // retransmission_number
        buf.extend_from_slice(&20u16.to_le_bytes());
        buf.extend_from_slice(&[0, 0, 0]); // presentation_delay
        buf
    };
    let resp = ascs.write_control_point(&mut db, &qos_pdu);
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::QosConfigured);

    let enable_pdu = vec![opcode::ENABLE, 1, 1, 0];
    let resp = ascs.write_control_point(&mut db, &enable_pdu);
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Enabling);

    let start_ready_pdu = vec![opcode::RECEIVER_START_READY, 1, 1];
    let resp = ascs.write_control_point(&mut db, &start_ready_pdu);
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Streaming);

    // ASCS 5.8: Release parks the ASE in Releasing; ASCS 5.9's Released operation is
    // what finishes the teardown, and it is the server's to perform.
    let release_pdu = vec![opcode::RELEASE, 1, 1];
    let resp = ascs.write_control_point(&mut db, &release_pdu);
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Releasing);
    assert_eq!(
        db.read(ascs.ase(1).unwrap().value_handle, 0).unwrap(),
        &[1, AseState::Releasing as u8]
    );

    assert_eq!(ascs.released(&mut db, false), vec![1]);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Idle);
    assert_eq!(
        db.read(ascs.ase(1).unwrap().value_handle, 0).unwrap(),
        &[1, 0]
    );
}

#[test]
fn test_enable_before_qos_is_rejected() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));

    let enable_pdu = vec![opcode::ENABLE, 1, 1, 0];
    let resp = ascs.write_control_point(&mut db, &enable_pdu);
    assert_eq!(
        resp,
        vec![
            opcode::ENABLE,
            1,
            1,
            response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
            reason_code::NONE
        ]
    );
    assert_eq!(ascs.ase(1).unwrap().state, AseState::CodecConfigured);
}

#[test]
fn test_unknown_ase_id_is_rejected() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    let resp = ascs.write_control_point(&mut db, &config_codec_pdu(&[99]));
    assert_eq!(
        resp,
        vec![
            opcode::CONFIG_CODEC,
            1,
            99,
            response_code::INVALID_ASE_ID,
            reason_code::NONE
        ]
    );
}

#[test]
fn test_unsupported_opcode_is_rejected() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    let resp = ascs.write_control_point(&mut db, &[0xFF, 0x00]);
    assert_eq!(
        resp,
        vec![
            0xFF,
            1,
            0,
            response_code::UNSUPPORTED_OPCODE,
            reason_code::NONE
        ]
    );
}

#[test]
fn test_truncated_pdu_reports_invalid_length() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    let resp = ascs.write_control_point(&mut db, &[opcode::CONFIG_CODEC, 1, 1]);
    assert_eq!(
        resp,
        vec![
            opcode::CONFIG_CODEC,
            1,
            0,
            response_code::INVALID_LENGTH,
            reason_code::NONE
        ]
    );
}

#[test]
fn test_sink_disable_returns_to_qos_configured_without_receiver_stop_ready() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
    ascs.set_ase_state(1, AseState::QosConfigured);
    ascs.write_control_point(&mut db, &[opcode::ENABLE, 1, 1, 0]);

    let resp = ascs.write_control_point(&mut db, &[opcode::DISABLE, 1, 1]);
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::QosConfigured);
}

#[test]
fn test_source_disable_then_receiver_stop_ready() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[], &[2]);
    ascs.write_control_point(&mut db, &config_codec_pdu(&[2]));
    ascs.set_ase_state(2, AseState::QosConfigured);
    ascs.write_control_point(&mut db, &[opcode::ENABLE, 1, 2, 0]);

    let resp = ascs.write_control_point(&mut db, &[opcode::DISABLE, 1, 2]);
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(2).unwrap().state, AseState::Disabling);

    let resp = ascs.write_control_point(&mut db, &[opcode::RECEIVER_STOP_READY, 1, 2]);
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(2).unwrap().state, AseState::QosConfigured);
}

#[test]
fn test_receiver_stop_ready_on_sink_is_rejected() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
    ascs.set_ase_state(1, AseState::QosConfigured);
    ascs.write_control_point(&mut db, &[opcode::ENABLE, 1, 1, 0]);
    ascs.write_control_point(&mut db, &[opcode::DISABLE, 1, 1]);

    let resp = ascs.write_control_point(&mut db, &[opcode::RECEIVER_STOP_READY, 1, 1]);
    assert_eq!(resp[3], response_code::INVALID_ASE_STATE_MACHINE_TRANSITION);
}

#[test]
fn test_release_from_idle_is_rejected() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
    let resp = ascs.write_control_point(&mut db, &[opcode::RELEASE, 1, 1]);
    assert_eq!(resp[3], response_code::INVALID_ASE_STATE_MACHINE_TRANSITION);
}
