// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Port of Bumble's device_test.py / host_test.py test suite.
//!
//! Tests virtual device lifecycle, multi-device connections, MTU negotiations,
//! GATT read/write operations, and notification generation over L2CAP.

use simble::VirtualDevice;
use simble::att::opcode;
use simble::gatt::{AttributePermissions, CharacteristicProperties};
use simble::l2cap::{L2capHeader, cid};
use simble::types::{Address, AddressType, Uuid};

#[test]
fn test_device_lifecycle_and_connection_management() {
    let dev_addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x11, 0x22, 0x33]);
    let mut dev = VirtualDevice::new("VirtualHeartRate", dev_addr, AddressType::Random);

    assert_eq!(dev.name, "VirtualHeartRate");
    assert_eq!(dev.connections.len(), 0);

    // 1. Central connects
    let peer_addr1 = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    dev.on_connected(0x0001, peer_addr1);
    assert_eq!(dev.connections.len(), 1);
    assert_eq!(dev.connections.get(&0x0001).unwrap().mtu, 23);

    // 2. Second Central connects
    let peer_addr2 = Address::from_be_bytes([0x22, 0x33, 0x44, 0x55, 0x66, 0x77]);
    dev.on_connected(0x0002, peer_addr2);
    assert_eq!(dev.connections.len(), 2);

    // 3. Central 1 disconnects
    dev.on_disconnected(0x0001);
    assert_eq!(dev.connections.len(), 1);
    assert!(!dev.connections.contains_key(&0x0001));
    assert!(dev.connections.contains_key(&0x0002));
}

#[test]
fn test_device_end_to_end_gatt_interaction() {
    let dev_addr = Address::from_be_bytes([0xF0, 0xDE, 0xF1, 0x00, 0x11, 0x22]);
    let mut dev = VirtualDevice::new("SensorNode", dev_addr, AddressType::Random);

    // Add Battery Service (0x180F) with Battery Level Characteristic (0x2A19)
    let svc_h = dev.gatt_db.add_service(Uuid::from_u16(0x180F), true);
    let (decl_h, val_h) = dev.gatt_db.add_characteristic(
        Uuid::from_u16(0x2A19),
        CharacteristicProperties(CharacteristicProperties::READ | CharacteristicProperties::NOTIFY),
        vec![98], // 98%
        AttributePermissions::default(),
    );
    let cccd_h = dev.gatt_db.add_cccd();

    assert_eq!(svc_h, 0x0001);
    assert_eq!(decl_h, 0x0002);
    assert_eq!(val_h, 0x0003);
    assert_eq!(cccd_h, 0x0004);

    let conn_h = 0x0010;
    dev.on_connected(conn_h, Address::ANY);

    // 1. Client sends Exchange MTU Request (MTU = 256)
    let mtu_req = [opcode::EXCHANGE_MTU_REQ, 0x00, 0x01];
    let mtu_l2cap = L2capHeader::serialize(cid::ATT, &mtu_req);
    let mtu_resp = dev
        .process_l2cap_packet(conn_h, &mtu_l2cap)
        .unwrap()
        .unwrap();
    let (_, mtu_payload) = L2capHeader::parse(&mtu_resp).unwrap();
    assert_eq!(mtu_payload[0], opcode::EXCHANGE_MTU_RSP);
    assert_eq!(dev.connections.get(&conn_h).unwrap().mtu, 256);

    // 2. Client Reads Battery Level
    let mut read_req = vec![opcode::READ_REQ];
    read_req.extend_from_slice(&val_h.to_le_bytes());
    let read_l2cap = L2capHeader::serialize(cid::ATT, &read_req);
    let read_resp = dev
        .process_l2cap_packet(conn_h, &read_l2cap)
        .unwrap()
        .unwrap();
    let (_, read_payload) = L2capHeader::parse(&read_resp).unwrap();
    assert_eq!(read_payload[0], opcode::READ_RSP);
    assert_eq!(read_payload[1], 98);

    // 3. Client enables notifications by writing CCCD [0x01, 0x00]
    let mut write_req = vec![opcode::WRITE_REQ];
    write_req.extend_from_slice(&cccd_h.to_le_bytes());
    write_req.extend_from_slice(&[0x01, 0x00]);
    let write_l2cap = L2capHeader::serialize(cid::ATT, &write_req);
    let write_resp = dev
        .process_l2cap_packet(conn_h, &write_l2cap)
        .unwrap()
        .unwrap();
    let (_, write_payload) = L2capHeader::parse(&write_resp).unwrap();
    assert_eq!(write_payload[0], opcode::WRITE_RSP);

    // 4. Device updates battery level to 95% and emits notification
    dev.gatt_db.write(val_h, &[95]).unwrap();
    let notif_l2cap = dev.create_notification(val_h, &[95]);
    let (_, notif_payload) = L2capHeader::parse(&notif_l2cap).unwrap();
    assert_eq!(notif_payload[0], opcode::HANDLE_VALUE_NTF);
    assert_eq!(
        u16::from_le_bytes([notif_payload[1], notif_payload[2]]),
        val_h
    );
    assert_eq!(notif_payload[3], 95);
}
