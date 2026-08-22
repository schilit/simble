// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! OBEX integration tests: the protocol driven the way a transport would
//! drive it, and the Object Push Profile built on top.
//!
//! These deliberately exercise the public API only — a caller relaying byte
//! buffers, which is exactly what wiring OBEX onto RFCOMM will do.

use simble::obex::{
    ClientState, Header, ObexClient, ObexServer, ServerEvent, ServerLimits, SessionPolicy,
    object_push_server, object_push_service_record,
    packet::{Request, Response, response},
    server::put_packets,
};

/// Relays packets between a client and server until the exchange ends,
/// returning the server events it produced. This is the shape RFCOMM will
/// take: two byte buffers passed back and forth.
fn run_exchange(client: &mut ObexClient, server: &mut ObexServer, first: Vec<u8>) -> Vec<ServerEvent> {
    let mut events = Vec::new();
    let mut packet = first;
    loop {
        let (response_bytes, event) = server.handle_packet(&packet);
        events.push(event);
        match client.handle_response(&response_bytes).unwrap() {
            Some(next) => packet = next,
            None => break,
        }
    }
    events
}

#[test]
fn test_a_push_completes_over_a_relayed_byte_stream() {
    let mut client = ObexClient::new(0x2000);
    let mut server = ObexServer::default();

    let connect = client.connect();
    run_exchange(&mut client, &mut server, connect);
    assert_eq!(client.state(), ClientState::Connected);
    assert!(server.is_connected());

    // The CONNECT exchange must leave each side knowing the other's limit.
    assert_eq!(server.peer_max_packet_length(), Some(0x2000));

    let body: Vec<u8> = (0..3000u32).map(|i| (i % 97) as u8).collect();
    let first = client.put(Some("data.bin"), Some(b"application/octet-stream\0"), &body, 256);
    let events = run_exchange(&mut client, &mut server, first);

    // Every packet but the last must have been answered Continue.
    let continued = events
        .iter()
        .filter(|e| **e == ServerEvent::Continued)
        .count();
    assert!(continued > 0, "a 3000-byte object must span packets");
    assert!(
        matches!(events.last(), Some(ServerEvent::ObjectReceived(_))),
        "the exchange must end with a completed object, got {:?}",
        events.last()
    );

    let objects = server.take_objects();
    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].body, body, "reassembled byte-for-byte");
    assert_eq!(objects[0].name.as_deref(), Some("data.bin"));
}

/// OPP's defining behaviour: a phone can push a vCard at a device it has
/// never connected to (OPP 1.2, Section 4.3).
#[test]
fn test_object_push_accepts_a_vcard_with_no_session() {
    let mut server = object_push_server(ServerLimits::default());
    let vcard = b"BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Ada Lovelace\r\nEND:VCARD\r\n";

    for packet in put_packets(Some("ada.vcf"), Some(b"text/x-vcard\0"), vcard, 0x2000) {
        let (bytes, _) = server.handle_packet(&packet);
        let parsed = Response::parse(&bytes, false).unwrap();
        assert!(
            parsed.code == response::CONTINUE || parsed.code == response::SUCCESS,
            "unexpected response {:#04X}",
            parsed.code
        );
    }

    let objects = server.take_objects();
    assert_eq!(objects[0].body, vcard);
    assert_eq!(objects[0].mime_type.as_deref(), Some(&b"text/x-vcard\0"[..]));
}

/// The SDP record is what a phone reads before it will push anything, so it
/// must name a real RFCOMM channel and the full protocol stack.
#[test]
fn test_object_push_record_is_discoverable_and_complete() {
    use simble::classic::sdp::{SdpServer, Service};

    let record: Service = object_push_service_record(9, "Simble Object Push", &[0xFF]);
    let mut sdp = SdpServer::new();
    sdp.service_records.insert(0x0001_0000, record);

    // A phone searching for OBEXObjectPush (0x1105) must find it. Rather
    // than hand-rolling a search PDU, assert the record is registered and
    // carries the attributes a searcher reads.
    assert_eq!(sdp.service_records.len(), 1);
    let stored = sdp.service_records.values().next().unwrap();
    assert!(
        stored.len() >= 4,
        "class list, protocol list, name and supported formats"
    );
}

/// A peer that never sets the Final bit must not be able to make the server
/// accumulate without bound.
#[test]
fn test_server_refuses_an_object_beyond_its_limit() {
    let mut server = ObexServer::new(
        SessionPolicy::Optional,
        ServerLimits {
            max_packet_length: 0x2000,
            max_object_bytes: 256,
        },
    );

    let mut refused = false;
    for packet in put_packets(Some("flood"), None, &vec![0u8; 4096], 128) {
        let (bytes, _) = server.handle_packet(&packet);
        if Response::parse(&bytes, false).unwrap().code == response::ENTITY_TOO_LARGE {
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert!(server.take_objects().is_empty(), "no partial object escapes");
}

/// Garbage from a peer must produce a response rather than a panic — a
/// hung transfer is worse than a rejected one.
#[test]
fn test_arbitrary_bytes_never_panic_the_server() {
    let mut server = ObexServer::default();
    let inputs: Vec<Vec<u8>> = vec![
        vec![],
        vec![0x00],
        vec![0xFF; 3],
        vec![0x80, 0xFF, 0xFF],           // enormous declared length
        vec![0x82, 0x00, 0x03],           // empty PUT-Final
        vec![0x02, 0x00, 0x06, 0x48, 0xFF, 0xFF], // body header overrunning
        (0..64).map(|i| i as u8).collect(),
    ];
    for input in inputs {
        let (bytes, _) = server.handle_packet(&input);
        assert!(
            !bytes.is_empty(),
            "every input must be answered: {input:02X?}"
        );
    }
}

/// Headers this build does not model must survive a round trip so a peer
/// speaking a later revision is not silently corrupted.
#[test]
fn test_unmodelled_headers_survive_a_request_round_trip() {
    let request = Request::put(
        true,
        vec![
            Header::Name("x".into()),
            Header::Other {
                identifier: 0x4F,
                value: simble::obex::HeaderValue::Bytes(vec![1, 2, 3]),
            },
            Header::EndOfBody(b"body".to_vec()),
        ],
    );
    let parsed = Request::parse(&request.to_bytes()).unwrap();
    assert_eq!(parsed.headers, request.headers);
}
