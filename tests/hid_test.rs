// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Classic HID tests, written from the HID Profile v1.1.1 spec (Bumble has
//! no `hid_test.py` to port): L2CAP channel establishment on both HID PSMs,
//! GET_REPORT/SET_REPORT round trips including handshake error responses,
//! protocol mode get/set, report DATA over the interrupt channel,
//! HID_CONTROL commands, and SDP service record structure/discovery.

use simble::classic::hid::{
    self, DEVICE_SUBCLASS_COMBO, HID_CONTROL_PSM, HID_INTERRUPT_PSM, HidDevice, HidDeviceEvent,
    HidHost, HidHostEvent, HidMessage, InterruptData, handshake_code, protocol_mode, report_type,
};
use simble::classic::sdp::{
    DataElement, SdpClient, SdpServer, SdpUuid, ServiceAttribute, attribute_id as sdp_attribute_id,
};
use simble::devices::helpers::hid_reports::{KEYBOARD_REPORT_MAP, ascii_to_hid};
use simble::l2cap::classic::ClassicChannelManager;

const BOOT_KEYBOARD_REPORT_ID: u8 = 1;

/// Builds an 8-byte boot keyboard input report for one ASCII character,
/// reusing the transport-independent keycode helpers shared with BLE HOGP.
fn boot_keyboard_report(c: char) -> Vec<u8> {
    let (modifier, keycode) = ascii_to_hid(c).expect("mappable character");
    vec![modifier, 0x00, keycode, 0x00, 0x00, 0x00, 0x00, 0x00]
}

/// Opens a Classic Basic Mode L2CAP channel between a client/server manager
/// pair on `psm`, driving the connect -> configure -> configure sequence to
/// completion. Returns the client and server CIDs.
fn open_channel(
    client_mgr: &mut ClassicChannelManager,
    server_mgr: &mut ClassicChannelManager,
    client_cid: u16,
    request: &simble::packets::l2cap_signaling::ConnectionRequestHeader,
) -> (u16, u16) {
    let response = server_mgr
        .on_connection_request(request, 2048)
        .expect("server accepts the connection request");
    let server_cid = response.destination_cid.get();
    client_mgr
        .on_connection_response(client_cid, &response)
        .expect("client accepts the connection response");

    let (client_cfg_req, client_options) = client_mgr
        .make_configuration_request(client_cid)
        .expect("client can build a configuration request");
    let server_cfg_rsp = server_mgr
        .on_configuration_request(client_cfg_req.destination_cid.get(), &client_options)
        .expect("server accepts client configuration");
    client_mgr
        .on_configuration_response(client_cid, &server_cfg_rsp)
        .expect("client accepts server's configuration response");

    let (server_cfg_req, server_options) = server_mgr
        .make_configuration_request(server_cid)
        .expect("server can build a configuration request");
    let client_cfg_rsp = client_mgr
        .on_configuration_request(server_cfg_req.destination_cid.get(), &server_options)
        .expect("client accepts server configuration");
    server_mgr
        .on_configuration_response(server_cid, &client_cfg_rsp)
        .expect("server accepts client's configuration response");

    (client_cid, server_cid)
}

/// A `HidDevice` pre-loaded with a boot keyboard input report and an LED
/// output report, the shape most host round-trip tests need.
fn keyboard_device() -> HidDevice {
    let mut device = HidDevice::new();
    device.put_report(report_type::INPUT, BOOT_KEYBOARD_REPORT_ID, vec![0x00; 8]);
    device.put_report(report_type::OUTPUT, BOOT_KEYBOARD_REPORT_ID, vec![0x00]);
    device
}

#[test]
fn test_channel_establishment_on_both_psms() {
    let mut host_mgr = ClassicChannelManager::new();
    let mut device_mgr = ClassicChannelManager::new();
    hid::register_psms(&mut device_mgr).unwrap();
    assert!(device_mgr.is_server_registered(HID_CONTROL_PSM));
    assert!(device_mgr.is_server_registered(HID_INTERRUPT_PSM));

    // Control first, then interrupt (HID Profile v1.1.1, 5.2.2).
    let (control_cid, request) = hid::connect_control_channel(&mut host_mgr, 672).unwrap();
    let (host_control, device_control) =
        open_channel(&mut host_mgr, &mut device_mgr, control_cid, &request);
    let (interrupt_cid, request) = hid::connect_interrupt_channel(&mut host_mgr, 672).unwrap();
    let (host_interrupt, device_interrupt) =
        open_channel(&mut host_mgr, &mut device_mgr, interrupt_cid, &request);

    for (mgr, cid, psm) in [
        (&host_mgr, host_control, HID_CONTROL_PSM),
        (&host_mgr, host_interrupt, HID_INTERRUPT_PSM),
        (&device_mgr, device_control, HID_CONTROL_PSM),
        (&device_mgr, device_interrupt, HID_INTERRUPT_PSM),
    ] {
        let channel = mgr.get_channel(cid).unwrap();
        assert!(channel.is_open());
        assert_eq!(channel.psm, psm);
    }
}

#[test]
fn test_get_report_round_trip() {
    let mut device = keyboard_device();
    let host = HidHost::new();
    let report = boot_keyboard_report('a');
    device.put_report(report_type::INPUT, BOOT_KEYBOARD_REPORT_ID, report.clone());

    let request = host.get_report(report_type::INPUT, BOOT_KEYBOARD_REPORT_ID, None);
    let (response, events) = device.receive_control(&request).unwrap();
    assert!(events.is_empty());

    let mut expected_payload = vec![BOOT_KEYBOARD_REPORT_ID];
    expected_payload.extend(&report);
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::ControlData {
            report_type: report_type::INPUT,
            payload: expected_payload,
        }]
    );
}

#[test]
fn test_get_report_invalid_report_id_handshake() {
    let mut device = keyboard_device();
    let host = HidHost::new();

    let request = host.get_report(report_type::FEATURE, 9, None);
    let (response, events) = device.receive_control(&request).unwrap();
    assert!(events.is_empty());
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::Handshake(
            handshake_code::ERR_INVALID_REPORT_ID
        )]
    );
}

#[test]
fn test_get_report_buffer_size_truncates_response() {
    let mut device = keyboard_device();
    let host = HidHost::new();
    device.put_report(
        report_type::INPUT,
        BOOT_KEYBOARD_REPORT_ID,
        boot_keyboard_report('z'),
    );

    let request = host.get_report(report_type::INPUT, BOOT_KEYBOARD_REPORT_ID, Some(4));
    let (response, _) = device.receive_control(&request).unwrap();
    let events = host.receive_control(&response.unwrap()).unwrap();
    let [HidHostEvent::ControlData { payload, .. }] = events.as_slice() else {
        panic!("expected one ControlData event, got {events:?}");
    };
    assert_eq!(payload.len(), 4);
    assert_eq!(payload[0], BOOT_KEYBOARD_REPORT_ID);
}

#[test]
fn test_set_report_round_trip_updates_device_state() {
    let mut device = keyboard_device();
    let host = HidHost::new();

    // Caps Lock + Num Lock LEDs on.
    let request = host.set_report(report_type::OUTPUT, BOOT_KEYBOARD_REPORT_ID, &[0x03]);
    let (response, events) = device.receive_control(&request).unwrap();
    assert_eq!(
        events,
        vec![HidDeviceEvent::ReportSet {
            report_type: report_type::OUTPUT,
            report_id: BOOT_KEYBOARD_REPORT_ID,
            data: vec![0x03],
        }]
    );
    assert_eq!(
        device.report(report_type::OUTPUT, BOOT_KEYBOARD_REPORT_ID),
        Some([0x03].as_slice())
    );
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::Handshake(handshake_code::SUCCESSFUL)]
    );
}

#[test]
fn test_set_report_invalid_report_id_handshake() {
    let mut device = keyboard_device();
    let host = HidHost::new();

    let request = host.set_report(report_type::OUTPUT, 7, &[0x01]);
    let (response, events) = device.receive_control(&request).unwrap();
    assert!(events.is_empty());
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::Handshake(
            handshake_code::ERR_INVALID_REPORT_ID
        )]
    );
}

#[test]
fn test_set_report_empty_payload_invalid_parameter_handshake() {
    let mut device = keyboard_device();
    let host = HidHost::new();

    let request = HidMessage::SetReport {
        report_type: report_type::OUTPUT,
        payload: Vec::new(),
    }
    .to_bytes();
    let (response, _) = device.receive_control(&request).unwrap();
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::Handshake(
            handshake_code::ERR_INVALID_PARAMETER
        )]
    );
}

#[test]
fn test_protocol_get_set_round_trip() {
    let mut device = keyboard_device();
    let host = HidHost::new();

    let (response, _) = device.receive_control(&host.get_protocol()).unwrap();
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::ControlData {
            report_type: report_type::OTHER,
            payload: vec![protocol_mode::REPORT],
        }]
    );

    let (response, events) = device
        .receive_control(&host.set_protocol(protocol_mode::BOOT))
        .unwrap();
    assert_eq!(
        events,
        vec![HidDeviceEvent::ProtocolSet(protocol_mode::BOOT)]
    );
    assert_eq!(device.protocol_mode, protocol_mode::BOOT);
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::Handshake(handshake_code::SUCCESSFUL)]
    );

    let (response, _) = device.receive_control(&host.get_protocol()).unwrap();
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::ControlData {
            report_type: report_type::OTHER,
            payload: vec![protocol_mode::BOOT],
        }]
    );
}

#[test]
fn test_input_report_delivery_over_interrupt_channel() {
    let device = keyboard_device();
    let report = boot_keyboard_report('h');

    let pdu = device.send_input_report(report.clone());
    assert_eq!(
        hid::receive_interrupt(&pdu).unwrap(),
        Some(InterruptData {
            report_type: report_type::INPUT,
            payload: report,
        })
    );
}

#[test]
fn test_output_report_delivery_over_interrupt_channel() {
    let host = HidHost::new();

    let pdu = host.send_output_report(vec![BOOT_KEYBOARD_REPORT_ID, 0x01]);
    assert_eq!(
        hid::receive_interrupt(&pdu).unwrap(),
        Some(InterruptData {
            report_type: report_type::OUTPUT,
            payload: vec![BOOT_KEYBOARD_REPORT_ID, 0x01],
        })
    );
}

#[test]
fn test_non_data_on_interrupt_channel_is_ignored() {
    let host = HidHost::new();
    assert_eq!(hid::receive_interrupt(&host.get_protocol()).unwrap(), None);
    assert!(hid::receive_interrupt(&[]).is_err());
}

#[test]
fn test_control_commands_and_virtual_cable_unplug() {
    let mut device = keyboard_device();
    let host = HidHost::new();

    for (request, expected) in [
        (host.suspend(), HidDeviceEvent::Suspend),
        (host.exit_suspend(), HidDeviceEvent::ExitSuspend),
        (
            host.virtual_cable_unplug(),
            HidDeviceEvent::VirtualCableUnplug,
        ),
    ] {
        let (response, events) = device.receive_control(&request).unwrap();
        assert!(response.is_none());
        assert_eq!(events, vec![expected]);
    }

    assert_eq!(
        host.receive_control(&device.virtual_cable_unplug())
            .unwrap(),
        vec![HidHostEvent::VirtualCableUnplug]
    );
}

#[test]
fn test_unsupported_message_type_gets_handshake() {
    let mut device = keyboard_device();
    let host = HidHost::new();

    // 0x2- is a reserved HIDP message type.
    let (response, events) = device.receive_control(&[0x20]).unwrap();
    assert!(events.is_empty());
    assert_eq!(
        host.receive_control(&response.unwrap()).unwrap(),
        vec![HidHostEvent::Handshake(
            handshake_code::ERR_UNSUPPORTED_REQUEST
        )]
    );
}

#[test]
fn test_sdp_record_structure() {
    let records =
        hid::make_service_sdp_records(0x0001_0003, KEYBOARD_REPORT_MAP, DEVICE_SUBCLASS_COMBO);

    let service_classes =
        ServiceAttribute::find_attribute_in_list(&records, sdp_attribute_id::SERVICE_CLASS_ID_LIST)
            .and_then(DataElement::as_sequence)
            .unwrap();
    assert_eq!(service_classes[0].as_uuid(), Some(hid::HID_SERVICE_CLASS));

    let protocols = ServiceAttribute::find_attribute_in_list(
        &records,
        sdp_attribute_id::PROTOCOL_DESCRIPTOR_LIST,
    )
    .and_then(DataElement::as_sequence)
    .unwrap();
    let l2cap = protocols[0].as_sequence().unwrap();
    assert_eq!(l2cap[0].as_uuid(), Some(SdpUuid::BT_L2CAP_PROTOCOL_ID));
    assert_eq!(
        l2cap[1].as_unsigned_integer().map(|(value, _)| value),
        Some(u64::from(HID_CONTROL_PSM))
    );
    let hidp = protocols[1].as_sequence().unwrap();
    assert_eq!(hidp[0].as_uuid(), Some(hid::HIDP_PROTOCOL_ID));

    let additional = ServiceAttribute::find_attribute_in_list(
        &records,
        sdp_attribute_id::ADDITIONAL_PROTOCOL_DESCRIPTOR_LIST,
    )
    .and_then(DataElement::as_sequence)
    .unwrap();
    let interrupt_stack = additional[0].as_sequence().unwrap();
    let l2cap = interrupt_stack[0].as_sequence().unwrap();
    assert_eq!(
        l2cap[1].as_unsigned_integer().map(|(value, _)| value),
        Some(u64::from(HID_INTERRUPT_PSM))
    );

    let descriptor_list =
        ServiceAttribute::find_attribute_in_list(&records, hid::attribute_id::HID_DESCRIPTOR_LIST)
            .and_then(DataElement::as_sequence)
            .unwrap();
    let descriptor = descriptor_list[0].as_sequence().unwrap();
    assert_eq!(
        descriptor[0].as_unsigned_integer().map(|(value, _)| value),
        Some(u64::from(hid::REPORT_DESCRIPTOR_TYPE))
    );
    assert_eq!(
        descriptor[1],
        DataElement::text_string(KEYBOARD_REPORT_MAP.to_vec())
    );
}

#[test]
fn test_sdp_report_descriptor_discovery_round_trip() {
    let handle = 0x0001_0003;
    let mut sdp_server = SdpServer::new();
    sdp_server.service_records.insert(
        handle,
        hid::make_service_sdp_records(handle, KEYBOARD_REPORT_MAP, DEVICE_SUBCLASS_COMBO),
    );

    let mut sdp_client = SdpClient::new();
    let descriptor = hid::find_hid_report_descriptor(&mut sdp_client, |req| {
        sdp_server.handle_request(req, 1024)
    })
    .unwrap();
    assert_eq!(descriptor.as_deref(), Some(KEYBOARD_REPORT_MAP));
}
