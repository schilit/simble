// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! RFCOMM tests: frame/FCS parsing (including corrupt-frame rejection),
//! multiplexer startup, DLC open/close over PN/SABM/UA/DISC, credit-based
//! flow control on an open DLC, and SDP service-record discovery of an
//! RFCOMM channel.

use simble::classic::rfcomm::{
    self, DEFAULT_INITIAL_CREDITS, DEFAULT_L2CAP_MTU, DEFAULT_MAX_FRAME_SIZE, Multiplexer,
    MultiplexerEvent, MultiplexerState, RFCOMM_PSM, RfcommFrame, RfcommServer, Role,
};
use simble::classic::sdp::{SdpClient, SdpServer, SdpUuid};
use simble::l2cap::classic::ClassicChannelManager;

/// Drives both sides of a `Multiplexer` pair through the startup handshake
/// and an `open_dlc` on `channel`, returning the resulting DLCI.
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
    opened_dlci.expect("DLC did not open within the round budget")
}

#[test]
fn test_frame_known_vector_round_trip() {
    // address=0x03 (dlci=0, c/r=1), control=0x3f (SABM, P/F=1), length=0x01
    // (1-byte EA, value 0, no payload), fcs=0x1c.
    let data = [0x03, 0x3f, 0x01, 0x1c];
    let frame = RfcommFrame::parse(&data).expect("valid frame");
    assert_eq!(frame.dlci, 0);
    assert_eq!(frame.c_r, 1);
    assert_eq!(frame.p_f, 1);
    assert!(frame.information.is_empty());
    assert_eq!(frame.to_bytes(), data);
}

#[test]
fn test_frame_with_payload_round_trip() {
    let frame = RfcommFrame::uih(
        1,
        7,
        b"The quick brown fox jumps over the lazy dog".to_vec(),
        0,
    );
    let bytes = frame.to_bytes();
    let parsed = RfcommFrame::parse(&bytes).expect("valid frame");
    assert_eq!(parsed, frame);
}

#[test]
fn test_frame_corrupt_fcs_is_rejected() {
    let frame = RfcommFrame::uih(1, 7, b"data".to_vec(), 0);
    let mut bytes = frame.to_bytes();
    *bytes.last_mut().unwrap() ^= 0x01;
    assert!(RfcommFrame::parse(&bytes).is_none());
}

#[test]
fn test_multiplexer_startup_handshake() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);

    let sabm = initiator.start().unwrap();
    let (to_initiator, events) = responder.receive(&sabm).unwrap();
    assert_eq!(events, vec![MultiplexerEvent::Started]);
    assert_eq!(responder.state, MultiplexerState::Connected);

    let (out, events) = initiator.receive(&to_initiator[0]).unwrap();
    assert!(out.is_empty());
    assert_eq!(events, vec![MultiplexerEvent::Started]);
    assert_eq!(initiator.state, MultiplexerState::Connected);
}

/// Opens a DLC, exchanges data in both directions over it, then closes it.
#[test]
fn test_connection_data_and_disconnection() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);

    let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);
    assert!(initiator.dlcs.get(&dlci).unwrap().is_open());
    assert!(responder.dlcs.get(&dlci).unwrap().is_open());

    let frames = initiator
        .write(dlci, b"The quick brown fox jumps over the lazy dog")
        .unwrap();
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
    assert_eq!(received, b"The quick brown fox jumps over the lazy dog");

    let frames = responder
        .write(dlci, b"Lorem ipsum dolor sit amet")
        .unwrap();
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
    assert_eq!(received, b"Lorem ipsum dolor sit amet");

    // Responder closes its end; initiator sees the DISC and acks with UA.
    let disc = responder.close_dlc(dlci).unwrap();
    let (to_initiator, events) = initiator.receive(&disc).unwrap();
    assert_eq!(events, vec![MultiplexerEvent::DlcClosed(dlci)]);
    assert!(!initiator.dlcs.contains_key(&dlci));
    let (_, events) = responder.receive(&to_initiator[0]).unwrap();
    assert_eq!(events, vec![MultiplexerEvent::DlcClosed(dlci)]);
    assert!(!responder.dlcs.contains_key(&dlci));
}

/// `Multiplexer::receive` is synchronous and reports events directly with no
/// sink/callback registration step, so a data frame delivered immediately
/// after a DLC opens must still be reported correctly, with no window where
/// it could be dropped.
#[test]
fn test_data_delivered_immediately_after_dlc_opens() {
    let mut initiator = Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU);
    let mut responder = Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU);
    responder.listen(1, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
    let dlci = drive_open_dlc(&mut initiator, &mut responder, 1);

    let frames = responder.write(dlci, b"123").unwrap();
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
    assert_eq!(received, b"123");
}

/// An RFCOMM service record advertising `channel` under `uuid` is
/// discoverable over SDP.
#[test]
fn test_service_record() {
    let handle = 2u32;
    let channel = 1u8;
    let uuid = SdpUuid::Uuid128([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);

    let mut sdp_server = SdpServer::new();
    sdp_server.service_records.insert(
        handle,
        rfcomm::make_service_sdp_records(handle, channel, Some(uuid)),
    );

    let mut sdp_client = SdpClient::new();
    let channels =
        rfcomm::find_rfcomm_channels(&mut sdp_client, |req| sdp_server.handle_request(req, 1024))
            .unwrap();
    assert_eq!(channels.get(&channel), Some(&vec![uuid]));

    let found = rfcomm::find_rfcomm_channel_with_uuid(&mut sdp_client, uuid, |req| {
        sdp_server.handle_request(req, 1024)
    })
    .unwrap();
    assert_eq!(found, Some(channel));
}

/// Exercises the RFCOMM PSM registration/unregistration lifecycle directly
/// against `ClassicChannelManager`.
#[test]
fn test_server_registers_and_unregisters_psm() {
    let mut manager = ClassicChannelManager::new();
    let server = RfcommServer::new();
    server.register(&mut manager).unwrap();
    assert!(manager.is_server_registered(RFCOMM_PSM));

    manager.unregister_server(RFCOMM_PSM);
    assert!(!manager.is_server_registered(RFCOMM_PSM));
}
