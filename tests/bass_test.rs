// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Broadcast Audio Scan Service (BASS) tests: Control Point operation and Broadcast
//! Receive State wire round-trips, plus Scan Delegator semantics driven through GATT
//! writes to the Broadcast Audio Scan Control Point.

use simble::att::error_code as att_error_code;
use simble::gatt::GattDatabase;
use simble::profiles::bass::{
    ANY_BIS, BigEncryption, BroadcastAudioScanService, BroadcastReceiveState,
    ControlPointOperation, PeriodicAdvertisingSyncParams, PeriodicAdvertisingSyncState,
    SubgroupInfo, error_code, opcode,
};
use simble::types::Address;

fn advertiser() -> Address {
    Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF])
}

fn check_operation_round_trip(operation: &ControlPointOperation) {
    let serialized = operation.to_bytes();
    let parsed = ControlPointOperation::parse(&serialized).expect("operation parses");
    assert_eq!(&parsed, operation);
    assert_eq!(parsed.to_bytes(), serialized);
}

#[test]
fn test_scan_operations_round_trip() {
    check_operation_round_trip(&ControlPointOperation::RemoteScanStopped);
    check_operation_round_trip(&ControlPointOperation::RemoteScanStarted);
}

#[test]
fn test_add_source_operation_round_trip() {
    check_operation_round_trip(&ControlPointOperation::AddSource {
        advertiser_address_type: 0x01,
        advertiser_address: advertiser(),
        advertising_sid: 34,
        broadcast_id: 123456,
        pa_sync: PeriodicAdvertisingSyncParams::SynchronizeToPaPastNotAvailable,
        pa_interval: 456,
        subgroups: vec![],
    });

    check_operation_round_trip(&ControlPointOperation::AddSource {
        advertiser_address_type: 0x01,
        advertiser_address: advertiser(),
        advertising_sid: 34,
        broadcast_id: 123456,
        pa_sync: PeriodicAdvertisingSyncParams::SynchronizeToPaPastNotAvailable,
        pa_interval: 456,
        subgroups: vec![
            SubgroupInfo {
                bis_sync: 6677,
                metadata: vec![0xAA, 0xBB, 0xCC],
            },
            SubgroupInfo {
                bis_sync: 8899,
                metadata: vec![0xDD, 0xEE, 0xFF],
            },
        ],
    });
}

#[test]
fn test_modify_source_operation_round_trip() {
    check_operation_round_trip(&ControlPointOperation::ModifySource {
        source_id: 12,
        pa_sync: PeriodicAdvertisingSyncParams::SynchronizeToPaPastNotAvailable,
        pa_interval: 567,
        subgroups: vec![],
    });

    check_operation_round_trip(&ControlPointOperation::ModifySource {
        source_id: 12,
        pa_sync: PeriodicAdvertisingSyncParams::SynchronizeToPaPastNotAvailable,
        pa_interval: 567,
        subgroups: vec![
            SubgroupInfo {
                bis_sync: 6677,
                metadata: vec![0x11, 0x22, 0x33],
            },
            SubgroupInfo {
                bis_sync: 8899,
                metadata: vec![0x45, 0x67],
            },
        ],
    });
}

#[test]
fn test_set_broadcast_code_and_remove_source_round_trip() {
    check_operation_round_trip(&ControlPointOperation::SetBroadcastCode {
        source_id: 7,
        broadcast_code: [
            0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD,
            0xAE, 0xAF,
        ],
    });
    check_operation_round_trip(&ControlPointOperation::RemoveSource { source_id: 7 });
}

fn check_receive_state_round_trip(state: &BroadcastReceiveState) {
    let serialized = state.to_bytes();
    let parsed = BroadcastReceiveState::parse(&serialized).expect("receive state parses");
    assert_eq!(&parsed, state);
    assert_eq!(parsed.to_bytes(), serialized);
}

#[test]
fn test_broadcast_receive_state_round_trip() {
    let subgroups = vec![
        SubgroupInfo {
            bis_sync: 6677,
            metadata: vec![0x11, 0x22, 0x33],
        },
        SubgroupInfo {
            bis_sync: 8899,
            metadata: vec![0x45, 0x67],
        },
    ];

    check_receive_state_round_trip(&BroadcastReceiveState {
        source_id: 12,
        source_address_type: 0x00,
        source_address: advertiser(),
        source_adv_sid: 123,
        broadcast_id: 123456,
        pa_sync_state: PeriodicAdvertisingSyncState::SynchronizedToPa,
        big_encryption: BigEncryption::Decrypting,
        bad_code: vec![],
        subgroups: subgroups.clone(),
    });

    // With BIG_Encryption == BadCode the 16-byte Bad_Code field precedes the subgroups.
    check_receive_state_round_trip(&BroadcastReceiveState {
        source_id: 12,
        source_address_type: 0x00,
        source_address: advertiser(),
        source_adv_sid: 123,
        broadcast_id: 123456,
        pa_sync_state: PeriodicAdvertisingSyncState::SynchronizedToPa,
        big_encryption: BigEncryption::BadCode,
        bad_code: vec![
            0xA0, 0xA1, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xAB, 0xAC, 0xAD,
            0xAE, 0xAF,
        ],
        subgroups,
    });
}

fn add_source_pdu(pa_sync: PeriodicAdvertisingSyncParams, bis_sync: u32) -> Vec<u8> {
    ControlPointOperation::AddSource {
        advertiser_address_type: 0x01,
        advertiser_address: advertiser(),
        advertising_sid: 3,
        broadcast_id: 0x123456,
        pa_sync,
        pa_interval: 0x0640,
        subgroups: vec![SubgroupInfo {
            bis_sync,
            metadata: vec![0x03, 0x02, 0x04, 0x00],
        }],
    }
    .to_bytes()
}

#[test]
fn test_remote_scan_started_and_stopped() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);
    assert!(!bass.assistant_scanning());

    db.write(
        bass.control_point_value_handle,
        &[opcode::REMOTE_SCAN_STARTED],
    )
    .unwrap();
    assert!(bass.assistant_scanning());

    db.write(
        bass.control_point_value_handle,
        &[opcode::REMOTE_SCAN_STOPPED],
    )
    .unwrap();
    assert!(!bass.assistant_scanning());
}

#[test]
fn test_add_source_publishes_receive_state() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);

    // Empty value until a source is added.
    assert!(
        db.read(bass.receive_state_value_handles[0], 0)
            .unwrap()
            .is_empty()
    );

    db.write(
        bass.control_point_value_handle,
        &add_source_pdu(
            PeriodicAdvertisingSyncParams::SynchronizeToPaPastAvailable,
            0x01,
        ),
    )
    .unwrap();

    let published = db.read(bass.receive_state_value_handles[0], 0).unwrap();
    let state = BroadcastReceiveState::parse(published).expect("published state parses");
    assert_eq!(state, bass.receive_state(0).unwrap());
    assert_eq!(state.broadcast_id, 0x123456);
    assert_eq!(state.source_address, advertiser());
    // The simulator reports a requested sync as immediately achieved.
    assert_eq!(
        state.pa_sync_state,
        PeriodicAdvertisingSyncState::SynchronizedToPa
    );
    assert_eq!(state.big_encryption, BigEncryption::NotEncrypted);
    assert_eq!(state.subgroups[0].bis_sync, 0x01);
}

#[test]
fn test_add_source_without_pa_sync_stays_unsynchronized() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);

    db.write(
        bass.control_point_value_handle,
        &add_source_pdu(PeriodicAdvertisingSyncParams::DoNotSynchronizeToPa, ANY_BIS),
    )
    .unwrap();

    let state = bass.receive_state(0).unwrap();
    assert_eq!(
        state.pa_sync_state,
        PeriodicAdvertisingSyncState::NotSynchronizedToPa
    );
    // No PA sync means no BIS sync either.
    assert_eq!(state.subgroups[0].bis_sync, 0);
}

#[test]
fn test_add_source_with_all_slots_full_is_rejected() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);

    let pdu = add_source_pdu(
        PeriodicAdvertisingSyncParams::SynchronizeToPaPastAvailable,
        0x01,
    );
    db.write(bass.control_point_value_handle, &pdu).unwrap();
    assert_eq!(
        db.write(bass.control_point_value_handle, &pdu),
        Err(att_error_code::INSUFFICIENT_RESOURCES)
    );
}

#[test]
fn test_modify_source_updates_sync_and_subgroups() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);

    db.write(
        bass.control_point_value_handle,
        &add_source_pdu(
            PeriodicAdvertisingSyncParams::SynchronizeToPaPastAvailable,
            0x01,
        ),
    )
    .unwrap();
    let source_id = bass.receive_state(0).unwrap().source_id;

    db.write(
        bass.control_point_value_handle,
        &ControlPointOperation::ModifySource {
            source_id,
            pa_sync: PeriodicAdvertisingSyncParams::DoNotSynchronizeToPa,
            pa_interval: 0x0640,
            subgroups: vec![SubgroupInfo {
                bis_sync: 0,
                metadata: vec![],
            }],
        }
        .to_bytes(),
    )
    .unwrap();

    let state = bass.receive_state(0).unwrap();
    assert_eq!(
        state.pa_sync_state,
        PeriodicAdvertisingSyncState::NotSynchronizedToPa
    );
    assert!(state.subgroups[0].metadata.is_empty());
}

#[test]
fn test_remove_source_clears_receive_state() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);

    db.write(
        bass.control_point_value_handle,
        &add_source_pdu(
            PeriodicAdvertisingSyncParams::SynchronizeToPaPastAvailable,
            0x01,
        ),
    )
    .unwrap();
    let source_id = bass.receive_state(0).unwrap().source_id;

    db.write(
        bass.control_point_value_handle,
        &ControlPointOperation::RemoveSource { source_id }.to_bytes(),
    )
    .unwrap();

    assert!(bass.receive_state(0).is_none());
    assert!(
        db.read(bass.receive_state_value_handles[0], 0)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn test_set_broadcast_code_starts_decrypting() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);

    db.write(
        bass.control_point_value_handle,
        &add_source_pdu(
            PeriodicAdvertisingSyncParams::SynchronizeToPaPastAvailable,
            0x01,
        ),
    )
    .unwrap();
    let source_id = bass.receive_state(0).unwrap().source_id;

    bass.require_broadcast_code(&mut db, source_id).unwrap();
    assert_eq!(
        bass.receive_state(0).unwrap().big_encryption,
        BigEncryption::BroadcastCodeRequired
    );

    db.write(
        bass.control_point_value_handle,
        &ControlPointOperation::SetBroadcastCode {
            source_id,
            broadcast_code: [0x42; 16],
        }
        .to_bytes(),
    )
    .unwrap();

    let state = bass.receive_state(0).unwrap();
    assert_eq!(state.big_encryption, BigEncryption::Decrypting);
    // The published characteristic value tracks the transition.
    let published = db.read(bass.receive_state_value_handles[0], 0).unwrap();
    assert_eq!(BroadcastReceiveState::parse(published).unwrap(), state);
}

#[test]
fn test_operations_on_unknown_source_id_are_rejected() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);

    for operation in [
        ControlPointOperation::ModifySource {
            source_id: 99,
            pa_sync: PeriodicAdvertisingSyncParams::DoNotSynchronizeToPa,
            pa_interval: 0,
            subgroups: vec![],
        },
        ControlPointOperation::SetBroadcastCode {
            source_id: 99,
            broadcast_code: [0; 16],
        },
        ControlPointOperation::RemoveSource { source_id: 99 },
    ] {
        assert_eq!(
            db.write(bass.control_point_value_handle, &operation.to_bytes()),
            Err(error_code::INVALID_SOURCE_ID)
        );
    }
}

#[test]
fn test_unknown_opcode_is_rejected() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 1);

    assert_eq!(
        db.write(bass.control_point_value_handle, &[0xFF]),
        Err(error_code::OPCODE_NOT_SUPPORTED)
    );
    assert_eq!(
        db.write(bass.control_point_value_handle, &[]),
        Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)
    );
}

#[test]
fn test_multiple_receive_state_slots_get_distinct_source_ids() {
    let mut db = GattDatabase::new();
    let bass = BroadcastAudioScanService::register(&mut db, 2);

    let pdu = add_source_pdu(
        PeriodicAdvertisingSyncParams::SynchronizeToPaPastAvailable,
        0x01,
    );
    db.write(bass.control_point_value_handle, &pdu).unwrap();
    db.write(bass.control_point_value_handle, &pdu).unwrap();

    let first = bass.receive_state(0).unwrap();
    let second = bass.receive_state(1).unwrap();
    assert_ne!(first.source_id, second.source_id);
}
