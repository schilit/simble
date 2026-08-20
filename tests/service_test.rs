// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! End-to-end testing of the SimbleManager service runtime, REST router, and HCI channel.

use simble::api::dto::{CreateDeviceRequest, DeviceRole, SetAdvertisingRequest};
use simble::service::SimbleManager;
use simble::types::{Address, AddressType};

#[test]
fn test_simble_service_manager_e2e() {
    let manager = SimbleManager::new();

    // 1. Create a Keyboard device via API DTO
    let create_req = CreateDeviceRequest {
        name: "TestKeyboard".to_string(),
        address: Some(Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66])),
        address_type: AddressType::Random,
        role: DeviceRole::Peripheral,
        predefined_template: Some("keyboard".to_string()),
    };
    let resp = manager.create_device(create_req).expect("device creation");
    assert_eq!(resp.device_id, "TestKeyboard");

    // 2. Retrieve device and verify HCI channel creation
    let dev = manager.get_device("TestKeyboard").expect("get device");
    assert_eq!(dev.lock().unwrap().name, "TestKeyboard");
    let channel = manager
        .get_hci_channel("TestKeyboard")
        .expect("hci channel");

    // 3. Send an HCI command to Rootcanal
    let read_bd_addr_cmd = [0x09, 0x10, 0x00]; // Read BD ADDR
    channel.send_command(&read_bd_addr_cmd).unwrap();
    let host_pkt = channel.poll_host_packet().expect("poll packet");
    assert_eq!(host_pkt, vec![0x01, 0x09, 0x10, 0x00]);

    // 4. Update advertising
    manager
        .set_advertising(
            "TestKeyboard",
            SetAdvertisingRequest {
                enabled: true,
                interval_ms: Some(100),
                complete_local_name: Some("CustomKeyboardName".to_string()),
                service_uuids: vec![],
                manufacturer_data_hex: None,
            },
        )
        .unwrap();

    let list = manager.list_devices();
    assert_eq!(list.len(), 1);
    assert!(list[0].is_advertising);

    // 5. Remove device
    assert!(manager.remove_device("TestKeyboard"));
    assert_eq!(manager.list_devices().len(), 0);
}

#[test]
fn test_simble_http_rest_api_e2e() {
    let manager = SimbleManager::new();

    // 1. POST /v1/simble/devices -> create Heart Rate Monitor
    let req_body = br#"{"name":"HRM1","address":"F0:F1:F2:F3:F4:F5","address_type":"random","role":"peripheral","predefined_template":"heart_rate_monitor"}"#;
    let (status, resp_bytes) = manager.handle_http_request("POST", "/v1/simble/devices", req_body);
    assert_eq!(status, 200);
    assert!(String::from_utf8_lossy(&resp_bytes).contains("HRM1"));

    // 2. GET /v1/simble/devices -> lists HRM1
    let (status, list_bytes) = manager.handle_http_request("GET", "/v1/simble/devices", &[]);
    assert_eq!(status, 200);
    assert!(String::from_utf8_lossy(&list_bytes).contains("HRM1"));

    // 3. POST /v1/simble/devices/HRM1/advertising -> enable advertising
    let ad_body = br#"{"enabled":true,"complete_local_name":"HRM1"}"#;
    let (status, resp_bytes) =
        manager.handle_http_request("POST", "/v1/simble/devices/HRM1/advertising", ad_body);
    assert_eq!(status, 200);
    assert_eq!(resp_bytes, br#"{"status":"OK"}"#);

    // 4. PUT /v1/simble/devices/HRM1/characteristics/3 -> update heart rate measurement
    let val_body = &[0x00, 75]; // 75 BPM
    let (status, resp_bytes) =
        manager.handle_http_request("PUT", "/v1/simble/devices/HRM1/characteristics/3", val_body);
    assert_eq!(status, 200);
    assert_eq!(resp_bytes, br#"{"status":"OK"}"#);

    // 5. DELETE /v1/simble/devices/HRM1 -> remove device
    let (status, resp_bytes) =
        manager.handle_http_request("DELETE", "/v1/simble/devices/HRM1", &[]);
    assert_eq!(status, 200);
    assert_eq!(resp_bytes, br#"{"status":"DELETED"}"#);
}
