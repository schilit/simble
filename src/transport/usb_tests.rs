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
        UsbScene::new(UsbSelector::VidPid(0x0A12, 0x0001)).selector(),
        "0a12:0001"
    );
    assert_eq!(UsbScene::new(UsbSelector::Index(1)).selector(), "#1");
    assert_eq!(
        UsbScene::new(UsbSelector::BusAddress {
            bus_id: "02".to_string(),
            device_address: 4,
        })
        .selector(),
        "02/4"
    );
    assert!(
        UsbScene::new(UsbSelector::First)
            .selector()
            .contains("first")
    );
    assert_eq!(UsbScene::new(UsbSelector::First).device_count(), 0);
}

#[test]
fn test_usb_scene_rejects_a_bad_script_before_opening_the_dongle() {
    // Script validation runs first, so a script error is reported as a
    // script error even with no hardware present — the same ordering
    // NetsimScene has.
    let mut scene = UsbScene::new(UsbSelector::VidPid(0xFFFF, 0xFFFF));
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

// --- command flow control over the transport --------------------------------

/// One Command Complete, bare (no H4 type byte), granting `credits`.
fn command_complete(opcode: [u8; 2], credits: u8) -> Vec<u8> {
    vec![0x0E, 0x04, credits, opcode[0], opcode[1], 0x00]
}

/// The regression this whole transport change exists for. Seven commands are
/// queued at once, as `host::init_commands` and every scripted bring-up does;
/// only the first may go out until the controller grants more.
///
/// A CSR8510 answers `Reset` and discards the six behind it, with no error
/// anywhere — see `CommandCredits`.
#[test]
fn test_pump_sends_one_command_until_the_controller_grants_more() {
    let (mut transport, state) = mock_transport();
    let channel = HciChannel::new();
    for i in 0..7u8 {
        channel.send_command(&[i, 0x0C, 0x00]).unwrap();
    }

    transport.pump(&channel).unwrap();
    assert_eq!(
        state.borrow().commands,
        vec![vec![0x00, 0x0C, 0x00]],
        "only the first command may go out on one credit"
    );
    assert_eq!(transport.command_backlog(), (0, 6));

    // The controller answers, granting one.
    state
        .borrow_mut()
        .events_in
        .push_back(command_complete([0x00, 0x0C], 1));
    transport.pump(&channel).unwrap();
    assert_eq!(state.borrow().commands.len(), 2);
    assert_eq!(transport.command_backlog(), (0, 5));

    // A budget of four releases four at once.
    state
        .borrow_mut()
        .events_in
        .push_back(command_complete([0x01, 0x0C], 4));
    transport.pump(&channel).unwrap();
    assert_eq!(state.borrow().commands.len(), 6);
    assert_eq!(transport.command_backlog(), (0, 1));
}

/// The credit in an event must be spent in the *same* pump that received it,
/// or a caller pumping once per frame drains its queue at one command per
/// frame — bring-up alone would take seven.
#[test]
fn test_a_credit_arriving_releases_a_command_in_the_same_pump() {
    let (mut transport, state) = mock_transport();
    let channel = HciChannel::new();
    channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();
    transport.pump(&channel).unwrap();

    channel.send_command(&[0x01, 0x0C, 0x00]).unwrap();
    state
        .borrow_mut()
        .events_in
        .push_back(command_complete([0x03, 0x0C], 1));
    transport.pump(&channel).unwrap();

    assert_eq!(state.borrow().commands.len(), 2);
    assert_eq!(transport.command_backlog().1, 0);
}

/// ACL data is flow-controlled by the controller's buffer accounting, not by
/// the command budget, so it must not queue behind a stalled command.
#[test]
fn test_acl_data_is_not_held_by_the_command_budget() {
    let (mut transport, state) = mock_transport();
    let channel = HciChannel::new();
    channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();
    channel.send_command(&[0x01, 0x0C, 0x00]).unwrap();
    channel
        .send_acl_data(&[0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB])
        .unwrap();

    transport.pump(&channel).unwrap();

    assert_eq!(state.borrow().commands.len(), 1, "the second command waits");
    assert_eq!(
        state.borrow().acl_out,
        vec![vec![0x40, 0x00, 0x02, 0x00, 0xAA, 0xBB]],
        "the ACL packet does not"
    );
}

/// The bug switch really is the old behaviour, which is what makes the
/// hardware test's two runs comparable.
#[test]
fn test_flow_control_off_writes_every_command_at_once() {
    let (mut transport, state) = mock_transport();
    transport.set_command_flow_control(false);
    let channel = HciChannel::new();
    for i in 0..7u8 {
        channel.send_command(&[i, 0x0C, 0x00]).unwrap();
    }

    transport.pump(&channel).unwrap();

    assert_eq!(state.borrow().commands.len(), 7);
    assert_eq!(transport.command_backlog(), (1, 0));
}

/// An event still reaches the host after its credit has been taken from it;
/// the transport reads the field, it does not consume the event.
#[test]
fn test_the_event_carrying_a_credit_is_still_delivered() {
    let (mut transport, state) = mock_transport();
    let channel = HciChannel::new();
    state
        .borrow_mut()
        .events_in
        .push_back(command_complete([0x03, 0x0C], 4));

    transport.pump(&channel).unwrap();

    let packet = channel.poll_controller_packet().expect("event delivered");
    assert_eq!(packet[0], h4_type::HCI_EVENT);
    assert_eq!(&packet[1..], &command_complete([0x03, 0x0C], 4)[..]);
}

/// Whatever the previous owner of a dongle left in its buffers is thrown
/// away before this session's first command goes out — see
/// `discard_stale_traffic`, and the hardware note there.
#[test]
fn test_stale_traffic_is_discarded_before_the_session_starts() {
    let state = Rc::new(RefCell::new(MockState::default()));
    state
        .borrow_mut()
        .events_in
        .push_back(command_complete([0x09, 0x10], 1));
    state
        .borrow_mut()
        .acl_in
        .push_back(vec![0x40, 0x00, 0x00, 0x00]);

    let mut endpoints = MockEndpoints(state.clone());
    discard_stale_traffic(&mut endpoints).unwrap();

    assert!(state.borrow().events_in.is_empty());
    assert!(state.borrow().acl_in.is_empty());
}

// --- choosing a dongle -------------------------------------------------------

/// A dongle fixture: only the fields selection looks at.
fn dongle(bus: &str, address: u8, ports: &[u8], vid: u16, pid: u16) -> UsbDongle {
    UsbDongle {
        index: 0,
        bus_id: bus.to_string(),
        device_address: address,
        port_chain: ports.to_vec(),
        vendor_id: vid,
        product_id: pid,
        manufacturer: Some("Generic".to_string()),
        product: Some("CSR8510 A10".to_string()),
        serial_number: None,
    }
}

/// The pair actually plugged into the machine this was written on: two
/// CSR8510 clones, same vid:pid, no serial numbers, behind one hub.
fn two_identical_dongles() -> (Vec<UsbDongle>, Vec<usize>) {
    let all = vec![
        dongle("02", 2, &[3], 0x0BDA, 0x5411), // the hub they hang off
        dongle("02", 4, &[3, 4], 0x0A12, 0x0001),
        dongle("02", 6, &[3, 1], 0x0A12, 0x0001),
    ];
    // Bluetooth-class positions, sorted by bus then address.
    (all, vec![1, 2])
}

#[test]
fn test_selector_parses_each_form_by_its_separator() {
    assert_eq!(
        UsbSelector::parse("0a12:0001").unwrap(),
        UsbSelector::VidPid(0x0A12, 0x0001)
    );
    assert_eq!(UsbSelector::parse("#0").unwrap(), UsbSelector::Index(0));
    assert_eq!(UsbSelector::parse("#12").unwrap(), UsbSelector::Index(12));
    assert_eq!(
        UsbSelector::parse("02/4").unwrap(),
        UsbSelector::BusAddress {
            bus_id: "02".to_string(),
            device_address: 4,
        }
    );
    assert_eq!(
        UsbSelector::parse("02.3.4").unwrap(),
        UsbSelector::BusPort {
            bus_id: "02".to_string(),
            port_chain: vec![3, 4],
        }
    );
    // Whitespace from a shell or a JSON argument is not a different device.
    assert_eq!(UsbSelector::parse("  #1  ").unwrap(), UsbSelector::Index(1));
}

/// A bare number is not an index. `2` would be an entirely reasonable way to
/// mean "#2" and an entirely reasonable way to mean bus 2 — so it means
/// neither, and says so, rather than picking one silently.
#[test]
fn test_selector_rejects_ambiguous_and_malformed_forms() {
    for spec in ["", "2", "abc", "#x", "02/999", "02/x", "02.999", "0a12:zz"] {
        assert!(
            UsbSelector::parse(spec).is_err(),
            "{spec:?} should not parse"
        );
    }
}

#[test]
fn test_selector_round_trips_through_its_description() {
    for spec in ["0a12:0001", "#3", "02/4", "02.3.4"] {
        let parsed = UsbSelector::parse(spec).unwrap();
        assert_eq!(parsed.describe(), spec);
        assert_eq!(UsbSelector::parse(&parsed.describe()).unwrap(), parsed);
    }
}

/// The bug this task exists to fix: with two dongles of one model, `vid:pid`
/// names both, and the answer is an error listing them — not the first one.
#[test]
fn test_vid_pid_matching_two_devices_is_an_error_that_names_them() {
    let (all, bluetooth) = two_identical_dongles();
    let err = resolve_selection(&UsbSelector::VidPid(0x0A12, 0x0001), &all, &bluetooth)
        .expect_err("ambiguous");
    let message = err.to_string();
    assert!(message.contains("matches 2"), "{message}");
    // Every way to disambiguate is in the message, so the next attempt works.
    for name in ["#0", "#1", "02/4", "02/6", "02.3.4", "02.3.1"] {
        assert!(message.contains(name), "{name} missing from {message}");
    }
}

#[test]
fn test_vid_pid_matching_exactly_one_device_still_works() {
    let (all, bluetooth) = two_identical_dongles();
    // The hub is not a Bluetooth device, and is still reachable by ID —
    // which is what makes vid:pid the form for a dongle hiding behind a
    // vendor-specific class code.
    assert_eq!(
        resolve_selection(&UsbSelector::VidPid(0x0BDA, 0x5411), &all, &bluetooth).unwrap(),
        0
    );
}

#[test]
fn test_index_selects_within_the_bluetooth_devices_only() {
    let (all, bluetooth) = two_identical_dongles();
    assert_eq!(
        resolve_selection(&UsbSelector::Index(0), &all, &bluetooth).unwrap(),
        1
    );
    assert_eq!(
        resolve_selection(&UsbSelector::Index(1), &all, &bluetooth).unwrap(),
        2
    );
    let err = resolve_selection(&UsbSelector::Index(2), &all, &bluetooth).expect_err("no #2");
    assert!(err.to_string().contains("2 Bluetooth-class"), "{err}");
}

#[test]
fn test_bus_address_and_bus_port_name_one_device_each() {
    let (all, bluetooth) = two_identical_dongles();
    assert_eq!(
        resolve_selection(&UsbSelector::parse("02/6").unwrap(), &all, &bluetooth).unwrap(),
        2
    );
    assert_eq!(
        resolve_selection(&UsbSelector::parse("02.3.1").unwrap(), &all, &bluetooth).unwrap(),
        2
    );
    // The two forms disagree about which is which — that is the point of
    // having both. `02/4` is the device at address 4; `02.3.4` is whatever
    // is in that socket, which today is the same device and after a re-plug
    // still is.
    assert_eq!(
        resolve_selection(&UsbSelector::parse("02/4").unwrap(), &all, &bluetooth).unwrap(),
        1
    );
    assert_eq!(
        resolve_selection(&UsbSelector::parse("02.3.4").unwrap(), &all, &bluetooth).unwrap(),
        1
    );
}

/// Linux zero-pads bus ids and a human does not. `1` and `001` are the same
/// bus, and comparing them as text says otherwise.
#[test]
fn test_bus_ids_compare_numerically_when_both_sides_are_numbers() {
    let all = vec![dongle("001", 4, &[3], 0x0A12, 0x0001)];
    let bluetooth = vec![0];
    for spec in ["1/4", "001/4", "01/4"] {
        assert_eq!(
            resolve_selection(&UsbSelector::parse(spec).unwrap(), &all, &bluetooth).unwrap(),
            0,
            "{spec}"
        );
    }
    assert!(
        resolve_selection(&UsbSelector::parse("2/4").unwrap(), &all, &bluetooth).is_err(),
        "a different bus is a different bus"
    );
}

#[test]
fn test_selectors_that_match_nothing_say_so() {
    let (all, bluetooth) = two_identical_dongles();
    for spec in ["8087:0025", "02/9", "02.9.9"] {
        let err = resolve_selection(&UsbSelector::parse(spec).unwrap(), &all, &bluetooth)
            .expect_err(spec);
        assert!(err.to_string().contains("no USB device matches"), "{err}");
    }
}

#[test]
fn test_first_takes_the_lowest_bus_and_address_and_errors_with_none() {
    let (all, bluetooth) = two_identical_dongles();
    assert_eq!(
        resolve_selection(&UsbSelector::First, &all, &bluetooth).unwrap(),
        1
    );
    let err = resolve_selection(&UsbSelector::First, &all, &[]).expect_err("nothing plugged in");
    assert!(err.to_string().contains("no Bluetooth-class"), "{err}");
}

/// The listing is what a chooser reads, so every name it accepts has to
/// appear in it.
#[test]
fn test_a_dongle_describes_itself_with_all_of_its_names() {
    let mut d = dongle("02", 4, &[3, 4], 0x0A12, 0x0001);
    d.index = 1;
    assert_eq!(d.address_selector(), "02/4");
    assert_eq!(d.port_selector(), "02.3.4");
    let described = d.describe();
    for name in ["#1", "0a12:0001", "02/4", "02.3.4", "Generic CSR8510 A10"] {
        assert!(described.contains(name), "{name} missing from {described}");
    }
}
