use super::*;
use crate::transport::HciChannel;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// Shared recorder/scripter behind the mock, the `DuplexMock` philosophy
/// of the sibling transports' tests: outbound packets are recorded,
/// inbound completions are scripted per endpoint.
#[derive(Default)]
struct MockState {
    commands: Vec<Vec<u8>>,
    acl_out: Vec<Vec<u8>>,
    events_in: VecDeque<Vec<u8>>,
    acl_in: VecDeque<Vec<u8>>,
    fail_event_recv: bool,
    fail_command_send: bool,
}

struct MockEndpoints(Rc<RefCell<MockState>>);

impl UsbEndpoints for MockEndpoints {
    fn send_command(&mut self, cmd: &[u8]) -> Result<(), SimbleError> {
        let mut state = self.0.borrow_mut();
        if state.fail_command_send {
            return Err(SimbleError::Transport("mock command failure".to_string()));
        }
        state.commands.push(cmd.to_vec());
        Ok(())
    }
    fn send_acl(&mut self, acl: &[u8]) -> Result<(), SimbleError> {
        self.0.borrow_mut().acl_out.push(acl.to_vec());
        Ok(())
    }
    fn try_recv_event(&mut self) -> Result<Option<Vec<u8>>, SimbleError> {
        let mut state = self.0.borrow_mut();
        if state.fail_event_recv {
            return Err(SimbleError::Transport("mock event failure".to_string()));
        }
        Ok(state.events_in.pop_front())
    }
    fn try_recv_acl(&mut self) -> Result<Option<Vec<u8>>, SimbleError> {
        Ok(self.0.borrow_mut().acl_in.pop_front())
    }
}

fn mock_transport() -> (UsbTransport, Rc<RefCell<MockState>>) {
    let state = Rc::new(RefCell::new(MockState::default()));
    let transport = UsbTransport::with_endpoints(Box::new(MockEndpoints(state.clone())));
    (transport, state)
}

#[test]
fn test_pump_routes_command_to_control_endpoint_without_h4_byte() {
    let (mut transport, state) = mock_transport();
    let channel = HciChannel::new();
    channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();

    transport.pump(&channel).unwrap();

    assert_eq!(state.borrow().commands, vec![vec![0x03, 0x0C, 0x00]]);
    assert!(state.borrow().acl_out.is_empty());
}

#[test]
fn test_pump_routes_acl_to_bulk_out_without_h4_byte() {
    let (mut transport, state) = mock_transport();
    let channel = HciChannel::new();
    channel
        .send_acl_data(&[0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB])
        .unwrap();

    transport.pump(&channel).unwrap();

    assert_eq!(
        state.borrow().acl_out,
        vec![vec![0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB]]
    );
    assert!(state.borrow().commands.is_empty());
}

#[test]
fn test_pump_rejects_sco_and_unknown_h4_types() {
    for bad_type in [h4_type::HCI_SCO_DATA, h4_type::HCI_ISO_DATA, 0xFF] {
        let (mut transport, _state) = mock_transport();
        let channel = HciChannel::new();
        channel
            .host_to_ctrl_tx
            .send(vec![bad_type, 0x00, 0x00])
            .unwrap();
        assert!(matches!(
            transport.pump(&channel),
            Err(SimbleError::Transport(_))
        ));
    }
}

#[test]
fn test_pump_passes_events_through_with_h4_byte_restored() {
    let (mut transport, state) = mock_transport();
    // Command Complete for Reset.
    let event = vec![0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
    state.borrow_mut().events_in.push_back(event.clone());
    let channel = HciChannel::new();

    transport.pump(&channel).unwrap();

    let mut expected = vec![h4_type::HCI_EVENT];
    expected.extend_from_slice(&event);
    assert_eq!(channel.poll_controller_packet().unwrap(), expected);
    assert!(channel.poll_controller_packet().is_none());
}

#[test]
fn test_pump_reassembles_acl_fragmented_across_transfers() {
    let (mut transport, state) = mock_transport();
    // One 8-byte-payload ACL packet split mid-header and mid-payload.
    let acl = [0x40, 0x00, 0x08, 0x00, 1, 2, 3, 4, 5, 6, 7, 8];
    {
        let mut s = state.borrow_mut();
        s.acl_in.push_back(acl[..3].to_vec());
        s.acl_in.push_back(acl[3..9].to_vec());
        s.acl_in.push_back(acl[9..].to_vec());
    }
    let channel = HciChannel::new();

    transport.pump(&channel).unwrap();

    let mut expected = vec![h4_type::HCI_ACL_DATA];
    expected.extend_from_slice(&acl);
    assert_eq!(channel.poll_controller_packet().unwrap(), expected);
    assert!(channel.poll_controller_packet().is_none());
}

#[test]
fn test_pump_splits_two_acl_packets_arriving_in_one_transfer() {
    let (mut transport, state) = mock_transport();
    let first = [0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB];
    let second = [0x41, 0x00, 0x01, 0x00, 0xCC];
    let mut combined = first.to_vec();
    combined.extend_from_slice(&second);
    state.borrow_mut().acl_in.push_back(combined);
    let channel = HciChannel::new();

    transport.pump(&channel).unwrap();

    let mut expected_first = vec![h4_type::HCI_ACL_DATA];
    expected_first.extend_from_slice(&first);
    let mut expected_second = vec![h4_type::HCI_ACL_DATA];
    expected_second.extend_from_slice(&second);
    assert_eq!(channel.poll_controller_packet().unwrap(), expected_first);
    assert_eq!(channel.poll_controller_packet().unwrap(), expected_second);
}

#[test]
fn test_partial_acl_packet_is_held_until_completed_on_a_later_pump() {
    let (mut transport, state) = mock_transport();
    let acl = [0x40, 0x00, 0x03, 0x00, 0x01, 0x02, 0x03];
    state.borrow_mut().acl_in.push_back(acl[..5].to_vec());
    let channel = HciChannel::new();

    transport.pump(&channel).unwrap();
    assert!(channel.poll_controller_packet().is_none());

    state.borrow_mut().acl_in.push_back(acl[5..].to_vec());
    transport.pump(&channel).unwrap();

    let mut expected = vec![h4_type::HCI_ACL_DATA];
    expected.extend_from_slice(&acl);
    assert_eq!(channel.poll_controller_packet().unwrap(), expected);
}

#[test]
fn test_pump_propagates_endpoint_errors() {
    let (mut transport, state) = mock_transport();
    state.borrow_mut().fail_event_recv = true;
    let channel = HciChannel::new();
    assert!(matches!(
        transport.pump(&channel),
        Err(SimbleError::Transport(_))
    ));

    let (mut transport, state) = mock_transport();
    state.borrow_mut().fail_command_send = true;
    let channel = HciChannel::new();
    channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();
    assert!(matches!(
        transport.pump(&channel),
        Err(SimbleError::Transport(_))
    ));
}

#[test]
fn test_is_bluetooth_hci_matches_device_level_class() {
    assert!(is_bluetooth_hci((0xE0, 0x01, 0x01), &[]));
}

#[test]
fn test_is_bluetooth_hci_matches_interface_level_class_on_composite_device() {
    // Composite dongle: generic device class, HCI declared per-interface.
    assert!(is_bluetooth_hci(
        (0x00, 0x00, 0x00),
        &[(0xFF, 0x00, 0x00), (0xE0, 0x01, 0x01)]
    ));
}

#[test]
fn test_is_bluetooth_hci_rejects_non_bluetooth_devices() {
    assert!(!is_bluetooth_hci((0x03, 0x01, 0x01), &[(0x03, 0x01, 0x01)]));
    assert!(!is_bluetooth_hci((0xE0, 0x01, 0x03), &[])); // AMP controller, not primary
    assert!(!is_bluetooth_hci((0x00, 0x00, 0x00), &[]));
}

#[test]
fn test_select_endpoints_finds_standard_hci_layout() {
    // Typical dongle interface 0: interrupt IN 0x81, bulk IN 0x82,
    // bulk OUT 0x02, plus an unrelated isochronous pair to ignore.
    let endpoints = [
        (0x81, TRANSFER_TYPE_INTERRUPT),
        (0x82, TRANSFER_TYPE_BULK),
        (0x02, TRANSFER_TYPE_BULK),
        (0x83, 0x01),
        (0x03, 0x01),
    ];
    assert_eq!(
        select_endpoints(&endpoints).unwrap(),
        EndpointAddresses {
            event_in: 0x81,
            acl_in: 0x82,
            acl_out: 0x02
        }
    );
}

#[test]
fn test_select_endpoints_tolerates_nonstandard_addresses() {
    let endpoints = [
        (0x02 | ENDPOINT_IN, TRANSFER_TYPE_INTERRUPT),
        (0x01 | ENDPOINT_IN, TRANSFER_TYPE_BULK),
        (0x01, TRANSFER_TYPE_BULK),
    ];
    assert_eq!(
        select_endpoints(&endpoints).unwrap(),
        EndpointAddresses {
            event_in: 0x82,
            acl_in: 0x81,
            acl_out: 0x01
        }
    );
}

#[test]
fn test_select_endpoints_errors_when_incomplete() {
    let missing_bulk_out = [(0x81, TRANSFER_TYPE_INTERRUPT), (0x82, TRANSFER_TYPE_BULK)];
    assert!(matches!(
        select_endpoints(&missing_bulk_out),
        Err(SimbleError::Transport(_))
    ));
    assert!(select_endpoints(&[]).is_err());
}

#[test]
fn test_parse_vid_pid() {
    assert_eq!(parse_vid_pid("0a12:0001").unwrap(), (0x0A12, 0x0001));
    assert_eq!(parse_vid_pid("8087:0025").unwrap(), (0x8087, 0x0025));
    assert!(parse_vid_pid("0a120001").is_err());
    assert!(parse_vid_pid("zzzz:0001").is_err());
    assert!(parse_vid_pid("0a12:").is_err());
}

#[test]
fn test_usb_scene_reports_its_selector_without_touching_hardware() {
    // Selecting the backend must not open anything: the agent picks a
    // dongle before it is plugged in as often as after.
    assert_eq!(
        UsbScene::new(Some((0x0A12, 0x0001))).selector(),
        "0a12:0001"
    );
    assert!(UsbScene::new(None).selector().contains("first"));
    assert_eq!(UsbScene::new(None).device_count(), 0);
}

#[test]
fn test_usb_scene_rejects_a_bad_script_before_opening_the_dongle() {
    // Script validation runs first, so a script error is reported as a
    // script error even with no hardware present — the same ordering
    // NetsimScene has.
    let mut scene = UsbScene::new(Some((0xFFFF, 0xFFFF)));
    let err = scene
        .add_peripheral("F0:DE:C0:00:00:01".parse().unwrap(), "let a = 1;")
        .unwrap_err();
    assert!(err.contains("BluetoothGattServer"), "{err}");
    assert_eq!(scene.device_count(), 0);
}

#[test]
fn test_in_transfer_len_rounds_up_to_max_packet_size_multiple() {
    assert_eq!(in_transfer_len(16, 257), 272);
    assert_eq!(in_transfer_len(64, 4096), 4096);
    assert_eq!(in_transfer_len(512, 4096), 4096);
    assert_eq!(in_transfer_len(64, 1), 64);
    assert_eq!(in_transfer_len(64, 0), 64);
}

#[test]
fn test_acl_reassembler_handles_byte_at_a_time_delivery() {
    let acl = [0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB];
    let mut reassembler = AclInReassembler::default();
    for &byte in &acl[..acl.len() - 1] {
        reassembler.feed(&[byte]);
        assert_eq!(reassembler.next_packet(), None);
    }
    reassembler.feed(&acl[acl.len() - 1..]);
    let mut expected = vec![h4_type::HCI_ACL_DATA];
    expected.extend_from_slice(&acl);
    assert_eq!(reassembler.next_packet().unwrap(), expected);
}

#[test]
fn test_acl_reassembler_handles_zero_length_payload() {
    let acl = [0x40, 0x00, 0x00, 0x00];
    let mut reassembler = AclInReassembler::default();
    reassembler.feed(&acl);
    assert_eq!(
        reassembler.next_packet().unwrap(),
        vec![h4_type::HCI_ACL_DATA, 0x40, 0x00, 0x00, 0x00]
    );
    assert_eq!(reassembler.next_packet(), None);
}
