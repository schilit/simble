// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Port of Bumble's gatt_test.py test suite.
//!
//! Tests ATT wire protocols, GATT database handle allocations, discovery procedures,
//! characteristic read/write operations, and notification generation.

use simble::att::{AttErrorRsp, AttExchangeMtuReq, AttExchangeMtuRsp, AttPdu, error_code, opcode};
use simble::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use simble::types::Uuid;
use zerocopy::IntoBytes;

#[test]
fn test_att_error_response_roundtrip() {
    let err = AttErrorRsp::new(opcode::READ_REQ, 0x0024, error_code::READ_NOT_PERMITTED);
    let bytes = err.as_bytes();

    let pdu = AttPdu::parse(bytes).expect("Parse error response");
    match pdu {
        AttPdu::ErrorRsp(r) => {
            assert_eq!(r.request_opcode, opcode::READ_REQ);
            assert_eq!(r.attribute_handle.get(), 0x0024);
            assert_eq!(r.error_code, error_code::READ_NOT_PERMITTED);
        }
        _ => panic!("Expected ErrorRsp"),
    }
}

#[test]
fn test_att_exchange_mtu_roundtrip() {
    let req = AttExchangeMtuReq::new(256);
    let bytes = req.as_bytes();

    let pdu = AttPdu::parse(bytes).expect("Parse MTU req");
    match pdu {
        AttPdu::ExchangeMtuReq(r) => assert_eq!(r.client_rx_mtu.get(), 256),
        _ => panic!("Expected ExchangeMtuReq"),
    }

    let rsp = AttExchangeMtuRsp::new(512);
    let pdu_rsp = AttPdu::parse(rsp.as_bytes()).expect("Parse MTU rsp");
    match pdu_rsp {
        AttPdu::ExchangeMtuRsp(r) => assert_eq!(r.server_rx_mtu.get(), 512),
        _ => panic!("Expected ExchangeMtuRsp"),
    }
}

#[test]
fn test_att_read_by_group_type_service_discovery() {
    let mut db = GattDatabase::new();

    // Add Generic Access Service (0x1800) and Battery Service (0x180F)
    let s1 = db.add_service(Uuid::from_u16(0x1800), true);
    let s2 = db.add_service(Uuid::from_u16(0x180F), true);

    assert_eq!(s1, 0x0001);
    assert_eq!(s2, 0x0002);

    // Read by group type: Primary Service (0x2800) across 0x0001..=0xFFFF
    let results = db.read_by_type(0x0001, 0xFFFF, Uuid::PRIMARY_SERVICE);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].0, 0x0001);
    assert_eq!(results[0].1, &[0x00, 0x18]); // 0x1800 in LE
    assert_eq!(results[1].0, 0x0002);
    assert_eq!(results[1].1, &[0x0F, 0x18]); // 0x180F in LE
}

#[test]
fn test_att_read_by_type_characteristic_discovery() {
    let mut db = GattDatabase::new();
    db.add_service(Uuid::from_u16(0x180D), true); // Heart Rate Service

    // Add Heart Rate Measurement (0x2A37)
    let (decl_h, val_h) = db.add_characteristic(
        Uuid::from_u16(0x2A37),
        CharacteristicProperties(CharacteristicProperties::NOTIFY),
        vec![0x00, 0x50],
        AttributePermissions::default(),
    );

    // Discover characteristics (UUID 0x2803)
    let results = db.read_by_type(0x0001, 0xFFFF, Uuid::CHARACTERISTIC);
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].0, decl_h);

    // Characteristic declaration format: Properties (1) + Value Handle (2) + UUID (2)
    let decl_val = results[0].1;
    assert_eq!(decl_val[0], CharacteristicProperties::NOTIFY);
    assert_eq!(u16::from_le_bytes([decl_val[1], decl_val[2]]), val_h);
    assert_eq!(u16::from_le_bytes([decl_val[3], decl_val[4]]), 0x2A37);
}

#[test]
fn test_att_find_information_descriptor_discovery() {
    let mut db = GattDatabase::new();
    db.add_service(Uuid::from_u16(0x180D), true);
    let (_, val_h) = db.add_characteristic(
        Uuid::from_u16(0x2A37),
        CharacteristicProperties(CharacteristicProperties::NOTIFY),
        vec![0x00, 0x50],
        AttributePermissions::default(),
    );
    let cccd_h = db.add_cccd();

    let info = db.find_information(val_h, 0xFFFF);
    assert_eq!(info.len(), 2);
    assert_eq!(info[0], (val_h, Uuid::from_u16(0x2A37)));
    assert_eq!(info[1], (cccd_h, Uuid::CCCD));
}

#[test]
fn test_gatt_permissions_enforcement() {
    let mut db = GattDatabase::new();

    // Create write-only attribute
    let write_only_h = db.add_attribute(
        Uuid::from_u16(0x2A00),
        AttributePermissions {
            read: false,
            write: true,
            read_auth: false,
            write_auth: false,
            encrypt_read: false,
            encrypt_write: false,
        },
        vec![0x01],
    );

    // Read should fail
    assert_eq!(
        db.read(write_only_h, 0),
        Err(error_code::READ_NOT_PERMITTED)
    );

    // Write should succeed
    assert_eq!(db.write(write_only_h, &[0x02]), Ok(()));

    // Non-existent handle
    assert_eq!(db.read(0x9999, 0), Err(error_code::INVALID_HANDLE));
}

#[test]
fn test_att_read_blob_long_attribute() {
    let mut db = GattDatabase::new();
    let long_payload = vec![0x42; 100];
    let (_, val_h) = db.add_characteristic(
        Uuid::from_u16(0x2A00),
        CharacteristicProperties(CharacteristicProperties::READ),
        long_payload.clone(),
        AttributePermissions::default(),
    );

    // Read initial slice from offset 0
    let chunk0 = db.read(val_h, 0).unwrap();
    assert_eq!(chunk0.len(), 100);

    // Read with offset 30
    let chunk30 = db.read(val_h, 30).unwrap();
    assert_eq!(chunk30.len(), 70);
    assert_eq!(chunk30, &long_payload[30..]);

    // Offset out of range
    assert_eq!(db.read(val_h, 150), Err(error_code::INVALID_OFFSET));
}

#[test]
fn test_client_server_gatt_discovery_and_interaction() {
    use simble::l2cap::L2capHeader;
    use simble::{Address, AddressType, GattClient, VirtualDevice};

    // 1. Setup Server (VirtualDevice)
    let server_addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    let mut server = VirtualDevice::new("HRM_Server", server_addr, AddressType::Random);
    let svc_h = server.gatt_db.add_service(Uuid::from_u16(0x180D), true);
    let (decl_h, val_h) = server.gatt_db.add_characteristic(
        Uuid::from_u16(0x2A37),
        CharacteristicProperties(CharacteristicProperties::READ | CharacteristicProperties::WRITE),
        vec![75],
        AttributePermissions::default(),
    );

    assert_eq!(svc_h, 1);
    assert_eq!(decl_h, 2);
    assert_eq!(val_h, 3);

    // 2. Setup Client
    let client_addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let mut client = GattClient::new(0x0001, server_addr);
    server.on_connected(0x0001, client_addr);

    // 3. Client Discovers Services
    let req = client.create_discover_services_request(0x0001, 0xFFFF);
    let resp = server.process_l2cap_packet(0x0001, &req).unwrap().unwrap();
    let (_, payload) = L2capHeader::parse(&resp).unwrap();
    client.on_discover_services_response(payload).unwrap();
    assert_eq!(client.services.len(), 1);
    assert_eq!(client.services[0].uuid, Uuid::from_u16(0x180D));

    // 4. Client Discovers Characteristics
    let req = client.create_discover_characteristics_request(0x0001, 0xFFFF);
    let resp = server.process_l2cap_packet(0x0001, &req).unwrap().unwrap();
    let (_, payload) = L2capHeader::parse(&resp).unwrap();
    client
        .on_discover_characteristics_response(Uuid::from_u16(0x180D), payload)
        .unwrap();

    let ch = client
        .find_characteristic(Uuid::from_u16(0x2A37))
        .expect("Found HRM");
    assert_eq!(ch.value_handle, val_h);

    // 5. Client Writes Value
    let write_req = client.create_write_request(ch.value_handle, &[82]);
    let write_resp = server
        .process_l2cap_packet(0x0001, &write_req)
        .unwrap()
        .unwrap();
    let (_, write_payload) = L2capHeader::parse(&write_resp).unwrap();
    assert_eq!(write_payload[0], opcode::WRITE_RSP);

    // 6. Client Reads Value
    let read_req = client.create_read_request(ch.value_handle);
    let read_resp = server
        .process_l2cap_packet(0x0001, &read_req)
        .unwrap()
        .unwrap();
    let (_, read_payload) = L2capHeader::parse(&read_resp).unwrap();
    assert_eq!(read_payload[0], opcode::READ_RSP);
    assert_eq!(read_payload[1], 82);
}
