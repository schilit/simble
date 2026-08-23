// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Volume Control Service (VCS) tests: the Volume Control Point state machine (VCS Section
//! 3.3) — relative and absolute volume, mute/unmute, saturation at the ends of the range,
//! Change_Counter rejection of stale writes, and the Volume_Setting_Persisted flag — plus
//! the raw-ATT path that proves a peer's write reaches the handler rather than overwriting
//! the control point's bytes.

use simble::VirtualDevice;
use simble::att::{error_code as att_error_code, opcode as att_opcode};
use simble::gatt::GattDatabase;
use simble::l2cap::{L2capHeader, cid};
use simble::profiles::vcp::{
    DEFAULT_VOLUME_STEP_SIZE, MUTE_MUTED, MUTE_NOT_MUTED, VOLUME_SETTING_PERSISTED,
    VolumeControlService, error_code, opcode,
};
use simble::types::{Address, AddressType};

fn register(db: &mut GattDatabase) -> VolumeControlService {
    VolumeControlService::register(db, 128, MUTE_NOT_MUTED)
}

#[test]
fn test_init_service_state() {
    let mut db = GattDatabase::new();
    let vcs = register(&mut db);

    assert_eq!(
        db.read(vcs.volume_state_value_handle, 0).unwrap(),
        &[128, 0, 0]
    );
    assert_eq!(db.read(vcs.volume_flags_value_handle, 0).unwrap(), &[0x00]);
}

#[test]
fn test_unsupported_opcode_is_rejected() {
    let mut db = GattDatabase::new();
    let mut vcs = register(&mut db);
    assert_eq!(
        vcs.write_control_point(&mut db, &[0x07, 0]),
        Err(error_code::OPCODE_NOT_SUPPORTED)
    );
}

#[test]
fn test_wrong_length_is_rejected() {
    let mut db = GattDatabase::new();
    let mut vcs = register(&mut db);
    // Every opcode but Set Absolute Volume is exactly [opcode, change_counter].
    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::MUTE]),
        Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)
    );
    // Set Absolute Volume needs its Volume_Setting operand.
    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::SET_ABSOLUTE_VOLUME, 0]),
        Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)
    );
}

#[test]
fn test_wrong_change_counter_is_rejected() {
    let mut db = GattDatabase::new();
    let mut vcs = register(&mut db);

    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::MUTE, 1]),
        Err(error_code::INVALID_CHANGE_COUNTER)
    );
    // Rejected means nothing moved, counter included.
    assert_eq!(vcs.volume_state().change_counter, 0);
    assert_eq!(vcs.volume_state().mute, MUTE_NOT_MUTED);
}

#[test]
fn test_set_absolute_volume() {
    let mut db = GattDatabase::new();
    let mut vcs = register(&mut db);

    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::SET_ABSOLUTE_VOLUME, 0, 200]),
        Ok(())
    );
    assert_eq!(vcs.volume_state().volume_setting, 200);
    assert_eq!(vcs.volume_state().change_counter, 1);
    // The state characteristic is republished, so a subscribed client sees it.
    assert_eq!(
        db.read(vcs.volume_state_value_handle, 0).unwrap(),
        &[200, 0, 1]
    );
}

#[test]
fn test_relative_volume_up_and_down() {
    let mut db = GattDatabase::new();
    let mut vcs = register(&mut db);

    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::RELATIVE_VOLUME_UP, 0]),
        Ok(())
    );
    assert_eq!(
        vcs.volume_state().volume_setting,
        128 + DEFAULT_VOLUME_STEP_SIZE
    );
    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::RELATIVE_VOLUME_DOWN, 1]),
        Ok(())
    );
    assert_eq!(vcs.volume_state().volume_setting, 128);
    assert_eq!(vcs.volume_state().change_counter, 2);
}

#[test]
fn test_relative_volume_saturates_at_both_ends() {
    let mut db = GattDatabase::new();
    let mut vcs = VolumeControlService::register(&mut db, 250, MUTE_NOT_MUTED);

    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::RELATIVE_VOLUME_UP, 0]),
        Ok(())
    );
    assert_eq!(vcs.volume_state().volume_setting, 255);

    let mut db = GattDatabase::new();
    let mut vcs = VolumeControlService::register(&mut db, 5, MUTE_NOT_MUTED);
    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::RELATIVE_VOLUME_DOWN, 0]),
        Ok(())
    );
    assert_eq!(vcs.volume_state().volume_setting, 0);
}

// VCS 3.3.1: an operation that leaves Volume Setting and Mute alone is not an error, but it
// must not advance the change counter -- otherwise a client that muted an already-muted
// device would silently invalidate every other client's counter.
#[test]
fn test_no_op_does_not_advance_the_change_counter() {
    let mut db = GattDatabase::new();
    let mut vcs = VolumeControlService::register(&mut db, 0, MUTE_MUTED);

    assert_eq!(vcs.write_control_point(&mut db, &[opcode::MUTE, 0]), Ok(()));
    assert_eq!(vcs.volume_state().change_counter, 0);

    // Already at 0, so Relative Volume Down changes nothing either.
    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::RELATIVE_VOLUME_DOWN, 0]),
        Ok(())
    );
    assert_eq!(vcs.volume_state().change_counter, 0);
}

#[test]
fn test_mute_and_unmute() {
    let mut db = GattDatabase::new();
    let mut vcs = register(&mut db);

    assert_eq!(vcs.write_control_point(&mut db, &[opcode::MUTE, 0]), Ok(()));
    assert_eq!(vcs.volume_state().mute, MUTE_MUTED);
    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::UNMUTE, 1]),
        Ok(())
    );
    assert_eq!(vcs.volume_state().mute, MUTE_NOT_MUTED);
    assert_eq!(vcs.volume_state().change_counter, 2);
}

#[test]
fn test_unmute_relative_volume_up_does_both() {
    let mut db = GattDatabase::new();
    let mut vcs = VolumeControlService::register(&mut db, 100, MUTE_MUTED);

    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::UNMUTE_RELATIVE_VOLUME_UP, 0]),
        Ok(())
    );
    let state = vcs.volume_state();
    assert_eq!(state.mute, MUTE_NOT_MUTED);
    assert_eq!(state.volume_setting, 100 + DEFAULT_VOLUME_STEP_SIZE);
    // One operation, one counter increment, even though two fields moved.
    assert_eq!(state.change_counter, 1);
}

// VCS 3.2.1.1 - the flag distinguishes a user-chosen volume from the power-on default, so it
// flips on the first client-driven change and not before.
#[test]
fn test_volume_setting_persisted_flag() {
    let mut db = GattDatabase::new();
    let mut vcs = register(&mut db);

    assert_eq!(db.read(vcs.volume_flags_value_handle, 0).unwrap(), &[0x00]);
    assert_eq!(
        vcs.write_control_point(&mut db, &[opcode::SET_ABSOLUTE_VOLUME, 0, 64]),
        Ok(())
    );
    assert_eq!(
        db.read(vcs.volume_flags_value_handle, 0).unwrap(),
        &[VOLUME_SETTING_PERSISTED]
    );
}

// The defect this handler exists for: before `register()` attached an AttributeHandler, a
// peer's ATT Write Request to the Volume Control Point overwrote the control point's stored
// bytes and the Volume State never moved. This drives a real Write Request through
// `VirtualDevice` the way a connected central would.
#[test]
fn test_control_point_dispatch_through_att_write() {
    let addr = Address::from_be_bytes([0xC0, 0xFF, 0xEE, 0x01, 0x02, 0x03]);
    let mut dev = VirtualDevice::new("Speaker", addr, AddressType::Random);
    let vcs = VolumeControlService::register(&mut dev.gatt_db, 128, MUTE_NOT_MUTED);

    let conn_h = 0x0040;
    dev.on_connected(conn_h, Address::ANY);

    let mut write_req = vec![att_opcode::WRITE_REQ];
    write_req.extend_from_slice(&vcs.control_point_value_handle.to_le_bytes());
    write_req.push(opcode::SET_ABSOLUTE_VOLUME);
    write_req.push(0); // change counter
    write_req.push(42); // volume setting
    let l2cap = L2capHeader::serialize(cid::ATT, &write_req);
    let resp = dev.process_l2cap_packet(conn_h, &l2cap).unwrap().unwrap();
    let (_, payload) = L2capHeader::parse(&resp).unwrap();
    assert_eq!(payload[0], att_opcode::WRITE_RSP);

    assert_eq!(vcs.volume_state().volume_setting, 42);
    assert_eq!(
        dev.gatt_db.read(vcs.volume_state_value_handle, 0).unwrap(),
        &[42, 0, 1]
    );
}
