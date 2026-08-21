// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Tests for the Android-Bluetooth-API-shaped ergonomic layer: building a
//! peripheral via `BluetoothGattServer`/`BluetoothGattService`, and
//! confirming `BluetoothGattServerCallback` methods fire for real read/write/
//! connection-state events driven through the underlying `VirtualDevice`.

use std::sync::{Arc, Mutex};

use simble::android::device::BluetoothDevice;
use simble::android::gatt_server::{
    BluetoothGattServer, BluetoothGattServerCallback, BluetoothProfile,
};
use simble::android::gatt_service::{
    BluetoothGatt, BluetoothGattCharacteristic, BluetoothGattDescriptor, BluetoothGattService,
};
use simble::att::opcode;
use simble::l2cap::{L2capHeader, cid};
use simble::types::Uuid;
use simble::{Address, AddressType, VirtualDevice};

#[derive(Debug, Clone, PartialEq)]
enum RecordedEvent {
    ConnectionState {
        status: i32,
        new_state: i32,
    },
    ServiceAdded {
        status: i32,
    },
    CharacteristicRead {
        request_id: i32,
        offset: i32,
        uuid: Uuid,
    },
    CharacteristicWrite {
        uuid: Uuid,
        value: Vec<u8>,
        response_needed: bool,
    },
    DescriptorRead {
        uuid: Uuid,
    },
    MtuChanged {
        mtu: i32,
    },
    NotificationSent {
        status: i32,
    },
}

#[derive(Default)]
struct RecordingCallback {
    events: Arc<Mutex<Vec<RecordedEvent>>>,
}

impl BluetoothGattServerCallback for RecordingCallback {
    fn on_connection_state_change(
        &mut self,
        _device: &BluetoothDevice,
        status: i32,
        new_state: i32,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedEvent::ConnectionState { status, new_state });
    }

    fn on_service_added(&mut self, status: i32, _service: &BluetoothGattService) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedEvent::ServiceAdded { status });
    }

    fn on_characteristic_read_request(
        &mut self,
        _device: &BluetoothDevice,
        request_id: i32,
        offset: i32,
        characteristic: &BluetoothGattCharacteristic,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedEvent::CharacteristicRead {
                request_id,
                offset,
                uuid: characteristic.uuid,
            });
    }

    fn on_characteristic_write_request(
        &mut self,
        _device: &BluetoothDevice,
        _request_id: i32,
        characteristic: &BluetoothGattCharacteristic,
        _prepared_write: bool,
        response_needed: bool,
        _offset: i32,
        value: &[u8],
    ) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedEvent::CharacteristicWrite {
                uuid: characteristic.uuid,
                value: value.to_vec(),
                response_needed,
            });
    }

    fn on_descriptor_read_request(
        &mut self,
        _device: &BluetoothDevice,
        _request_id: i32,
        _offset: i32,
        descriptor: &BluetoothGattDescriptor,
    ) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedEvent::DescriptorRead {
                uuid: descriptor.uuid,
            });
    }

    fn on_mtu_changed(&mut self, _device: &BluetoothDevice, mtu: i32) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedEvent::MtuChanged { mtu });
    }

    fn on_notification_sent(&mut self, _device: &BluetoothDevice, status: i32) {
        self.events
            .lock()
            .unwrap()
            .push(RecordedEvent::NotificationSent { status });
    }
}

/// Registers a Heart Rate service (0x180D) with a single Heart Rate
/// Measurement characteristic (0x2A37, read+write) and returns the server
/// plus the shared event log and the characteristic's real attribute handle
/// (found via the underlying `GattDatabase`, since `add_service` consumes
/// the `BluetoothGattService` builder passed into it).
fn build_server_with_characteristic() -> (BluetoothGattServer, Arc<Mutex<Vec<RecordedEvent>>>, u16)
{
    let events = Arc::<Mutex<Vec<RecordedEvent>>>::default();
    let callback = RecordingCallback {
        events: events.clone(),
    };

    let addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    let device = VirtualDevice::new("HRM", addr, AddressType::Random);
    let mut server = BluetoothGattServer::new(device, Box::new(callback));

    let mut service = BluetoothGattService::new(
        Uuid::from_u16(0x180D),
        BluetoothGattService::SERVICE_TYPE_PRIMARY,
    );
    let mut characteristic = BluetoothGattCharacteristic::new(
        Uuid::from_u16(0x2A37),
        BluetoothGattCharacteristic::PROPERTY_READ | BluetoothGattCharacteristic::PROPERTY_WRITE,
        BluetoothGattCharacteristic::PERMISSION_READ
            | BluetoothGattCharacteristic::PERMISSION_WRITE,
    );
    characteristic.set_value(vec![75u8]);
    service.add_characteristic(characteristic);

    assert!(server.add_service(service));

    let registered = server
        .get_service(Uuid::from_u16(0x180D))
        .expect("service registered");
    let value_handle = registered.characteristics[0]
        .value_handle
        .expect("characteristic handle assigned");

    (server, events, value_handle)
}

#[test]
fn test_add_service_fires_on_service_added() {
    let (_server, events, _handle) = build_server_with_characteristic();
    assert_eq!(
        events.lock().unwrap().first(),
        Some(&RecordedEvent::ServiceAdded {
            status: BluetoothGatt::GATT_SUCCESS
        })
    );
}

#[test]
fn test_connect_and_disconnect_fire_connection_state_change() {
    let (mut server, events, _handle) = build_server_with_characteristic();
    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;

    server.device.on_connected(conn_handle, peer);
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&RecordedEvent::ConnectionState {
                status: BluetoothGatt::GATT_SUCCESS,
                new_state: BluetoothProfile::STATE_CONNECTED,
            })
    );

    server.device.on_disconnected(conn_handle);
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&RecordedEvent::ConnectionState {
                status: BluetoothGatt::GATT_SUCCESS,
                new_state: BluetoothProfile::STATE_DISCONNECTED,
            })
    );
}

#[test]
fn test_real_att_read_fires_characteristic_read_request() {
    let (mut server, events, value_handle) = build_server_with_characteristic();
    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;
    server.device.on_connected(conn_handle, peer);

    let mut read_req = vec![opcode::READ_REQ];
    read_req.extend_from_slice(&value_handle.to_le_bytes());
    let l2cap_read = L2capHeader::serialize(cid::ATT, &read_req);

    let resp = server
        .device
        .process_l2cap_packet(conn_handle, &l2cap_read)
        .unwrap()
        .unwrap();
    let (_, resp_payload) = L2capHeader::parse(&resp).unwrap();
    assert_eq!(resp_payload[0], opcode::READ_RSP);
    assert_eq!(&resp_payload[1..], &[75u8]);

    assert!(
        events
            .lock()
            .unwrap()
            .contains(&RecordedEvent::CharacteristicRead {
                request_id: 0,
                offset: 0,
                uuid: Uuid::from_u16(0x2A37),
            })
    );
}

#[test]
fn test_real_att_write_fires_characteristic_write_request() {
    let (mut server, events, value_handle) = build_server_with_characteristic();
    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;
    server.device.on_connected(conn_handle, peer);

    let mut write_req = vec![opcode::WRITE_REQ];
    write_req.extend_from_slice(&value_handle.to_le_bytes());
    write_req.extend_from_slice(&[82u8]);
    let l2cap_write = L2capHeader::serialize(cid::ATT, &write_req);

    let resp = server
        .device
        .process_l2cap_packet(conn_handle, &l2cap_write)
        .unwrap()
        .unwrap();
    let (_, resp_payload) = L2capHeader::parse(&resp).unwrap();
    assert_eq!(resp_payload[0], opcode::WRITE_RSP);

    assert!(
        events
            .lock()
            .unwrap()
            .contains(&RecordedEvent::CharacteristicWrite {
                uuid: Uuid::from_u16(0x2A37),
                value: vec![82u8],
                response_needed: true,
            })
    );

    // The underlying GattDatabase (not just the callback) actually applied
    // the write — proving this isn't just a cosmetic parallel path.
    assert_eq!(
        server.device.gatt_db.read(value_handle, 0).unwrap(),
        &[82u8]
    );
}

#[test]
fn test_write_cmd_reports_response_not_needed() {
    let (mut server, events, value_handle) = build_server_with_characteristic();
    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;
    server.device.on_connected(conn_handle, peer);

    let mut write_cmd = vec![opcode::WRITE_CMD];
    write_cmd.extend_from_slice(&value_handle.to_le_bytes());
    write_cmd.extend_from_slice(&[9u8]);
    let l2cap_write = L2capHeader::serialize(cid::ATT, &write_cmd);

    let resp = server
        .device
        .process_l2cap_packet(conn_handle, &l2cap_write)
        .unwrap();
    assert!(resp.is_none());

    assert!(
        events
            .lock()
            .unwrap()
            .contains(&RecordedEvent::CharacteristicWrite {
                uuid: Uuid::from_u16(0x2A37),
                value: vec![9u8],
                response_needed: false,
            })
    );
}

#[test]
fn test_exchange_mtu_fires_mtu_changed() {
    let (mut server, events, _handle) = build_server_with_characteristic();
    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;
    server.device.on_connected(conn_handle, peer);

    let mtu_req = [opcode::EXCHANGE_MTU_REQ, 0x00, 0x01];
    let l2cap_req = L2capHeader::serialize(cid::ATT, &mtu_req);
    let resp = server
        .device
        .process_l2cap_packet(conn_handle, &l2cap_req)
        .unwrap()
        .unwrap();
    let (_, resp_payload) = L2capHeader::parse(&resp).unwrap();
    assert_eq!(resp_payload[0], opcode::EXCHANGE_MTU_RSP);

    assert!(
        events
            .lock()
            .unwrap()
            .contains(&RecordedEvent::MtuChanged { mtu: 256 })
    );
}

#[test]
fn test_notify_characteristic_changed_fires_notification_sent() {
    let (mut server, events, _handle) = build_server_with_characteristic();
    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;
    server.device.on_connected(conn_handle, peer);

    let registered = server.get_service(Uuid::from_u16(0x180D)).unwrap();
    let mut characteristic = registered.characteristics[0].clone();
    characteristic.set_value(vec![80u8]);

    let android_device = BluetoothDevice::new(peer, AddressType::Random);

    let status = server.notify_characteristic_changed(&android_device, &characteristic, false);
    assert_eq!(status, BluetoothGatt::GATT_SUCCESS);
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&RecordedEvent::NotificationSent {
                status: BluetoothGatt::GATT_SUCCESS
            })
    );
}

#[test]
fn test_notify_characteristic_changed_with_confirm_sends_indication() {
    let (mut server, events, _handle) = build_server_with_characteristic();
    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;
    server.device.on_connected(conn_handle, peer);

    let registered = server.get_service(Uuid::from_u16(0x180D)).unwrap();
    let mut characteristic = registered.characteristics[0].clone();
    characteristic.set_value(vec![81u8]);

    let android_device = BluetoothDevice::new(peer, AddressType::Random);

    let status = server.notify_characteristic_changed(&android_device, &characteristic, true);
    assert_eq!(status, BluetoothGatt::GATT_SUCCESS);
    assert!(
        events
            .lock()
            .unwrap()
            .contains(&RecordedEvent::NotificationSent {
                status: BluetoothGatt::GATT_SUCCESS
            })
    );

    // A second indication before confirmation must fail (mirrors the
    // underlying VirtualDevice's pending-indication guard).
    let status_again = server.notify_characteristic_changed(&android_device, &characteristic, true);
    assert_eq!(status_again, BluetoothGatt::GATT_FAILURE);
}

#[test]
fn test_notify_characteristic_changed_without_connection_fails() {
    let (mut server, _events, _handle) = build_server_with_characteristic();
    let registered = server.get_service(Uuid::from_u16(0x180D)).unwrap();
    let characteristic = registered.characteristics[0].clone();
    let unconnected = BluetoothDevice::new(
        Address::from_be_bytes([1, 2, 3, 4, 5, 6]),
        AddressType::Random,
    );

    let status = server.notify_characteristic_changed(&unconnected, &characteristic, false);
    assert_eq!(status, BluetoothGatt::GATT_FAILURE);
}

#[test]
fn test_send_response_applies_value_to_database() {
    let (mut server, _events, value_handle) = build_server_with_characteristic();
    let peer = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
    let conn_handle = 0x0040;
    server.device.on_connected(conn_handle, peer);

    let mut write_req = vec![opcode::WRITE_REQ];
    write_req.extend_from_slice(&value_handle.to_le_bytes());
    write_req.extend_from_slice(&[1u8]);
    let l2cap_write = L2capHeader::serialize(cid::ATT, &write_req);
    let _ = server
        .device
        .process_l2cap_packet(conn_handle, &l2cap_write)
        .unwrap();

    let android_device = BluetoothDevice::new(peer, AddressType::Random);
    server.send_response(&android_device, 0, BluetoothGatt::GATT_SUCCESS, 0, &[200u8]);

    assert_eq!(
        server.device.gatt_db.read(value_handle, 0).unwrap(),
        &[200u8]
    );
}

#[test]
fn test_descriptor_read_and_write_are_routed_separately_from_characteristic() {
    let events = Arc::<Mutex<Vec<RecordedEvent>>>::default();
    let callback = RecordingCallback {
        events: events.clone(),
    };
    let addr = Address::from_be_bytes([0x22, 0x22, 0x22, 0x22, 0x22, 0x22]);
    let device = VirtualDevice::new("Desc", addr, AddressType::Random);
    let mut server = BluetoothGattServer::new(device, Box::new(callback));

    let mut service = BluetoothGattService::new(
        Uuid::from_u16(0x180F),
        BluetoothGattService::SERVICE_TYPE_PRIMARY,
    );
    let mut characteristic = BluetoothGattCharacteristic::new(
        Uuid::from_u16(0x2A19),
        BluetoothGattCharacteristic::PROPERTY_READ,
        BluetoothGattCharacteristic::PERMISSION_READ,
    );
    characteristic.set_value(vec![50u8]);
    let descriptor = BluetoothGattDescriptor::new(
        Uuid::from_u16(0x2901),
        BluetoothGattCharacteristic::PERMISSION_READ
            | BluetoothGattCharacteristic::PERMISSION_WRITE,
    );
    characteristic.add_descriptor(descriptor);
    service.add_characteristic(characteristic);
    assert!(server.add_service(service));

    let registered = server.get_service(Uuid::from_u16(0x180F)).unwrap();
    let descriptor_handle = registered.characteristics[0].descriptors[0]
        .handle
        .expect("descriptor handle assigned");

    let peer = Address::from_be_bytes([0x33, 0x33, 0x33, 0x33, 0x33, 0x33]);
    let conn_handle = 0x0007;
    server.device.on_connected(conn_handle, peer);

    let mut read_req = vec![opcode::READ_REQ];
    read_req.extend_from_slice(&descriptor_handle.to_le_bytes());
    let l2cap_read = L2capHeader::serialize(cid::ATT, &read_req);
    let _ = server
        .device
        .process_l2cap_packet(conn_handle, &l2cap_read)
        .unwrap()
        .unwrap();

    // The descriptor-shaped callback fired (not the characteristic one) for
    // the descriptor's own handle.
    let events = events.lock().unwrap();
    assert!(events.contains(&RecordedEvent::DescriptorRead {
        uuid: Uuid::from_u16(0x2901)
    }));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, RecordedEvent::CharacteristicRead { .. }))
    );
}
