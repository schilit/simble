// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Tests virtual device lifecycle, multi-device connections, MTU negotiations,
//! GATT read/write operations, and notification generation over L2CAP.

use std::sync::{Arc, Mutex};

use simble::VirtualDevice;
use simble::att::opcode;
use simble::device::{
    AttServerObserver, BondSecurity, BondStore, ConnectionRole, MemoryBondStore, SubscriptionReason,
};
use simble::gatt::{AttributePermissions, CharacteristicProperties, service_uuid};
use simble::l2cap::{L2capHeader, cid};
use simble::smp::PairingKey;
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
fn test_read_by_group_type_splits_mixed_uuid_lengths() {
    // One Attribute Data List carries entries of a single length (Core Spec
    // Vol 3, Part F, 3.4.4.10), and each entry's group end handle bounds its
    // own service (Vol 3, Part G, 2.5.2). Violating either makes a client
    // slice a 128-bit service UUID into phantom 16-bit services and report
    // every later characteristic inside the first service.
    let mut dev = VirtualDevice::new(
        "Mixed",
        Address::from_be_bytes([0xF0, 0xDE, 0xF1, 0x00, 0x11, 0x44]),
        AddressType::Random,
    );
    // A 16-bit service with one characteristic…
    let first = dev.gatt_db.add_service(Uuid::from_u16(0x181A), true);
    dev.gatt_db.add_characteristic(
        Uuid::from_u16(0x2A6E),
        CharacteristicProperties(CharacteristicProperties::READ),
        vec![0x00, 21],
        AttributePermissions::default(),
    );
    // …then a 128-bit one, whose declaration value is 16 bytes.
    let custom = Uuid::from_u128_bytes([
        0x01, 0xEE, 0xFF, 0xC0, 0x00, 0x00, 0xE5, 0xB1, 0x11, 0x4A, 0xDE, 0xC0, 0x01, 0x00, 0x7B,
        0x5E,
    ]);
    let second = dev.gatt_db.add_service(custom, true);

    let conn_h = 0x0010;
    dev.on_connected(conn_h, Address::ANY);

    let mut req = vec![opcode::READ_BY_GROUP_TYPE_REQ];
    req.extend_from_slice(&0x0001u16.to_le_bytes());
    req.extend_from_slice(&0xFFFFu16.to_le_bytes());
    req.extend_from_slice(&service_uuid::PRIMARY_SERVICE.to_le_bytes());
    let resp = dev
        .process_l2cap_packet(conn_h, &L2capHeader::serialize(cid::ATT, &req))
        .unwrap()
        .unwrap();
    let (_, payload) = L2capHeader::parse(&resp).unwrap();

    assert_eq!(payload[0], opcode::READ_BY_GROUP_TYPE_RSP);
    let item_len = payload[1] as usize;
    assert_eq!(item_len, 6, "16-bit service: 2 handles + a 2-byte UUID");
    let entries = &payload[2..];
    assert_eq!(
        entries.len() % item_len,
        0,
        "the list must divide evenly into equal-length entries"
    );
    assert_eq!(
        entries.len() / item_len,
        1,
        "the 128-bit service has a different length and belongs to a later response"
    );

    // The first service's group must end before the second's declaration,
    // not run to 0xFFFF.
    let group_end = u16::from_le_bytes([entries[2], entries[3]]);
    assert_eq!(u16::from_le_bytes([entries[0], entries[1]]), first);
    assert!(
        group_end < second,
        "group end {group_end:#06x} must stop before the next service {second:#06x}"
    );

    // Continuing from the last handle returns the 128-bit service, alone.
    let mut req2 = vec![opcode::READ_BY_GROUP_TYPE_REQ];
    req2.extend_from_slice(&(group_end + 1).to_le_bytes());
    req2.extend_from_slice(&0xFFFFu16.to_le_bytes());
    req2.extend_from_slice(&service_uuid::PRIMARY_SERVICE.to_le_bytes());
    let resp2 = dev
        .process_l2cap_packet(conn_h, &L2capHeader::serialize(cid::ATT, &req2))
        .unwrap()
        .unwrap();
    let (_, payload2) = L2capHeader::parse(&resp2).unwrap();
    assert_eq!(payload2[1] as usize, 20, "128-bit service: 2 handles + 16");
    assert_eq!(payload2[2..].len(), 20);
}

#[test]
fn test_find_information_response_carries_the_real_16_bit_uuid() {
    // A Find Information Response must carry a 16-bit UUID as its own two
    // little-endian bytes (Core Spec Vol 3, Part F, 3.4.3.2). Encoding it by
    // truncating the 128-bit form yields the tail of the Bluetooth base UUID
    // (0x34FB) instead, so a real central discovers a nonexistent descriptor
    // and can never enable notifications.
    let mut dev = VirtualDevice::new(
        "Notifier",
        Address::from_be_bytes([0xF0, 0xDE, 0xF1, 0x00, 0x11, 0x33]),
        AddressType::Random,
    );
    dev.gatt_db.add_service(Uuid::from_u16(0x180D), true);
    let (_, value_handle) = dev.gatt_db.add_characteristic(
        Uuid::from_u16(0x2A37),
        CharacteristicProperties(CharacteristicProperties::READ | CharacteristicProperties::NOTIFY),
        vec![0x00, 72],
        AttributePermissions::default(),
    );
    let cccd_handle = dev.gatt_db.add_cccd();

    let conn_h = 0x0010;
    dev.on_connected(conn_h, Address::ANY);

    let mut req = vec![opcode::FIND_INFORMATION_REQ];
    req.extend_from_slice(&value_handle.to_le_bytes());
    req.extend_from_slice(&0xFFFFu16.to_le_bytes());
    let resp = dev
        .process_l2cap_packet(conn_h, &L2capHeader::serialize(cid::ATT, &req))
        .unwrap()
        .unwrap();
    let (_, payload) = L2capHeader::parse(&resp).unwrap();

    assert_eq!(payload[0], opcode::FIND_INFORMATION_RSP);
    assert_eq!(payload[1], 0x01, "format 0x01 = 16-bit UUIDs");
    // Entries are (handle, uuid) pairs; find the CCCD's.
    let mut found = None;
    let (entries, _) = payload[2..].as_chunks::<4>();
    for entry in entries {
        let handle = u16::from_le_bytes([entry[0], entry[1]]);
        let uuid = u16::from_le_bytes([entry[2], entry[3]]);
        if handle == cccd_handle {
            found = Some(uuid);
        }
    }
    assert_eq!(
        found,
        Some(0x2902),
        "the CCCD must be discoverable as 0x2902, not 0x34FB"
    );
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

/// One captured `on_subscription_changed` callback invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubscriptionEvent {
    connection_handle: u16,
    cccd_handle: u16,
    prev_notify: bool,
    cur_notify: bool,
    prev_indicate: bool,
    cur_indicate: bool,
    reason: SubscriptionReason,
}

/// Observer that records subscription events for assertion; the shared
/// `Arc` lets the test keep reading after the box moves into the device.
#[derive(Debug, Default)]
struct SubscriptionRecorder {
    events: Arc<Mutex<Vec<SubscriptionEvent>>>,
}

impl AttServerObserver for SubscriptionRecorder {
    fn on_subscription_changed(
        &mut self,
        connection_handle: u16,
        cccd_handle: u16,
        prev_notify: bool,
        cur_notify: bool,
        prev_indicate: bool,
        cur_indicate: bool,
        reason: SubscriptionReason,
    ) {
        self.events.lock().unwrap().push(SubscriptionEvent {
            connection_handle,
            cccd_handle,
            prev_notify,
            cur_notify,
            prev_indicate,
            cur_indicate,
            reason,
        });
    }
}

/// Sends an ATT Write Request for `value` to `handle` over L2CAP and
/// asserts the Write Response came back.
fn write_attribute(dev: &mut VirtualDevice, conn: u16, handle: u16, value: &[u8]) {
    let mut req = vec![opcode::WRITE_REQ];
    req.extend_from_slice(&handle.to_le_bytes());
    req.extend_from_slice(value);
    let l2cap = L2capHeader::serialize(cid::ATT, &req);
    let resp = dev.process_l2cap_packet(conn, &l2cap).unwrap().unwrap();
    let (_, payload) = L2capHeader::parse(&resp).unwrap();
    assert_eq!(payload[0], opcode::WRITE_RSP);
}

/// Registers a notify+indicate characteristic followed by its CCCD,
/// returning (value handle, CCCD handle).
fn add_notifying_characteristic(dev: &mut VirtualDevice) -> (u16, u16) {
    dev.gatt_db.add_service(Uuid::from_u16(0x180D), true);
    let (_, val_h) = dev.gatt_db.add_characteristic(
        Uuid::from_u16(0x2A37),
        CharacteristicProperties(
            CharacteristicProperties::NOTIFY | CharacteristicProperties::INDICATE,
        ),
        vec![0x00],
        AttributePermissions::default(),
    );
    let cccd_h = dev.gatt_db.add_cccd();
    (val_h, cccd_h)
}

#[test]
fn test_connection_role_and_address_lookup() {
    let dev_addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x00, 0x00, 0x01]);
    let mut dev = VirtualDevice::new("DualRole", dev_addr, AddressType::Random);
    dev.is_advertising = true;

    // Role defaults to Peripheral through the legacy entry point, and a
    // peripheral-role connection stops advertising.
    let peer_a = Address::from_be_bytes([0x11, 0x11, 0x11, 0x11, 0x11, 0x11]);
    dev.on_connected(0x0001, peer_a);
    assert_eq!(
        dev.connections.get(&0x0001).unwrap().role,
        ConnectionRole::Peripheral
    );
    assert!(!dev.is_advertising);

    // A central-role connection carries its own role and leaves the
    // device's advertising state alone.
    dev.is_advertising = true;
    let peer_b = Address::from_be_bytes([0x22, 0x22, 0x22, 0x22, 0x22, 0x22]);
    dev.on_connected_with_role(0x0002, peer_b, ConnectionRole::Central);
    assert_eq!(
        dev.connections.get(&0x0002).unwrap().role,
        ConnectionRole::Central
    );
    assert!(dev.is_advertising);

    // Address-keyed lookup finds each connection.
    assert_eq!(dev.connection_by_address(peer_a).unwrap().handle, 0x0001);
    assert_eq!(dev.connection_by_address(peer_b).unwrap().handle, 0x0002);
    let absent = Address::from_be_bytes([0x33, 0x33, 0x33, 0x33, 0x33, 0x33]);
    assert!(dev.connection_by_address(absent).is_none());

    dev.on_disconnected(0x0001);
    assert!(dev.connection_by_address(peer_a).is_none());
}

#[test]
fn test_cccd_subscription_events_on_write_and_disconnect() {
    let dev_addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x00, 0x00, 0x02]);
    let mut dev = VirtualDevice::new("Subscribable", dev_addr, AddressType::Random);
    let (_, cccd_h) = add_notifying_characteristic(&mut dev);

    let recorder = SubscriptionRecorder::default();
    let events = Arc::clone(&recorder.events);
    dev.observer = Some(Box::new(recorder));

    let conn_h = 0x0010;
    let peer = Address::from_be_bytes([0x44, 0x44, 0x44, 0x44, 0x44, 0x44]);
    dev.on_connected(conn_h, peer);

    // Subscribe to indications only.
    write_attribute(&mut dev, conn_h, cccd_h, &[0x02, 0x00]);
    // Switch to notifications only: both bits transition in one event.
    write_attribute(&mut dev, conn_h, cccd_h, &[0x01, 0x00]);
    // Rewriting the identical value must not fire an event.
    write_attribute(&mut dev, conn_h, cccd_h, &[0x01, 0x00]);
    // Unsubscribe entirely.
    write_attribute(&mut dev, conn_h, cccd_h, &[0x00, 0x00]);

    // Re-subscribe, then disconnect while subscribed.
    write_attribute(&mut dev, conn_h, cccd_h, &[0x01, 0x00]);
    dev.on_disconnected(conn_h);

    let events = events.lock().unwrap();
    assert_eq!(
        *events,
        vec![
            SubscriptionEvent {
                connection_handle: conn_h,
                cccd_handle: cccd_h,
                prev_notify: false,
                cur_notify: false,
                prev_indicate: false,
                cur_indicate: true,
                reason: SubscriptionReason::Write,
            },
            SubscriptionEvent {
                connection_handle: conn_h,
                cccd_handle: cccd_h,
                prev_notify: false,
                cur_notify: true,
                prev_indicate: true,
                cur_indicate: false,
                reason: SubscriptionReason::Write,
            },
            SubscriptionEvent {
                connection_handle: conn_h,
                cccd_handle: cccd_h,
                prev_notify: true,
                cur_notify: false,
                prev_indicate: false,
                cur_indicate: false,
                reason: SubscriptionReason::Write,
            },
            SubscriptionEvent {
                connection_handle: conn_h,
                cccd_handle: cccd_h,
                prev_notify: false,
                cur_notify: true,
                prev_indicate: false,
                cur_indicate: false,
                reason: SubscriptionReason::Write,
            },
            SubscriptionEvent {
                connection_handle: conn_h,
                cccd_handle: cccd_h,
                prev_notify: true,
                cur_notify: false,
                prev_indicate: false,
                cur_indicate: false,
                reason: SubscriptionReason::Disconnect,
            },
        ]
    );

    // The live CCCD value was reset by the disconnect.
    assert_eq!(dev.gatt_db.read(cccd_h, 0).unwrap(), &[0x00, 0x00]);
}

#[test]
fn test_bonded_peer_subscription_restore_round_trip() {
    let dev_addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x00, 0x00, 0x03]);
    let mut dev = VirtualDevice::new("Bonder", dev_addr, AddressType::Random);
    let (_, cccd_h) = add_notifying_characteristic(&mut dev);

    // Seed a bond as if pairing had already completed for this peer.
    let peer = Address::from_be_bytes([0x55, 0x55, 0x55, 0x55, 0x55, 0x55]);
    let mut store = MemoryBondStore::new();
    let mut security = BondSecurity {
        secure_connections: true,
        key_size: 16,
        ..BondSecurity::default()
    };
    security.keys.ltk = Some(PairingKey::new([0x5A; 16]));
    store.store_security(peer, security);
    dev.bond_store = Some(Box::new(store));

    let recorder = SubscriptionRecorder::default();
    let events = Arc::clone(&recorder.events);
    dev.observer = Some(Box::new(recorder));

    // Subscribe while connected: the bonded peer's state is persisted.
    let conn_h = 0x0020;
    dev.on_connected(conn_h, peer);
    write_attribute(&mut dev, conn_h, cccd_h, &[0x01, 0x00]);
    assert_eq!(
        dev.bond_store.as_deref().unwrap().load_cccds(peer),
        vec![(cccd_h, 0x0001)]
    );

    // Disconnect resets the live value but keeps the bond record.
    dev.on_disconnected(conn_h);
    assert_eq!(dev.gatt_db.read(cccd_h, 0).unwrap(), &[0x00, 0x00]);
    assert_eq!(
        dev.bond_store.as_deref().unwrap().load_cccds(peer),
        vec![(cccd_h, 0x0001)]
    );

    // Reconnect restores the subscription and reports it as BondRestore.
    let conn_h2 = 0x0021;
    dev.on_connected(conn_h2, peer);
    assert_eq!(dev.gatt_db.read(cccd_h, 0).unwrap(), &[0x01, 0x00]);

    let events = events.lock().unwrap();
    let restore = events
        .iter()
        .find(|e| e.reason == SubscriptionReason::BondRestore)
        .expect("BondRestore event fired");
    assert_eq!(
        *restore,
        SubscriptionEvent {
            connection_handle: conn_h2,
            cccd_handle: cccd_h,
            prev_notify: false,
            cur_notify: true,
            prev_indicate: false,
            cur_indicate: false,
            reason: SubscriptionReason::BondRestore,
        }
    );
}

#[test]
fn test_include_declaration_value_encoding() {
    let dev_addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0x00, 0x00, 0x04]);
    let mut dev = VirtualDevice::new("Includer", dev_addr, AddressType::Random);

    // A secondary battery service occupying handles 0x0001..=0x0003.
    let bas_h = dev.gatt_db.add_service(Uuid::from_u16(0x180F), false);
    dev.gatt_db.add_characteristic(
        Uuid::from_u16(0x2A19),
        CharacteristicProperties(CharacteristicProperties::READ),
        vec![100],
        AttributePermissions::read_only(),
    );

    // A primary service including it.
    dev.gatt_db.add_service(Uuid::from_u16(0x180D), true);
    let inc_h = dev
        .gatt_db
        .add_include(bas_h, 0x0003, Some(Uuid::from_u16(0x180F)));

    // Core Spec Vol 3, Part G, 3.2: start LE + end LE + 16-bit UUID LE.
    let attr = dev.gatt_db.attributes.get(&inc_h).unwrap();
    assert_eq!(attr.uuid, Uuid::from_u16(service_uuid::INCLUDE));
    assert_eq!(
        dev.gatt_db.read(inc_h, 0).unwrap(),
        &[0x01, 0x00, 0x03, 0x00, 0x0F, 0x18]
    );

    // A 128-bit included-service UUID is omitted from the value (clients
    // read it from the included service's declaration instead).
    let custom = Uuid::from_u128_bytes([0xAB; 16]);
    let inc128_h = dev.gatt_db.add_include(0x0010, 0x0014, Some(custom));
    assert_eq!(
        dev.gatt_db.read(inc128_h, 0).unwrap(),
        &[0x10, 0x00, 0x14, 0x00]
    );

    // Include declarations are readable through the normal ATT read path.
    let conn_h = 0x0001;
    dev.on_connected(conn_h, Address::ANY);
    let mut read_req = vec![opcode::READ_REQ];
    read_req.extend_from_slice(&inc_h.to_le_bytes());
    let l2cap = L2capHeader::serialize(cid::ATT, &read_req);
    let resp = dev.process_l2cap_packet(conn_h, &l2cap).unwrap().unwrap();
    let (_, payload) = L2capHeader::parse(&resp).unwrap();
    assert_eq!(payload[0], opcode::READ_RSP);
    assert_eq!(&payload[1..], &[0x01, 0x00, 0x03, 0x00, 0x0F, 0x18]);
}
