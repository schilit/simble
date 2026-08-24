use super::*;

fn connect(server: &mut ObexServer) -> Response {
    let (bytes, event) = server.handle_packet(&Request::connect(0x1000, Vec::new()).to_bytes());
    assert_eq!(event, ServerEvent::Connected);
    Response::parse(&bytes, true).unwrap()
}

#[test]
fn test_connect_exchanges_maximum_packet_lengths() {
    let mut server = ObexServer::default();
    let response = connect(&mut server);
    assert_eq!(response.code, response::SUCCESS);
    assert_eq!(
        response.connect.unwrap().max_packet_length.get(),
        ServerLimits::default().max_packet_length,
        "the server advertises its own limit"
    );
    assert_eq!(
        server.peer_max_packet_length(),
        Some(0x1000),
        "and remembers the peer's"
    );
    assert!(server.is_connected());
}

/// The whole reason this state machine exists: a body split across
/// packets must be answered Continue until the Final bit arrives, and
/// reassembled in order.
#[test]
fn test_multi_packet_put_continues_then_succeeds() {
    let mut server = ObexServer::default();
    connect(&mut server);

    let body: Vec<u8> = (0..500u32).map(|i| (i % 251) as u8).collect();
    // A small maximum packet size to force several chunks.
    let packets = put_packets(Some("note.txt"), Some(b"text/plain\0"), &body, 128);
    assert!(packets.len() > 3, "expected a chunked transfer");

    for (i, packet) in packets.iter().enumerate() {
        let (response_bytes, event) = server.handle_packet(packet);
        let response = Response::parse(&response_bytes, false).unwrap();
        if i + 1 < packets.len() {
            assert_eq!(
                response.code,
                response::CONTINUE,
                "packet {i} of {}",
                packets.len()
            );
            assert_eq!(event, ServerEvent::Continued);
        } else {
            assert_eq!(response.code, response::SUCCESS, "the last packet");
            assert!(matches!(event, ServerEvent::ObjectReceived(_)));
        }
    }

    let objects = server.take_objects();
    assert_eq!(objects.len(), 1);
    let object = &objects[0];
    assert_eq!(object.name.as_deref(), Some("note.txt"));
    assert_eq!(object.mime_type.as_deref(), Some(&b"text/plain\0"[..]));
    assert_eq!(object.declared_length, Some(500));
    assert_eq!(object.body, body, "reassembled in order and complete");
    assert!(
        server.take_objects().is_empty(),
        "collection is destructive"
    );
}

#[test]
fn test_single_packet_put_succeeds_immediately() {
    let mut server = ObexServer::default();
    let packets = put_packets(Some("x"), None, b"hi", 0x2000);
    assert_eq!(packets.len(), 1);
    let (bytes, event) = server.handle_packet(&packets[0]);
    assert_eq!(
        Response::parse(&bytes, false).unwrap().code,
        response::SUCCESS
    );
    assert!(matches!(event, ServerEvent::ObjectReceived(_)));
    assert_eq!(server.take_objects()[0].body, b"hi");
}

/// OPP allows a push with no session; PBAP and MAP do not. The policy
/// is explicit so neither behaviour is an accident.
#[test]
fn test_session_policy_governs_operations_without_connect() {
    let mut open = ObexServer::new(SessionPolicy::Optional, ServerLimits::default());
    let (bytes, _) = open.handle_packet(&put_packets(Some("x"), None, b"hi", 0x2000)[0]);
    assert_eq!(
        Response::parse(&bytes, false).unwrap().code,
        response::SUCCESS,
        "OPP accepts a bare push"
    );

    let mut strict = ObexServer::new(SessionPolicy::Required, ServerLimits::default());
    let (bytes, event) = strict.handle_packet(&put_packets(Some("x"), None, b"hi", 0x2000)[0]);
    assert_eq!(
        Response::parse(&bytes, false).unwrap().code,
        response::SERVICE_UNAVAILABLE
    );
    assert_eq!(event, ServerEvent::Rejected(response::SERVICE_UNAVAILABLE));

    // After connecting, the same push is accepted.
    connect(&mut strict);
    let (bytes, _) = strict.handle_packet(&put_packets(Some("x"), None, b"hi", 0x2000)[0]);
    assert_eq!(
        Response::parse(&bytes, false).unwrap().code,
        response::SUCCESS
    );
}

/// A peer must not be able to make the server hold unbounded memory by
/// never setting the Final bit.
#[test]
fn test_an_oversized_object_is_refused_and_the_partial_dropped() {
    let mut server = ObexServer::new(
        SessionPolicy::Optional,
        ServerLimits {
            max_packet_length: 0x2000,
            max_object_bytes: 100,
        },
    );
    let body = vec![0xAB; 500];
    let packets = put_packets(Some("big"), None, &body, 128);

    let mut refused = false;
    for packet in &packets {
        let (bytes, event) = server.handle_packet(packet);
        if Response::parse(&bytes, false).unwrap().code == response::ENTITY_TOO_LARGE {
            assert_eq!(event, ServerEvent::Rejected(response::ENTITY_TOO_LARGE));
            refused = true;
            break;
        }
    }
    assert!(refused, "the server must stop an object above its limit");
    assert!(
        server.take_objects().is_empty(),
        "and must not surface a partial object"
    );
}

#[test]
fn test_abort_discards_the_transfer_in_progress() {
    let mut server = ObexServer::default();
    connect(&mut server);
    let packets = put_packets(Some("x"), None, &vec![0u8; 400], 128);
    server.handle_packet(&packets[0]); // mid-transfer

    let (bytes, event) = server.handle_packet(&Request::abort().to_bytes());
    assert_eq!(
        Response::parse(&bytes, false).unwrap().code,
        response::SUCCESS
    );
    assert_eq!(event, ServerEvent::Aborted);
    assert!(
        server.take_objects().is_empty(),
        "no partial object escapes"
    );

    // A fresh transfer after the abort is unaffected by the abandoned one.
    for packet in put_packets(Some("y"), None, b"ok", 0x2000) {
        server.handle_packet(&packet);
    }
    assert_eq!(server.take_objects()[0].body, b"ok");
}

#[test]
fn test_disconnect_ends_the_session() {
    let mut server = ObexServer::new(SessionPolicy::Required, ServerLimits::default());
    connect(&mut server);
    let (bytes, event) = server.handle_packet(&Request::disconnect(Vec::new()).to_bytes());
    assert_eq!(
        Response::parse(&bytes, false).unwrap().code,
        response::SUCCESS
    );
    assert_eq!(event, ServerEvent::Disconnected);
    assert!(!server.is_connected());
}

/// A malformed packet must produce a response, not a panic and not
/// silence — a peer waiting for an answer would otherwise hang.
#[test]
fn test_malformed_packets_are_answered_bad_request() {
    let mut server = ObexServer::default();
    for bad in [
        vec![0x80, 0x00],                         // truncated prefix
        vec![0x80, 0x00, 0x01],                   // length below prefix
        vec![0x82, 0x00, 0x06, 0x01, 0x00, 0x02], // bad inner header
        vec![0x80, 0x00, 0x05, 0x10, 0x00],       // CONNECT missing fields
    ] {
        let (bytes, event) = server.handle_packet(&bad);
        assert_eq!(
            Response::parse(&bytes, false).unwrap().code,
            response::BAD_REQUEST,
            "input {bad:02X?}"
        );
        assert_eq!(event, ServerEvent::Rejected(response::BAD_REQUEST));
    }
}

#[test]
fn test_unimplemented_operations_are_answered_not_silently_dropped() {
    let mut server = ObexServer::default();
    let (bytes, event) = server.handle_packet(&Request::get(true, Vec::new()).to_bytes());
    assert_eq!(
        Response::parse(&bytes, false).unwrap().code,
        response::NOT_IMPLEMENTED
    );
    assert_eq!(event, ServerEvent::Rejected(response::NOT_IMPLEMENTED));
}

/// Chunking must respect the peer's advertised packet size, including
/// the per-packet and per-header overhead.
#[test]
fn test_chunking_respects_the_peer_maximum_packet_length() {
    let body = vec![0x5A; 1000];
    for max in [64u16, 128, 255, 512] {
        let packets = put_packets(Some("f"), None, &body, max);
        for packet in &packets {
            assert!(
                packet.len() <= usize::from(max),
                "packet of {} bytes exceeds the {max}-byte maximum",
                packet.len()
            );
        }
        // And the transfer still reassembles exactly.
        let mut server = ObexServer::default();
        for packet in &packets {
            server.handle_packet(packet);
        }
        assert_eq!(server.take_objects()[0].body, body, "max {max}");
    }
}
