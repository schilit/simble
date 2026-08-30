use super::*;
use crate::transport::HciChannel;
use crate::transport::h4_type;
use std::io::Cursor;

#[test]
fn test_read_http_body_reads_exactly_content_length() {
    // The body follows the headers on the same stream; read_http_headers stops at
    // the blank line, so read_http_body must pick up exactly Content-Length bytes
    // and no more (a trailing pipelined byte stays unread).
    let headers = "POST /v1/run HTTP/1.1\r\nContent-Length: 5\r\n\r\n";
    let mut stream = Cursor::new(b"hello!".to_vec()); // 6 bytes; only 5 are the body
    let body = read_http_body(&mut stream, headers).unwrap();
    assert_eq!(body, "hello");

    // No Content-Length → no body, and the stream is left untouched.
    let mut empty = Cursor::new(Vec::new());
    assert_eq!(
        read_http_body(&mut empty, "GET /v1/clock HTTP/1.1\r\n\r\n").unwrap(),
        ""
    );

    // A length longer than the stream is an error, not a hang or panic.
    let mut short = Cursor::new(b"ab".to_vec());
    assert!(read_http_body(&mut short, "POST / HTTP/1.1\r\nContent-Length: 9\r\n\r\n").is_err());
}

#[test]
fn test_sha1_matches_known_vector() {
    // "abc" -> a9993e364706816aba3e25717850c26c9cd0d89d (FIPS 180-4 Appendix A.1)
    assert_eq!(
        sha1(b"abc"),
        [
            0xa9, 0x99, 0x3e, 0x36, 0x47, 0x06, 0x81, 0x6a, 0xba, 0x3e, 0x25, 0x71, 0x78, 0x50,
            0xc2, 0x6c, 0x9c, 0xd0, 0xd8, 0x9d
        ]
    );
}

#[test]
fn test_base64_encode_known_vectors() {
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
}

#[test]
fn test_expected_accept_matches_rfc6455_example() {
    // RFC 6455 Section 1.3 worked example.
    assert_eq!(
        expected_accept("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

#[test]
fn test_encode_frame_short_length_masked_round_trips() {
    let payload = vec![h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00];
    let frame = encode_masked_binary_frame(&payload);
    assert_eq!(frame[0], 0x80 | OPCODE_BINARY);
    assert_eq!(frame[1] & 0x80, 0x80, "client frame must be masked");

    let mut reader = WsFrameReader::default();
    reader.feed(&frame);
    let decoded = reader.next_frame().unwrap();
    assert!(decoded.fin);
    assert_eq!(decoded.opcode, OPCODE_BINARY);
    assert_eq!(decoded.payload, payload);
}

#[test]
fn test_encode_frame_extended_lengths_round_trip() {
    for size in [200usize, 70_000] {
        let payload = vec![0xAB; size];
        let frame = encode_frame(OPCODE_BINARY, &payload, None);
        let mut reader = WsFrameReader::default();
        reader.feed(&frame);
        assert_eq!(reader.next_frame().unwrap().payload, payload);
    }
}

#[test]
fn test_frame_reader_handles_message_split_across_reads() {
    let payload = vec![h4_type::HCI_EVENT, 0x03, 0x02, 0xAA, 0xBB];
    let frame = encode_frame(OPCODE_BINARY, &payload, None);
    let mut reader = WsFrameReader::default();
    for chunk in frame.chunks(3) {
        reader.feed(chunk);
    }
    assert_eq!(reader.next_frame().unwrap().payload, payload);
}

#[test]
fn test_server_handshake_echoes_correct_accept_and_query() {
    // A client request with the RFC 6455 example key on netsim's path.
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let request = format!(
        "GET /v1/websocket/bt?name=web&address=AA:BB HTTP/1.1\r\n\
         Host: localhost:7681\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\r\n"
    );
    // A duplex mock whose reads drain `request`, writes collect the reply.
    struct Mock {
        inbound: Cursor<Vec<u8>>,
        outbound: Vec<u8>,
    }
    impl Read for Mock {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.inbound.read(buf)
        }
    }
    impl Write for Mock {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.outbound.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut mock = Mock {
        inbound: Cursor::new(request.into_bytes()),
        outbound: Vec::new(),
    };
    let query = server_handshake(&mut mock).unwrap();
    assert_eq!(query, "name=web&address=AA:BB");
    let reply = String::from_utf8(mock.outbound).unwrap();
    assert!(reply.starts_with("HTTP/1.1 101 "));
    assert!(reply.contains(&format!(
        "Sec-WebSocket-Accept: {}\r\n",
        expected_accept(key)
    )));
}

#[test]
fn test_server_handshake_rejects_missing_key() {
    // A request with no Sec-WebSocket-Key must fail before any reply.
    struct Rw(Cursor<Vec<u8>>);
    impl Read for Rw {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            self.0.read(buf)
        }
    }
    impl Write for Rw {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let request = b"GET /bt HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\r\n".to_vec();
    assert!(server_handshake(&mut Rw(Cursor::new(request))).is_err());
}

/// A mock stream whose reads come from a queue of masked client frames and
/// whose writes are captured — enough to drive `WsServerConn::pump`.
struct ServerMock {
    inbound: Cursor<Vec<u8>>,
    outbound: Vec<u8>,
}
impl Read for ServerMock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inbound.read(buf)?;
        if n == 0 {
            Err(std::io::Error::from(std::io::ErrorKind::WouldBlock))
        } else {
            Ok(n)
        }
    }
}
impl Write for ServerMock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.outbound.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_server_conn_bridges_both_directions() {
    // Client (host) sends a masked HCI command; it must land on the
    // channel's host side unchanged.
    let cmd = [h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00];
    let inbound = encode_masked_binary_frame(&cmd);
    let mut conn = WsServerConn::new(ServerMock {
        inbound: Cursor::new(inbound),
        outbound: Vec::new(),
    });
    let channel = HciChannel::new();

    // Controller (dongle) has an event ready for the host.
    let evt = [h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
    channel.receive_from_controller(evt.to_vec()).unwrap();

    conn.pump(&channel).unwrap();

    // Host packet arrived, ready for the dongle.
    assert_eq!(channel.poll_host_packet().unwrap(), cmd);

    // Event went out as one unmasked binary frame.
    let mut reader = WsFrameReader::default();
    reader.feed(&conn.stream.outbound);
    let sent = reader.next_frame().unwrap();
    assert_eq!(sent.opcode, OPCODE_BINARY);
    assert_eq!(sent.payload, evt);
    assert_eq!(
        conn.stream.outbound[1] & 0x80,
        0,
        "server frame must be unmasked"
    );
}

#[test]
fn test_close_does_not_discard_messages_decoded_in_the_same_batch() {
    // A client that sends a request and immediately closes. The request
    // must still be delivered; the close is reported on the next call.
    // Returning the error straight from the read loop (as this did before
    // the message layer existed) dropped the whole final batch.
    let cmd = [h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00];
    let mut inbound = encode_masked_binary_frame(&cmd);
    inbound.extend_from_slice(&encode_frame(OPCODE_CLOSE, &[], Some([1, 2, 3, 4])));
    let mut conn = WsServerConn::new(ServerMock {
        inbound: Cursor::new(inbound),
        outbound: Vec::new(),
    });
    let channel = HciChannel::new();

    conn.pump(&channel)
        .expect("the batch before the Close is still delivered");
    assert_eq!(channel.poll_host_packet().unwrap(), cmd);
    assert!(
        conn.pump(&channel).is_err(),
        "the close is reported on the following pump"
    );
}

#[test]
fn test_text_messages_round_trip_through_the_message_layer() {
    // The MCP-over-WebSocket path: JSON-RPC in a masked client text frame
    // out, an unmasked server text frame back.
    let request = br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#;
    let mut conn = WsServerConn::new(ServerMock {
        inbound: Cursor::new(encode_frame(OPCODE_TEXT, request, Some([9, 8, 7, 6]))),
        outbound: Vec::new(),
    });

    assert_eq!(conn.poll_messages().unwrap(), vec![request.to_vec()]);

    conn.send_text(r#"{"jsonrpc":"2.0","id":1,"result":{}}"#)
        .unwrap();
    let mut reader = WsFrameReader::default();
    reader.feed(&conn.stream.outbound);
    let sent = reader.next_frame().unwrap();
    assert_eq!(sent.opcode, OPCODE_TEXT);
    assert_eq!(sent.payload, br#"{"jsonrpc":"2.0","id":1,"result":{}}"#);
    assert_eq!(
        conn.stream.outbound[1] & 0x80,
        0,
        "server frame must be unmasked"
    );
}

#[test]
fn test_server_conn_replies_to_ping_unmasked() {
    let ping = encode_frame(OPCODE_PING, b"hi", Some([1, 2, 3, 4]));
    let mut conn = WsServerConn::new(ServerMock {
        inbound: Cursor::new(ping),
        outbound: Vec::new(),
    });
    conn.pump(&HciChannel::new()).unwrap();
    let mut reader = WsFrameReader::default();
    reader.feed(&conn.stream.outbound);
    let pong = reader.next_frame().unwrap();
    assert_eq!(pong.opcode, OPCODE_PONG);
    assert_eq!(pong.payload, b"hi");
}
