use super::*;
use crate::packets::ConnectionResponseHeader;

fn connection_request_event(addr: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0x04, 0x04, 0x0A];
    packet.extend_from_slice(&addr);
    packet.extend_from_slice(&[0x04, 0x04, 0x24]); // class of device
    packet.push(0x01); // ACL
    packet
}

fn connection_complete_event(handle: u16, addr: [u8; 6]) -> Vec<u8> {
    let mut packet = vec![0x04, 0x03, 0x0B, 0x00];
    packet.extend_from_slice(&handle.to_le_bytes());
    packet.extend_from_slice(&addr);
    packet.push(0x01); // ACL
    packet.push(0x00); // encryption off
    packet
}

fn host() -> ClassicHost {
    let mut host = ClassicHost::new("SimbleClassic", [0x04, 0x04, 0x24]);
    host.register_handler(Box::new(SdpHandler::default()))
        .unwrap();
    host
}

#[test]
fn test_bring_up_makes_the_device_discoverable_and_connectable() {
    let commands = host().start_commands();
    // The last command must enable both scans, or a peer never sees it.
    let scan = commands.last().expect("bring-up is not empty");
    assert_eq!(&scan[1..3], &opcode::WRITE_SCAN_ENABLE);
    assert_eq!(scan[4], scan_enable::INQUIRY_AND_PAGE);
    // The name and class of device are set before scanning starts.
    assert!(commands.iter().any(|c| c[1..3] == opcode::WRITE_LOCAL_NAME));
    assert!(
        commands
            .iter()
            .any(|c| c[1..3] == opcode::WRITE_CLASS_OF_DEVICE)
    );
}

#[test]
fn test_inbound_page_is_accepted_and_tracked() {
    let mut host = host();
    let addr = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

    let out = host.handle_packet(&connection_request_event(addr)).unwrap();
    assert_eq!(
        &out[0][1..3],
        &opcode::ACCEPT_CONNECTION_REQUEST,
        "an unanswered page times out"
    );

    assert!(host.connection().is_none());
    host.handle_packet(&connection_complete_event(0x0080, addr))
        .unwrap();
    let (handle, peer) = host.connection().expect("connection tracked");
    assert_eq!(handle, 0x0080);
    assert_eq!(peer, Address::new(addr));
}

#[test]
fn test_l2cap_handshake_opens_an_sdp_channel() {
    let mut host = host();
    let addr = [0x11; 6];
    host.handle_packet(&connection_request_event(addr)).unwrap();
    host.handle_packet(&connection_complete_event(0x0080, addr))
        .unwrap();

    // Peer opens SDP: Connection Request for PSM 0x0001, source CID 0x0040.
    let request = ConnectionRequestHeader {
        psm: SDP_PSM.into(),
        source_cid: 0x0040u16.into(),
    };
    let pdu = signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes());
    // signaling_pdu already wraps in an L2CAP header; feed it as ACL.
    let out = host.handle_packet(&acl_packet(0x0080, &pdu)).unwrap();
    assert_eq!(out.len(), 2, "connection response, then our config request");

    // The response must accept (result 0x0000) and name our local CID.
    // H4(1) + ACL header(4) + L2CAP header(4) + signalling header(4).
    let response_body = &out[0][13..];
    let (response, _) = ConnectionResponseHeader::ref_from_prefix(response_body).unwrap();
    assert_eq!(response.result.get(), 0x0000, "SDP PSM must be accepted");
    let local_cid = response.destination_cid.get();
    assert_ne!(local_cid, 0);

    // Peer configures us, and acks our configuration: channel opens.
    let mut config = ConfigurationRequestHeader {
        destination_cid: local_cid.into(),
        flags: 0u16.into(),
    }
    .as_bytes()
    .to_vec();
    config.extend_from_slice(&[0x01, 0x02, 0xA0, 0x02]); // MTU option, 672
    host.handle_packet(&acl_packet(
        0x0080,
        &signaling_pdu(signaling_code::CONFIGURATION_REQUEST, 2, &config),
    ))
    .unwrap();

    let ack = ConfigurationResponseHeader {
        source_cid: local_cid.into(),
        flags: 0u16.into(),
        result: 0u16.into(),
    };
    host.handle_packet(&acl_packet(
        0x0080,
        &signaling_pdu(signaling_code::CONFIGURATION_RESPONSE, 1, ack.as_bytes()),
    ))
    .unwrap();

    assert!(
        host.has_open_channel(),
        "both sides configured — the channel must be open"
    );
}

#[test]
fn test_sdp_request_on_an_open_channel_is_answered() {
    let mut host = host();
    let addr = [0x11; 6];
    host.handle_packet(&connection_request_event(addr)).unwrap();
    host.handle_packet(&connection_complete_event(0x0080, addr))
        .unwrap();
    let request = ConnectionRequestHeader {
        psm: SDP_PSM.into(),
        source_cid: 0x0040u16.into(),
    };
    let out = host
        .handle_packet(&acl_packet(
            0x0080,
            &signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes()),
        ))
        .unwrap();
    let (response, _) = ConnectionResponseHeader::ref_from_prefix(&out[0][13..]).unwrap();
    let local_cid = response.destination_cid.get();

    // A malformed SDP request still gets an SDP error response, which
    // proves the data path reaches the server and comes back.
    let out = host.handle_channel_data(0x0080, local_cid, &[0xFF, 0x00, 0x00]);
    assert_eq!(out.len(), 1, "SDP must answer on the same channel");
    let reply = &out[0][9..];
    assert_eq!(
        reply[0], 0x01,
        "SDP ErrorResponse PDU id, i.e. the server ran"
    );
}

#[test]
fn test_spp_record_is_discoverable_through_sdp() {
    // A peer finds SPP by searching for the Serial Port service class;
    // the record must carry the class and the RFCOMM channel.
    let mut handler = SdpHandler::default();
    handler
        .server_mut()
        .service_records
        .insert(0x00010001, spp_service_record(0x00010001, 3, "Simble SPP"));

    let record = &handler.server_mut().service_records[&0x00010001];
    let class_list = record
        .iter()
        .find(|a| a.id == attribute_id::SERVICE_CLASS_ID_LIST)
        .expect("record names its service class");
    assert_eq!(
        class_list.value,
        DataElement::sequence(vec![DataElement::uuid(SdpUuid::Uuid16(0x1101))])
    );

    // The protocol descriptor must be L2CAP then RFCOMM/channel 3, or a
    // peer cannot work out where to connect.
    let protocols = record
        .iter()
        .find(|a| a.id == attribute_id::PROTOCOL_DESCRIPTOR_LIST)
        .expect("record names its protocol stack");
    let DataElement::Sequence(layers) = &protocols.value else {
        panic!("protocol descriptor list must be a sequence");
    };
    assert_eq!(layers.len(), 2);
    assert_eq!(
        layers[1],
        DataElement::sequence(vec![
            DataElement::uuid(SdpUuid::Uuid16(0x0003)),
            DataElement::unsigned_integer(3, 1),
        ])
    );
}

/// A peer that answers every continuation with another continuation
/// would keep an SDP client asking for ever. The watchdog stops it, and
/// — this is the part that matters — says the answer is a *prefix*
/// rather than letting a partial record list pass for the whole
/// database. Acting on a truncated answer opens a DLC on a channel
/// nobody is listening to.
#[test]
fn test_an_endless_sdp_continuation_is_stopped_and_declared_partial() {
    use crate::classic::sdp::{SDP_CONTINUATION_WATCHDOG, SdpPdu};

    let (mut query, results) = SdpQueryHandler::searching(SdpUuid::Uuid16(0x1101));
    assert_eq!(query.poll_output(672).len(), 1, "the query goes out once");

    // A response that never ends: an empty attribute list and a
    // continuation state that is never the null one.
    let endless = SdpPdu::ServiceSearchAttributeResponse {
        transaction_id: 1,
        attribute_lists: Vec::new(),
        continuation_state: vec![0x01, 0x00],
    }
    .to_bytes();

    let mut asked_again = 0;
    for _ in 0..=SDP_CONTINUATION_WATCHDOG {
        asked_again += query.on_data(&endless, 672).len();
    }
    assert_eq!(
        asked_again as u32, SDP_CONTINUATION_WATCHDOG,
        "the client asks again exactly {SDP_CONTINUATION_WATCHDOG} times, \
         then gives up"
    );

    let results = results.lock().expect("results readable");
    assert!(results.answered, "giving up is still an outcome to report");
    assert!(
        results.truncated,
        "and it must be reported as partial, not as a peer with no services"
    );
}

#[test]
fn test_disconnect_restores_discoverability() {
    let mut host = host();
    let addr = [0x11; 6];
    host.handle_packet(&connection_request_event(addr)).unwrap();
    host.handle_packet(&connection_complete_event(0x0080, addr))
        .unwrap();

    let out = host
        .handle_packet(&[0x04, 0x05, 0x04, 0x00, 0x80, 0x00, 0x13])
        .unwrap();
    assert!(host.connection().is_none());
    assert_eq!(&out[0][1..3], &opcode::WRITE_SCAN_ENABLE);
    assert_eq!(out[0][4], scan_enable::INQUIRY_AND_PAGE);
}

// ---------------------------------------------------------------------
// SCO / eSCO — the call audio
// ---------------------------------------------------------------------

/// A Connection Request with a synchronous link type.
fn synchronous_request_event(addr: [u8; 6], kind: u8) -> Vec<u8> {
    let mut packet = vec![0x04, 0x04, 0x0A];
    packet.extend_from_slice(&addr);
    packet.extend_from_slice(&[0x04, 0x04, 0x24]);
    packet.push(kind);
    packet
}

/// A Synchronous Connection Complete (event 0x2C, 17 parameter bytes).
fn synchronous_complete_event(
    status: u8,
    handle: u16,
    addr: [u8; 6],
    kind: u8,
    air_mode: u8,
) -> Vec<u8> {
    let mut packet = vec![0x04, 0x2C, 0x11, status];
    packet.extend_from_slice(&handle.to_le_bytes());
    packet.extend_from_slice(&addr);
    packet.push(kind);
    packet.push(0x00); // transmission interval
    packet.push(0x00); // retransmission window
    packet.extend_from_slice(&60u16.to_le_bytes()); // rx packet length
    packet.extend_from_slice(&60u16.to_le_bytes()); // tx packet length
    packet.push(air_mode);
    packet
}

/// A host with an ACL up, ready for audio.
fn linked_host(addr: [u8; 6]) -> ClassicHost {
    let mut host = host();
    host.handle_packet(&connection_request_event(addr)).unwrap();
    host.handle_packet(&connection_complete_event(0x0080, addr))
        .unwrap();
    host
}

#[test]
fn test_a_synchronous_connection_request_is_answered_with_the_synchronous_command() {
    // The trap this exists for: Connection Request announces an inbound
    // ACL *and* an inbound SCO with the same event code, and answering a
    // synchronous one with plain Accept Connection Request gets silence
    // — the controller has no page to match it against.
    let addr = [0x11; 6];
    let mut linked = linked_host(addr);

    let out = linked
        .handle_packet(&synchronous_request_event(addr, link_type::ESCO))
        .unwrap();
    assert_eq!(
        &out[0][1..3],
        &opcode::ACCEPT_SYNCHRONOUS_CONNECTION_REQUEST,
        "an eSCO request needs Accept *Synchronous* Connection Request"
    );
    assert_eq!(&out[0][4..10], &addr, "and it names the peer");

    // The ACL form still gets the ACL answer.
    let mut fresh = host();
    let out = fresh
        .handle_packet(&connection_request_event(addr))
        .unwrap();
    assert_eq!(&out[0][1..3], &opcode::ACCEPT_CONNECTION_REQUEST);
}

#[test]
fn test_a_refusing_host_rejects_with_the_synchronous_command_and_its_reason() {
    let addr = [0x11; 6];
    let mut host = linked_host(addr);
    host.set_sco_policy(ScoPolicy::Reject(0x0D));

    let out = host
        .handle_packet(&synchronous_request_event(addr, link_type::SCO))
        .unwrap();
    assert_eq!(
        &out[0][1..3],
        &opcode::REJECT_SYNCHRONOUS_CONNECTION_REQUEST
    );
    assert_eq!(*out[0].last().expect("a reason"), 0x0D);
    assert!(host.sco().is_none(), "refusing leaves no handle behind");
}

#[test]
fn test_setup_synchronous_connection_carries_the_codecs_parameters() {
    use crate::classic::hfp::AudioCodec;

    let mut host = linked_host([0x11; 6]);
    host.set_sco_parameters(
        AudioCodec::Msbc.voice_setting(),
        AudioCodec::Msbc.esco_packet_type(),
    );
    let out = host.setup_sco();
    let params = &out[0][4..];
    assert_eq!(
        u16::from_le_bytes([params[0], params[1]]),
        0x0080,
        "the ACL handle the audio hangs off"
    );
    // handle(2) tx_bw(4) rx_bw(4) max_latency(2) voice_setting(2)
    // retransmission_effort(1) packet_type(2)
    assert_eq!(
        u16::from_le_bytes([params[12], params[13]]),
        0x0063,
        "mSBC asks for transparent air coding, not CVSD"
    );
    assert_eq!(
        u16::from_le_bytes([params[15], params[16]]),
        0x0008,
        "and EV3, because wideband speech needs an extended link"
    );
    assert_eq!(params.len(), 17);
}

#[test]
fn test_audio_is_carried_on_the_synchronous_handle_and_not_the_acl_handle() {
    let addr = [0x11; 6];
    let mut host = linked_host(addr);
    host.handle_packet(&synchronous_complete_event(
        0x00,
        0x0081,
        addr,
        link_type::ESCO,
        0x03,
    ))
    .unwrap();
    let sco = host.sco().expect("the audio link is up");
    assert_eq!(sco.handle, 0x0081);
    assert_eq!(sco.link_type, link_type::ESCO);
    assert_eq!(sco.air_mode, 0x03);

    let out = host.send_sco(&[0xAA, 0xBB, 0xCC]);
    assert_eq!(out[0][0], crate::transport::h4_type::HCI_SCO_DATA);
    assert_eq!(u16::from_le_bytes([out[0][1], out[0][2]]) & 0x0FFF, 0x0081);
    assert_eq!(out[0][3], 3, "the length octet is the payload's");

    // Audio addressed to this host's own SCO handle is taken in.
    host.handle_packet(&[0x03, 0x81, 0x00, 0x03, 0xAA, 0xBB, 0xCC])
        .unwrap();
    // Audio on the *ACL* handle is not: it is a well-formed packet that
    // means something else.
    host.handle_packet(&[0x03, 0x80, 0x00, 0x03, 0x11, 0x22, 0x33])
        .unwrap();
    assert_eq!(host.take_sco_received(), vec![vec![0xAA, 0xBB, 0xCC]]);
}

#[test]
fn test_hanging_up_the_audio_keeps_the_acl_and_its_channels() {
    let addr = [0x11; 6];
    let mut host = linked_host(addr);
    host.handle_packet(&synchronous_complete_event(
        0x00,
        0x0081,
        addr,
        link_type::SCO,
        0x02,
    ))
    .unwrap();

    // Disconnection Complete on the *audio* handle.
    let out = host
        .handle_packet(&[0x04, 0x05, 0x04, 0x00, 0x81, 0x00, 0x13])
        .unwrap();
    assert!(host.sco().is_none(), "the audio is gone");
    assert!(
        host.connection().is_some(),
        "and the ACL, which carries the call's AT commands, is not"
    );
    assert!(
        out.is_empty(),
        "nothing is re-enabled: the device never became unreachable"
    );

    // Now the ACL. The audio has to go with it whether or not the
    // controller says so separately.
    let mut host = linked_host(addr);
    host.handle_packet(&synchronous_complete_event(
        0x00,
        0x0081,
        addr,
        link_type::SCO,
        0x02,
    ))
    .unwrap();
    host.handle_packet(&[0x04, 0x05, 0x04, 0x00, 0x80, 0x00, 0x13])
        .unwrap();
    assert!(host.sco().is_none());
    assert!(host.connection().is_none());
}

#[test]
fn test_a_failed_setup_is_recorded_rather_than_ignored() {
    // A host that only watches for success re-sends the setup forever
    // and reports nothing, which looks exactly like a slow link.
    let addr = [0x11; 6];
    let mut host = linked_host(addr);
    host.handle_packet(&synchronous_complete_event(
        0x0D,
        0x0000,
        addr,
        link_type::SCO,
        0x02,
    ))
    .unwrap();
    assert!(host.sco().is_none());
    assert_eq!(host.sco_failure(), Some(0x0D));
    assert!(
        host.send_sco(&[0x01]).is_empty(),
        "and there is nowhere to put audio"
    );
}

// ---------------------------------------------------------------------
// RFCOMM / SPP
// ---------------------------------------------------------------------

use crate::classic::rfcomm::{Multiplexer, MultiplexerEvent, RFCOMM_PSM, Role};

/// Shuttles RFCOMM frames between a peer multiplexer and our handler
/// until neither has anything more to say, returning the events the peer
/// saw. Both sides are synchronous, so a bounded loop settles.
fn shuttle(
    peer: &mut Multiplexer,
    handler: &mut RfcommHandler,
    start: Vec<Vec<u8>>,
) -> Vec<MultiplexerEvent> {
    let mut to_handler = start;
    let mut peer_events = Vec::new();
    for _ in 0..8 {
        if to_handler.is_empty() {
            break;
        }
        let mut to_peer = Vec::new();
        for frame in to_handler.drain(..) {
            to_peer.extend(handler.on_data(&frame, 672));
        }
        for frame in to_peer {
            if let Ok((out, events)) = peer.receive(&frame) {
                to_handler.extend(out);
                peer_events.extend(events);
            }
        }
    }
    peer_events
}

/// Opens a multiplexer session and a DLC on `channel`, the way a peer
/// does: SABM(0), then PN, then SABM on the data DLCI.
fn open_session(channel: u8) -> (Multiplexer, RfcommHandler, SharedRfcommPort) {
    let (mut handler, port) = RfcommHandler::echoing(channel);
    let mut peer = Multiplexer::new(Role::Initiator, 672);
    let sabm = peer.start().expect("initiator starts the multiplexer");
    shuttle(&mut peer, &mut handler, vec![sabm]);
    let pn = peer.open_dlc(channel, 127, 7).expect("PN for the channel");
    shuttle(&mut peer, &mut handler, vec![pn]);
    (peer, handler, port)
}

#[test]
fn test_rfcomm_handler_serves_the_rfcomm_psm() {
    let (handler, _port) = RfcommHandler::echoing(3);
    assert_eq!(handler.psm(), RFCOMM_PSM, "SPP rides RFCOMM on PSM 3");
}

#[test]
fn test_rfcomm_session_and_dlc_open() {
    let (peer, _handler, port) = open_session(3);
    assert!(peer.is_connected(), "the multiplexer session must come up");
    assert!(
        port.lock().unwrap().is_open(),
        "the DLC the SDP record advertises must open"
    );
}

#[test]
fn test_rfcomm_dlc_open_is_refused_on_an_unadvertised_channel() {
    // A peer that opens a channel we never listened on must get DM, not
    // a half-open DLC — otherwise the SDP record and reality diverge.
    let (mut peer, mut handler, port) = open_session(3);
    let pn = peer.open_dlc(9, 127, 7).expect("PN for a stray channel");
    let events = shuttle(&mut peer, &mut handler, vec![pn]);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, MultiplexerEvent::DlcOpenRejected(_))),
        "an unlistened channel must be rejected: {events:?}"
    );
    // The channel we do serve is unaffected.
    assert!(port.lock().unwrap().is_open());
}

#[test]
fn test_rfcomm_carries_data_from_the_peer_to_the_device() {
    let (mut peer, mut handler, port) = open_session(3);
    let dlci = port.lock().unwrap().open_dlci.expect("a DLC is open");

    let frames = peer.write(dlci, b"AT+HELLO\r").expect("peer writes");
    shuttle(&mut peer, &mut handler, frames);

    let received = port.lock().unwrap().take_received();
    assert_eq!(
        received,
        vec![b"AT+HELLO\r".to_vec()],
        "the device must see exactly what the peer sent"
    );
    assert_eq!(port.lock().unwrap().received_count(), 1);
    assert!(
        port.lock().unwrap().take_received().is_empty(),
        "draining is destructive"
    );
}

#[test]
fn test_rfcomm_carries_data_from_the_device_to_the_peer() {
    let (mut peer, mut handler, port) = open_session(3);

    // The device speaks first: nothing inbound prompted this.
    port.lock().unwrap().write(b"READY\r\n".to_vec());
    let frames = handler.poll_output(672);
    assert!(!frames.is_empty(), "queued data must leave on a poll");

    let events = shuttle(&mut peer, &mut handler, frames);
    let delivered: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            MultiplexerEvent::DataReceived(_, data) => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        delivered,
        vec![b"READY\r\n".to_vec()],
        "the peer must receive what the device wrote"
    );
}

#[test]
fn test_rfcomm_echo_answers_the_peer() {
    // The default port behaviour, and what a terminal app on a phone
    // sees: type a line, get it back.
    let (mut peer, mut handler, port) = open_session(3);
    let dlci = port.lock().unwrap().open_dlci.unwrap();

    let frames = peer.write(dlci, b"ping").expect("peer writes");
    let events = shuttle(&mut peer, &mut handler, frames);

    let echoed: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            MultiplexerEvent::DataReceived(_, data) => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        echoed,
        vec![b"ping".to_vec()],
        "the port echoes: {events:?}"
    );
}

#[test]
fn test_rfcomm_data_queued_before_the_dlc_opens_is_not_lost() {
    // A device may write before a peer has opened the port; that data
    // must wait rather than being dropped on the floor.
    let (mut handler, port) = RfcommHandler::echoing(3);
    port.lock().unwrap().write(b"early".to_vec());
    assert!(
        handler.poll_output(672).is_empty(),
        "nothing can be sent with no DLC open"
    );

    let mut peer = Multiplexer::new(Role::Initiator, 672);
    let sabm = peer.start().unwrap();
    shuttle(&mut peer, &mut handler, vec![sabm]);
    let pn = peer.open_dlc(3, 127, 7).unwrap();
    let events = shuttle(&mut peer, &mut handler, vec![pn]);

    // Opening the DLC flushes what was waiting.
    let delivered: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            MultiplexerEvent::DataReceived(_, data) => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(delivered, vec![b"early".to_vec()], "queued data survives");
}

/// The whole path an Android terminal app takes: page the device, open
/// the RFCOMM L2CAP channel, run the multiplexer handshake, exchange
/// data, and disconnect — driven through [`ClassicHost`] as H4 packets
/// rather than by calling the handler directly.
#[test]
fn test_spp_end_to_end_through_the_host() {
    let mut host = ClassicHost::new("SimbleClassic", [0x04, 0x04, 0x24]);
    host.register_handler(Box::new(SdpHandler::default()))
        .unwrap();
    let (rfcomm, port) = RfcommHandler::echoing(3);
    host.register_handler(Box::new(rfcomm)).unwrap();

    let addr = [0x22; 6];
    let handle = 0x0081;
    let peer_cid = 0x0041u16;
    host.handle_packet(&connection_request_event(addr)).unwrap();
    host.handle_packet(&connection_complete_event(handle, addr))
        .unwrap();

    // L2CAP: connect to the RFCOMM PSM, then configure in both directions.
    let request = ConnectionRequestHeader {
        psm: RFCOMM_PSM.into(),
        source_cid: peer_cid.into(),
    };
    let out = host
        .handle_packet(&acl_packet(
            handle,
            &signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes()),
        ))
        .unwrap();
    let (response, _) = ConnectionResponseHeader::ref_from_prefix(&out[0][13..]).unwrap();
    assert_eq!(response.result.get(), 0x0000, "RFCOMM PSM must be accepted");
    let local_cid = response.destination_cid.get();

    let mut config = ConfigurationRequestHeader {
        destination_cid: local_cid.into(),
        flags: 0u16.into(),
    }
    .as_bytes()
    .to_vec();
    config.extend_from_slice(&[0x01, 0x02, 0xA0, 0x02]); // MTU 672
    host.handle_packet(&acl_packet(
        handle,
        &signaling_pdu(signaling_code::CONFIGURATION_REQUEST, 2, &config),
    ))
    .unwrap();
    let ack = ConfigurationResponseHeader {
        source_cid: local_cid.into(),
        flags: 0u16.into(),
        result: 0u16.into(),
    };
    host.handle_packet(&acl_packet(
        handle,
        &signaling_pdu(signaling_code::CONFIGURATION_RESPONSE, 1, ack.as_bytes()),
    ))
    .unwrap();
    assert!(host.has_open_channel(), "the RFCOMM channel must be open");

    // RFCOMM over that channel: peer frames go in as ACL packets and the
    // host's replies come back the same way. Strip H4 + ACL + L2CAP
    // headers (1 + 4 + 4) to recover each SDU.
    let mut peer = Multiplexer::new(Role::Initiator, 672);
    let feed = |host: &mut ClassicHost, peer: &mut Multiplexer, sdus: Vec<Vec<u8>>| {
        let mut events = Vec::new();
        let mut next = Vec::new();
        for sdu in sdus {
            // An inbound frame is addressed to *our* CID; the host looks
            // the channel up by it.
            let out = host
                .handle_packet(&acl_packet(
                    handle,
                    &L2capHeader::serialize(local_cid, &sdu),
                ))
                .unwrap();
            for packet in out {
                if let Ok((frames, evts)) = peer.receive(&packet[9..]) {
                    next.extend(frames);
                    events.extend(evts);
                }
            }
        }
        (next, events)
    };

    // Multiplexer up.
    let start = peer.start().unwrap();
    let (mut pending, _) = feed(&mut host, &mut peer, vec![start]);
    assert!(peer.is_connected(), "multiplexer up through the real host");
    assert!(pending.is_empty(), "UA(0) needs no further reply");

    // DLC open on the advertised channel; the handshake settles in a few
    // exchanges (PN response, SABM, UA + MSC).
    pending.push(peer.open_dlc(3, 127, 7).unwrap());
    for _ in 0..6 {
        if pending.is_empty() {
            break;
        }
        let (next, _) = feed(&mut host, &mut peer, std::mem::take(&mut pending));
        pending = next;
    }
    let dlci = port
        .lock()
        .unwrap()
        .open_dlci
        .expect("the DLC must open through the host");

    // Data from the peer reaches the device, and the echo comes back.
    let frames = peer.write(dlci, b"hello serial").unwrap();
    let (_, events) = feed(&mut host, &mut peer, frames);
    assert_eq!(
        port.lock().unwrap().take_received(),
        vec![b"hello serial".to_vec()],
        "the device sees the peer's bytes through the full stack"
    );
    let echoed: Vec<Vec<u8>> = events
        .iter()
        .filter_map(|e| match e {
            MultiplexerEvent::DataReceived(_, data) => Some(data.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        echoed,
        vec![b"hello serial".to_vec()],
        "and the echo returns over the same path"
    );

    // Tearing down the ACL link closes everything.
    host.handle_packet(&[0x04, 0x05, 0x04, 0x00, 0x81, 0x00, 0x13])
        .unwrap();
    assert!(host.connection().is_none());
}

#[test]
fn test_rfcomm_dlc_close_marks_the_port_closed() {
    let (mut peer, mut handler, port) = open_session(3);
    let dlci = port.lock().unwrap().open_dlci.unwrap();

    let disc = peer.close_dlc(dlci).expect("peer closes the DLC");
    shuttle(&mut peer, &mut handler, vec![disc]);

    assert!(
        !port.lock().unwrap().is_open(),
        "the port must close when the peer disconnects the DLC"
    );
}

/// A multiplexer session belongs to the L2CAP channel carrying it
/// (RFCOMM spec §5.1). Keeping one past the channel's death makes the
/// device look dead to the *next* peer: the stale session drops the new
/// SABM on DLCI 0 and nothing is ever answered. Caught live — a second
/// Bumble client could not open a session until the handler was reset.
#[test]
fn test_rfcomm_session_is_reset_when_the_channel_closes() {
    let (mut first, mut handler, port) = open_session(3);
    assert!(first.is_connected(), "the first peer's session comes up");
    let disconnect = first.disconnect().expect("first peer tears down");
    shuttle(&mut first, &mut handler, vec![disconnect]);

    // What the host does when the channel or the ACL goes away.
    handler.on_channel_closed();
    assert!(
        !port.lock().unwrap().is_open(),
        "no DLC survives the channel that carried it"
    );

    // A second peer, arriving on a fresh channel, must be answered.
    let mut second = Multiplexer::new(Role::Initiator, 672);
    let sabm = second.start().expect("second peer starts a session");
    shuttle(&mut second, &mut handler, vec![sabm]);
    assert!(
        second.is_connected(),
        "a new peer must get a session after the previous one left"
    );

    let pn = second.open_dlc(3, 127, 7).expect("PN for the channel");
    shuttle(&mut second, &mut handler, vec![pn]);
    assert!(
        port.lock().unwrap().is_open(),
        "and must be able to open the advertised DLC again"
    );
}

/// The mirror of the reset: a device that queued bytes with nobody
/// connected still has them when someone does connect. The port outlives
/// the session because it belongs to the device, not to the peer.
#[test]
fn test_a_write_survives_the_session_that_was_not_there_to_carry_it() {
    let (mut peer, mut handler, port) = open_session(3);
    let disconnect = peer.disconnect().expect("peer tears down");
    shuttle(&mut peer, &mut handler, vec![disconnect]);
    handler.on_channel_closed();

    port.lock().unwrap().write(b"queued while alone".to_vec());
    assert!(
        handler.poll_output(672).is_empty(),
        "with no DLC open there is nowhere to send it, so it waits"
    );

    let mut next = Multiplexer::new(Role::Initiator, 672);
    let sabm = next.start().expect("a new peer arrives");
    shuttle(&mut next, &mut handler, vec![sabm]);
    let pn = next.open_dlc(3, 127, 7).expect("PN for the channel");
    let events = shuttle(&mut next, &mut handler, vec![pn]);

    let delivered: Vec<Vec<u8>> = events
        .into_iter()
        .filter_map(|event| match event {
            MultiplexerEvent::DataReceived(_, data) => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(
        delivered,
        vec![b"queued while alone".to_vec()],
        "the queued write reaches the peer that eventually connects"
    );
}

// -- multi-channel dispatch ------------------------------------------
//
// Everything above tests one handler on one PSM with one channel. These
// test the shape the two-PSM and two-channel profiles need, without
// either profile in the way.

/// A handler on two PSMs that records every callback it receives, so a
/// test can assert on *which channel* the host said something happened
/// on rather than only that it happened.
#[derive(Debug, Default)]
struct TwoPsmHandler {
    opened: Vec<(u16, u16)>,
    lost: Vec<u16>,
    closed: usize,
    seen: Vec<(u16, Vec<u8>)>,
}

const PSM_A: u16 = 0x1001;
const PSM_B: u16 = 0x1003;

impl ProtocolHandler for TwoPsmHandler {
    fn psm(&self) -> u16 {
        PSM_A
    }

    fn psms(&self) -> Vec<u16> {
        vec![PSM_A, PSM_B]
    }

    fn on_data(&mut self, _data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        unreachable!("a multi-PSM handler is always routed by channel")
    }

    fn on_channel_open(&mut self, channel: HandlerChannel) {
        self.opened.push((channel.psm, channel.cid));
    }

    fn on_channel_lost(&mut self, cid: u16) {
        self.lost.push(cid);
    }

    fn on_channel_closed(&mut self) {
        self.closed += 1;
    }

    fn on_channel_data(&mut self, channel: HandlerChannel, data: &[u8]) -> Vec<Vec<u8>> {
        self.seen.push((channel.psm, data.to_vec()));
        vec![vec![channel.psm as u8]]
    }
}

/// Brings up an ACL and opens `psm` as a server, returning the local CID
/// of the resulting channel once it is fully configured.
fn open_server_channel(host: &mut ClassicHost, handle: u16, psm: u16, peer_cid: u16) -> u16 {
    let request = ConnectionRequestHeader {
        psm: psm.into(),
        source_cid: peer_cid.into(),
    };
    let out = host
        .handle_packet(&acl_packet(
            handle,
            &signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes()),
        ))
        .unwrap();
    let (response, _) = ConnectionResponseHeader::ref_from_prefix(&out[0][13..]).unwrap();
    assert_eq!(response.result.get(), 0x0000, "PSM {psm:#06x} was refused");
    let local_cid = response.destination_cid.get();

    let mut config = ConfigurationRequestHeader {
        destination_cid: local_cid.into(),
        flags: 0u16.into(),
    }
    .as_bytes()
    .to_vec();
    config.extend_from_slice(&[0x01, 0x02, 0xA0, 0x02]); // MTU 672
    host.handle_packet(&acl_packet(
        handle,
        &signaling_pdu(signaling_code::CONFIGURATION_REQUEST, 2, &config),
    ))
    .unwrap();
    let ack = ConfigurationResponseHeader {
        source_cid: local_cid.into(),
        flags: 0u16.into(),
        result: 0u16.into(),
    };
    host.handle_packet(&acl_packet(
        handle,
        &signaling_pdu(signaling_code::CONFIGURATION_RESPONSE, 1, ack.as_bytes()),
    ))
    .unwrap();
    local_cid
}

fn connected_host_with(handler: Box<dyn ProtocolHandler>, handle: u16) -> ClassicHost {
    let mut host = ClassicHost::new("SimbleClassic", [0x04, 0x04, 0x24]);
    host.register_handler(handler).unwrap();
    let addr = [0x33; 6];
    host.handle_packet(&connection_request_event(addr)).unwrap();
    host.handle_packet(&connection_complete_event(handle, addr))
        .unwrap();
    host
}

#[test]
fn test_one_handler_serves_both_of_its_psms() {
    let handle = 0x0082;
    let mut host = connected_host_with(Box::new(TwoPsmHandler::default()), handle);

    let cid_a = open_server_channel(&mut host, handle, PSM_A, 0x0050);
    let cid_b = open_server_channel(&mut host, handle, PSM_B, 0x0051);
    assert_ne!(cid_a, cid_b, "two channels, two CIDs");

    // Both channels reached the same handler, each announced with its
    // own PSM. One handler, two PSMs, and the CIDs are distinguishable.
    let handler = host.handler::<TwoPsmHandler>().unwrap();
    assert_eq!(handler.opened, vec![(PSM_A, cid_a), (PSM_B, cid_b)]);

    // Data on each channel arrives labelled with the channel it came in
    // on, and the reply goes back on that same channel.
    host.handle_channel_data(handle, cid_a, b"from a");
    host.handle_channel_data(handle, cid_b, b"from b");
    let handler = host.handler::<TwoPsmHandler>().unwrap();
    assert_eq!(
        handler.seen,
        vec![(PSM_A, b"from a".to_vec()), (PSM_B, b"from b".to_vec())]
    );
}

#[test]
fn test_losing_one_channel_does_not_end_a_two_channel_session() {
    let handle = 0x0083;
    let mut host = connected_host_with(Box::new(TwoPsmHandler::default()), handle);
    let cid_a = open_server_channel(&mut host, handle, PSM_A, 0x0050);
    let cid_b = open_server_channel(&mut host, handle, PSM_B, 0x0051);

    // Peer disconnects the second channel only.
    let mut params = cid_b.to_le_bytes().to_vec();
    params.extend_from_slice(&0x0051u16.to_le_bytes());
    host.handle_packet(&acl_packet(
        handle,
        &signaling_pdu(signaling_code::DISCONNECTION_REQUEST, 3, &params),
    ))
    .unwrap();

    let handler = host.handler::<TwoPsmHandler>().unwrap();
    assert_eq!(handler.lost, vec![cid_b], "the right channel was named");
    assert_eq!(
        handler.closed, 0,
        "a session with a channel still open is not over — discarding it \
         here is what would make an AVDTP media channel closing kill the \
         signalling session"
    );

    // Now the first one goes too: that *is* the end of the session.
    let mut params = cid_a.to_le_bytes().to_vec();
    params.extend_from_slice(&0x0050u16.to_le_bytes());
    host.handle_packet(&acl_packet(
        handle,
        &signaling_pdu(signaling_code::DISCONNECTION_REQUEST, 4, &params),
    ))
    .unwrap();
    let handler = host.handler::<TwoPsmHandler>().unwrap();
    assert_eq!(handler.lost, vec![cid_b, cid_a]);
    assert_eq!(handler.closed, 1, "the last channel ends the session");
}

/// A handler that asks the host to open a channel for it, the way an
/// AVDTP media transport is opened.
#[derive(Debug, Default)]
struct ChannelRequestingHandler {
    wanted: Vec<u16>,
    opened: Vec<(u16, u16)>,
}

impl ProtocolHandler for ChannelRequestingHandler {
    fn psm(&self) -> u16 {
        PSM_A
    }

    fn on_data(&mut self, _data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        Vec::new()
    }

    fn poll_channel_requests(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.wanted)
    }

    fn on_channel_open(&mut self, channel: HandlerChannel) {
        self.opened.push((channel.psm, channel.cid));
    }
}

#[test]
fn test_a_handler_can_ask_the_host_to_open_a_channel() {
    let handle = 0x0084;
    let mut host = connected_host_with(
        Box::new(ChannelRequestingHandler {
            wanted: vec![PSM_A],
            opened: Vec::new(),
        }),
        handle,
    );

    // Bringing the ACL up already drained the request: `handle_packet`
    // polls at the end of every packet, so a channel asked for before
    // there was a link opens the moment there is one. Re-arm it and
    // poll explicitly to look at the packet.
    assert_eq!(
        host.handler::<ChannelRequestingHandler>().unwrap().opened,
        Vec::new(),
        "the channel is not open until the peer has answered"
    );
    host.handler_mut::<ChannelRequestingHandler>()
        .unwrap()
        .wanted = vec![PSM_A];

    // The request turns into a real L2CAP Connection Request on the
    // wire — a handler cannot send one itself, because the channel
    // manager belongs to the host.
    let out = host.poll();
    assert_eq!(out.len(), 1, "the requested channel was not opened");
    let signalling = &out[0][9..];
    assert_eq!(
        signalling[0],
        signaling_code::CONNECTION_REQUEST,
        "the host sent something other than a Connection Request"
    );
    let (request, _) = ConnectionRequestHeader::ref_from_prefix(&signalling[4..]).unwrap();
    assert_eq!(request.psm.get(), PSM_A);

    // Asking once opens once: a request drained twice would leave a
    // device opening a new channel on every tick for ever.
    assert!(host.poll().is_empty());
}

#[test]
fn test_a_single_psm_handler_still_sees_the_old_callbacks() {
    // The compatibility claim, made explicitly rather than inferred from
    // the seventeen tests above that would break if it were false: a
    // handler that implements only `psm`/`on_data` is routed, replied
    // to, and told when its channel goes — with no new method on it.
    let handle = 0x0085;
    let mut host = connected_host_with(Box::new(SdpHandler::default()), handle);
    let cid = open_server_channel(&mut host, handle, SDP_PSM, 0x0052);

    let out = host.handle_channel_data(handle, cid, &[0xFF, 0x00, 0x00]);
    assert_eq!(out.len(), 1, "the SDP server did not answer");
    assert_eq!(
        out[0][9], 0x01,
        "an SDP error response is the honest answer to a malformed PDU"
    );
}

/// An A2DP Audio Sink record publishes an L2CAP PSM and no RFCOMM channel
/// at all. The SDP client used to read a ProtocolDescriptorList
/// *positionally* — "the second element of the second layer is the RFCOMM
/// server channel" — which is true only for a record that stacks RFCOMM
/// over L2CAP. A sink stacks AVDTP: `[[L2CAP, 0x0019], [AVDTP, 0x0103]]`.
/// So the old reader reported the AVDTP *version's* low byte as RFCOMM
/// server channel 3, and never read the PSM the record exists to publish.
///
/// Both halves are asserted here, because both bit: a phone looking for a
/// speaker got a plausible wrong number, and a phone looking for the AVDTP
/// PSM got nothing.
#[test]
fn test_an_a2dp_audio_sink_record_yields_its_psm_and_no_phantom_rfcomm_channel() {
    use crate::classic::a2dp::make_audio_sink_service_sdp_records;
    use crate::classic::avdtp::AVDTP_PSM;
    use crate::classic::sdp::SdpPdu;

    const AUDIO_SINK: SdpUuid = SdpUuid::Uuid16(0x110B);

    let (mut query, results) = SdpQueryHandler::searching(AUDIO_SINK);
    assert_eq!(query.poll_output(672).len(), 1, "the query goes out once");

    // The record a real speaker publishes, built by the same code that
    // serves one — so a change to the record shape breaks this test rather
    // than silently changing what a client can read.
    let attributes = make_audio_sink_service_sdp_records(0x0001_000B, None);
    let flattened: Vec<DataElement> = attributes
        .iter()
        .flat_map(|attribute| {
            [
                DataElement::unsigned_integer_16(attribute.id),
                attribute.value.clone(),
            ]
        })
        .collect();
    let response = SdpPdu::ServiceSearchAttributeResponse {
        transaction_id: 1,
        attribute_lists: DataElement::sequence(vec![DataElement::sequence(flattened)]).to_bytes(),
        continuation_state: vec![0x00],
    }
    .to_bytes();
    assert!(
        query.on_data(&response, 672).is_empty(),
        "a final response needs no follow-up request"
    );

    let results = results.lock().expect("results readable");
    assert!(results.answered);
    assert_eq!(
        results.psm_for(AUDIO_SINK),
        Some(AVDTP_PSM),
        "the Audio Sink record's L2CAP PSM is what a source must open",
    );
    assert!(
        results.rfcomm_channels.is_empty(),
        "a record with no RFCOMM layer must contribute no RFCOMM channel; it \
         offered {:?}",
        results.rfcomm_channels,
    );
    assert_eq!(
        results.channel_for(AUDIO_SINK),
        None,
        "and asking for an RFCOMM channel by that service class must say no",
    );
}

/// The counterpart: an RFCOMM record still reports its channel, and now
/// also reports the PSM RFCOMM itself runs on. Identifying layers by UUID
/// must not have cost the case the positional reader got right.
#[test]
fn test_an_rfcomm_record_still_yields_its_server_channel() {
    use crate::classic::sdp::SdpPdu;

    const SERIAL_PORT: SdpUuid = SdpUuid::Uuid16(0x1101);
    const RFCOMM_PSM: u16 = 0x0003;

    let (mut query, results) = SdpQueryHandler::searching(SERIAL_PORT);
    let _ = query.poll_output(672);

    let attributes = [
        ServiceAttribute::new(
            attribute_id::SERVICE_CLASS_ID_LIST,
            DataElement::sequence(vec![DataElement::uuid(SERIAL_PORT)]),
        ),
        ServiceAttribute::new(
            attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID),
                    DataElement::unsigned_integer_16(RFCOMM_PSM),
                ]),
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::Uuid16(0x0003)),
                    DataElement::unsigned_integer_8(7),
                ]),
            ]),
        ),
    ];
    let flattened: Vec<DataElement> = attributes
        .iter()
        .flat_map(|attribute| {
            [
                DataElement::unsigned_integer_16(attribute.id),
                attribute.value.clone(),
            ]
        })
        .collect();
    let response = SdpPdu::ServiceSearchAttributeResponse {
        transaction_id: 1,
        attribute_lists: DataElement::sequence(vec![DataElement::sequence(flattened)]).to_bytes(),
        continuation_state: vec![0x00],
    }
    .to_bytes();
    query.on_data(&response, 672);

    let results = results.lock().expect("results readable");
    assert_eq!(
        results.channel_for(SERIAL_PORT),
        Some(7),
        "the RFCOMM layer's parameter is still the server channel",
    );
    assert_eq!(
        results.psm_for(SERIAL_PORT),
        Some(RFCOMM_PSM),
        "and the L2CAP layer underneath it is now read too",
    );
}

/// A refused page is reported, not silently dropped.
///
/// The Connection Complete arm used to match only `status == 0x00`, so a
/// failure fell through to the catch-all and vanished. The host then waited
/// on a connection that was never coming, and the only evidence was that
/// nothing happened. These are the real bytes a CSR8510 sent when it paged a
/// dongle that was still page-scanning under a host that had died: status
/// `0x10`, Connection Accept Timeout.
#[test]
fn a_refused_page_is_reported_rather_than_dropped() {
    let mut host = host();
    let peer = Address::from_be_bytes([0x00, 0x16, 0xA4, 0x6F, 0xA5, 0x19]);
    host.create_connection(peer);
    assert_eq!(
        host.connection_failure(),
        None,
        "a fresh page has no verdict"
    );

    // 04 | 03 0B | status 0x10 | handle | bd_addr (LE) | link type | encryption
    let refused = [
        0x04, 0x03, 0x0B, 0x10, 0x48, 0x00, 0x19, 0xA5, 0x6F, 0xA4, 0x16, 0x00, 0x01, 0x00,
    ];
    let out = host.handle_packet(&refused).expect("the event parses");

    assert!(out.is_empty(), "a refused page owes the controller nothing");
    assert_eq!(
        host.connection_failure(),
        Some(0x10),
        "the status must survive: 0x10 is Connection Accept Timeout, and \
         without it the symptom is twenty seconds of silence"
    );
    assert_eq!(host.connection(), None, "and no link was established");
}

use crate::l2cap::AclPacketBoundary;

/// One HCI ACL fragment with the boundary flag spelled out. The sibling
/// `acl_packet` always says "first", which is the whole story only while
/// nothing is ever split.
fn acl_fragment(handle: u16, boundary: AclPacketBoundary, payload: &[u8]) -> Vec<u8> {
    use crate::l2cap::HciAclHeader;
    let header = HciAclHeader::new(handle, boundary, payload.len() as u16);
    let mut packet = Vec::with_capacity(5 + payload.len());
    packet.push(crate::transport::h4_type::HCI_ACL_DATA);
    packet.extend_from_slice(header.as_bytes());
    packet.extend_from_slice(payload);
    packet
}

/// An L2CAP frame larger than the controller's ACL data packet length
/// arrives as several HCI ACL packets, and only the first carries the L2CAP
/// header. Reading each one as a fresh frame turns a continuation
/// fragment's payload bytes into a length and a CID.
///
/// This is the exact shape of a real failure: a Pixel 9 Pro streaming A2DP
/// into a CSR8510 fragmented its first media packet, and the two SBC bytes
/// that happened to land at the front of the continuation were read as
/// `cid=0xdbb6`. Not a dropped packet — a *fabricated* one, routed to a
/// channel that does not exist, while the audio never reached the sink.
///
/// Both simulated controllers carry a 672-byte SDU whole, so nothing in this
/// tree exercised the fragmented path until hardware did.
#[test]
fn test_an_l2cap_frame_split_across_acl_packets_is_reassembled_before_routing() {
    let handle = 0x0048;
    let mut host = host();
    let addr = [0x11; 6];
    host.handle_packet(&connection_request_event(addr)).unwrap();
    host.handle_packet(&connection_complete_event(handle, addr))
        .unwrap();
    let local_cid = open_server_channel(&mut host, handle, SDP_PSM, 0x0040);

    // A malformed SDP request, which the server answers with an error — the
    // same one the unfragmented test uses. What is under test is whether it
    // arrives at all, not what it says.
    let sdu = [0xFFu8, 0x00, 0x00];
    let mut frame = Vec::new();
    frame.extend_from_slice(&(sdu.len() as u16).to_le_bytes());
    frame.extend_from_slice(&local_cid.to_le_bytes());
    frame.extend_from_slice(&sdu);

    // Split inside the SDU: the first packet carries the L2CAP header and
    // one payload byte, the second the remaining two. Those two bytes are
    // what the old code read as a length and a CID.
    let first = acl_fragment(handle, AclPacketBoundary::FirstAutoFlushable, &frame[..5]);
    let rest = acl_fragment(handle, AclPacketBoundary::Continuing, &frame[5..]);

    let out = host.handle_packet(&first).expect("first fragment accepted");
    assert!(
        out.is_empty(),
        "half a frame is not a frame — it must produce no reply; it produced {out:?}",
    );
    let out = host
        .handle_packet(&rest)
        .expect("the continuation completes the frame");
    assert_eq!(
        out.len(),
        1,
        "the reassembled SDP request must be answered — before the fix the \
         continuation was parsed as a fresh frame on an invented CID and the \
         request was lost",
    );
    assert_eq!(
        out[0][9], 0x01,
        "and the answer is the SDP server's, i.e. the frame reached it whole",
    );
    let reply_cid = u16::from_le_bytes([out[0][7], out[0][8]]);
    assert_eq!(
        reply_cid, 0x0040,
        "the reply belongs to the channel the *first* fragment named",
    );
}

/// A continuation fragment with no first fragment is a protocol error, not
/// something to guess at. Saying so beats silently accumulating bytes into
/// whichever frame happens to come next.
#[test]
fn test_a_stray_continuation_fragment_is_refused_rather_than_guessed_at() {
    let handle = 0x0048;
    let mut host = host();
    let addr = [0x11; 6];
    host.handle_packet(&connection_request_event(addr)).unwrap();
    host.handle_packet(&connection_complete_event(handle, addr))
        .unwrap();

    let stray = acl_fragment(
        handle,
        AclPacketBoundary::Continuing,
        &[0x77, 0x6D, 0xB6, 0xDD],
    );
    assert!(
        host.handle_packet(&stray).is_err(),
        "a continuation with nothing to continue must be reported",
    );
}
