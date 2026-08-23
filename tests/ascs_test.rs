// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio Stream Control Service (ASCS) tests: full ASE state-machine lifecycles driven
//! through the public GATT database API, plus invalid Control Point opcode/transition
//! rejection.

use simble::gatt::GattDatabase;
use simble::profiles::ascs::{
    AseState, AudioRole, AudioStreamControlService, opcode, reason_code, response_code,
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

fn update_metadata_pdu(ase_ids: &[u8]) -> Vec<u8> {
    let mut buf = vec![opcode::UPDATE_METADATA, ase_ids.len() as u8];
    for &ase_id in ase_ids {
        buf.push(ase_id);
        buf.push(0); // metadata length
    }
    buf
}

/// A well-formed single-ASE PDU for `op`, so the only thing under test in the matrix below
/// is the state machine - never the parser.
fn pdu_for(op: u8, ase_ids: &[u8]) -> Vec<u8> {
    match op {
        opcode::CONFIG_CODEC => config_codec_pdu(ase_ids),
        opcode::CONFIG_QOS => config_qos_pdu(ase_ids),
        opcode::ENABLE => enable_pdu(ase_ids),
        opcode::UPDATE_METADATA => update_metadata_pdu(ase_ids),
        _ => id_op_pdu(op, ase_ids),
    }
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

    // ASCS 5.8: Release lands in Releasing, not Idle - and a client sees that notification
    // before the one that follows. ASCS 5.9's Released operation is the second half.
    let resp = ascs.write_control_point(&mut db, &id_op_pdu(opcode::RELEASE, &[1, 2]));
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(resp[6], response_code::SUCCESS);
    for ase_id in [1, 2] {
        assert_eq!(ascs.ase(ase_id).unwrap().state, AseState::Releasing);
        let value = db.read(ascs.ase(ase_id).unwrap().value_handle, 0).unwrap();
        assert_eq!(value, &[ase_id, AseState::Releasing as u8]);
    }

    assert_eq!(ascs.released(&mut db, false), vec![1, 2]);
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

// ---------------------------------------------------------------------------
// The state x opcode matrix
// ---------------------------------------------------------------------------
//
// One row per ASE state, one column per ASE Control Point opcode, transcribed from the
// per-operation state-machine tables in ASCS Section 5 (5.1 Config Codec, 5.2 Config QoS,
// 5.3 Enable, 5.4 Receiver Start Ready, 5.5 Disable, 5.6 Receiver Stop Ready, 5.7 Update
// Metadata, 5.8 Release). Each of those tables lists the states in which the operation is
// valid and the state the ASE lands in; every other state is a rejection carrying
// Response_Code 0x04 (Invalid ASE State Machine Transition).
//
// Two assertions per cell, and the second is the point: a rejected operation must leave the
// ASE exactly where it was. A rejection that still mutates state is the bug shape this
// table exists to catch - the response code alone cannot see it.
//
// Sink and Source ASEs get separate tables because two operations are direction-dependent
// (ASCS 3.4): Disable sends a Source ASE to Disabling but a Sink ASE straight back to QoS
// Configured, so a Sink ASE has no reachable Disabling row at all and Receiver Stop Ready
// is never valid on one.

const ASE_ID: u8 = 1;

/// Column order for every `Row::outcome` below.
const ALL_OPCODES: [u8; 8] = [
    opcode::CONFIG_CODEC,
    opcode::CONFIG_QOS,
    opcode::ENABLE,
    opcode::RECEIVER_START_READY,
    opcode::DISABLE,
    opcode::RECEIVER_STOP_READY,
    opcode::UPDATE_METADATA,
    opcode::RELEASE,
];

/// One row of the ASCS Section 5 tables.
struct Row {
    /// The state the ASE is in when the operation arrives.
    state: AseState,
    /// Indexed by [`ALL_OPCODES`]. `Some(next)` means the operation is accepted
    /// (Response_Code 0x00, Success) and the ASE lands in `next` - which for Update
    /// Metadata is the state it was already in. `None` means rejected with 0x04, state
    /// unchanged.
    outcome: [Option<AseState>; 8],
}

use AseState::{
    CodecConfigured as CC, Disabling as DIS, Enabling as EN, Idle as ID, QosConfigured as QC,
    Releasing as REL, Streaming as ST,
};

// Column order, for reading the tables below:
//   codec | qos | enable | rx-start | disable | rx-stop | metadata | release

/// Sink ASE. Disabling has no row: ASCS 3.4 routes a Sink ASE's Disable straight to QoS
/// Configured, so the state is unreachable on a Sink (asserted separately by
/// `test_ascs_sink_disable_skips_receiver_stop_ready`).
const SINK_TABLE: [Row; 6] = [
    Row {
        state: ID,
        outcome: [Some(CC), None, None, None, None, None, None, None],
    },
    Row {
        state: CC,
        outcome: [Some(CC), Some(QC), None, None, None, None, None, Some(REL)],
    },
    Row {
        state: QC,
        outcome: [
            Some(CC),
            Some(QC),
            Some(EN),
            None,
            None,
            None,
            None,
            Some(REL),
        ],
    },
    Row {
        state: EN,
        outcome: [
            None,
            None,
            None,
            Some(ST),
            Some(QC),
            None,
            Some(EN),
            Some(REL),
        ],
    },
    Row {
        state: ST,
        outcome: [None, None, None, None, Some(QC), None, Some(ST), Some(REL)],
    },
    // ASCS 5.8/5.9: Releasing is where a release waits out the CIS teardown. Nothing a
    // client writes moves an ASE out of it - only the server's own Released operation.
    Row {
        state: REL,
        outcome: [None, None, None, None, None, None, None, None],
    },
];

/// Source ASE. Identical to the Sink table except for the two direction-dependent
/// operations: Disable lands in Disabling, and Receiver Stop Ready is valid there.
const SOURCE_TABLE: [Row; 7] = [
    Row {
        state: ID,
        outcome: [Some(CC), None, None, None, None, None, None, None],
    },
    Row {
        state: CC,
        outcome: [Some(CC), Some(QC), None, None, None, None, None, Some(REL)],
    },
    Row {
        state: QC,
        outcome: [
            Some(CC),
            Some(QC),
            Some(EN),
            None,
            None,
            None,
            None,
            Some(REL),
        ],
    },
    Row {
        state: EN,
        outcome: [
            None,
            None,
            None,
            Some(ST),
            Some(DIS),
            None,
            Some(EN),
            Some(REL),
        ],
    },
    Row {
        state: ST,
        outcome: [None, None, None, None, Some(DIS), None, Some(ST), Some(REL)],
    },
    Row {
        state: DIS,
        outcome: [None, None, None, None, None, Some(QC), None, Some(REL)],
    },
    Row {
        state: REL,
        outcome: [None, None, None, None, None, None, None, None],
    },
];

/// Builds a service whose single ASE sits in `state`, reached only through public API -
/// real Control Point writes, no test-only state setter. If a walk lands somewhere else the
/// setup itself fails, so a matrix row can never silently test the wrong state.
fn ase_in_state(role: AudioRole, state: AseState) -> (GattDatabase, AudioStreamControlService) {
    let mut db = GattDatabase::new();
    let ids = [ASE_ID];
    let (sink, source): (&[u8], &[u8]) = match role {
        AudioRole::Sink => (&ids, &[]),
        AudioRole::Source => (&[], &ids),
    };
    let mut ascs = AudioStreamControlService::register(&mut db, sink, source);

    if state != AseState::Idle {
        ascs.write_control_point(&mut db, &config_codec_pdu(&ids));
    }
    if !matches!(state, AseState::Idle | AseState::CodecConfigured) {
        ascs.write_control_point(&mut db, &config_qos_pdu(&ids));
    }
    if matches!(
        state,
        AseState::Enabling | AseState::Streaming | AseState::Disabling | AseState::Releasing
    ) {
        ascs.write_control_point(&mut db, &enable_pdu(&ids));
    }
    if matches!(
        state,
        AseState::Streaming | AseState::Disabling | AseState::Releasing
    ) {
        ascs.write_control_point(&mut db, &id_op_pdu(opcode::RECEIVER_START_READY, &ids));
    }
    if state == AseState::Disabling {
        ascs.write_control_point(&mut db, &id_op_pdu(opcode::DISABLE, &ids));
    }
    if state == AseState::Releasing {
        ascs.write_control_point(&mut db, &id_op_pdu(opcode::RELEASE, &ids));
    }

    assert_eq!(
        ascs.ase(ASE_ID).unwrap().state,
        state,
        "setup walk for {role:?}/{state:?} landed in the wrong state"
    );
    (db, ascs)
}

#[test]
fn test_ascs_state_by_opcode_matrix() {
    let mut cells = 0;
    for (role, table) in [
        (AudioRole::Sink, &SINK_TABLE[..]),
        (AudioRole::Source, &SOURCE_TABLE[..]),
    ] {
        for row in table {
            for (column, &op) in ALL_OPCODES.iter().enumerate() {
                let (mut db, mut ascs) = ase_in_state(role, row.state);
                let resp = ascs.write_control_point(&mut db, &pdu_for(op, &[ASE_ID]));

                // `[Opcode, Number_of_ASEs, ASE_ID, Response_Code, Reason]` for a one-ASE
                // operation (ASCS 5, ASE Control Point notification format).
                assert_eq!(
                    resp.len(),
                    5,
                    "{role:?}/{:?} + opcode {op:#04x}: malformed response {resp:?}",
                    row.state
                );
                assert_eq!(resp[0], op);
                assert_eq!(resp[1], 1);
                assert_eq!(resp[2], ASE_ID);
                assert_eq!(resp[4], reason_code::NONE);

                let got_state = ascs.ase(ASE_ID).unwrap().state;
                match row.outcome[column] {
                    Some(next) => {
                        assert_eq!(
                            resp[3],
                            response_code::SUCCESS,
                            "{role:?}/{:?} + opcode {op:#04x} should be accepted",
                            row.state
                        );
                        assert_eq!(
                            got_state, next,
                            "{role:?}/{:?} + opcode {op:#04x} landed in {got_state:?}",
                            row.state
                        );
                    }
                    None => {
                        assert_eq!(
                            resp[3],
                            response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                            "{role:?}/{:?} + opcode {op:#04x} should be rejected",
                            row.state
                        );
                        // The half that makes the table worth writing.
                        assert_eq!(
                            got_state, row.state,
                            "{role:?}: rejected opcode {op:#04x} still moved the ASE out of \
                             {:?} into {got_state:?}",
                            row.state
                        );
                    }
                }

                // The published characteristic value must agree with the state machine
                // whichever way the cell went - a rejection must not notify a phantom state.
                let handle = ascs.ase(ASE_ID).unwrap().value_handle;
                let value = db.read(handle, 0).unwrap();
                assert_eq!(
                    value[1], got_state as u8,
                    "{role:?}/{:?} + opcode {op:#04x}: published value disagrees with state",
                    row.state
                );
                cells += 1;
            }
        }
    }
    // 13 reachable (role, state) pairs x 8 opcodes.
    assert_eq!(cells, 104);
}

#[test]
fn test_ascs_every_opcode_reports_invalid_length_on_a_truncated_pdu() {
    // Two truncation shapes per opcode: the Number_of_ASEs byte missing entirely, and a
    // per-ASE parameter record cut short mid-way. Both are INVALID_LENGTH (0x02) per
    // ASCS 5, and neither may disturb the ASE.
    let short_records: [(u8, Vec<u8>); 8] = [
        // 6 of the 9 fixed header bytes (ASE_ID, Target_Latency, Target_PHY, Codec_ID(5),
        // Codec_Specific_Configuration_Length).
        (
            opcode::CONFIG_CODEC,
            vec![opcode::CONFIG_CODEC, 1, ASE_ID, 3, 1, 0x06],
        ),
        // 6 of the 16 bytes of a Config QoS record.
        (
            opcode::CONFIG_QOS,
            vec![opcode::CONFIG_QOS, 1, ASE_ID, 1, 1, 100],
        ),
        // ASE_ID present, Metadata_Length missing.
        (opcode::ENABLE, vec![opcode::ENABLE, 1, ASE_ID]),
        // Number_of_ASEs says one, no ASE_ID follows.
        (
            opcode::RECEIVER_START_READY,
            vec![opcode::RECEIVER_START_READY, 1],
        ),
        (opcode::DISABLE, vec![opcode::DISABLE, 1]),
        (
            opcode::RECEIVER_STOP_READY,
            vec![opcode::RECEIVER_STOP_READY, 1],
        ),
        (
            opcode::UPDATE_METADATA,
            vec![opcode::UPDATE_METADATA, 1, ASE_ID],
        ),
        (opcode::RELEASE, vec![opcode::RELEASE, 1]),
    ];

    for (op, short) in short_records {
        for pdu in [vec![op], short] {
            // A Source ASE in Streaming: every opcode is plausible here, so a length check
            // that leaked into the state machine would show up as a state change.
            let (mut db, mut ascs) = ase_in_state(AudioRole::Source, AseState::Streaming);
            let resp = ascs.write_control_point(&mut db, &pdu);
            assert_eq!(resp[0], op, "opcode echo for {pdu:?}");
            assert_eq!(
                resp[3],
                response_code::INVALID_LENGTH,
                "truncated {pdu:?} should be INVALID_LENGTH, got {resp:?}"
            );
            assert_eq!(
                ascs.ase(ASE_ID).unwrap().state,
                AseState::Streaming,
                "truncated {pdu:?} moved the ASE"
            );
        }
    }
}

#[test]
fn test_ascs_metadata_length_overrunning_the_pdu_is_invalid_length() {
    for op in [opcode::ENABLE, opcode::UPDATE_METADATA] {
        let (mut db, mut ascs) = ase_in_state(AudioRole::Source, AseState::Streaming);
        // Metadata_Length claims five bytes; only two follow.
        let resp = ascs.write_control_point(&mut db, &[op, 1, ASE_ID, 5, 0x03, 0x02]);
        assert_eq!(resp[3], response_code::INVALID_LENGTH);
        assert_eq!(ascs.ase(ASE_ID).unwrap().state, AseState::Streaming);
    }
}

// ---------------------------------------------------------------------------
// ASCS 5.9 - the Released operation
// ---------------------------------------------------------------------------

#[test]
fn test_ascs_released_without_codec_cache_returns_the_ase_to_idle() {
    let (mut db, mut ascs) = ase_in_state(AudioRole::Sink, AseState::Releasing);
    assert_eq!(ascs.released(&mut db, false), vec![ASE_ID]);

    let ase = ascs.ase(ASE_ID).unwrap();
    assert_eq!(ase.state, AseState::Idle);
    // Idle carries no additional parameters (ASCS 5, ASE characteristic format).
    assert_eq!(
        db.read(ase.value_handle, 0).unwrap(),
        &[ASE_ID, AseState::Idle as u8]
    );
    // The QoS mapping must not outlive the release, or a stale CIG/CIS would be reported
    // the next time this ASE reaches QoS Configured.
    assert_eq!(ase.cig_id, 0);
    assert_eq!(ase.cis_id, 0);
    assert_eq!(ase.presentation_delay, 0);
    assert!(ase.metadata.is_empty());
}

#[test]
fn test_ascs_released_with_codec_cache_returns_the_ase_to_codec_configured() {
    let (mut db, mut ascs) = ase_in_state(AudioRole::Sink, AseState::Releasing);
    let configured = ascs.ase(ASE_ID).unwrap().codec_id;

    assert_eq!(ascs.released(&mut db, true), vec![ASE_ID]);
    let ase = ascs.ase(ASE_ID).unwrap();
    assert_eq!(ase.state, AseState::CodecConfigured);
    assert_eq!(
        ase.codec_id, configured,
        "the cached codec configuration is the point"
    );
    assert_eq!(ase.cig_id, 0, "QoS does not survive a release");
    assert_eq!(
        db.read(ase.value_handle, 0).unwrap()[1],
        AseState::CodecConfigured as u8
    );

    // And the client can pick straight back up at Config QoS.
    let resp = ascs.write_control_point(&mut db, &config_qos_pdu(&[ASE_ID]));
    assert_eq!(resp[3], response_code::SUCCESS);
    assert_eq!(ascs.ase(ASE_ID).unwrap().state, AseState::QosConfigured);
}

#[test]
fn test_ascs_released_is_a_no_op_outside_releasing() {
    for state in [
        AseState::Idle,
        AseState::CodecConfigured,
        AseState::QosConfigured,
        AseState::Enabling,
        AseState::Streaming,
    ] {
        let (mut db, mut ascs) = ase_in_state(AudioRole::Sink, state);
        assert!(
            ascs.released(&mut db, false).is_empty(),
            "Released fired on an ASE in {state:?}"
        );
        assert_eq!(ascs.ase(ASE_ID).unwrap().state, state);
    }
}

// ---------------------------------------------------------------------------
// ASCS 3.2 - link loss
// ---------------------------------------------------------------------------

#[test]
fn test_ascs_cis_loss_returns_the_ase_to_qos_configured() {
    // The three states in which an ASE can have a CIS. Disabling is Source-only.
    for (role, state) in [
        (AudioRole::Sink, AseState::Enabling),
        (AudioRole::Sink, AseState::Streaming),
        (AudioRole::Source, AseState::Enabling),
        (AudioRole::Source, AseState::Streaming),
        (AudioRole::Source, AseState::Disabling),
    ] {
        let (mut db, mut ascs) = ase_in_state(role, state);
        let before = ascs.ase(ASE_ID).unwrap();
        let (cig_id, cis_id) = (before.cig_id, before.cis_id);
        let presentation_delay = before.presentation_delay;

        assert_eq!(
            ascs.on_cis_loss(&mut db, cig_id, cis_id),
            vec![ASE_ID],
            "CIS loss in {role:?}/{state:?}"
        );
        let ase = ascs.ase(ASE_ID).unwrap();
        assert_eq!(ase.state, AseState::QosConfigured, "{role:?}/{state:?}");
        // QoS survives: the ASE is still QoS-configured, it just has no CIS, so the client
        // may Enable again without repeating Config QoS.
        assert_eq!(ase.presentation_delay, presentation_delay);
        assert_eq!(
            db.read(ase.value_handle, 0).unwrap()[1],
            AseState::QosConfigured as u8
        );

        let resp = ascs.write_control_point(&mut db, &enable_pdu(&[ASE_ID]));
        assert_eq!(resp[3], response_code::SUCCESS);
    }
}

#[test]
fn test_ascs_cis_loss_is_ignored_where_no_cis_exists() {
    for state in [
        AseState::Idle,
        AseState::CodecConfigured,
        AseState::QosConfigured,
        AseState::Releasing,
    ] {
        let (mut db, mut ascs) = ase_in_state(AudioRole::Sink, state);
        let ase = ascs.ase(ASE_ID).unwrap();
        let (cig_id, cis_id) = (ase.cig_id, ase.cis_id);
        assert!(
            ascs.on_cis_loss(&mut db, cig_id, cis_id).is_empty(),
            "CIS loss moved an ASE in {state:?}"
        );
        assert_eq!(ascs.ase(ASE_ID).unwrap().state, state);
    }
}

#[test]
fn test_ascs_cis_loss_only_touches_the_ase_mapped_to_that_cis() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1, 2], &[]);
    ascs.write_control_point(&mut db, &config_codec_pdu(&[1, 2]));
    // `config_qos_pdu` maps each ASE to CIG 1, CIS <ase_id> - so ASE 1 and ASE 2 sit on
    // different CISes of the same CIG.
    ascs.write_control_point(&mut db, &config_qos_pdu(&[1, 2]));
    ascs.write_control_point(&mut db, &enable_pdu(&[1, 2]));

    assert_eq!(ascs.on_cis_loss(&mut db, 1, 2), vec![2]);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Enabling);
    assert_eq!(ascs.ase(2).unwrap().state, AseState::QosConfigured);

    // A CIG that matches nothing moves nothing.
    assert!(ascs.on_cis_loss(&mut db, 9, 1).is_empty());
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Enabling);
}

#[test]
fn test_ascs_acl_loss_moves_every_live_ase_to_releasing() {
    let mut db = GattDatabase::new();
    let mut ascs = AudioStreamControlService::register(&mut db, &[1, 2, 3], &[]);
    // ASE 1 stays Idle, ASE 2 reaches Codec Configured, ASE 3 reaches Streaming.
    ascs.write_control_point(&mut db, &config_codec_pdu(&[2, 3]));
    ascs.write_control_point(&mut db, &config_qos_pdu(&[3]));
    ascs.write_control_point(&mut db, &enable_pdu(&[3]));
    ascs.write_control_point(&mut db, &id_op_pdu(opcode::RECEIVER_START_READY, &[3]));

    assert_eq!(ascs.on_acl_loss(&mut db), vec![2, 3]);
    assert_eq!(ascs.ase(1).unwrap().state, AseState::Idle);
    for ase_id in [2, 3] {
        let ase = ascs.ase(ase_id).unwrap();
        assert_eq!(ase.state, AseState::Releasing);
        assert_eq!(
            db.read(ase.value_handle, 0).unwrap(),
            &[ase_id, AseState::Releasing as u8]
        );
    }

    // ASCS 3.2 hands off to 5.9: Released is what finishes the teardown, exactly as it does
    // for a client-driven Release.
    assert_eq!(ascs.released(&mut db, false), vec![2, 3]);
    for ase_id in [1, 2, 3] {
        assert_eq!(ascs.ase(ase_id).unwrap().state, AseState::Idle);
    }
    // Idempotent: a second ACL-loss report has nothing left to move.
    assert!(ascs.on_acl_loss(&mut db).is_empty());
}

#[test]
fn test_ascs_acl_loss_does_not_restart_a_release_already_in_flight() {
    let (mut db, mut ascs) = ase_in_state(AudioRole::Source, AseState::Releasing);
    assert!(ascs.on_acl_loss(&mut db).is_empty());
    assert_eq!(ascs.ase(ASE_ID).unwrap().state, AseState::Releasing);
}
