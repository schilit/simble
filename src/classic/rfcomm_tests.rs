use super::*;

#[test]
fn test_fcs_known_vector() {
    // ETSI TS 07.10 known-good vector: address=0x03, control=0x3f,
    // length=0x01 (1-byte, EA set, value 0), fcs=0x1c.
    assert_eq!(compute_fcs(&[0x03, 0x3f, 0x01]), 0x1c);
}

#[test]
fn test_frame_round_trip_known_vector() {
    let data = [0x03, 0x3f, 0x01, 0x1c];
    let frame = RfcommFrame::parse(&data).expect("valid frame");
    assert_eq!(frame.frame_type, frame_type::SABM);
    assert_eq!(frame.c_r, 1);
    assert_eq!(frame.dlci, 0);
    assert_eq!(frame.p_f, 1);
    assert!(frame.information.is_empty());
    assert_eq!(frame.to_bytes(), data);
}

#[test]
fn test_frame_round_trip_with_payload() {
    let frame = RfcommFrame::uih(1, 5, b"hello".to_vec(), 0);
    let bytes = frame.to_bytes();
    let parsed = RfcommFrame::parse(&bytes).expect("valid frame");
    assert_eq!(parsed, frame);
}

#[test]
fn test_frame_corrupt_fcs_rejected() {
    let frame = RfcommFrame::uih(1, 5, b"hello".to_vec(), 0);
    let mut bytes = frame.to_bytes();
    *bytes.last_mut().unwrap() ^= 0xFF;
    assert!(RfcommFrame::parse(&bytes).is_none());
}

#[test]
fn test_frame_truncated_rejected() {
    assert!(RfcommFrame::parse(&[0x03]).is_none());
    assert!(RfcommFrame::parse(&[]).is_none());
}

#[test]
fn test_mcc_pn_round_trip() {
    let pn = MccPn {
        dlci: 4,
        cl: 0xF0,
        priority: 7,
        ack_timer: 0,
        max_frame_size: 1000,
        max_retransmissions: 0,
        initial_credits: 7,
    };
    let mcc = make_mcc(mcc_type::PN, true, &pn.to_bytes());
    let (parsed_type, c_r, value) = parse_mcc(&mcc).expect("valid mcc");
    assert_eq!(parsed_type, mcc_type::PN);
    assert!(c_r);
    assert_eq!(MccPn::parse(value), Some(pn));
}

/// Drives both sides of a `Multiplexer` pair to a fully open DLC by
/// alternately feeding each side's output into the other.
fn drive_open_dlc(initiator: &mut Multiplexer, responder: &mut Multiplexer, channel: u8) -> u8 {
    let sabm = initiator.start().unwrap();
    let mut to_responder = vec![sabm];
    let mut to_initiator: Vec<Vec<u8>> = Vec::new();
    let mut opened_dlci = None;
    let mut requested_open = false;

    for _ in 0..10 {
        let mut next_to_initiator = Vec::new();
        for frame in to_responder.drain(..) {
            let (out, events) = responder.receive(&frame).unwrap();
            next_to_initiator.extend(out);
            for event in events {
                if let MultiplexerEvent::DlcOpened(dlci) = event {
                    opened_dlci = Some(dlci);
                }
            }
        }
        let mut next_to_responder = Vec::new();
        for frame in to_initiator.drain(..) {
            let (out, events) = initiator.receive(&frame).unwrap();
            next_to_responder.extend(out);
            for event in events {
                if let MultiplexerEvent::DlcOpened(dlci) = event {
                    opened_dlci = Some(dlci);
                }
            }
        }
        to_initiator = next_to_initiator;
        to_responder = next_to_responder;

        if !requested_open && initiator.is_connected() && responder.is_connected() {
            requested_open = true;
            to_responder.push(
                initiator
                    .open_dlc(channel, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS)
                    .unwrap(),
            );
        }

        if opened_dlci.is_some() && to_initiator.is_empty() && to_responder.is_empty() {
            break;
        }
    }
    opened_dlci.expect("DLC did not open")
}

#[test]
fn test_multiplexer_startup_handshake() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);

    let sabm = initiator.start().unwrap();
    assert_eq!(initiator.state, MultiplexerState::Connecting);

    let (to_initiator, events) = responder.receive(&sabm).unwrap();
    assert_eq!(responder.state, MultiplexerState::Connected);
    assert_eq!(events, vec![MultiplexerEvent::Started]);

    let (out, events) = initiator.receive(&to_initiator[0]).unwrap();
    assert!(out.is_empty());
    assert_eq!(events, vec![MultiplexerEvent::Started]);
    assert_eq!(initiator.state, MultiplexerState::Connected);
}

#[test]
fn test_open_dlc_and_send_receive_with_credits() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

    let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);
    assert!(initiator.dlcs.get(&dlci).unwrap().is_open());
    assert!(responder.dlcs.get(&dlci).unwrap().is_open());
    assert_eq!(
        initiator.dlcs.get(&dlci).unwrap().tx_credits,
        DEFAULT_INITIAL_CREDITS
    );

    // Initiator -> responder data.
    let frames = initiator.write(dlci, b"The quick brown fox").unwrap();
    assert!(initiator.dlcs.get(&dlci).unwrap().tx_credits < DEFAULT_INITIAL_CREDITS);
    let mut received = Vec::new();
    for frame in frames {
        let (_, events) = responder.receive(&frame).unwrap();
        for event in events {
            if let MultiplexerEvent::DataReceived(got_dlci, data) = event {
                assert_eq!(got_dlci, dlci);
                received.extend(data);
            }
        }
    }
    assert_eq!(received, b"The quick brown fox");

    // Responder -> initiator data.
    let frames = responder.write(dlci, b"Lorem ipsum").unwrap();
    let mut received = Vec::new();
    for frame in frames {
        let (_, events) = initiator.receive(&frame).unwrap();
        for event in events {
            if let MultiplexerEvent::DataReceived(got_dlci, data) = event {
                assert_eq!(got_dlci, dlci);
                received.extend(data);
            }
        }
    }
    assert_eq!(received, b"Lorem ipsum");
}

#[test]
fn test_credit_exhaustion_blocks_send_until_refilled() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, 1);

    let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);
    // Responder granted the initiator only 1 initial tx credit.
    assert_eq!(initiator.dlcs.get(&dlci).unwrap().tx_credits, 1);

    let frames = initiator.write(dlci, b"first").unwrap();
    assert_eq!(frames.len(), 1);
    assert_eq!(initiator.dlcs.get(&dlci).unwrap().tx_credits, 0);

    // A second write has no credit left to send immediately.
    let frames = initiator.write(dlci, b"second").unwrap();
    assert!(frames.is_empty());

    // Peer grants 3 fresh credits via a credit-only UIH frame (P/F=1,
    // no data beyond the leading credit octet); the buffered "second"
    // write can now drain, spending one of them.
    let credit_grant = RfcommFrame::uih(0, dlci, vec![3], 1).to_bytes();
    let (out, events) = initiator.receive(&credit_grant).unwrap();
    assert!(events.is_empty());
    assert_eq!(out.len(), 1);
    assert_eq!(initiator.dlcs.get(&dlci).unwrap().tx_credits, 2);
}

#[test]
fn test_close_dlc_disc_ua() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
    let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);

    let disc = initiator.close_dlc(dlci).unwrap();
    assert_eq!(
        initiator.dlcs.get(&dlci).unwrap().state,
        DlcState::Disconnecting
    );

    let (to_initiator, events) = responder.receive(&disc).unwrap();
    assert_eq!(events, vec![MultiplexerEvent::DlcClosed(dlci)]);
    assert!(!responder.dlcs.contains_key(&dlci));

    let (_, events) = initiator.receive(&to_initiator[0]).unwrap();
    assert_eq!(events, vec![MultiplexerEvent::DlcClosed(dlci)]);
    assert!(!initiator.dlcs.contains_key(&dlci));
}

#[test]
fn test_disconnect_multiplexer() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    let sabm = initiator.start().unwrap();
    let (to_initiator, _) = responder.receive(&sabm).unwrap();
    initiator.receive(&to_initiator[0]).unwrap();

    let disc = initiator.disconnect().unwrap();
    let (to_initiator, events) = responder.receive(&disc).unwrap();
    assert_eq!(events, vec![MultiplexerEvent::Disconnected]);
    assert_eq!(responder.state, MultiplexerState::Disconnected);

    let (_, events) = initiator.receive(&to_initiator[0]).unwrap();
    assert_eq!(events, vec![MultiplexerEvent::Disconnected]);
    assert_eq!(initiator.state, MultiplexerState::Disconnected);
}

#[test]
fn test_open_dlc_rejected_unlisted_channel() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    // No listen() call: channel 1 is not being served.
    let sabm = initiator.start().unwrap();
    let (to_initiator, _) = responder.receive(&sabm).unwrap();
    initiator.receive(&to_initiator[0]).unwrap();

    let pn = initiator
        .open_dlc(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS)
        .unwrap();
    let (to_initiator, _) = responder.receive(&pn).unwrap();
    let (_, events) = initiator.receive(&to_initiator[0]).unwrap();
    assert_eq!(events, vec![MultiplexerEvent::DlcOpenRejected(1)]);
    assert_eq!(initiator.state, MultiplexerState::Connected);
}

#[test]
fn test_service_record_sdp_round_trip() {
    use crate::classic::sdp::SdpServer;

    let handle = 2u32;
    let channel = 1u8;
    let uuid = SdpUuid::Uuid128([
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x01,
    ]);

    let mut sdp_server = SdpServer::new();
    sdp_server.service_records.insert(
        handle,
        make_service_sdp_records(handle, channel, Some(uuid)),
    );

    let mut sdp_client = SdpClient::new();
    let channels =
        find_rfcomm_channels(&mut sdp_client, |req| sdp_server.handle_request(req, 1024)).unwrap();
    assert_eq!(channels.get(&channel), Some(&vec![uuid]));

    let found = find_rfcomm_channel_with_uuid(&mut sdp_client, uuid, |req| {
        sdp_server.handle_request(req, 1024)
    })
    .unwrap();
    assert_eq!(found, Some(channel));
}

#[test]
fn test_server_psm_registration_lifecycle() {
    let mut manager = ClassicChannelManager::new();
    let server = RfcommServer::new();
    server.register(&mut manager).unwrap();
    assert!(manager.is_server_registered(RFCOMM_PSM));
    manager.unregister_server(RFCOMM_PSM);
    assert!(!manager.is_server_registered(RFCOMM_PSM));
}

/// Brings a responder multiplexer's session up by feeding it SABM(0).
fn connected_responder() -> Multiplexer {
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder
        .receive(&RfcommFrame::sabm(1, 0).to_bytes())
        .expect("SABM(0) accepted");
    responder
}

fn pn_command_frame(dlci: u8, cl: u8, initial_credits: u8) -> Vec<u8> {
    let pn = MccPn {
        dlci,
        cl,
        priority: 7,
        ack_timer: 0,
        max_frame_size: DEFAULT_MAX_FRAME_SIZE,
        max_retransmissions: 0,
        initial_credits,
    };
    RfcommFrame::uih(1, 0, make_mcc(mcc_type::PN, true, &pn.to_bytes()), 0).to_bytes()
}

/// Extracts the single PN record from a multiplexer's reply frames.
fn sole_pn_response(frames: &[Vec<u8>]) -> MccPn {
    let frame = RfcommFrame::parse(&frames[0]).expect("valid frame");
    let (mcc, c_r, value) = parse_mcc(&frame.information).expect("valid mcc");
    assert_eq!(mcc, mcc_type::PN);
    assert!(!c_r, "a PN response must have C/R clear");
    MccPn::parse(value).expect("valid PN")
}

#[test]
fn test_a_bare_sabm_on_a_listened_channel_opens_the_dlc_with_defaults() {
    // PN is optional before SABM (RFCOMM 1.1 5.5.3): a peer may open a
    // DLC with a bare SABM. Dropping it silently left that peer waiting
    // out its own T1.
    let mut responder = connected_responder();
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

    let (out, events) = responder
        .receive(&RfcommFrame::sabm(1, 2).to_bytes())
        .unwrap();

    assert_eq!(events, vec![MultiplexerEvent::DlcOpened(2)]);
    let ua = RfcommFrame::parse(&out[0]).expect("valid frame");
    assert_eq!(ua.frame_type, frame_type::UA);
    assert_eq!(ua.dlci, 2);

    let dlc = responder.dlcs.get(&2).expect("DLC created");
    assert!(dlc.is_open());
    // Nothing was negotiated, so N1 is its default and CFC is off.
    assert_eq!(dlc.tx_max_frame_size, DEFAULT_FRAME_SIZE_WITHOUT_PN);
    assert!(!dlc.cfc);
    assert_eq!(responder.cfc, CreditFlowControl::NotSupported);
}

#[test]
fn test_a_bare_sabm_on_an_unlistened_channel_is_answered_with_dm() {
    let mut responder = connected_responder();

    let (out, events) = responder
        .receive(&RfcommFrame::sabm(1, 8).to_bytes())
        .unwrap();

    assert!(events.is_empty());
    let dm = RfcommFrame::parse(&out[0]).expect("valid frame");
    assert_eq!(dm.frame_type, frame_type::DM);
    assert_eq!(dm.dlci, 8);
    assert!(responder.dlcs.is_empty());
}

#[test]
fn test_a_disc_on_an_unknown_dlci_is_answered_with_dm() {
    let mut responder = connected_responder();

    let (out, _) = responder
        .receive(&RfcommFrame::disc(1, 6).to_bytes())
        .unwrap();

    let dm = RfcommFrame::parse(&out[0]).expect("valid frame");
    assert_eq!(dm.frame_type, frame_type::DM);
    assert_eq!(dm.dlci, 6);
}

#[test]
fn test_an_unrecognized_mcc_command_is_answered_with_nsc() {
    // TS 07.10 5.4.6.3.8: the answer to an unimplemented command is NSC,
    // not silence, and its value is the offending command's type octet.
    const RPN: u8 = 0x24;
    let mut responder = connected_responder();

    let rpn = RfcommFrame::uih(1, 0, make_mcc(RPN, true, &[0x0B]), 0).to_bytes();
    let (out, _) = responder.receive(&rpn).unwrap();

    let frame = RfcommFrame::parse(&out[0]).expect("valid frame");
    let (mcc, c_r, value) = parse_mcc(&frame.information).expect("valid mcc");
    assert_eq!(mcc, mcc_type::NSC);
    assert!(!c_r, "NSC is a response");
    assert_eq!(value, [(RPN << 2) | 0b11]);
}

#[test]
fn test_an_unrecognized_mcc_response_is_not_answered() {
    // NSC is itself an MCC type; answering a response would let two peers
    // that both lack it trade NSCs indefinitely.
    let mut responder = connected_responder();

    let rpn_response = RfcommFrame::uih(1, 0, make_mcc(0x24, false, &[0x0B]), 0).to_bytes();
    let (out, _) = responder.receive(&rpn_response).unwrap();

    assert!(out.is_empty());
}

#[test]
fn test_a_pn_asking_for_no_cfc_is_not_answered_with_cfc() {
    // RFCOMM 1.1 5.5.3: 0xE0 may only answer 0xF0. Answering it to a
    // 0x00 request makes our credit octets look like application data.
    let mut responder = connected_responder();
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

    let (out, _) = responder
        .receive(&pn_command_frame(2, pn_cl::NO_CFC, 0))
        .unwrap();

    let pn = sole_pn_response(&out);
    assert_eq!(pn.cl, pn_cl::NO_CFC);
    assert_eq!(pn.initial_credits, 0);
    assert_eq!(responder.cfc, CreditFlowControl::NotSupported);
}

#[test]
fn test_a_pn_asking_for_cfc_is_answered_with_cfc() {
    let mut responder = connected_responder();
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

    let (out, _) = responder
        .receive(&pn_command_frame(2, pn_cl::CFC_COMMAND, 7))
        .unwrap();

    let pn = sole_pn_response(&out);
    assert_eq!(pn.cl, pn_cl::CFC_RESPONSE);
    assert_eq!(pn.initial_credits, DEFAULT_INITIAL_CREDITS);
    assert_eq!(responder.cfc, CreditFlowControl::Supported);
}

#[test]
fn test_a_dlc_without_cfc_sends_no_credit_octet() {
    // The whole point of honouring cl=0x00: the peer's byte stream must
    // contain exactly what was written, with no credit octet spliced in.
    let mut responder = connected_responder();
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
    responder
        .receive(&pn_command_frame(2, pn_cl::NO_CFC, 0))
        .unwrap();
    responder
        .receive(&RfcommFrame::sabm(1, 2).to_bytes())
        .unwrap();

    let frames = responder.write(2, b"hello").unwrap();

    assert_eq!(frames.len(), 1, "no credit-only frame should be emitted");
    let frame = RfcommFrame::parse(&frames[0]).expect("valid frame");
    assert_eq!(frame.p_f, 0, "P/F marks a credit octet, which is absent");
    assert_eq!(frame.information, b"hello");
}

#[test]
fn test_a_dlc_without_cfc_is_not_gated_on_credits() {
    // Without CFC the peer grants no credits at all, so gating on them
    // would mean never sending anything.
    let mut responder = connected_responder();
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
    responder
        .receive(&pn_command_frame(2, pn_cl::NO_CFC, 0))
        .unwrap();
    responder
        .receive(&RfcommFrame::sabm(1, 2).to_bytes())
        .unwrap();
    assert_eq!(responder.dlcs.get(&2).unwrap().tx_credits, 0);

    assert!(!responder.write(2, b"first").unwrap().is_empty());
    assert!(!responder.write(2, b"second").unwrap().is_empty());
}

#[test]
fn test_a_dlc_with_cfc_still_sends_the_credit_octet() {
    let mut responder = connected_responder();
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
    responder
        .receive(&pn_command_frame(2, pn_cl::CFC_COMMAND, 7))
        .unwrap();
    responder
        .receive(&RfcommFrame::sabm(1, 2).to_bytes())
        .unwrap();

    let frames = responder.write(2, b"hello").unwrap();

    let frame = RfcommFrame::parse(&frames[0]).expect("valid frame");
    assert_eq!(frame.p_f, 1);
    assert_eq!(&frame.information[1..], b"hello");
}

#[test]
fn test_a_peer_disc_on_the_session_closes_its_open_dlcs() {
    // A DLCI only has meaning inside its multiplexer, so leaving DLCs
    // Connected after the session goes down lets `write` hand the caller
    // frames to send on a channel that no longer exists.
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
    let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);

    let (_, events) = responder
        .receive(&RfcommFrame::disc(1, 0).to_bytes())
        .unwrap();

    assert_eq!(
        events,
        vec![
            MultiplexerEvent::DlcClosed(dlci),
            MultiplexerEvent::Disconnected
        ]
    );
    assert!(responder.dlcs.is_empty());
    assert!(responder.write(dlci, b"x").is_err());
}

#[test]
fn test_our_own_session_disconnect_closes_its_open_dlcs() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
    let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);

    initiator.disconnect().unwrap();
    let (_, events) = initiator
        .receive(&RfcommFrame::ua(0, 0).to_bytes())
        .unwrap();

    assert_eq!(
        events,
        vec![
            MultiplexerEvent::DlcClosed(dlci),
            MultiplexerEvent::Disconnected
        ]
    );
    assert!(initiator.dlcs.is_empty());
}

#[test]
fn test_initial_credits_beyond_the_field_width_are_clamped_not_masked() {
    // `& 0x07` turns a request for 8 credits into a grant of 0: simble
    // believes it granted 8, the peer reads 0 and never transmits.
    let mut responder = connected_responder();
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, 20);

    let (out, _) = responder
        .receive(&pn_command_frame(2, pn_cl::CFC_COMMAND, 7))
        .unwrap();

    let pn = sole_pn_response(&out);
    assert_eq!(pn.initial_credits, MAX_INITIAL_CREDITS);
    assert_eq!(
        responder.dlcs.get(&2).unwrap().rx_credits,
        MAX_INITIAL_CREDITS,
        "what we think we granted must match what went on the wire"
    );
}

#[test]
fn test_open_dlc_clamps_initial_credits_to_the_field_width() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    initiator.start().unwrap();
    initiator
        .receive(&RfcommFrame::ua(0, 0).to_bytes())
        .unwrap();

    let bytes = initiator.open_dlc(1, DEFAULT_MAX_FRAME_SIZE, 200).unwrap();

    let frame = RfcommFrame::parse(&bytes).expect("valid frame");
    let (_, _, value) = parse_mcc(&frame.information).expect("valid mcc");
    let pn = MccPn::parse(value).expect("valid PN");
    assert_eq!(pn.initial_credits, MAX_INITIAL_CREDITS);
}

#[test]
fn test_data_delivered_immediately_after_dlc_opens() {
    // `receive` is synchronous and returns events directly, so a UIH
    // data frame delivered right after the UA that completes a DLC open
    // must still be reported correctly, with no window where it could
    // be dropped.
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
    let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);

    let data_frame = RfcommFrame::uih(0, dlci, b"123".to_vec(), 0).to_bytes();
    let (_, events) = initiator.receive(&data_frame).unwrap();
    assert_eq!(
        events,
        vec![MultiplexerEvent::DataReceived(dlci, b"123".to_vec())]
    );
}
