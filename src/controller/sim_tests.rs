use super::*;

fn addr(s: &str) -> Address {
    s.parse().unwrap()
}

/// LE Set Advertising Data (Flags 0x06) then LE Set Advertising Enable.
fn enable_adv(ch: &HciChannel) {
    ch.send_command(&[0x08, 0x20, 0x04, 0x03, 0x02, 0x01, 0x06])
        .unwrap();
    ch.send_command(&[0x0A, 0x20, 0x01, 0x01]).unwrap();
}
/// LE Set Scan Enable (enable = on).
fn enable_scan(ch: &HciChannel) {
    ch.send_command(&[0x0C, 0x20, 0x02, 0x01, 0x00]).unwrap();
}
/// Drain a host channel and return only the LE Meta subevents of `subevent`.
fn le_subevents(ch: &HciChannel, subevent: u8) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(p) = ch.poll_controller_packet() {
        if p.len() >= 4 && p[0] == h4_type::HCI_EVENT && p[1] == event::LE_META && p[3] == subevent
        {
            out.push(p);
        }
    }
    out
}

#[test]
fn test_advertising_reaches_every_scanner() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let s1 = link.add_device(addr("AA:BB:CC:00:00:02"));
    let s2 = link.add_device(addr("AA:BB:CC:00:00:03"));
    enable_adv(&a);
    enable_scan(&s1);
    enable_scan(&s2);

    link.tick();

    for s in [&s1, &s2] {
        let reports = le_subevents(s, event::LE_ADVERTISING_REPORT);
        assert_eq!(reports.len(), 1);
        let r = &reports[0];
        // p: 04 3E len | 02 num event_type addr_type | addr(6) | data_len data… rssi
        assert_eq!(&r[7..13], &addr_le(addr("AA:BB:CC:00:00:01")));
        let data_len = r[13] as usize;
        assert_eq!(&r[14..14 + data_len], &[0x02, 0x01, 0x06]);
    }
    assert!(le_subevents(&a, event::LE_ADVERTISING_REPORT).is_empty());
}

#[test]
fn test_many_advertisers_one_scanner() {
    let mut link = Link::new();
    let scanner = link.add_device(addr("AA:BB:CC:00:00:FF"));
    for i in 1..=5u8 {
        let adv = link.add_device(addr(&format!("AA:BB:CC:00:00:0{i}")));
        enable_adv(&adv);
    }
    enable_scan(&scanner);
    link.tick();
    assert_eq!(link.device_count(), 6);
    assert_eq!(
        le_subevents(&scanner, event::LE_ADVERTISING_REPORT).len(),
        5
    );
}

/// LE CS Create Config as a host sends it: 28 parameter bytes, with
/// `create_context = 1` so the peer is configured too and `role` at
/// offset 10.
fn cs_create_config(handle: u16, config_id: u8, role: u8) -> Vec<u8> {
    let mut params = Vec::with_capacity(28);
    params.extend_from_slice(&handle.to_le_bytes());
    params.push(config_id);
    params.push(0x01); // create context: both controllers
    params.push(0x02); // main mode: PBR
    params.push(0xFF); // sub mode: none
    params.push(0x03); // min main mode steps
    params.push(0x13); // max main mode steps
    params.push(0x00); // main mode repetition
    params.push(0x03); // mode 0 steps
    params.push(role);
    params.push(0x00); // RTT type
    params.push(0x01); // CS sync PHY: LE 1M
    params.extend_from_slice(&[0xFF; 10]); // channel map
    params.push(0x01); // channel map repetition
    params.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // selection / ch3c / companion
    let mut command = vec![0x90, 0x20, params.len() as u8];
    command.extend_from_slice(&params);
    command
}

/// Connects `central` to `peripheral` and returns the connection handle.
fn connect(link: &mut Link, central: &HciChannel, peripheral: &HciChannel, to: Address) -> u16 {
    enable_adv(peripheral);
    let mut cmd = vec![0x0D, 0x20, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00];
    cmd.extend_from_slice(&addr_le(to));
    central.send_command(&cmd).unwrap();
    link.tick();
    let cc = le_subevents(central, event::LE_CONNECTION_COMPLETE);
    let _ = le_subevents(peripheral, event::LE_CONNECTION_COMPLETE);
    u16::from_le_bytes([cc[0][5], cc[0][6]])
}

/// The RSSI byte an advertising report ends with.
fn report_rssi(report: &[u8]) -> i8 {
    *report.last().unwrap() as i8
}

#[test]
fn test_advertising_reports_carry_the_rssi_the_geometry_implies() {
    let mut link = Link::new();
    link.set_path_loss(PathLossModel {
        shadowing_sigma_db: 0.0, // isolate the distance term
        ..PathLossModel::default()
    });
    let advertiser_address = addr("AA:BB:CC:00:00:01");
    let adv = link.add_device(advertiser_address);
    let scan = link.add_device(addr("AA:BB:CC:00:00:02"));
    enable_adv(&adv);
    enable_scan(&scan);

    let mut readings = Vec::new();
    for distance in [1.0, 4.0, 16.0] {
        assert!(link.set_position(advertiser_address, Position::new(distance, 0.0)));
        link.tick();
        let reports = le_subevents(&scan, event::LE_ADVERTISING_REPORT);
        readings.push(report_rssi(&reports[0]));
    }
    assert!(
        readings.windows(2).all(|w| w[1] < w[0]),
        "RSSI must fall as the advertiser moves away: {readings:?}"
    );
    // Two doublings at n = 2.7 is 10·2.7·log10(4) ≈ 16.3 dB.
    assert!(
        (f64::from(readings[0] - readings[1]) - 16.3).abs() < 1.5,
        "{readings:?}"
    );
}

#[test]
fn test_shadowing_makes_a_stationary_devices_rssi_jitter() {
    // The single most misleading thing the old constant did: hold still
    // and RSSI never moved, so any estimate looked rock solid.
    let mut link = Link::new();
    link.set_noise_seed(4);
    let advertiser = addr("AA:BB:CC:00:00:01");
    let adv = link.add_device(advertiser);
    let scan = link.add_device(addr("AA:BB:CC:00:00:02"));
    link.set_position(advertiser, Position::new(5.0, 0.0));
    enable_adv(&adv);
    enable_scan(&scan);

    let mut readings = Vec::new();
    for _ in 0..24 {
        link.tick();
        for report in le_subevents(&scan, event::LE_ADVERTISING_REPORT) {
            readings.push(report_rssi(&report));
        }
    }
    let distinct: std::collections::BTreeSet<i8> = readings.iter().copied().collect();
    assert!(
        distinct.len() > 3,
        "a stationary device's RSSI should still move: {distinct:?}"
    );
}

#[test]
fn test_a_device_that_never_moved_is_at_the_origin() {
    let mut link = Link::new();
    let a = addr("AA:BB:CC:00:00:01");
    let b = addr("AA:BB:CC:00:00:02");
    link.add_device(a);
    link.add_device(b);
    assert_eq!(link.distance_between(a, b), Some(0.0));
    link.set_position(b, Position::new(3.0, 4.0));
    assert_eq!(link.distance_between(a, b), Some(5.0));
    assert!(!link.set_position(addr("AA:BB:CC:00:00:09"), Position::default()));
    assert!(
        link.distance_between(a, addr("AA:BB:CC:00:00:09"))
            .is_none()
    );
}

#[test]
fn test_channel_sounding_tones_recover_the_true_separation() {
    // The end-to-end claim of the whole ranging path: the radio is told
    // where the devices are, the two hosts are told nothing but their own
    // tones, and combining the two sets recovers the distance.
    let mut link = Link::new();
    link.set_noise_seed(2);
    let initiator_address = addr("AA:BB:CC:00:00:01");
    let reflector_address = addr("AA:BB:CC:00:00:02");
    let initiator = link.add_device(initiator_address);
    let reflector = link.add_device(reflector_address);

    let truth = 7.25;
    link.set_position(reflector_address, Position::new(truth, 0.0));
    let handle = connect(&mut link, &initiator, &reflector, reflector_address);

    initiator
        .send_command(&cs_create_config(handle, 1, 0x00))
        .unwrap();
    link.tick();
    assert_eq!(
        le_subevents(&initiator, event::LE_CS_CONFIG_COMPLETE).len(),
        1
    );
    assert_eq!(
        le_subevents(&reflector, event::LE_CS_CONFIG_COMPLETE).len(),
        1,
        "the reflector's host must be told it is in a procedure"
    );

    // LE CS Procedure Enable: handle(2) config_id(1) enable(1).
    let mut enable = vec![0x94, 0x20, 0x04];
    enable.extend_from_slice(&handle.to_le_bytes());
    enable.extend_from_slice(&[0x01, 0x01]);
    initiator.send_command(&enable).unwrap();
    link.tick();
    assert_eq!(
        le_subevents(&initiator, event::LE_CS_PROCEDURE_ENABLE_COMPLETE).len(),
        1
    );
    let _ = le_subevents(&reflector, event::LE_CS_PROCEDURE_ENABLE_COMPLETE);

    link.tick();
    let local = subevent_tones(&initiator);
    let remote = subevent_tones(&reflector);
    assert_eq!(local.tones.len(), cs_plan::TONES_PER_SUBEVENT);
    assert_eq!(remote.tones.len(), cs_plan::TONES_PER_SUBEVENT);
    assert_eq!(
        local.procedure_counter, remote.procedure_counter,
        "both ends must label the same procedure the same way"
    );

    let estimate = crate::cs::estimate_from_tones(&local.tones, &remote.tones)
        .expect("an estimate from the radio's own tones");
    assert!(
        (estimate.distance_m - truth).abs() < 0.25,
        "true {truth} m, estimated {} m (±{})",
        estimate.distance_m,
        estimate.std_error_m
    );
}

#[test]
fn test_one_ends_tones_alone_say_nothing_about_distance() {
    // Why the Ranging Service exists, asserted against the radio: the
    // initiator's own subevent results contain no recoverable distance,
    // because the oscillator offset is redrawn on every hop.
    let mut link = Link::new();
    link.set_noise_seed(6);
    let reflector_address = addr("AA:BB:CC:00:00:02");
    let initiator = link.add_device(addr("AA:BB:CC:00:00:01"));
    let reflector = link.add_device(reflector_address);
    link.set_position(reflector_address, Position::new(9.0, 0.0));
    let handle = connect(&mut link, &initiator, &reflector, reflector_address);
    initiator
        .send_command(&cs_create_config(handle, 1, 0x00))
        .unwrap();
    let mut enable = vec![0x94, 0x20, 0x04];
    enable.extend_from_slice(&handle.to_le_bytes());
    enable.extend_from_slice(&[0x01, 0x01]);
    initiator.send_command(&enable).unwrap();
    link.tick();
    link.tick();

    let local = subevent_tones(&initiator);
    // Pretend the peer reported a flat zero phase — i.e. skip the RAS
    // transfer and fit the local tones alone.
    let flat: Vec<crate::cs::Tone> = local
        .tones
        .iter()
        .map(|t| crate::cs::Tone {
            i: 2047,
            q: 0,
            ..*t
        })
        .collect();
    let alone = crate::cs::estimate_from_tones(&local.tones, &flat).expect("a fit");
    assert!(
        (alone.distance_m - 9.0).abs() > 1.0,
        "one end alone landed at {} m, which would mean the model is wrong",
        alone.distance_m
    );
}

#[test]
fn test_no_measurements_are_produced_until_a_procedure_is_enabled() {
    let mut link = Link::new();
    let reflector_address = addr("AA:BB:CC:00:00:02");
    let initiator = link.add_device(addr("AA:BB:CC:00:00:01"));
    let reflector = link.add_device(reflector_address);
    link.set_position(reflector_address, Position::new(3.0, 0.0));
    let handle = connect(&mut link, &initiator, &reflector, reflector_address);

    link.tick();
    assert!(
        le_subevents(&initiator, event::LE_CS_SUBEVENT_RESULT).is_empty(),
        "a connection alone is not a Channel Sounding procedure"
    );

    initiator
        .send_command(&cs_create_config(handle, 1, 0x00))
        .unwrap();
    link.tick();
    let _ = le_subevents(&initiator, event::LE_CS_CONFIG_COMPLETE);
    link.tick();
    assert!(
        le_subevents(&initiator, event::LE_CS_SUBEVENT_RESULT).is_empty(),
        "a configuration alone is not a procedure either"
    );
}

/// Drains `channel` and parses the first LE CS Subevent Result on it.
fn subevent_tones(channel: &HciChannel) -> crate::cs::SubeventResult {
    let events = le_subevents(channel, event::LE_CS_SUBEVENT_RESULT);
    let body = &events.first().expect("a subevent result")[3..];
    crate::cs::parse_subevent_result(body).expect("parsed")
}

#[test]
fn test_connection_and_acl_roundtrip() {
    let mut link = Link::new();
    let central = link.add_device(addr("AA:BB:CC:00:00:01"));
    let peripheral = link.add_device(addr("AA:BB:CC:00:00:02"));
    enable_adv(&peripheral);

    // Central issues LE Create Connection to the peripheral's address.
    // params: scan_interval(2) scan_window(2) filter_policy(1)
    //         peer_addr_type(1) peer_addr(6) …
    let mut cmd = vec![0x0D, 0x20, 0x0C, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00];
    cmd.extend_from_slice(&addr_le(addr("AA:BB:CC:00:00:02")));
    central.send_command(&cmd).unwrap();

    link.tick();

    let cc = le_subevents(&central, event::LE_CONNECTION_COMPLETE);
    let pc = le_subevents(&peripheral, event::LE_CONNECTION_COMPLETE);
    assert_eq!(cc.len(), 1);
    assert_eq!(pc.len(), 1);
    let handle = u16::from_le_bytes([cc[0][5], cc[0][6]]);
    assert_eq!(handle, u16::from_le_bytes([pc[0][5], pc[0][6]]));
    assert_eq!(cc[0][7], 0x00); // central role
    assert_eq!(pc[0][7], 0x01); // peripheral role

    // Central sends ACL on the connection; the peripheral's host receives it.
    let payload = [0xDE, 0xAD, 0xBE, 0xEF];
    let mut acl = vec![handle as u8, (handle >> 8) as u8, 0x04, 0x00];
    acl.extend_from_slice(&payload);
    central.send_acl_data(&acl).unwrap();
    link.tick();
    let got = peripheral.poll_controller_packet().expect("acl delivered");
    assert_eq!(got[0], h4_type::HCI_ACL_DATA);
    assert_eq!(&got[5..9], &payload);
}

// ---------------------------------------------------------------------
// BR/EDR (Bluetooth Classic)
//
// The first block is one test per command, each asserting *which event
// answers it*. That is deliberate and it is not redundant with the
// end-to-end test: this project has shipped the same bug four times — a
// command answered with a Command Complete where the host was waiting on
// a Command Status plus a later completion event — and an end-to-end
// test cannot catch it, because a host that hangs looks exactly like a
// host that is merely slow.
// ---------------------------------------------------------------------

/// Build an HCI command body: opcode then parameter length then
/// parameters. `HciChannel::send_command` adds the H4 type byte.
fn cmd(opcode: u16, params: &[u8]) -> Vec<u8> {
    let mut p = opcode.to_le_bytes().to_vec();
    p.push(params.len() as u8);
    p.extend_from_slice(params);
    p
}

/// Every HCI event the host has been handed, as (code, parameters).
fn events(ch: &HciChannel) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    while let Some(p) = ch.poll_controller_packet() {
        if p.first() == Some(&h4_type::HCI_EVENT) && p.len() >= 3 {
            out.push((p[1], p[3..].to_vec()));
        }
    }
    out
}

/// The status byte of the Command Status answering `opcode`, if one came.
fn command_status_for(evts: &[(u8, Vec<u8>)], opcode: u16) -> Option<u8> {
    evts.iter().find_map(|(code, params)| {
        (*code == event::COMMAND_STATUS
            && params.len() >= 4
            && u16::from_le_bytes([params[2], params[3]]) == opcode)
            .then(|| params[0])
    })
}

/// The return parameters of the Command Complete answering `opcode`.
fn command_complete_for(evts: &[(u8, Vec<u8>)], opcode: u16) -> Option<Vec<u8>> {
    evts.iter().find_map(|(code, params)| {
        (*code == event::COMMAND_COMPLETE
            && params.len() >= 3
            && u16::from_le_bytes([params[1], params[2]]) == opcode)
            .then(|| params[3..].to_vec())
    })
}

/// Which event codes arrived, in order.
fn event_codes(evts: &[(u8, Vec<u8>)]) -> Vec<u8> {
    evts.iter().map(|(code, _)| *code).collect()
}

/// A 248-byte NUL-padded Write Local Name parameter.
fn name_param(name: &str) -> Vec<u8> {
    let mut p = vec![0u8; 248];
    let b = name.as_bytes();
    p[..b.len()].copy_from_slice(b);
    p
}

/// The parameters of a Create Connection naming `peer` (little-endian).
fn page_params(peer: [u8; 6]) -> Vec<u8> {
    let mut p = peer.to_vec();
    p.extend_from_slice(&[
        0x18, 0xCC, // packet type
        0x01, 0x00, // page scan repetition mode, reserved
        0x00, 0x00, // clock offset
        0x01, // allow role switch
    ]);
    p
}

/// Address `AA:BB:CC:00:00:01` on the wire, and its peer `…:02`.
const WIRE_A: [u8; 6] = [0x01, 0x00, 0x00, 0xCC, 0xBB, 0xAA];
const WIRE_B: [u8; 6] = [0x02, 0x00, 0x00, 0xCC, 0xBB, 0xAA];

/// Bring a classic device up the way a real host does: name, Class of
/// Device, then Scan Enable.
fn classic_bring_up(ch: &HciChannel, name: &str, scan: u8) {
    ch.send_command(&cmd(opcode::WRITE_LOCAL_NAME, &name_param(name)))
        .unwrap();
    ch.send_command(&cmd(opcode::WRITE_CLASS_OF_DEVICE, &[0x04, 0x04, 0x24]))
        .unwrap();
    ch.send_command(&cmd(opcode::WRITE_SCAN_ENABLE, &[scan]))
        .unwrap();
}

/// Two devices, connected: A pages B, B accepts. Returns the handle.
fn connect_classic(link: &mut Link, a: &HciChannel, b: &HciChannel) -> u16 {
    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params(WIRE_B)))
        .unwrap();
    link.tick();
    let mut accept = WIRE_A.to_vec();
    accept.push(0x01); // stay peripheral
    b.send_command(&cmd(opcode::ACCEPT_CONNECTION_REQUEST, &accept))
        .unwrap();
    link.tick();
    let evts = events(a);
    let (_, complete) = evts
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
        .expect("the page must complete");
    assert_eq!(complete[0], STATUS_SUCCESS);
    u16::from_le_bytes([complete[1], complete[2]])
}

#[test]
fn test_inquiry_is_answered_with_command_status_then_results_then_complete() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Findable", 0x03);
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::INQUIRY, &[0x33, 0x8B, 0x9E, 0x01, 0x00]))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::INQUIRY),
        Some(STATUS_SUCCESS),
        "Inquiry is answered with a Command Status, never a Command \
             Complete: {evts:?}"
    );
    assert!(
        command_complete_for(&evts, opcode::INQUIRY).is_none(),
        "a Command Complete for Inquiry strands a host waiting on \
             Inquiry Complete"
    );
    let codes = event_codes(&evts);
    let result = codes
        .iter()
        .position(|c| *c == event::INQUIRY_RESULT)
        .expect("a discoverable device must be reported");
    let complete = codes
        .iter()
        .position(|c| *c == event::INQUIRY_COMPLETE)
        .expect("an inquiry must end, or discovery never finishes");
    assert!(
        result < complete,
        "results must precede Inquiry Complete, which means 'that is \
             everything': {codes:?}"
    );
}

#[test]
fn test_inquiry_reports_the_peers_class_of_device_and_address() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Findable", 0x03);
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::INQUIRY, &[0x33, 0x8B, 0x9E, 0x01, 0x00]))
        .unwrap();
    link.tick();

    let evts = events(&a);
    let (_, params) = evts
        .iter()
        .find(|(code, _)| *code == event::INQUIRY_RESULT)
        .expect("one result");
    assert_eq!(params[0], 1, "Num_Responses");
    assert_eq!(&params[1..7], &WIRE_B, "BD_ADDR, little-endian");
    // BD_ADDR(6) + PSRM(1) + Reserved(2) = 9, then Class of Device.
    assert_eq!(
        &params[10..13],
        &[0x04, 0x04, 0x24],
        "the Class of Device the peer's host wrote — this is what a \
             scanning UI renders as a headset icon"
    );
    assert_eq!(params.len(), 1 + 14, "one 14-octet response");
}

#[test]
fn test_inquiry_does_not_find_a_device_that_is_not_discoverable() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let _quiet = link.add_device(addr("AA:BB:CC:00:00:02"));
    let page_only = link.add_device(addr("AA:BB:CC:00:00:03"));
    // `_quiet` never writes Scan Enable at all; `page_only` enables page
    // scan but not inquiry scan.
    classic_bring_up(&page_only, "PageOnly", 0x02);
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::INQUIRY, &[0x33, 0x8B, 0x9E, 0x01, 0x00]))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert!(
        !event_codes(&evts).contains(&event::INQUIRY_RESULT),
        "neither a device that never enabled scanning nor one that is \
             only connectable may appear in an inquiry: {evts:?}"
    );
    assert!(
        event_codes(&evts).contains(&event::INQUIRY_COMPLETE),
        "an inquiry that finds nothing must still complete"
    );
}

#[test]
fn test_inquiry_cancel_is_answered_with_command_complete_and_no_inquiry_complete() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Findable", 0x03);
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::INQUIRY, &[0x33, 0x8B, 0x9E, 0x08, 0x00]))
        .unwrap();
    a.send_command(&cmd(opcode::INQUIRY_CANCEL, &[])).unwrap();
    link.tick();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_complete_for(&evts, opcode::INQUIRY_CANCEL),
        Some(vec![STATUS_SUCCESS]),
        "Inquiry Cancel is one of the few BR/EDR commands answered with \
             a Command Complete: {evts:?}"
    );
    assert!(
        !event_codes(&evts).contains(&event::INQUIRY_COMPLETE),
        "a cancelled inquiry sends no Inquiry Complete (Vol 4, Part E, \
             Section 7.1.2) — a host that waits for one waits forever on \
             real hardware too"
    );
}

#[test]
fn test_create_connection_is_answered_with_command_status_then_a_page_at_the_peer() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let _ = events(&a);
    let _ = events(&b);

    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params(WIRE_B)))
        .unwrap();
    link.tick();

    let a_evts = events(&a);
    assert_eq!(
        command_status_for(&a_evts, opcode::CREATE_CONNECTION),
        Some(STATUS_SUCCESS),
        "Create Connection answers with a Command Status; a Command \
             Complete here is the bug that hangs a pairing host: {a_evts:?}"
    );
    assert!(
        command_complete_for(&a_evts, opcode::CREATE_CONNECTION).is_none(),
        "and never with a Command Complete"
    );
    assert!(
        !event_codes(&a_evts).contains(&event::CONNECTION_COMPLETE),
        "the initiator is not connected until the peer's host accepts"
    );

    let b_evts = events(&b);
    let (_, request) = b_evts
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_REQUEST)
        .expect("the paged device's host must see a Connection Request");
    assert_eq!(&request[0..6], &WIRE_A, "naming who is paging it");
    assert_eq!(&request[6..9], &[0x00, 0x00, 0x00], "initiator's CoD");
    assert_eq!(request[9], LINK_TYPE_ACL);
}

#[test]
fn test_accept_connection_request_is_answered_with_status_then_completes_both_ends() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params(WIRE_B)))
        .unwrap();
    link.tick();
    let _ = events(&a);
    let _ = events(&b);

    let mut accept = WIRE_A.to_vec();
    accept.push(0x01); // stay peripheral
    b.send_command(&cmd(opcode::ACCEPT_CONNECTION_REQUEST, &accept))
        .unwrap();
    link.tick();

    let b_evts = events(&b);
    assert_eq!(
        command_status_for(&b_evts, opcode::ACCEPT_CONNECTION_REQUEST),
        Some(STATUS_SUCCESS),
        "Accept Connection Request answers with a Command Status: {b_evts:?}"
    );
    assert!(command_complete_for(&b_evts, opcode::ACCEPT_CONNECTION_REQUEST).is_none());

    let a_evts = events(&a);
    let (_, a_complete) = a_evts
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
        .expect("the initiator must be told the connection came up");
    let (_, b_complete) = b_evts
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
        .expect("the acceptor must be told too");
    assert_eq!(a_complete[0], STATUS_SUCCESS);
    assert_eq!(b_complete[0], STATUS_SUCCESS);
    assert_eq!(
        &a_complete[1..3],
        &b_complete[1..3],
        "one link, one handle — the handle is the only name the ACL \
             router knows"
    );
    assert_eq!(
        &a_complete[3..9],
        &WIRE_B,
        "each is told the other's address"
    );
    assert_eq!(&b_complete[3..9], &WIRE_A);
    assert_eq!(a_complete[9], LINK_TYPE_ACL);
}

#[test]
fn test_reject_connection_request_is_answered_with_status_and_completes_with_the_reason() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Grumpy", 0x03);
    link.tick();
    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params(WIRE_B)))
        .unwrap();
    link.tick();
    let _ = events(&a);
    let _ = events(&b);

    let mut reject = WIRE_A.to_vec();
    reject.push(STATUS_CONNECTION_REJECTED_RESOURCES);
    b.send_command(&cmd(opcode::REJECT_CONNECTION_REQUEST, &reject))
        .unwrap();
    link.tick();

    let b_evts = events(&b);
    assert_eq!(
        command_status_for(&b_evts, opcode::REJECT_CONNECTION_REQUEST),
        Some(STATUS_SUCCESS),
        "the *command* succeeded even though the connection did not"
    );
    let a_evts = events(&a);
    let (_, a_complete) = a_evts
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
        .expect("a refused page still owes the initiator a completion");
    assert_eq!(
        a_complete[0], STATUS_CONNECTION_REJECTED_RESOURCES,
        "carrying the reason the peer's host gave"
    );
    let (_, b_complete) = b_evts
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
        .expect(
            "and the rejecting host is owed one too — its Reject \
                     Connection Request was answered with a Command Status, \
                     which is a promise of an event to come",
        );
    assert_eq!(b_complete[0], STATUS_CONNECTION_REJECTED_RESOURCES);
}

#[test]
fn test_answering_a_page_nobody_sent_is_refused_with_a_command_status() {
    // The wrong-event-type trap in miniature: an error answer to a
    // status-type command must still be a Command *Status*.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    let mut accept = [0x99, 0x00, 0x00, 0xCC, 0xBB, 0xAA].to_vec();
    accept.push(0x01);
    a.send_command(&cmd(opcode::ACCEPT_CONNECTION_REQUEST, &accept))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::ACCEPT_CONNECTION_REQUEST),
        Some(STATUS_UNKNOWN_CONNECTION),
        "refusal comes back as a Command Status, not a Command \
             Complete: {evts:?}"
    );
    assert!(command_complete_for(&evts, opcode::ACCEPT_CONNECTION_REQUEST).is_none());
}

#[test]
fn test_paging_a_device_that_is_not_connectable_ends_in_page_timeout() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    // Discoverable but not connectable: findable, unpageable.
    classic_bring_up(&b, "Shy", 0x01);
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params(WIRE_B)))
        .unwrap();
    for _ in 0..PAGE_TIMEOUT_TICKS + 1 {
        link.tick();
    }

    let evts = events(&a);
    let (_, complete) = evts
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
        .expect("a page nobody answers must still end, or the host waits forever");
    assert_eq!(complete[0], STATUS_PAGE_TIMEOUT);
}

#[test]
fn test_paging_an_address_that_is_nobody_ends_in_page_timeout() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params([0xEE; 6])))
        .unwrap();
    for _ in 0..PAGE_TIMEOUT_TICKS + 1 {
        link.tick();
    }

    let evts = events(&a);
    assert_eq!(
        evts.iter()
            .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
            .map(|(_, p)| p[0]),
        Some(STATUS_PAGE_TIMEOUT)
    );
}

#[test]
fn test_a_page_whose_host_never_answers_times_out_and_frees_the_peer() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Silent", 0x03);
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params(WIRE_B)))
        .unwrap();
    // B's host sees the Connection Request and simply never answers it.
    for _ in 0..PAGE_TIMEOUT_TICKS + 1 {
        link.tick();
    }
    let evts = events(&a);
    assert_eq!(
        evts.iter()
            .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
            .map(|(_, p)| p[0]),
        Some(STATUS_PAGE_TIMEOUT),
        "an unanswered Connection Request must not leave the initiator \
             waiting for ever: {evts:?}"
    );
    let _ = events(&b);

    // And B must be free to field the next page, not stuck holding the
    // stale one — a state with no exit is the other half of this
    // project's recurring bug.
    assert_eq!(
        connect_classic(&mut link, &a, &b),
        0x0001,
        "the peer takes a fresh page after the previous one timed out"
    );
}

#[test]
fn test_create_connection_cancel_completes_then_ends_the_page() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    let peer = [0xEE; 6];
    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params(peer)))
        .unwrap();
    a.send_command(&cmd(opcode::CREATE_CONNECTION_CANCEL, &peer))
        .unwrap();
    link.tick();

    let evts = events(&a);
    let ret = command_complete_for(&evts, opcode::CREATE_CONNECTION_CANCEL)
        .expect("Create Connection Cancel answers with a Command Complete");
    assert_eq!(ret[0], STATUS_SUCCESS);
    assert_eq!(&ret[1..7], &peer, "the Command Complete echoes the address");
    let (_, complete) = evts
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_COMPLETE)
        .expect("a cancelled page still owes a Connection Complete");
    assert_eq!(
        complete[0], STATUS_UNKNOWN_CONNECTION,
        "carrying Unknown Connection Identifier (Vol 4, Part E, Section \
             7.1.7), not Page Timeout and not success"
    );
}

#[test]
fn test_remote_name_request_is_answered_with_status_then_the_name() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Simble Classic", 0x03);
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(
        opcode::REMOTE_NAME_REQUEST,
        &[0x02, 0x00, 0x00, 0xCC, 0xBB, 0xAA, 0x01, 0x00, 0x00, 0x00],
    ))
    .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::REMOTE_NAME_REQUEST),
        Some(STATUS_SUCCESS),
        "Remote Name Request answers with a Command Status: {evts:?}"
    );
    assert!(command_complete_for(&evts, opcode::REMOTE_NAME_REQUEST).is_none());

    let (_, params) = evts
        .iter()
        .find(|(code, _)| *code == event::REMOTE_NAME_REQUEST_COMPLETE)
        .expect("and then a Remote Name Request Complete");
    assert_eq!(params[0], STATUS_SUCCESS);
    assert_eq!(&params[1..7], &WIRE_B);
    assert_eq!(params.len(), 255, "the name field is a fixed 248 bytes");
    assert_eq!(
        String::from_utf8_lossy(&params[7..]).trim_end_matches('\0'),
        "Simble Classic",
        "the name is whatever the peer's host wrote with Write Local Name"
    );
}

#[test]
fn test_remote_name_request_for_an_unreachable_device_still_completes() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    // Discoverable but not connectable: a Remote Name Request pages the
    // device, so it gets no answer — which is exactly what an "unknown
    // device" entry in a phone's Bluetooth list means.
    classic_bring_up(&b, "Shy", 0x01);
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(
        opcode::REMOTE_NAME_REQUEST,
        &[0x02, 0x00, 0x00, 0xCC, 0xBB, 0xAA, 0x01, 0x00, 0x00, 0x00],
    ))
    .unwrap();
    link.tick();

    let evts = events(&a);
    let (_, params) = evts
        .iter()
        .find(|(code, _)| *code == event::REMOTE_NAME_REQUEST_COMPLETE)
        .expect("an unanswerable name request must still complete");
    assert_eq!(params[0], STATUS_PAGE_TIMEOUT);
}

#[test]
fn test_scan_enable_and_name_and_class_of_device_round_trip() {
    // The Write/Read pairs are all Command Complete, and the Reads prove
    // the Writes were stored rather than merely acknowledged — the
    // catch-all would have passed the Writes and failed the Reads.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    classic_bring_up(&a, "RoundTrip", 0x03);
    a.send_command(&cmd(opcode::READ_SCAN_ENABLE, &[])).unwrap();
    a.send_command(&cmd(opcode::READ_CLASS_OF_DEVICE, &[]))
        .unwrap();
    a.send_command(&cmd(opcode::READ_LOCAL_NAME, &[])).unwrap();
    link.tick();

    let evts = events(&a);
    for opcode in [
        opcode::WRITE_LOCAL_NAME,
        opcode::WRITE_CLASS_OF_DEVICE,
        opcode::WRITE_SCAN_ENABLE,
    ] {
        assert_eq!(
            command_complete_for(&evts, opcode),
            Some(vec![STATUS_SUCCESS]),
            "the Write commands are Command Complete, not Command Status"
        );
    }
    assert_eq!(
        command_complete_for(&evts, opcode::READ_SCAN_ENABLE),
        Some(vec![STATUS_SUCCESS, 0x03])
    );
    assert_eq!(
        command_complete_for(&evts, opcode::READ_CLASS_OF_DEVICE),
        Some(vec![STATUS_SUCCESS, 0x04, 0x04, 0x24])
    );
    let name = command_complete_for(&evts, opcode::READ_LOCAL_NAME).unwrap();
    assert_eq!(name[0], STATUS_SUCCESS);
    assert_eq!(
        String::from_utf8_lossy(&name[1..]).trim_end_matches('\0'),
        "RoundTrip"
    );
}

#[test]
fn test_reset_makes_a_classic_device_invisible_again() {
    // Scan Enable is 0x00 at power-on, which is why every BR/EDR bring-up
    // writes it *after* the Reset. A simulator that let it survive would
    // hide a real ordering bug.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Findable", 0x03);
    link.tick();
    b.send_command(&cmd(opcode::RESET, &[])).unwrap();
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::INQUIRY, &[0x33, 0x8B, 0x9E, 0x01, 0x00]))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert!(
        !event_codes(&evts).contains(&event::INQUIRY_RESULT),
        "a device that has been Reset is no longer discoverable: {evts:?}"
    );
}

#[test]
fn test_a_second_page_to_a_peer_already_connected_is_refused() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    connect_classic(&mut link, &a, &b);
    let _ = events(&a);

    a.send_command(&cmd(opcode::CREATE_CONNECTION, &page_params(WIRE_B)))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::CREATE_CONNECTION),
        Some(STATUS_CONNECTION_ALREADY_EXISTS),
        "BR/EDR allows one ACL link per pair of devices: {evts:?}"
    );
}

#[test]
fn test_acl_is_routed_between_two_connected_classic_devices() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let handle = connect_classic(&mut link, &a, &b);
    let _ = events(&b);

    let payload = [0xC0, 0xFF, 0xEE];
    let mut acl = vec![handle as u8, (handle >> 8) as u8, 0x03, 0x00];
    acl.extend_from_slice(&payload);

    a.send_acl_data(&acl).unwrap();
    link.tick();
    let got = b.poll_controller_packet().expect("ACL reaches the peer");
    assert_eq!(got[0], h4_type::HCI_ACL_DATA);
    assert_eq!(&got[5..8], &payload);

    // And back, on the same handle — one link, addressed from both ends.
    b.send_acl_data(&acl).unwrap();
    link.tick();
    let got = a.poll_controller_packet().expect("and back again");
    assert_eq!(&got[5..8], &payload);
}

#[test]
fn test_disconnecting_a_classic_link_tells_both_hosts() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let handle = connect_classic(&mut link, &a, &b);
    let _ = events(&b);

    let mut params = handle.to_le_bytes().to_vec();
    params.push(REASON_REMOTE_USER);
    a.send_command(&cmd(opcode::DISCONNECT, &params)).unwrap();
    link.tick();

    for (who, ch) in [("initiator", &a), ("acceptor", &b)] {
        let evts = events(ch);
        assert!(
            event_codes(&evts).contains(&event::DISCONNECTION_COMPLETE),
            "the {who} must be told the link is gone: {evts:?}"
        );
    }
}

// --- SCO / eSCO: the call-audio link ---------------------------------

/// Setup Synchronous Connection as a host sends it (Vol 4, Part E,
/// Section 7.1.26): 17 parameter bytes, with the Voice Setting *before*
/// the retransmission effort and the packet types.
fn setup_sco_params(acl_handle: u16, voice_setting: u16, packet_type: u16) -> Vec<u8> {
    let mut params = Vec::with_capacity(17);
    params.extend_from_slice(&acl_handle.to_le_bytes());
    params.extend_from_slice(&8000u32.to_le_bytes()); // Transmit_Bandwidth
    params.extend_from_slice(&8000u32.to_le_bytes()); // Receive_Bandwidth
    params.extend_from_slice(&0xFFFFu16.to_le_bytes()); // Max_Latency: don't care
    params.extend_from_slice(&voice_setting.to_le_bytes());
    params.push(0xFF); // Retransmission_Effort: don't care
    params.extend_from_slice(&packet_type.to_le_bytes());
    params
}

/// Accept Synchronous Connection Request (Section 7.1.27): the same 15
/// bytes with a BD_ADDR in front of them instead of a handle.
fn accept_sco_params(peer: [u8; 6], voice_setting: u16, packet_type: u16) -> Vec<u8> {
    let mut params = peer.to_vec();
    params.extend_from_slice(&setup_sco_params(0, voice_setting, packet_type)[2..]);
    params
}

/// HV1|HV2|HV3 — a plain SCO link.
const SCO_PACKET_TYPES: u16 = 0x0007;
/// EV3 — an extended (eSCO) link, which is what mSBC rides.
const ESCO_PACKET_TYPES_EV3: u16 = 0x0008;
/// Voice Setting 0x0060: CVSD air coding, 16-bit linear input.
const VOICE_SETTING_CVSD: u16 = 0x0060;
/// Voice Setting 0x0063: transparent air coding — the controller passes
/// the payload through, which is how wideband speech is carried.
const VOICE_SETTING_TRANSPARENT: u16 = 0x0063;

/// The Synchronous Connection Complete events on a channel.
fn sync_completes(evts: &[(u8, Vec<u8>)]) -> Vec<Vec<u8>> {
    evts.iter()
        .filter(|(code, _)| *code == event::SYNCHRONOUS_CONNECTION_COMPLETE)
        .map(|(_, p)| p.clone())
        .collect()
}

/// Bring up an ACL and then a SCO link on it, returning both handles.
fn connect_sco(
    link: &mut Link,
    a: &HciChannel,
    b: &HciChannel,
    voice_setting: u16,
    packet_type: u16,
) -> (u16, u16) {
    classic_bring_up(b, "Acceptor", 0x03);
    link.tick();
    let acl_handle = connect_classic(link, a, b);
    let _ = (events(a), events(b));

    a.send_command(&cmd(
        opcode::SETUP_SYNCHRONOUS_CONNECTION,
        &setup_sco_params(acl_handle, voice_setting, packet_type),
    ))
    .unwrap();
    link.tick();
    b.send_command(&cmd(
        opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST,
        &accept_sco_params(WIRE_A, voice_setting, packet_type),
    ))
    .unwrap();
    link.tick();

    let completes = sync_completes(&events(a));
    let complete = completes.first().expect("the setup must complete");
    assert_eq!(complete[0], STATUS_SUCCESS, "{complete:?}");
    let _ = events(b);
    (acl_handle, u16::from_le_bytes([complete[1], complete[2]]))
}

#[test]
fn test_setup_synchronous_connection_is_answered_with_status_then_asks_the_peer() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let acl_handle = connect_classic(&mut link, &a, &b);
    let _ = (events(&a), events(&b));

    a.send_command(&cmd(
        opcode::SETUP_SYNCHRONOUS_CONNECTION,
        &setup_sco_params(acl_handle, VOICE_SETTING_CVSD, SCO_PACKET_TYPES),
    ))
    .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::SETUP_SYNCHRONOUS_CONNECTION),
        Some(STATUS_SUCCESS),
        "{evts:?}"
    );
    assert!(
        command_complete_for(&evts, opcode::SETUP_SYNCHRONOUS_CONNECTION).is_none(),
        "a Command Complete here hangs the host: {evts:?}"
    );
    assert!(
        sync_completes(&evts).is_empty(),
        "nothing completes until the far end has answered: {evts:?}"
    );

    // The peer's host learns about it exactly one way: a Connection
    // Request whose link type is *not* ACL.
    let peer = events(&b);
    let (_, request) = peer
        .iter()
        .find(|(code, _)| *code == event::CONNECTION_REQUEST)
        .expect("the peer's host must be asked");
    assert_eq!(&request[0..6], &WIRE_A, "BD_ADDR, little-endian");
    assert_eq!(
        request[9], LINK_TYPE_SCO,
        "HV-only packet types mean a plain SCO link"
    );
}

#[test]
fn test_accept_synchronous_connection_request_completes_both_ends_with_one_handle() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let acl_handle = connect_classic(&mut link, &a, &b);
    let _ = (events(&a), events(&b));

    a.send_command(&cmd(
        opcode::SETUP_SYNCHRONOUS_CONNECTION,
        &setup_sco_params(acl_handle, VOICE_SETTING_CVSD, SCO_PACKET_TYPES),
    ))
    .unwrap();
    link.tick();
    let _ = (events(&a), events(&b));

    b.send_command(&cmd(
        opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST,
        &accept_sco_params(WIRE_A, VOICE_SETTING_CVSD, SCO_PACKET_TYPES),
    ))
    .unwrap();
    link.tick();

    let acceptor = events(&b);
    assert_eq!(
        command_status_for(&acceptor, opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST),
        Some(STATUS_SUCCESS),
        "{acceptor:?}"
    );
    let initiator = events(&a);
    let (mut sco_handle, mut seen) = (0u16, 0);
    for (who, completes) in [
        ("initiator", sync_completes(&initiator)),
        ("acceptor", sync_completes(&acceptor)),
    ] {
        let complete = completes
            .first()
            .unwrap_or_else(|| panic!("the {who} is owed a completion"));
        assert_eq!(complete[0], STATUS_SUCCESS, "{who}: {complete:?}");
        let handle = u16::from_le_bytes([complete[1], complete[2]]);
        assert_ne!(handle, acl_handle, "{who}: SCO gets a handle of its own");
        if seen == 0 {
            sco_handle = handle;
        } else {
            assert_eq!(handle, sco_handle, "both ends name the same SCO link");
        }
        seen += 1;
        assert_eq!(complete[9], LINK_TYPE_SCO, "{who}: link type");
        assert_eq!(complete[16], air_mode::CVSD, "{who}: air mode");
    }
    assert_eq!(seen, 2);
}

#[test]
fn test_a_transparent_esco_setup_reports_transparent_air_mode() {
    // Wideband speech: mSBC rides an eSCO link in transparent mode, and
    // the air mode a host reads out of the completion event is how it
    // knows the controller will not touch its frames.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let acl_handle = connect_classic(&mut link, &a, &b);
    let _ = (events(&a), events(&b));
    a.send_command(&cmd(
        opcode::SETUP_SYNCHRONOUS_CONNECTION,
        &setup_sco_params(acl_handle, VOICE_SETTING_TRANSPARENT, ESCO_PACKET_TYPES_EV3),
    ))
    .unwrap();
    link.tick();
    let request = events(&b)
        .into_iter()
        .find(|(code, _)| *code == event::CONNECTION_REQUEST)
        .expect("the peer's host must be asked")
        .1;
    assert_eq!(request[9], LINK_TYPE_ESCO, "EV3 means an extended link");

    b.send_command(&cmd(
        opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST,
        &accept_sco_params(WIRE_A, VOICE_SETTING_TRANSPARENT, ESCO_PACKET_TYPES_EV3),
    ))
    .unwrap();
    link.tick();
    let complete = sync_completes(&events(&a))
        .into_iter()
        .next()
        .expect("the setup must complete");
    assert_eq!(complete[9], LINK_TYPE_ESCO);
    assert_eq!(complete[16], air_mode::TRANSPARENT);
}

#[test]
fn test_enhanced_setup_synchronous_connection_takes_its_air_mode_from_the_coding_format() {
    // The Enhanced form carries no Voice Setting at all: the air mode
    // comes out of a five-octet Coding_Format 46 bytes earlier in a
    // 59-byte parameter block. Reading it at the plain form's offset
    // gives whatever the input bandwidth's low byte happens to be.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let acl_handle = connect_classic(&mut link, &a, &b);
    let _ = (events(&a), events(&b));

    let mut params = vec![0u8; 59];
    params[0..2].copy_from_slice(&acl_handle.to_le_bytes());
    params[10] = 0x03; // Transmit_Coding_Format: transparent
    params[15] = 0x03; // Receive_Coding_Format
    params[56..58].copy_from_slice(&ESCO_PACKET_TYPES_EV3.to_le_bytes());
    params[58] = 0xFF; // Retransmission_Effort: don't care
    a.send_command(&cmd(opcode::ENHANCED_SETUP_SYNCHRONOUS_CONNECTION, &params))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::ENHANCED_SETUP_SYNCHRONOUS_CONNECTION),
        Some(STATUS_SUCCESS),
        "{evts:?}"
    );
    b.send_command(&cmd(
        opcode::ENHANCED_ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST,
        &accept_sco_params(WIRE_A, VOICE_SETTING_TRANSPARENT, ESCO_PACKET_TYPES_EV3),
    ))
    .unwrap();
    link.tick();

    let complete = sync_completes(&events(&a))
        .into_iter()
        .next()
        .expect("the enhanced setup must complete too");
    assert_eq!(complete[0], STATUS_SUCCESS);
    assert_eq!(complete[9], LINK_TYPE_ESCO);
    assert_eq!(complete[16], air_mode::TRANSPARENT);
}

#[test]
fn test_reject_synchronous_connection_request_leaves_no_half_open_handle() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let acl_handle = connect_classic(&mut link, &a, &b);
    let _ = (events(&a), events(&b));

    a.send_command(&cmd(
        opcode::SETUP_SYNCHRONOUS_CONNECTION,
        &setup_sco_params(acl_handle, VOICE_SETTING_CVSD, SCO_PACKET_TYPES),
    ))
    .unwrap();
    link.tick();
    let _ = (events(&a), events(&b));

    let mut reject = WIRE_A.to_vec();
    reject.push(STATUS_CONNECTION_REJECTED_RESOURCES);
    b.send_command(&cmd(opcode::REJECT_SYNCHRONOUS_CONNECTION_REQUEST, &reject))
        .unwrap();
    link.tick();

    let acceptor = events(&b);
    assert_eq!(
        command_status_for(&acceptor, opcode::REJECT_SYNCHRONOUS_CONNECTION_REQUEST),
        Some(STATUS_SUCCESS),
        "the refusal itself succeeded: {acceptor:?}"
    );
    for (who, evts) in [("initiator", events(&a)), ("acceptor", acceptor)] {
        let complete = sync_completes(&evts)
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("the {who} is owed a completion: {evts:?}"));
        assert_eq!(
            complete[0], STATUS_CONNECTION_REJECTED_RESOURCES,
            "{who} must be told why"
        );
        assert_eq!(
            u16::from_le_bytes([complete[1], complete[2]]),
            0,
            "{who}: a refused link gets no handle"
        );
    }

    // And nothing is left half-open: the ACL still carries data, and a
    // second setup on it starts from scratch rather than being refused
    // as already existing.
    a.send_command(&cmd(
        opcode::SETUP_SYNCHRONOUS_CONNECTION,
        &setup_sco_params(acl_handle, VOICE_SETTING_CVSD, SCO_PACKET_TYPES),
    ))
    .unwrap();
    link.tick();
    assert_eq!(
        command_status_for(&events(&a), opcode::SETUP_SYNCHRONOUS_CONNECTION),
        Some(STATUS_SUCCESS),
        "a refused setup must not poison the ACL"
    );
    assert!(
        events(&b)
            .iter()
            .any(|(code, _)| *code == event::CONNECTION_REQUEST),
        "and the peer is asked again"
    );
}

#[test]
fn test_setting_up_audio_on_a_handle_with_no_acl_is_refused_with_a_status() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(
        opcode::SETUP_SYNCHRONOUS_CONNECTION,
        &setup_sco_params(0x0BAD, VOICE_SETTING_CVSD, SCO_PACKET_TYPES),
    ))
    .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::SETUP_SYNCHRONOUS_CONNECTION),
        Some(STATUS_UNKNOWN_CONNECTION),
        "{evts:?}"
    );
    assert!(
        command_complete_for(&evts, opcode::SETUP_SYNCHRONOUS_CONNECTION).is_none(),
        "an error answer to a Command-Status command is still a status"
    );
}

#[test]
fn test_answering_a_synchronous_request_nobody_sent_is_refused_with_a_status() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let _ = connect_classic(&mut link, &a, &b);
    let _ = (events(&a), events(&b));

    b.send_command(&cmd(
        opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST,
        &accept_sco_params(WIRE_A, VOICE_SETTING_CVSD, SCO_PACKET_TYPES),
    ))
    .unwrap();
    link.tick();

    let evts = events(&b);
    assert_eq!(
        command_status_for(&evts, opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST),
        Some(STATUS_UNKNOWN_CONNECTION),
        "{evts:?}"
    );
    assert!(
        sync_completes(&evts).is_empty(),
        "and no link is invented: {evts:?}"
    );
}

#[test]
fn test_sco_audio_is_routed_between_the_two_ends_of_a_synchronous_link() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    let (acl_handle, sco_handle) =
        connect_sco(&mut link, &a, &b, VOICE_SETTING_CVSD, SCO_PACKET_TYPES);

    let payload = [0x11, 0x22, 0x33, 0x44];
    let mut sco = vec![sco_handle as u8, (sco_handle >> 8) as u8, 0x04];
    sco.extend_from_slice(&payload);
    a.send_sco_data(&sco).unwrap();
    link.tick();

    let got = b
        .poll_controller_packet()
        .expect("audio must reach the far end");
    assert_eq!(got[0], h4_type::HCI_SCO_DATA);
    assert_eq!(&got[4..8], &payload, "the payload crosses untouched");

    // And back, on the same handle.
    b.send_sco_data(&sco).unwrap();
    link.tick();
    let got = a.poll_controller_packet().expect("and back again");
    assert_eq!(&got[4..8], &payload);

    // Audio addressed to the *ACL* handle is not audio. It reaches
    // nobody rather than being quietly delivered to the signalling link.
    let mut misaddressed = vec![acl_handle as u8, (acl_handle >> 8) as u8, 0x04];
    misaddressed.extend_from_slice(&payload);
    a.send_sco_data(&misaddressed).unwrap();
    link.tick();
    assert!(
        b.poll_controller_packet().is_none(),
        "a SCO packet on the ACL handle must not be delivered"
    );
}

#[test]
fn test_disconnecting_the_sco_handle_leaves_the_acl_up() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    let (acl_handle, sco_handle) =
        connect_sco(&mut link, &a, &b, VOICE_SETTING_CVSD, SCO_PACKET_TYPES);

    let mut params = sco_handle.to_le_bytes().to_vec();
    params.push(REASON_REMOTE_USER);
    a.send_command(&cmd(opcode::DISCONNECT, &params)).unwrap();
    link.tick();

    for (who, ch) in [("initiator", &a), ("acceptor", &b)] {
        let evts = events(ch);
        let (_, body) = evts
            .iter()
            .find(|(code, _)| *code == event::DISCONNECTION_COMPLETE)
            .unwrap_or_else(|| panic!("the {who} must be told the audio is gone: {evts:?}"));
        assert_eq!(
            u16::from_le_bytes([body[1], body[2]]),
            sco_handle,
            "{who}: the audio handle went, not the ACL's"
        );
    }

    // Hanging up the audio does not hang up the call's signalling.
    let mut acl = vec![acl_handle as u8, (acl_handle >> 8) as u8, 0x01, 0x00, 0x5A];
    acl.truncate(5);
    a.send_acl_data(&acl).unwrap();
    link.tick();
    assert!(
        b.poll_controller_packet().is_some(),
        "the ACL must still carry AT commands after the audio stops"
    );
}

#[test]
fn test_dropping_the_acl_takes_its_sco_link_with_it() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    let (acl_handle, sco_handle) =
        connect_sco(&mut link, &a, &b, VOICE_SETTING_CVSD, SCO_PACKET_TYPES);

    let mut params = acl_handle.to_le_bytes().to_vec();
    params.push(REASON_REMOTE_USER);
    a.send_command(&cmd(opcode::DISCONNECT, &params)).unwrap();
    link.tick();

    for (who, ch) in [("initiator", &a), ("acceptor", &b)] {
        let evts = events(ch);
        let handles: Vec<u16> = evts
            .iter()
            .filter(|(code, _)| *code == event::DISCONNECTION_COMPLETE)
            .map(|(_, body)| u16::from_le_bytes([body[1], body[2]]))
            .collect();
        assert!(
            handles.contains(&sco_handle),
            "{who} keeps a SCO handle it will never hear from again: {handles:?}"
        );
        assert!(
            handles.contains(&acl_handle),
            "{who} must be told about the ACL too: {handles:?}"
        );
    }

    // Nothing is left routing: audio on the dead handle goes nowhere.
    let mut sco = vec![sco_handle as u8, (sco_handle >> 8) as u8, 0x01, 0x5A];
    sco.truncate(4);
    a.send_sco_data(&sco).unwrap();
    link.tick();
    assert!(b.poll_controller_packet().is_none());
}

// --- the Command Status contract -------------------------------------
//
// One test per Command-Status-only command this controller implements,
// each asserting the same two things: that the *answer* is a Command
// Status and not a Command Complete, and that the completion event the
// Status promises actually arrives, after it. Getting either wrong does
// not fail — it hangs, silently, in whatever host sent the command.
//
// scripts/check_hci_command_answers.py keeps the list itself honest
// against the Core specification; these keep the behaviour honest.

/// The LE Meta subevents of `subevent` among events already drained.
fn le_subevents_in(evts: &[(u8, Vec<u8>)], subevent: u8) -> Vec<Vec<u8>> {
    evts.iter()
        .filter(|(code, p)| *code == event::LE_META && p.first() == Some(&subevent))
        .map(|(_, p)| p.clone())
        .collect()
}

/// Where the Command Status answering `opcode` sits in the event stream,
/// and where the LE Meta subevent `subevent` sits. Both must exist, and
/// the Status must come first — a completion event that arrives before
/// the answer to the command is a different bug with the same symptom.
fn status_then_subevent(evts: &[(u8, Vec<u8>)], opcode: u16, subevent: u8) -> (usize, usize) {
    let status = evts
        .iter()
        .position(|(code, p)| {
            *code == event::COMMAND_STATUS
                && p.len() >= 4
                && u16::from_le_bytes([p[2], p[3]]) == opcode
        })
        .unwrap_or_else(|| panic!("no Command Status for {opcode:#06X}: {evts:?}"));
    let complete = evts
        .iter()
        .position(|(code, p)| *code == event::LE_META && p.first() == Some(&subevent))
        .unwrap_or_else(|| {
            panic!("no completion subevent {subevent:#04X} for {opcode:#06X}: {evts:?}")
        });
    assert!(
        status < complete,
        "the Command Status must precede the completion event: {evts:?}"
    );
    (status, complete)
}

/// LE Connection Update parameters: the same interval both ways, so the
/// value the completion event reports is unambiguous.
fn connection_update_params(handle: u16, interval: u16, latency: u16, timeout: u16) -> Vec<u8> {
    let mut p = handle.to_le_bytes().to_vec();
    p.extend_from_slice(&interval.to_le_bytes()); // Conn_Interval_Min
    p.extend_from_slice(&interval.to_le_bytes()); // Conn_Interval_Max
    p.extend_from_slice(&latency.to_le_bytes());
    p.extend_from_slice(&timeout.to_le_bytes());
    p.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]); // Min/Max_CE_Length
    p
}

#[test]
fn test_le_connection_update_answers_with_status_then_update_complete() {
    let mut link = Link::new();
    let central = link.add_device(addr("AA:BB:CC:00:00:01"));
    let peripheral_address = addr("AA:BB:CC:00:00:02");
    let peripheral = link.add_device(peripheral_address);
    let handle = connect(&mut link, &central, &peripheral, peripheral_address);
    let _ = events(&central);
    let _ = events(&peripheral);

    central
        .send_command(&cmd(
            opcode::LE_CONNECTION_UPDATE,
            &connection_update_params(handle, 0x0028, 0x0004, 0x0100),
        ))
        .unwrap();
    link.tick();

    let evts = events(&central);
    assert!(
        command_complete_for(&evts, opcode::LE_CONNECTION_UPDATE).is_none(),
        "a Command Complete here strands the host on an LE Connection \
             Update Complete that never comes: {evts:?}"
    );
    assert_eq!(
        command_status_for(&evts, opcode::LE_CONNECTION_UPDATE),
        Some(STATUS_SUCCESS)
    );
    status_then_subevent(
        &evts,
        opcode::LE_CONNECTION_UPDATE,
        event::LE_CONNECTION_UPDATE_COMPLETE,
    );
    // subevent, status, handle(2), interval(2), latency(2), timeout(2)
    let update = &le_subevents_in(&evts, event::LE_CONNECTION_UPDATE_COMPLETE)[0];
    assert_eq!(update[1], STATUS_SUCCESS);
    assert_eq!(u16::from_le_bytes([update[2], update[3]]), handle);
    assert_eq!(u16::from_le_bytes([update[4], update[5]]), 0x0028);
    assert_eq!(u16::from_le_bytes([update[6], update[7]]), 0x0004);
    assert_eq!(u16::from_le_bytes([update[8], update[9]]), 0x0100);
}

#[test]
fn test_le_connection_update_tells_the_peripheral_too() {
    // A connection has one set of parameters and two ends. A peripheral
    // that was never told its own link had changed would be a fiction no
    // real link produces — and hosts do act on this event.
    let mut link = Link::new();
    let central = link.add_device(addr("AA:BB:CC:00:00:01"));
    let peripheral_address = addr("AA:BB:CC:00:00:02");
    let peripheral = link.add_device(peripheral_address);
    let handle = connect(&mut link, &central, &peripheral, peripheral_address);
    let _ = events(&peripheral);

    central
        .send_command(&cmd(
            opcode::LE_CONNECTION_UPDATE,
            &connection_update_params(handle, 0x0028, 0, 0x0100),
        ))
        .unwrap();
    link.tick();

    let evts = events(&peripheral);
    let updates = le_subevents_in(&evts, event::LE_CONNECTION_UPDATE_COMPLETE);
    assert_eq!(updates.len(), 1, "the peripheral is told once: {evts:?}");
    assert_eq!(u16::from_le_bytes([updates[0][4], updates[0][5]]), 0x0028);
}

#[test]
fn test_le_connection_update_on_a_dead_handle_is_refused_with_status() {
    // The error path answers with a Command Status too. Answering an
    // error with a Command Complete is the same hang as answering a
    // success with one.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(
        opcode::LE_CONNECTION_UPDATE,
        &connection_update_params(0x0BAD, 0x0028, 0, 0x0100),
    ))
    .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::LE_CONNECTION_UPDATE),
        Some(STATUS_UNKNOWN_CONNECTION),
        "{evts:?}"
    );
    assert!(command_complete_for(&evts, opcode::LE_CONNECTION_UPDATE).is_none());
    assert!(
        le_subevents_in(&evts, event::LE_CONNECTION_UPDATE_COMPLETE).is_empty(),
        "a refused update completes nothing"
    );
}

#[test]
fn test_le_set_phy_answers_with_status_then_phy_update_complete() {
    let mut link = Link::new();
    let central = link.add_device(addr("AA:BB:CC:00:00:01"));
    let peripheral_address = addr("AA:BB:CC:00:00:02");
    let peripheral = link.add_device(peripheral_address);
    let handle = connect(&mut link, &central, &peripheral, peripheral_address);
    let _ = events(&central);
    let _ = events(&peripheral);

    // All_PHYs = 0 (the host has a preference both ways), TX and RX both
    // LE 2M only.
    let mut params = handle.to_le_bytes().to_vec();
    params.extend_from_slice(&[0x00, 0x02, 0x02, 0x00, 0x00]);
    central
        .send_command(&cmd(opcode::LE_SET_PHY, &params))
        .unwrap();
    link.tick();

    let evts = events(&central);
    assert!(
        command_complete_for(&evts, opcode::LE_SET_PHY).is_none(),
        "LE Set PHY is Command Status only (Vol 4, Part E, 7.8.49): {evts:?}"
    );
    assert_eq!(
        command_status_for(&evts, opcode::LE_SET_PHY),
        Some(STATUS_SUCCESS)
    );
    status_then_subevent(&evts, opcode::LE_SET_PHY, event::LE_PHY_UPDATE_COMPLETE);
    // subevent, status, handle(2), tx_phy, rx_phy
    let phy = &le_subevents_in(&evts, event::LE_PHY_UPDATE_COMPLETE)[0];
    assert_eq!(phy[1], STATUS_SUCCESS);
    assert_eq!(u16::from_le_bytes([phy[2], phy[3]]), handle);
    assert_eq!((phy[4], phy[5]), (le_phy::LE_2M, le_phy::LE_2M));

    // And the peer sees the same change with the directions swapped —
    // one end's TX is the other end's RX.
    let peer = events(&peripheral);
    let phy = &le_subevents_in(&peer, event::LE_PHY_UPDATE_COMPLETE)[0];
    assert_eq!((phy[4], phy[5]), (le_phy::LE_2M, le_phy::LE_2M));
}

#[test]
fn test_le_set_phy_with_no_preference_still_completes() {
    // All_PHYs bits 0 and 1 set means "no preference either way", and the
    // spec still requires the completion event: a host that asks for the
    // PHY it already has must not be left waiting.
    let mut link = Link::new();
    let central = link.add_device(addr("AA:BB:CC:00:00:01"));
    let peripheral_address = addr("AA:BB:CC:00:00:02");
    let peripheral = link.add_device(peripheral_address);
    let handle = connect(&mut link, &central, &peripheral, peripheral_address);
    let _ = events(&central);

    let mut params = handle.to_le_bytes().to_vec();
    params.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x00]);
    central
        .send_command(&cmd(opcode::LE_SET_PHY, &params))
        .unwrap();
    link.tick();

    let evts = events(&central);
    let phy = &le_subevents_in(&evts, event::LE_PHY_UPDATE_COMPLETE)[0];
    assert_eq!(
        (phy[4], phy[5]),
        (le_phy::LE_1M, le_phy::LE_1M),
        "no preference keeps the PHY the connection was established on"
    );
}

#[test]
fn test_le_set_phy_on_a_dead_handle_is_refused_with_status() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(
        opcode::LE_SET_PHY,
        &[0xAD, 0x0B, 0x00, 0x02, 0x02, 0x00, 0x00],
    ))
    .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::LE_SET_PHY),
        Some(STATUS_UNKNOWN_CONNECTION),
        "{evts:?}"
    );
    assert!(command_complete_for(&evts, opcode::LE_SET_PHY).is_none());
}

/// LE Create CIS for one stream: count, then the CIS handle the host
/// picked and the ACL link it runs over.
fn create_cis_params(cis_handle: u16, acl_handle: u16) -> Vec<u8> {
    let mut p = vec![0x01];
    p.extend_from_slice(&cis_handle.to_le_bytes());
    p.extend_from_slice(&acl_handle.to_le_bytes());
    p
}

#[test]
fn test_le_create_cis_answers_with_status_then_asks_the_peer() {
    // LE Create CIS's completion event is an LE CIS Established that only
    // arrives once the peripheral's host has answered — so what the
    // central's Command Status promises first is the peer's LE CIS
    // Request. A Command Complete here would tell the host the stream was
    // done before anyone had been asked.
    let mut link = Link::new();
    let central = link.add_device(addr("AA:BB:CC:00:00:01"));
    let peripheral_address = addr("AA:BB:CC:00:00:02");
    let peripheral = link.add_device(peripheral_address);
    let acl = connect(&mut link, &central, &peripheral, peripheral_address);
    let _ = events(&central);
    let _ = events(&peripheral);

    central
        .send_command(&cmd(opcode::LE_CREATE_CIS, &create_cis_params(0x0060, acl)))
        .unwrap();
    link.tick();

    let evts = events(&central);
    assert!(
        command_complete_for(&evts, opcode::LE_CREATE_CIS).is_none(),
        "LE Create CIS is Command Status only (Vol 4, Part E, 7.8.99): {evts:?}"
    );
    assert_eq!(
        command_status_for(&evts, opcode::LE_CREATE_CIS),
        Some(STATUS_SUCCESS)
    );
    assert!(
        le_subevents_in(&evts, event::LE_CIS_ESTABLISHED).is_empty(),
        "nothing is established until the peripheral accepts: {evts:?}"
    );

    let peer = events(&peripheral);
    let requests = le_subevents_in(&peer, event::LE_CIS_REQUEST);
    assert_eq!(requests.len(), 1, "the peer's host is asked: {peer:?}");
    // subevent, acl_handle(2), cis_handle(2), cig_id, cis_id
    assert_eq!(u16::from_le_bytes([requests[0][1], requests[0][2]]), acl);
    assert_eq!(u16::from_le_bytes([requests[0][3], requests[0][4]]), 0x0060);
}

#[test]
fn test_le_create_cis_on_a_dead_acl_handle_is_refused_with_status() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(
        opcode::LE_CREATE_CIS,
        &create_cis_params(0x0060, 0x0BAD),
    ))
    .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::LE_CREATE_CIS),
        Some(STATUS_UNKNOWN_CONNECTION),
        "a stream is opened on an ACL link, and that one does not \
             exist: {evts:?}"
    );
    assert!(command_complete_for(&evts, opcode::LE_CREATE_CIS).is_none());
}

#[test]
fn test_le_accept_cis_request_answers_with_status_then_establishes_both_ends() {
    let mut link = Link::new();
    let central = link.add_device(addr("AA:BB:CC:00:00:01"));
    let peripheral_address = addr("AA:BB:CC:00:00:02");
    let peripheral = link.add_device(peripheral_address);
    let acl = connect(&mut link, &central, &peripheral, peripheral_address);
    central
        .send_command(&cmd(opcode::LE_CREATE_CIS, &create_cis_params(0x0060, acl)))
        .unwrap();
    link.tick();
    let _ = events(&central);
    let _ = events(&peripheral);

    peripheral
        .send_command(&cmd(
            opcode::LE_ACCEPT_CIS_REQUEST,
            &0x0060u16.to_le_bytes(),
        ))
        .unwrap();
    link.tick();

    let evts = events(&peripheral);
    assert!(
        command_complete_for(&evts, opcode::LE_ACCEPT_CIS_REQUEST).is_none(),
        "LE Accept CIS Request is Command Status only (Vol 4, Part E, \
             7.8.101): {evts:?}"
    );
    assert_eq!(
        command_status_for(&evts, opcode::LE_ACCEPT_CIS_REQUEST),
        Some(STATUS_SUCCESS)
    );
    status_then_subevent(
        &evts,
        opcode::LE_ACCEPT_CIS_REQUEST,
        event::LE_CIS_ESTABLISHED,
    );
    let established = &le_subevents_in(&evts, event::LE_CIS_ESTABLISHED)[0];
    assert_eq!(established[1], STATUS_SUCCESS);
    assert_eq!(u16::from_le_bytes([established[2], established[3]]), 0x0060);

    // The central, which has been waiting since its own Command Status,
    // is told at the same moment.
    let evts = events(&central);
    let established = le_subevents_in(&evts, event::LE_CIS_ESTABLISHED);
    assert_eq!(
        established.len(),
        1,
        "the central's LE Create CIS finally completes: {evts:?}"
    );
    assert_eq!(established[0][1], STATUS_SUCCESS);
}

#[test]
fn test_le_accept_cis_request_for_a_stream_nobody_asked_for_is_refused() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::LE_ACCEPT_CIS_REQUEST, &[0x60, 0x00]))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::LE_ACCEPT_CIS_REQUEST),
        Some(STATUS_UNKNOWN_CONNECTION),
        "{evts:?}"
    );
    assert!(command_complete_for(&evts, opcode::LE_ACCEPT_CIS_REQUEST).is_none());
}

#[test]
fn test_no_command_status_command_is_ever_answered_with_command_complete() {
    // The catch-all used to answer *everything* it did not recognise with
    // a success Command Complete, which is the right shape for 278 of the
    // 339 commands in Core v6.3 and a silent hang for the other 61. This
    // sweeps the whole derived table: modelled or not, none of them may
    // come back as a Command Complete, and every one must be answered.
    //
    // scripts/check_hci_command_answers.py checks the table against the
    // specification; this checks the controller against the table.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    for &opcode in COMMAND_STATUS_OPCODES {
        a.send_command(&cmd(opcode, &[])).unwrap();
    }
    link.tick();

    let evts = events(&a);
    for &opcode in COMMAND_STATUS_OPCODES {
        assert!(
            command_complete_for(&evts, opcode).is_none(),
            "{opcode:#06X} is Command-Status-only; a Command Complete \
                 leaves its host waiting for an event that never comes"
        );
        assert!(
            command_status_for(&evts, opcode).is_some(),
            "{opcode:#06X} was answered with nothing at all, which hangs \
                 a host just as thoroughly"
        );
    }
}

#[test]
fn test_an_unmodelled_command_status_command_says_so() {
    // The honest minimum for a command this controller does not model:
    // the right *shape* and an error a host can act on, rather than a
    // success it cannot.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    // Sniff Mode (0x0803) — this controller models no low-power mode at
    // all, and says so instead of pretending. (This was LE Enable
    // Encryption until security landed; the point of the test is the
    // *shape* of an honest refusal, so it needs an opcode that is still
    // genuinely unmodelled.)
    a.send_command(&cmd(0x0803, &[0x00; 10])).unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, 0x0803),
        Some(STATUS_UNKNOWN_HCI_COMMAND),
        "{evts:?}"
    );
}

// --- security: Secure Simple Pairing, link keys, encryption ----------
//
// One test per command, asserting the answer's *event type* and its
// completion event in order — the same shape as the nineteen BR/EDR
// tests above, for the same reason: a wrong event type does not fail,
// it hangs.

/// Enables Simple Pairing Mode, then the usual BR/EDR bring-up. A host
/// that skips the first of these is told Pairing Not Allowed rather than
/// getting SSP it never switched on.
fn ssp_bring_up(ch: &HciChannel, name: &str, scan: u8) {
    ch.send_command(&cmd(opcode::WRITE_SIMPLE_PAIRING_MODE, &[0x01]))
        .unwrap();
    classic_bring_up(ch, name, scan);
}

/// What a host answers a security question with, for [`ssp_host_tick`].
#[derive(Clone, Copy)]
struct SspPolicy {
    /// The key to answer Link Key Request with, or `None` for the
    /// negative reply that starts pairing.
    stored_key: Option<[u8; 16]>,
    io: u8,
    auth: u8,
    /// The answer to User Confirmation Request.
    accept: bool,
    /// The digits to answer User Passkey Request with, if asked.
    passkey: Option<u32>,
}

impl SspPolicy {
    /// A device with a screen and a button, no stored bond, and a user
    /// who says yes.
    fn agreeable(io: u8, auth: u8) -> Self {
        Self {
            stored_key: None,
            io,
            auth,
            accept: true,
            passkey: None,
        }
    }
}

/// Drains one host's events and answers every security question in them,
/// returning what it saw. This *is* the host half of SSP: the controller
/// asks four questions and hangs on any one that goes unanswered.
fn ssp_host_tick(ch: &HciChannel, policy: SspPolicy) -> Vec<(u8, Vec<u8>)> {
    let evts = events(ch);
    for (code, params) in &evts {
        let peer = &params[..6.min(params.len())];
        match *code {
            event::LINK_KEY_REQUEST => match policy.stored_key {
                Some(key) => {
                    let mut p = peer.to_vec();
                    p.extend_from_slice(&key);
                    ch.send_command(&cmd(opcode::LINK_KEY_REQUEST_REPLY, &p))
                        .unwrap();
                }
                None => {
                    ch.send_command(&cmd(opcode::LINK_KEY_REQUEST_NEGATIVE_REPLY, peer))
                        .unwrap();
                }
            },
            event::IO_CAPABILITY_REQUEST => {
                let mut p = peer.to_vec();
                p.extend_from_slice(&[policy.io, 0x00, policy.auth]);
                ch.send_command(&cmd(opcode::IO_CAPABILITY_REQUEST_REPLY, &p))
                    .unwrap();
            }
            event::USER_CONFIRMATION_REQUEST => {
                let reply = if policy.accept {
                    opcode::USER_CONFIRMATION_REQUEST_REPLY
                } else {
                    opcode::USER_CONFIRMATION_REQUEST_NEGATIVE_REPLY
                };
                ch.send_command(&cmd(reply, peer)).unwrap();
            }
            event::USER_PASSKEY_REQUEST => match policy.passkey.filter(|_| policy.accept) {
                Some(passkey) => {
                    let mut p = peer.to_vec();
                    p.extend_from_slice(&passkey.to_le_bytes());
                    ch.send_command(&cmd(opcode::USER_PASSKEY_REQUEST_REPLY, &p))
                        .unwrap();
                }
                None => {
                    ch.send_command(&cmd(opcode::USER_PASSKEY_REQUEST_NEGATIVE_REPLY, peer))
                        .unwrap();
                }
            },
            _ => {}
        }
    }
    evts
}

/// Runs both hosts' SSP policies for as many ticks as a pairing needs,
/// returning every event each host saw, in order.
fn run_ssp(
    link: &mut Link,
    a: &HciChannel,
    b: &HciChannel,
    a_policy: SspPolicy,
    b_policy: SspPolicy,
) -> (Vec<(u8, Vec<u8>)>, Vec<(u8, Vec<u8>)>) {
    let (mut a_seen, mut b_seen) = (Vec::new(), Vec::new());
    // Each round trip costs one tick: the controller asks in one, the
    // host answers into the next. Eight is comfortably past the four
    // questions a full pairing asks.
    for _ in 0..8 {
        link.tick();
        a_seen.extend(ssp_host_tick(a, a_policy));
        b_seen.extend(ssp_host_tick(b, b_policy));
    }
    (a_seen, b_seen)
}

/// Just the event codes, in order — what an assertion about sequencing
/// actually wants to look at.
fn codes(evts: &[(u8, Vec<u8>)]) -> Vec<u8> {
    evts.iter().map(|(code, _)| *code).collect()
}

/// The parameters of the first `code` event, if one came.
fn first_of(evts: &[(u8, Vec<u8>)], code: u8) -> Option<&[u8]> {
    evts.iter()
        .find(|(c, _)| *c == code)
        .map(|(_, p)| p.as_slice())
}

/// Two connected devices with Simple Pairing switched on at both ends.
fn ssp_pair_connected(link: &mut Link) -> (Arc<HciChannel>, Arc<HciChannel>, u16) {
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    ssp_bring_up(&a, "Initiator", 0x03);
    ssp_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let handle = connect_classic(link, &a, &b);
    let _ = events(&a);
    let _ = events(&b);
    (a, b, handle)
}

/// An LE central connected to an LE peripheral. Returns both channels and
/// the connection handle.
fn le_pair_connected(link: &mut Link) -> (Arc<HciChannel>, Arc<HciChannel>, u16) {
    let central = link.add_device(addr("AA:BB:CC:00:00:01"));
    let peripheral = link.add_device(addr("AA:BB:CC:00:00:02"));
    let handle = connect(link, &central, &peripheral, addr("AA:BB:CC:00:00:02"));
    let _ = events(&central);
    let _ = events(&peripheral);
    (central, peripheral, handle)
}

#[test]
fn test_write_simple_pairing_mode_is_answered_with_command_complete() {
    // 0x0C56, and it used to fall through the catch-all. The neighbouring
    // 0x0C45 is Write *Inquiry* Mode; confusing the two is the reason the
    // opcode is spelled out in `mod opcode` rather than written inline.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::WRITE_SIMPLE_PAIRING_MODE, &[0x01]))
        .unwrap();
    a.send_command(&cmd(opcode::READ_SIMPLE_PAIRING_MODE, &[]))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_complete_for(&evts, opcode::WRITE_SIMPLE_PAIRING_MODE).as_deref(),
        Some(&[STATUS_SUCCESS][..]),
    );
    assert_eq!(
        command_complete_for(&evts, opcode::READ_SIMPLE_PAIRING_MODE).as_deref(),
        Some(&[STATUS_SUCCESS, 0x01][..]),
        "Read Simple Pairing Mode must report what the write set: {evts:?}"
    );
}

#[test]
fn test_authentication_requested_asks_both_hosts_for_a_link_key() {
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);

    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();
    link.tick();

    let a_evts = events(&a);
    assert_eq!(
        command_status_for(&a_evts, opcode::AUTHENTICATION_REQUESTED),
        Some(STATUS_SUCCESS),
        "Authentication Requested is Command-Status-answered: {a_evts:?}"
    );
    assert!(
        command_complete_for(&a_evts, opcode::AUTHENTICATION_REQUESTED).is_none(),
        "a Command Complete here strands the host on the Authentication \
             Complete that never comes"
    );
    // Both ends, not just the asking one: whether pairing runs depends on
    // what *both* hosts have stored, so both have to be asked.
    assert_eq!(
        first_of(&a_evts, event::LINK_KEY_REQUEST),
        Some(&WIRE_B[..]),
        "{a_evts:?}"
    );
    let b_evts = events(&b);
    assert_eq!(
        first_of(&b_evts, event::LINK_KEY_REQUEST),
        Some(&WIRE_A[..]),
        "{b_evts:?}"
    );
}

#[test]
fn test_secure_simple_pairing_runs_in_order_and_keys_both_ends() {
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();

    let policy = SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM);
    let (a_seen, b_seen) = run_ssp(&mut link, &a, &b, policy, policy);

    // The order is the whole contract. A host that gets Simple Pairing
    // Complete before the key has nothing to store; one that gets
    // Authentication Complete first will start encrypting into a link
    // whose key it has not been told.
    let a_security: Vec<u8> = codes(&a_seen)
        .into_iter()
        .filter(|c| {
            matches!(
                *c,
                event::LINK_KEY_REQUEST
                    | event::IO_CAPABILITY_REQUEST
                    | event::IO_CAPABILITY_RESPONSE
                    | event::USER_CONFIRMATION_REQUEST
                    | event::LINK_KEY_NOTIFICATION
                    | event::SIMPLE_PAIRING_COMPLETE
                    | event::AUTHENTICATION_COMPLETE
            )
        })
        .collect();
    assert_eq!(
        a_security,
        vec![
            event::LINK_KEY_REQUEST,
            event::IO_CAPABILITY_REQUEST,
            event::IO_CAPABILITY_RESPONSE,
            event::USER_CONFIRMATION_REQUEST,
            event::LINK_KEY_NOTIFICATION,
            event::SIMPLE_PAIRING_COMPLETE,
            event::AUTHENTICATION_COMPLETE,
        ],
        "{a_seen:?}"
    );

    // The acceptor sees the same conversation minus the Authentication
    // Complete, which belongs to the host that asked and to nobody else.
    assert!(
        !codes(&b_seen).contains(&event::AUTHENTICATION_COMPLETE),
        "only the requesting host is owed an Authentication Complete: \
             {b_seen:?}"
    );
    assert!(codes(&b_seen).contains(&event::SIMPLE_PAIRING_COMPLETE));

    // The same sixteen octets at both ends, or the bond is worthless.
    let a_key = first_of(&a_seen, event::LINK_KEY_NOTIFICATION).unwrap();
    let b_key = first_of(&b_seen, event::LINK_KEY_NOTIFICATION).unwrap();
    assert_eq!(
        a_key[6..22],
        b_key[6..22],
        "the key must match at both ends"
    );
    assert_eq!(
        a_key[22],
        link_key_type::AUTHENTICATED_P192,
        "two DisplayYesNo devices that asked for MITM protection did \
             Numeric Comparison, so the key is an authenticated one"
    );
    // And both hosts were shown the same digits.
    let a_value = first_of(&a_seen, event::USER_CONFIRMATION_REQUEST).unwrap();
    let b_value = first_of(&b_seen, event::USER_CONFIRMATION_REQUEST).unwrap();
    assert_eq!(a_value[6..10], b_value[6..10]);
    assert_eq!(
        first_of(&a_seen, event::SIMPLE_PAIRING_COMPLETE).unwrap()[0],
        STATUS_SUCCESS
    );
}

#[test]
fn test_just_works_produces_an_unauthenticated_key() {
    // The same conversation, with neither host asking for MITM
    // protection. Byte for byte the same events go out; the difference
    // is the key type, and it is the only thing a service that requires
    // MITM protection has to go on.
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();

    let policy = SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, 0x00);
    let (a_seen, _) = run_ssp(&mut link, &a, &b, policy, policy);

    assert_eq!(
        first_of(&a_seen, event::LINK_KEY_NOTIFICATION).unwrap()[22],
        link_key_type::UNAUTHENTICATED_P192,
        "{a_seen:?}"
    );
}

#[test]
fn test_a_stored_link_key_at_both_ends_skips_pairing_entirely() {
    // The observable difference a bond makes. Same command, same link,
    // and *none* of the four SSP questions gets asked.
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();

    let bonded = SspPolicy {
        stored_key: Some([0xA5; 16]),
        ..SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM)
    };
    let (a_seen, b_seen) = run_ssp(&mut link, &a, &b, bonded, bonded);

    for (label, seen) in [("initiator", &a_seen), ("acceptor", &b_seen)] {
        for unwanted in [
            event::IO_CAPABILITY_REQUEST,
            event::USER_CONFIRMATION_REQUEST,
            event::LINK_KEY_NOTIFICATION,
            event::SIMPLE_PAIRING_COMPLETE,
        ] {
            assert!(
                !codes(seen).contains(&unwanted),
                "{label} was asked {unwanted:#04X} on a bonded reconnect: \
                     {seen:?}"
            );
        }
    }
    assert_eq!(
        first_of(&a_seen, event::AUTHENTICATION_COMPLETE).map(|p| p[0]),
        Some(STATUS_SUCCESS),
        "{a_seen:?}"
    );
}

#[test]
fn test_one_stored_key_and_one_missing_pairs_again() {
    // Half a bond is no bond. The acceptor forgot, so the whole pairing
    // runs again — and a controller that had only asked the *initiator*
    // would have authenticated against a key the peer no longer holds.
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();

    let remembers = SspPolicy {
        stored_key: Some([0xA5; 16]),
        ..SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM)
    };
    let forgot = SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM);
    let (a_seen, _) = run_ssp(&mut link, &a, &b, remembers, forgot);

    assert!(
        codes(&a_seen).contains(&event::LINK_KEY_NOTIFICATION),
        "a new key has to be made and told to both: {a_seen:?}"
    );
    assert_eq!(
        first_of(&a_seen, event::AUTHENTICATION_COMPLETE).map(|p| p[0]),
        Some(STATUS_SUCCESS)
    );
}

#[test]
fn test_a_refused_confirmation_fails_both_ends_and_leaves_the_link_clear() {
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();

    let willing = SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM);
    let refuses = SspPolicy {
        accept: false,
        ..willing
    };
    let (a_seen, b_seen) = run_ssp(&mut link, &a, &b, willing, refuses);

    assert_eq!(
        first_of(&a_seen, event::SIMPLE_PAIRING_COMPLETE).map(|p| p[0]),
        Some(STATUS_AUTHENTICATION_FAILURE),
        "{a_seen:?}"
    );
    assert_eq!(
        first_of(&b_seen, event::SIMPLE_PAIRING_COMPLETE).map(|p| p[0]),
        Some(STATUS_AUTHENTICATION_FAILURE),
        "the refusing side is told too, or it never learns why nothing \
             happened: {b_seen:?}"
    );
    assert_eq!(
        first_of(&a_seen, event::AUTHENTICATION_COMPLETE).map(|p| p[0]),
        Some(STATUS_AUTHENTICATION_FAILURE)
    );
    assert!(
        !codes(&a_seen).contains(&event::LINK_KEY_NOTIFICATION),
        "a refused pairing must not hand anyone a key: {a_seen:?}"
    );

    // And the link must not be half-encrypted afterwards. Asking to
    // encrypt now is refused at the requester and changes nothing at the
    // peer — the state a failed pairing has to leave behind.
    a.send_command(&cmd(
        opcode::SET_CONNECTION_ENCRYPTION,
        &[handle as u8, (handle >> 8) as u8, 0x01],
    ))
    .unwrap();
    link.tick();
    link.tick();
    let a_after = events(&a);
    let b_after = events(&b);
    assert_eq!(
        first_of(&a_after, event::ENCRYPTION_CHANGE).map(|p| p[0]),
        Some(STATUS_PIN_OR_KEY_MISSING),
        "{a_after:?}"
    );
    assert!(
        first_of(&b_after, event::ENCRYPTION_CHANGE).is_none(),
        "the peer must not be told encryption started: {b_after:?}"
    );
}

#[test]
fn test_passkey_entry_asks_the_keyboard_and_tells_the_display() {
    // KeyboardOnly against DisplayOnly, with MITM asked for: the model is
    // Passkey Entry, and the two ends get *different* events — the only
    // asymmetric moment in SSP.
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();

    let display = SspPolicy::agreeable(io_capability::DISPLAY_ONLY, AUTH_REQ_MITM);
    let keyboard = SspPolicy::agreeable(io_capability::KEYBOARD_ONLY, AUTH_REQ_MITM);

    // The keyboard side has to type what the display side is shown, and
    // nothing on the link tells it — so the test plays the person: watch
    // for the notification, read the digits, then answer with them.
    let (mut a_seen, mut b_seen) = (Vec::new(), Vec::new());
    let mut typed: Option<u32> = None;
    for _ in 0..8 {
        link.tick();
        a_seen.extend(ssp_host_tick(&a, display));
        // Read the display *before* the keyboard side answers: both
        // events leave the controller in the same tick, so a person who
        // only looked at the screen on the next one would be typing
        // nothing.
        if let Some(shown) = first_of(&a_seen, event::USER_PASSKEY_NOTIFICATION) {
            typed = Some(u32::from_le_bytes([shown[6], shown[7], shown[8], shown[9]]));
        }
        b_seen.extend(ssp_host_tick(
            &b,
            SspPolicy {
                passkey: typed,
                ..keyboard
            },
        ));
    }

    assert!(
        codes(&a_seen).contains(&event::USER_PASSKEY_NOTIFICATION),
        "the display side is told the passkey: {a_seen:?}"
    );
    assert!(
        codes(&b_seen).contains(&event::USER_PASSKEY_REQUEST),
        "the keyboard side is asked for it: {b_seen:?}"
    );
    assert!(
        !codes(&a_seen).contains(&event::USER_CONFIRMATION_REQUEST),
        "Passkey Entry does not ask for a confirmation: {a_seen:?}"
    );
    assert_eq!(
        first_of(&a_seen, event::LINK_KEY_NOTIFICATION).map(|p| p[22]),
        Some(link_key_type::AUTHENTICATED_P192),
        "a passkey a person typed makes an authenticated key: {a_seen:?}"
    );
}

#[test]
fn test_a_wrong_passkey_fails_the_pairing() {
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();

    let display = SspPolicy::agreeable(io_capability::DISPLAY_ONLY, AUTH_REQ_MITM);
    let fumbling = SspPolicy {
        passkey: Some(111_111),
        ..SspPolicy::agreeable(io_capability::KEYBOARD_ONLY, AUTH_REQ_MITM)
    };
    let (a_seen, _) = run_ssp(&mut link, &a, &b, display, fumbling);

    assert_eq!(
        first_of(&a_seen, event::SIMPLE_PAIRING_COMPLETE).map(|p| p[0]),
        Some(STATUS_AUTHENTICATION_FAILURE),
        "digits that do not match must not make a key: {a_seen:?}"
    );
    assert!(!codes(&a_seen).contains(&event::LINK_KEY_NOTIFICATION));
}

#[test]
fn test_authentication_without_simple_pairing_mode_says_pairing_not_allowed() {
    // The honest answer for a host that never sent Write Simple Pairing
    // Mode. Real hardware would fall back to legacy PIN pairing, which is
    // not modelled — and running SSP anyway would hide the omission until
    // the same host met a real controller.
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    let b = link.add_device(addr("AA:BB:CC:00:00:02"));
    classic_bring_up(&a, "Initiator", 0x03);
    classic_bring_up(&b, "Acceptor", 0x03);
    link.tick();
    let handle = connect_classic(&mut link, &a, &b);
    let _ = events(&a);
    let _ = events(&b);

    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();
    let policy = SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM);
    let (a_seen, b_seen) = run_ssp(&mut link, &a, &b, policy, policy);

    assert_eq!(
        first_of(&a_seen, event::AUTHENTICATION_COMPLETE).map(|p| p[0]),
        Some(STATUS_PAIRING_NOT_ALLOWED),
        "{a_seen:?}"
    );
    assert!(
        !codes(&b_seen).contains(&event::SIMPLE_PAIRING_COMPLETE),
        "SSP never started, so nobody may be told it completed: {b_seen:?}"
    );
}

#[test]
fn test_set_connection_encryption_changes_both_ends() {
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();
    let policy = SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM);
    run_ssp(&mut link, &a, &b, policy, policy);

    a.send_command(&cmd(
        opcode::SET_CONNECTION_ENCRYPTION,
        &[handle as u8, (handle >> 8) as u8, 0x01],
    ))
    .unwrap();
    link.tick();
    link.tick();

    let a_evts = events(&a);
    assert_eq!(
        command_status_for(&a_evts, opcode::SET_CONNECTION_ENCRYPTION),
        Some(STATUS_SUCCESS),
        "Set Connection Encryption is Command-Status-answered: {a_evts:?}"
    );
    assert!(
        command_complete_for(&a_evts, opcode::SET_CONNECTION_ENCRYPTION).is_none(),
        "a Command Complete strands the host on the Encryption Change"
    );
    let expected = [
        STATUS_SUCCESS,
        handle as u8,
        (handle >> 8) as u8,
        ENCRYPTION_ON,
    ];
    assert_eq!(
        first_of(&a_evts, event::ENCRYPTION_CHANGE),
        Some(&expected[..]),
        "{a_evts:?}"
    );
    let b_evts = events(&b);
    assert_eq!(
        first_of(&b_evts, event::ENCRYPTION_CHANGE),
        Some(&expected[..]),
        "encryption is a property of the link, so both hosts are told: \
             {b_evts:?}"
    );
}

#[test]
fn test_set_connection_encryption_on_an_unknown_handle_is_refused_as_a_status() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::SET_CONNECTION_ENCRYPTION, &[0x99, 0x00, 0x01]))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::SET_CONNECTION_ENCRYPTION),
        Some(STATUS_UNKNOWN_CONNECTION),
        "an error answer to a status-type command is still a status: \
             {evts:?}"
    );
    assert!(first_of(&evts, event::ENCRYPTION_CHANGE).is_none());
}

#[test]
fn test_change_connection_link_key_notifies_both_and_completes() {
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();
    let policy = SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM);
    let (paired, _) = run_ssp(&mut link, &a, &b, policy, policy);
    let old_key = first_of(&paired, event::LINK_KEY_NOTIFICATION).unwrap()[6..22].to_vec();

    a.send_command(&cmd(
        opcode::CHANGE_CONNECTION_LINK_KEY,
        &handle.to_le_bytes(),
    ))
    .unwrap();
    link.tick();
    link.tick();

    let a_evts = events(&a);
    let b_evts = events(&b);
    assert_eq!(
        command_status_for(&a_evts, opcode::CHANGE_CONNECTION_LINK_KEY),
        Some(STATUS_SUCCESS),
        "{a_evts:?}"
    );
    let new_key = first_of(&a_evts, event::LINK_KEY_NOTIFICATION)
        .expect("a rotated key has to be told to its host")[6..22]
        .to_vec();
    assert_ne!(
        new_key, old_key,
        "a rotation that returns the old key is not one"
    );
    assert_eq!(
        first_of(&b_evts, event::LINK_KEY_NOTIFICATION).unwrap()[6..22],
        new_key[..],
        "both ends store the same new key or the next reconnect fails"
    );
    assert_eq!(
        first_of(&a_evts, event::CHANGE_CONNECTION_LINK_KEY_COMPLETE).map(|p| p[0]),
        Some(STATUS_SUCCESS),
        "{a_evts:?}"
    );
}

#[test]
fn test_change_connection_link_key_on_an_unauthenticated_link_is_disallowed() {
    let mut link = Link::new();
    let (a, _b, handle) = ssp_pair_connected(&mut link);

    a.send_command(&cmd(
        opcode::CHANGE_CONNECTION_LINK_KEY,
        &handle.to_le_bytes(),
    ))
    .unwrap();
    link.tick();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::CHANGE_CONNECTION_LINK_KEY),
        Some(STATUS_COMMAND_DISALLOWED),
        "there is no key to change: {evts:?}"
    );
    assert!(
        first_of(&evts, event::CHANGE_CONNECTION_LINK_KEY_COMPLETE).is_none(),
        "an error Command Status ends the command; a completion event \
             after one is a second answer to a question already answered"
    );
}

#[test]
fn test_link_key_selection_is_answered_with_a_command_status_and_nothing_else() {
    let mut link = Link::new();
    let a = link.add_device(addr("AA:BB:CC:00:00:01"));
    link.tick();
    let _ = events(&a);

    a.send_command(&cmd(opcode::LINK_KEY_SELECTION, &[0x01]))
        .unwrap();
    link.tick();

    let evts = events(&a);
    assert_eq!(
        command_status_for(&evts, opcode::LINK_KEY_SELECTION),
        Some(STATUS_SUCCESS),
        "{evts:?}"
    );
    assert!(command_complete_for(&evts, opcode::LINK_KEY_SELECTION).is_none());
}

#[test]
fn test_le_enable_encryption_asks_the_peer_for_the_key_then_encrypts_both() {
    // The step LE has been missing: `smp/pairing.rs` computes an LTK and
    // until now had no controller to hand it to, so no link ever actually
    // became encrypted.
    let mut link = Link::new();
    let (central, peripheral, handle) = le_pair_connected(&mut link);

    let ltk = [0x5A; 16];
    let mut params = handle.to_le_bytes().to_vec();
    params.extend_from_slice(&[0x11; 8]); // Random_Number
    params.extend_from_slice(&[0x22, 0x33]); // EDIV
    params.extend_from_slice(&ltk);
    central
        .send_command(&cmd(opcode::LE_ENABLE_ENCRYPTION, &params))
        .unwrap();
    link.tick();

    let central_evts = events(&central);
    assert_eq!(
        command_status_for(&central_evts, opcode::LE_ENABLE_ENCRYPTION),
        Some(STATUS_SUCCESS),
        "{central_evts:?}"
    );
    assert!(
        command_complete_for(&central_evts, opcode::LE_ENABLE_ENCRYPTION).is_none(),
        "LE Enable Encryption is Command-Status-answered"
    );

    // The peripheral's host is asked for the key, with the same Random
    // Number and EDIV the central named — that is how it finds which of
    // its stored keys applies.
    let requests = le_subevents(&peripheral, event::LE_LONG_TERM_KEY_REQUEST);
    let request = requests.first().expect("the peripheral is asked for a key");
    assert_eq!(
        &request[6..14],
        &[0x11; 8],
        "the Random Number is carried through"
    );
    assert_eq!(&request[14..16], &[0x22, 0x33], "and the EDIV");

    let mut reply = handle.to_le_bytes().to_vec();
    reply.extend_from_slice(&ltk);
    peripheral
        .send_command(&cmd(opcode::LE_LTK_REQUEST_REPLY, &reply))
        .unwrap();
    link.tick();

    let expected = [
        STATUS_SUCCESS,
        handle as u8,
        (handle >> 8) as u8,
        ENCRYPTION_ON,
    ];
    let peripheral_evts = events(&peripheral);
    assert_eq!(
        command_complete_for(&peripheral_evts, opcode::LE_LTK_REQUEST_REPLY).map(|p| p[0]),
        Some(STATUS_SUCCESS),
        "the reply *is* the answer, so it completes: {peripheral_evts:?}"
    );
    assert_eq!(
        first_of(&peripheral_evts, event::ENCRYPTION_CHANGE),
        Some(&expected[..]),
        "{peripheral_evts:?}"
    );
    let central_evts = events(&central);
    assert_eq!(
        first_of(&central_evts, event::ENCRYPTION_CHANGE),
        Some(&expected[..]),
        "{central_evts:?}"
    );
}

#[test]
fn test_le_encryption_with_no_key_fails_at_the_central_only() {
    let mut link = Link::new();
    let (central, peripheral, handle) = le_pair_connected(&mut link);

    let mut params = handle.to_le_bytes().to_vec();
    params.extend_from_slice(&[0x00; 10]);
    params.extend_from_slice(&[0x5A; 16]);
    central
        .send_command(&cmd(opcode::LE_ENABLE_ENCRYPTION, &params))
        .unwrap();
    link.tick();
    let _ = events(&central);

    // The peripheral has no key for this diversifier, which is the normal
    // way a bond that one side forgot shows up.
    peripheral
        .send_command(&cmd(
            opcode::LE_LTK_REQUEST_NEGATIVE_REPLY,
            &handle.to_le_bytes(),
        ))
        .unwrap();
    link.tick();

    let central_evts = events(&central);
    assert_eq!(
        first_of(&central_evts, event::ENCRYPTION_CHANGE).map(|p| p[0]),
        Some(STATUS_PIN_OR_KEY_MISSING),
        "{central_evts:?}"
    );
    assert!(
        first_of(&events(&peripheral), event::ENCRYPTION_CHANGE).is_none(),
        "the peripheral never asked for encryption, so it is owed no \
             Encryption Change"
    );
}

#[test]
fn test_association_model_follows_the_core_table() {
    use io_capability::{DISPLAY_ONLY, DISPLAY_YES_NO, KEYBOARD_ONLY, NO_INPUT_NO_OUTPUT};
    let mitm = AUTH_REQ_MITM;
    let none = 0x00;

    // Rule 1: neither side wants MITM protection, so the table is never
    // consulted — even for two DisplayYesNo devices, which is the case a
    // table-only reading gets wrong.
    assert_eq!(
        association_model(DISPLAY_YES_NO, none, DISPLAY_YES_NO, none),
        AssociationModel::JustWorks
    );

    // Rule 2, the table (Core Vol 3, Part C, 5.2.2.6 Table 5.7).
    assert_eq!(
        association_model(DISPLAY_YES_NO, mitm, DISPLAY_YES_NO, none),
        AssociationModel::NumericComparison,
        "one side asking is enough to escalate"
    );
    assert_eq!(
        association_model(DISPLAY_ONLY, mitm, DISPLAY_YES_NO, mitm),
        AssociationModel::JustWorks,
        "a DisplayOnly device cannot answer, so its confirmation is \
             automatic"
    );
    assert_eq!(
        association_model(KEYBOARD_ONLY, mitm, DISPLAY_ONLY, mitm),
        AssociationModel::PasskeyEntry
    );
    assert_eq!(
        association_model(DISPLAY_YES_NO, mitm, KEYBOARD_ONLY, mitm),
        AssociationModel::PasskeyEntry
    );
    assert_eq!(
        association_model(KEYBOARD_ONLY, mitm, KEYBOARD_ONLY, mitm),
        AssociationModel::PasskeyEntry,
        "two keyboards: the user types the same digits on both"
    );
    for other in [
        DISPLAY_ONLY,
        DISPLAY_YES_NO,
        KEYBOARD_ONLY,
        NO_INPUT_NO_OUTPUT,
    ] {
        assert_eq!(
            association_model(NO_INPUT_NO_OUTPUT, mitm, other, mitm),
            AssociationModel::JustWorks,
            "nothing a person does can protect a link to a device with \
                 no input and no output"
        );
    }
}

#[test]
fn test_a_derived_link_key_is_symmetric_and_stable() {
    // The three properties the sequence actually needs from a key. Not
    // the spec's f2 — see `derived_link_key` — but a key that failed any
    // of these would break the bonded-reconnect path.
    let a = addr("AA:BB:CC:00:00:01");
    let b = addr("AA:BB:CC:00:00:02");
    let c = addr("AA:BB:CC:00:00:03");
    assert_eq!(derived_link_key(a, b), derived_link_key(b, a));
    assert_eq!(derived_link_key(a, b), derived_link_key(a, b));
    assert_ne!(derived_link_key(a, b), derived_link_key(a, c));
}

#[test]
fn test_pairing_digits_are_six() {
    let key = derived_link_key(addr("AA:BB:CC:00:00:01"), addr("AA:BB:CC:00:00:02"));
    assert!(pairing_digits(&key, 0) < 1_000_000);
    assert!(pairing_digits(&key, 1) < 1_000_000);
    assert_ne!(
        pairing_digits(&key, 0),
        pairing_digits(&key, 1),
        "the confirmation value and the passkey are different numbers"
    );
}

#[test]
fn test_a_disconnect_drops_the_pairing_in_flight() {
    // A pairing that outlived its link would hand its Authentication
    // Complete to a handle that has since been reused.
    let mut link = Link::new();
    let (a, b, handle) = ssp_pair_connected(&mut link);
    a.send_command(&cmd(
        opcode::AUTHENTICATION_REQUESTED,
        &handle.to_le_bytes(),
    ))
    .unwrap();
    link.tick();
    let _ = events(&a);
    let _ = events(&b);

    let mut disconnect = handle.to_le_bytes().to_vec();
    disconnect.push(0x13);
    a.send_command(&cmd(opcode::DISCONNECT, &disconnect))
        .unwrap();
    link.tick();
    link.tick();

    let policy = SspPolicy::agreeable(io_capability::DISPLAY_YES_NO, AUTH_REQ_MITM);
    let (a_seen, _) = run_ssp(&mut link, &a, &b, policy, policy);
    assert!(
        !codes(&a_seen).contains(&event::AUTHENTICATION_COMPLETE),
        "the pairing died with the link: {a_seen:?}"
    );
}
