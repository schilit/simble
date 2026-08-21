// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Cross-layer Bluetooth Classic integration tests: LMP link establishment,
//! L2CAP Classic channel setup, and the SDP and RFCOMM protocols riding on
//! top of it, chained end to end between two simulated peers rather than
//! tested against directly-constructed peer objects in isolation.

use simble::classic::rfcomm::{
    DEFAULT_INITIAL_CREDITS, DEFAULT_L2CAP_MTU, DEFAULT_MAX_FRAME_SIZE, Multiplexer,
    MultiplexerEvent, RfcommClient, RfcommServer, Role,
};
use simble::classic::sdp::{
    DataElement, SdpClient, SdpServer, SdpUuid, Service, ServiceAttribute, attribute_id,
};
use simble::controller::lmp::{
    LmpLink, LmpLinkState, LmpNotAccepted, LmpRole, opcode as lmp_opcode,
};
use simble::l2cap::classic::{ClassicChannelManager, ClassicChannelSpec};
use simble::l2cap::{L2capHeader, cid};
use simble::smp::SmpPairingFailed;
use simble::{Address, AddressType, VirtualDevice};
use std::collections::HashMap;
use zerocopy::IntoBytes;

const SAMPLE_CLASS_UUID: SdpUuid = SdpUuid::Uuid128([
    0xAE, 0xD3, 0xF6, 0x3A, 0x14, 0xB1, 0xBB, 0x96, 0x85, 0x4B, 0xB4, 0xC8, 0x59, 0x56, 0xD5, 0xE6,
]);

fn sample_records() -> HashMap<u32, Service> {
    let handle = 0x0001_0001;
    HashMap::from([(
        handle,
        vec![
            ServiceAttribute::new(
                attribute_id::SERVICE_RECORD_HANDLE,
                DataElement::unsigned_integer_32(handle),
            ),
            ServiceAttribute::new(
                attribute_id::SERVICE_CLASS_ID_LIST,
                DataElement::sequence(vec![DataElement::uuid(SAMPLE_CLASS_UUID)]),
            ),
        ],
    )])
}

/// Shuttles bytes between two `LmpLink`s until both queues drain, mirroring
/// `tests/lmp_test.rs`'s handshake driver, since integration test binaries
/// don't share code across files.
fn run_lmp_handshake(central: &mut LmpLink, peripheral: &mut LmpLink) {
    let mut to_peripheral = vec![central.build_connection_request().expect("central starts")];
    let mut to_central: Vec<Vec<u8>> = Vec::new();

    for _ in 0..8 {
        if to_peripheral.is_empty() && to_central.is_empty() {
            return;
        }
        let mut next_to_central = Vec::new();
        for pkt in to_peripheral.drain(..) {
            next_to_central.extend(peripheral.receive(&pkt).expect("peripheral accepts"));
        }
        let mut next_to_peripheral = Vec::new();
        for pkt in to_central.drain(..) {
            next_to_peripheral.extend(central.receive(&pkt).expect("central accepts"));
        }
        to_central = next_to_central;
        to_peripheral = next_to_peripheral;
    }
    panic!("LMP handshake did not converge");
}

/// Opens a Classic Basic Mode L2CAP channel between a fresh client/server
/// manager pair, following the same connect -> configure -> configure
/// sequence as `tests/sdp_test.rs`'s `test_sdp_over_classic_l2cap_channel`.
/// Returns the client and server CIDs once both sides report `Open`.
fn open_classic_l2cap_channel(
    client_mgr: &mut ClassicChannelManager,
    server_mgr: &mut ClassicChannelManager,
    psm: u16,
) -> (u16, u16) {
    let (client_cid, request) = client_mgr
        .connect(&ClassicChannelSpec::new(psm))
        .expect("client can open a connection request");
    complete_classic_l2cap_open(client_mgr, server_mgr, client_cid, &request)
}

/// The configure -> configure half of channel setup, shared by
/// `open_classic_l2cap_channel` (which drives the connect step itself) and
/// the RFCOMM chain test (which drives the connect step through
/// `RfcommClient::connect` instead, to exercise that facade).
fn complete_classic_l2cap_open(
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

    assert!(client_mgr.get_channel(client_cid).unwrap().is_open());
    assert!(server_mgr.get_channel(server_cid).unwrap().is_open());

    (client_cid, server_cid)
}

/// Shuttles RFCOMM frames between an initiator and responder `Multiplexer`
/// until both queues drain, framing every hop with `L2capHeader` over the
/// two channel CIDs (mirroring `run_lmp_handshake`'s byte-shuttling, but for
/// the RFCOMM session/DLC handshake instead of LMP).
fn drive_rfcomm(
    initiator: &mut Multiplexer,
    responder: &mut Multiplexer,
    server_cid: u16,
    client_cid: u16,
    from_initiator: Vec<u8>,
) -> (Vec<MultiplexerEvent>, Vec<MultiplexerEvent>) {
    let mut to_responder = vec![from_initiator];
    let mut to_initiator: Vec<Vec<u8>> = Vec::new();
    let mut initiator_events = Vec::new();
    let mut responder_events = Vec::new();

    for _ in 0..16 {
        if to_responder.is_empty() && to_initiator.is_empty() {
            break;
        }
        let mut next_to_initiator = Vec::new();
        for pkt in to_responder.drain(..) {
            let framed = L2capHeader::serialize(server_cid, &pkt);
            let (_, payload) = L2capHeader::parse(&framed).unwrap();
            let (frames, events) = responder.receive(payload).expect("responder accepts frame");
            next_to_initiator.extend(frames);
            responder_events.extend(events);
        }
        let mut next_to_responder = Vec::new();
        for pkt in to_initiator.drain(..) {
            let framed = L2capHeader::serialize(client_cid, &pkt);
            let (_, payload) = L2capHeader::parse(&framed).unwrap();
            let (frames, events) = initiator.receive(payload).expect("initiator accepts frame");
            next_to_responder.extend(frames);
            initiator_events.extend(events);
        }
        to_initiator = next_to_initiator;
        to_responder = next_to_responder;
    }
    (initiator_events, responder_events)
}

/// Full chain: two `LmpLink`s converge on `Connected`, then the two
/// corresponding devices' `ClassicChannelManager`s open a Basic Mode L2CAP
/// channel on the SDP PSM, then an `SdpClient`/`SdpServer` pair runs a
/// service search over that channel.
///
/// `ClassicChannelManager::connect` takes no `LmpLink` (or any link-state
/// input) at all — nothing in its signature or body reads `LmpLink::state`.
/// So step 2 here does not, and currently *cannot*, depend on step 1 having
/// succeeded; a caller could open an L2CAP channel between two managers
/// whose underlying LMP links never connected, or don't exist at all, and
/// nothing here would object. This test still runs both steps in sequence
/// and asserts each succeeds on its own terms, but the sequencing itself is
/// enforced only by this test's control flow, not by any dependency in the
/// production code. Wiring a real gate (e.g. `ClassicChannelManager`
/// refusing to operate until told its underlying `LmpLink` is connected) is
/// the production gap this test is designed to surface, not something to
/// paper over here.
#[test]
fn test_lmp_l2cap_and_sdp_chained_end_to_end() {
    let mut central = LmpLink::new(LmpRole::Central, [0xFF; 8]);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0xFF; 8]);
    run_lmp_handshake(&mut central, &mut peripheral);
    assert_eq!(central.state, LmpLinkState::Connected);
    assert_eq!(peripheral.state, LmpLinkState::Connected);

    let mut client_mgr = ClassicChannelManager::new();
    let mut server_mgr = ClassicChannelManager::new();
    let mut server = SdpServer::new();
    server.register(&mut server_mgr).unwrap();
    server.service_records.extend(sample_records());

    let (client_cid, server_cid) = open_classic_l2cap_channel(
        &mut client_mgr,
        &mut server_mgr,
        simble::classic::sdp::SDP_PSM,
    );

    let mut client = SdpClient::new();
    let services = client
        .search_services(&[SAMPLE_CLASS_UUID], |req| {
            let framed = L2capHeader::serialize(server_cid, req);
            let (_, l2cap_payload) = L2capHeader::parse(&framed).unwrap();
            let response = server.handle_request(l2cap_payload, 2048);
            let framed_response = L2capHeader::serialize(client_cid, &response);
            let (_, response_payload) = L2capHeader::parse(&framed_response).unwrap();
            response_payload.to_vec()
        })
        .unwrap();
    assert_eq!(services, vec![0x0001_0001]);
}

/// Same LMP -> L2CAP handoff as above, isolated into its own test so a
/// regression in either layer's connection setup (independent of SDP) is
/// easy to localize.
#[test]
fn test_lmp_connected_link_followed_by_l2cap_channel_open() {
    let mut central = LmpLink::new(LmpRole::Central, [0x01; 8]);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0x01; 8]);
    run_lmp_handshake(&mut central, &mut peripheral);
    assert!(central.is_connected());
    assert!(peripheral.is_connected());

    let mut client_mgr = ClassicChannelManager::new();
    let mut server_mgr = ClassicChannelManager::new();
    server_mgr.register_server(0x1001).unwrap();

    let (client_cid, server_cid) =
        open_classic_l2cap_channel(&mut client_mgr, &mut server_mgr, 0x1001);
    assert!(client_mgr.get_channel(client_cid).unwrap().is_open());
    assert!(server_mgr.get_channel(server_cid).unwrap().is_open());
}

/// Cross-layer sequencing check: when the peripheral rejects the LMP
/// connection, a correctly-written caller must never proceed to open an
/// L2CAP channel over the (nonexistent) link. `ClassicChannelManager` has no
/// way to know the LMP handshake failed (see the doc comment on
/// `test_lmp_l2cap_and_sdp_chained_end_to_end`), so the guard has to live in
/// the caller; this test plays the role of that caller and asserts it
/// correctly refrains, rather than asserting any built-in protection exists.
#[test]
fn test_lmp_rejection_prevents_l2cap_channel_attempt() {
    let mut central = LmpLink::new(LmpRole::Central, [0; 8]);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0; 8]);
    peripheral.accept_connections = false;

    let req = central.build_connection_request().unwrap();
    let peripheral_responses = peripheral.receive(&req).unwrap();
    let (not_accepted, _) = LmpNotAccepted::parse(&peripheral_responses[0])
        .expect("peripheral must reply with LMP_not_accepted");
    assert_eq!(
        not_accepted.rejected_opcode,
        lmp_opcode::HOST_CONNECTION_REQ
    );
    central.receive(&peripheral_responses[0]).unwrap();

    assert_eq!(central.state, LmpLinkState::Rejected);
    assert_eq!(peripheral.state, LmpLinkState::Rejected);
    assert!(!central.is_connected());
    assert!(!peripheral.is_connected());

    let mut client_mgr = ClassicChannelManager::new();
    let mut server_mgr = ClassicChannelManager::new();
    server_mgr
        .register_server(simble::classic::sdp::SDP_PSM)
        .unwrap();

    let attempted_l2cap_connect = central.is_connected() && peripheral.is_connected();
    assert!(
        !attempted_l2cap_connect,
        "caller must not attempt to open an L2CAP channel over a rejected LMP link"
    );
    if attempted_l2cap_connect {
        open_classic_l2cap_channel(
            &mut client_mgr,
            &mut server_mgr,
            simble::classic::sdp::SDP_PSM,
        );
    }

    assert!(client_mgr.get_channel(0).is_none());
    assert!(server_mgr.get_channel(0).is_none());
}

/// Formerly a regression guard pinning down that `cid::SMP` was silently
/// dropped by `VirtualDevice::process_l2cap_packet` (it only routed
/// `cid::ATT`, falling through `_ => Ok(None)` for everything else). SMP
/// routing has since been wired up to a real `PairingSession` state machine
/// (`src/smp/pairing.rs`); this test now asserts the real behavior instead:
/// a Pairing Request addressed to a connected device reaches the SMP handler
/// and gets a real Pairing Response back, and an unsolicited Pairing Failed
/// (no session in progress) is still correctly ignored rather than starting
/// one out of nowhere.
#[test]
fn test_virtual_device_smp_packet_is_routed_to_pairing_session() {
    let addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    let mut dev = VirtualDevice::new("smp-routed", addr, AddressType::Random);
    let peer = Address::from_be_bytes([0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    let conn_handle = 0x0040;
    dev.on_connected(conn_handle, peer);

    // An unsolicited Pairing Failed with no pairing in progress is ignored,
    // not treated as the start of a session.
    let pairing_failed = SmpPairingFailed::new(0x05); // reason: pairing not supported
    let l2cap_packet = L2capHeader::serialize(cid::SMP, pairing_failed.as_bytes());
    assert_eq!(
        dev.process_l2cap_packet(conn_handle, &l2cap_packet),
        Ok(None)
    );
    assert!(
        dev.connections
            .get(&conn_handle)
            .unwrap()
            .pairing_session
            .is_none()
    );

    // A real Pairing Request reaches the SMP handler and gets a real
    // Pairing Response back, proving `cid::SMP` is routed end to end.
    let request = simble::smp::SmpPairingPacket {
        opcode: simble::smp::opcode::PAIRING_REQUEST,
        io_capability: simble::smp::io_capability::NO_INPUT_NO_OUTPUT,
        oob_data_flag: 0,
        auth_req: 0x01, // Bonding only
        max_encryption_key_size: 16,
        initiator_key_distribution: 0x03,
        responder_key_distribution: 0x03,
    };
    let request_packet = L2capHeader::serialize(cid::SMP, request.as_bytes());
    let response = dev
        .process_l2cap_packet(conn_handle, &request_packet)
        .unwrap()
        .expect("a Pairing Request must get a real Pairing Response");
    let (header, response_payload) = L2capHeader::parse(&response).expect("valid L2CAP frame");
    assert_eq!(header.cid.get(), cid::SMP);
    assert_eq!(response_payload[0], simble::smp::opcode::PAIRING_RESPONSE);
    assert!(
        dev.connections
            .get(&conn_handle)
            .unwrap()
            .pairing_session
            .is_some()
    );
}

/// The full chain: LMP link establishment, then an L2CAP Classic channel on
/// the RFCOMM PSM, then an RFCOMM multiplexer session brought up over that
/// channel, then a DLC opened within it, then application data exchanged
/// over the DLC and observed on the peer side as a `DataReceived` event.
///
/// Same non-dependency caveat as `test_lmp_l2cap_and_sdp_chained_end_to_end`
/// applies here too: nothing in `Multiplexer` or `ClassicChannelManager`
/// reads `LmpLink` state, so this test's ordering (LMP, then L2CAP, then
/// RFCOMM) is enforced by this test's control flow only, not by the
/// production code.
#[test]
fn test_lmp_l2cap_and_rfcomm_chained_end_to_end() {
    let mut central = LmpLink::new(LmpRole::Central, [0xFF; 8]);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0xFF; 8]);
    run_lmp_handshake(&mut central, &mut peripheral);
    assert!(central.is_connected());
    assert!(peripheral.is_connected());

    let mut client_mgr = ClassicChannelManager::new();
    let mut server_mgr = ClassicChannelManager::new();
    let mut rfcomm_server = RfcommServer::new();
    rfcomm_server.register(&mut server_mgr).unwrap();
    let server_channel = 1u8;
    assert!(rfcomm_server.listen(
        server_channel,
        DEFAULT_MAX_FRAME_SIZE,
        DEFAULT_INITIAL_CREDITS
    ));

    let (client_cid, request) = RfcommClient::connect(&mut client_mgr, DEFAULT_L2CAP_MTU).unwrap();
    let (client_cid, server_cid) =
        complete_classic_l2cap_open(&mut client_mgr, &mut server_mgr, client_cid, &request);

    let client_channel = client_mgr.get_channel(client_cid).unwrap();
    let server_channel_obj = server_mgr.get_channel(server_cid).unwrap();
    let mut initiator = Multiplexer::new(Role::Initiator, client_channel.peer_mtu);
    let mut responder = rfcomm_server.new_multiplexer(server_channel_obj);

    let start_frame = initiator.start().unwrap();
    let (initiator_events, responder_events) = drive_rfcomm(
        &mut initiator,
        &mut responder,
        server_cid,
        client_cid,
        start_frame,
    );
    assert!(initiator.is_connected());
    assert!(responder.is_connected());
    assert!(initiator_events.contains(&MultiplexerEvent::Started));
    assert!(responder_events.contains(&MultiplexerEvent::Started));

    let open_frame = initiator
        .open_dlc(
            server_channel,
            DEFAULT_MAX_FRAME_SIZE,
            DEFAULT_INITIAL_CREDITS,
        )
        .unwrap();
    let (initiator_events, responder_events) = drive_rfcomm(
        &mut initiator,
        &mut responder,
        server_cid,
        client_cid,
        open_frame,
    );
    let dlci = server_channel << 1;
    assert!(initiator_events.contains(&MultiplexerEvent::DlcOpened(dlci)));
    assert!(responder_events.contains(&MultiplexerEvent::DlcOpened(dlci)));
    assert!(initiator.dlcs.get(&dlci).unwrap().is_open());
    assert!(responder.dlcs.get(&dlci).unwrap().is_open());

    let payload = b"hello over rfcomm".to_vec();
    let data_frames = initiator.write(dlci, &payload).unwrap();
    let mut responder_events = Vec::new();
    for frame in data_frames {
        let (_, events) = drive_rfcomm(
            &mut initiator,
            &mut responder,
            server_cid,
            client_cid,
            frame,
        );
        responder_events.extend(events);
    }
    assert!(
        responder_events
            .iter()
            .any(|e| matches!(e, MultiplexerEvent::DataReceived(d, data) if *d == dlci && data == &payload)),
        "responder must observe the payload written on the initiator's DLC"
    );
}
