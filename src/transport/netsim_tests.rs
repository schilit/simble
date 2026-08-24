use super::*;
use crate::transport::h4_type;
use crate::transport::ws::WsFrameReader as Reader;
use crate::transport::ws::encode_frame as enc;
use std::io::Cursor;

#[test]
fn test_parse_ws_url() {
    let url = parse_ws_url("ws://localhost:9922/hci").unwrap();
    assert_eq!(url.host, "localhost");
    assert_eq!(url.port, 9922);
    assert_eq!(url.path, "/hci");

    assert_eq!(parse_ws_url("ws://example.com/hci").unwrap().port, 80);
    assert_eq!(parse_ws_url("ws://example.com:1234").unwrap().path, "/");
    assert!(parse_ws_url("http://example.com/hci").is_err());
}

#[test]
fn test_build_handshake_request_contains_required_headers() {
    let req = build_handshake_request("localhost", 9922, "/hci", "dGVzdGtleQ==");
    assert!(req.starts_with("GET /hci HTTP/1.1\r\n"));
    assert!(req.contains("Host: localhost:9922\r\n"));
    assert!(req.contains("Upgrade: websocket\r\n"));
    assert!(req.contains("Sec-WebSocket-Key: dGVzdGtleQ==\r\n"));
    assert!(req.ends_with("\r\n\r\n"));
}

#[test]
fn test_validate_handshake_response_accepts_correct_accept() {
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Sec-WebSocket-Accept: {}\r\n\r\n",
        expected_accept(key)
    );
    assert!(validate_handshake_response(&response, key).is_ok());
}

#[test]
fn test_validate_handshake_response_rejects_wrong_and_missing_accept() {
    let key = "dGhlIHNhbXBsZSBub25jZQ==";
    let wrong = "HTTP/1.1 101 Switching Protocols\r\n\
         Sec-WebSocket-Accept: not-the-right-value\r\n\r\n";
    assert!(validate_handshake_response(wrong, key).is_err());
    let missing = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n";
    assert!(validate_handshake_response(missing, key).is_err());
    let not_101 = "HTTP/1.1 404 Not Found\r\n\r\n";
    assert!(validate_handshake_response(not_101, key).is_err());
}

/// A duplex mock: reads drain `inbound` (WouldBlock at EOF, mimicking a
/// non-blocking socket), writes collect into `outbound`.
struct DuplexMock {
    inbound: Cursor<Vec<u8>>,
    outbound: Vec<u8>,
}
impl Read for DuplexMock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inbound.read(buf)?;
        if n == 0 {
            Err(std::io::Error::from(ErrorKind::WouldBlock))
        } else {
            Ok(n)
        }
    }
}
impl Write for DuplexMock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.outbound.extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_pump_bridges_both_directions() {
    let evt = [h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
    let stream = DuplexMock {
        inbound: Cursor::new(enc(OPCODE_BINARY, &evt, None)),
        outbound: Vec::new(),
    };
    let mut transport = NetsimTransport::new(stream);

    let channel = super::super::HciChannel::new();
    channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();
    transport.pump(&channel).unwrap();

    let mut reader = Reader::default();
    reader.feed(&transport.stream.outbound);
    let sent = reader.next_frame().unwrap();
    assert_eq!(sent.payload, vec![h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00]);
    assert_eq!(channel.poll_controller_packet().unwrap(), evt);
}

#[test]
fn test_pump_replies_to_ping_with_pong() {
    let ping = enc(OPCODE_PING, b"keepalive", None);
    let stream = DuplexMock {
        inbound: Cursor::new(ping),
        outbound: Vec::new(),
    };
    let mut transport = NetsimTransport::new(stream);
    transport.pump(&super::super::HciChannel::new()).unwrap();

    let mut reader = Reader::default();
    reader.feed(&transport.stream.outbound);
    let pong = reader.next_frame().unwrap();
    assert_eq!(pong.opcode, OPCODE_PONG);
    assert_eq!(pong.payload, b"keepalive");
}

#[test]
fn test_pump_errors_on_close_frame() {
    let stream = DuplexMock {
        inbound: Cursor::new(enc(OPCODE_CLOSE, &[], None)),
        outbound: Vec::new(),
    };
    let mut transport = NetsimTransport::new(stream);
    assert!(transport.pump(&super::super::HciChannel::new()).is_err());
}

/// btsnoop headers are fixed-size and densely packed; a struct that gained
/// padding would shift every field and produce an unreadable trace.
#[test]
fn test_btsnoop_header_layout_has_no_padding() {
    assert_eq!(size_of::<BtsnoopFileHeader>(), 16);
    assert_eq!(align_of::<BtsnoopFileHeader>(), 1);
    assert_eq!(size_of::<BtsnoopRecordHeader>(), 24);
    assert_eq!(align_of::<BtsnoopRecordHeader>(), 1);
}

/// The file header is exactly the 16 bytes every btsnoop reader looks for.
/// Datalink 1002 is 0x000003EA: big-endian it ends `03 EA`, little-endian
/// it would start `EA 03`, so this assertion alone catches a flipped
/// byte order.
#[test]
fn test_btsnoop_file_header_is_big_endian() {
    assert_eq!(
        BtsnoopFileHeader::new().as_bytes(),
        &[
            b'b', b't', b's', b'n', b'o', b'o', b'p', 0x00, // identification
            0x00, 0x00, 0x00, 0x01, // version 1, big-endian
            0x00, 0x00, 0x03, 0xEA, // datalink 1002 (H4), big-endian
        ]
    );

    let parsed = BtsnoopFileHeader::read_from_bytes(BtsnoopFileHeader::new().as_bytes())
        .expect("file header round-trips");
    assert_eq!(&parsed.identification, b"btsnoop\0");
    assert_eq!(parsed.version.get(), 1);
    assert_eq!(parsed.datalink.get(), BtsnoopFileHeader::DATALINK_H4);
}

/// Record headers are big-endian too. The values here are chosen so that
/// every field reads differently under the wrong byte order.
#[test]
fn test_btsnoop_record_header_is_big_endian() {
    // len 0x01020304, received, timestamp 0x0102030405060708.
    let header = BtsnoopRecordHeader::new(0x0102_0304, true, 0x0102_0304_0506_0708);
    assert_eq!(
        header.as_bytes(),
        &[
            0x01, 0x02, 0x03, 0x04, // original length
            0x01, 0x02, 0x03, 0x04, // included length (never truncated)
            0x00, 0x00, 0x00, 0x01, // flags: received
            0x00, 0x00, 0x00, 0x00, // cumulative drops
            0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, // timestamp
        ]
    );

    // Host→controller clears the direction bit; nothing else changes.
    let sent = BtsnoopRecordHeader::new(0x0102_0304, false, 0x0102_0304_0506_0708);
    assert_eq!(sent.flags.get(), 0);
    assert_eq!(&sent.as_bytes()[..8], &header.as_bytes()[..8]);
    assert_eq!(&sent.as_bytes()[12..], &header.as_bytes()[12..]);

    let parsed =
        BtsnoopRecordHeader::read_from_bytes(header.as_bytes()).expect("record round-trips");
    assert_eq!(parsed.original_length.get(), 0x0102_0304);
    assert_eq!(parsed.included_length.get(), 0x0102_0304);
    assert_eq!(parsed.flags.get(), BtsnoopRecordHeader::FLAG_RECEIVED);
    assert_eq!(parsed.cumulative_drops.get(), 0);
    assert_eq!(parsed.timestamp_micros.get(), 0x0102_0304_0506_0708);
}

/// The Unix-epoch offset is what makes timestamps land in the present day
/// rather than in year 0; a wrong constant yields a trace Wireshark opens
/// but dates absurdly. Check it converts back to a plausible year.
#[test]
fn test_btsnoop_epoch_offset_maps_unix_time_to_year_2026() {
    // 2026-01-01T00:00:00Z in Unix microseconds.
    const UNIX_2026: u64 = 1_767_225_600 * 1_000_000;
    let btsnoop = UNIX_2026 + BtsnoopRecordHeader::UNIX_EPOCH_OFFSET;
    // Years since year 0, using the mean Gregorian year (365.2425 days).
    let years = btsnoop as f64 / (365.2425 * 86_400.0 * 1e6);
    assert!(
        (2025.9..2026.1).contains(&years),
        "epoch offset should place Unix 2026 in btsnoop year 2026, got {years}"
    );
}

/// End-to-end: a traced `pump` must produce a file that is the 16-byte
/// btsnoop header followed by one well-formed record per packet, in both
/// directions, with the packet bytes carried verbatim.
#[test]
fn test_traced_pump_writes_a_readable_btsnoop_file() {
    let evt = [h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
    let cmd = [h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00];

    let path = std::env::temp_dir().join(format!(
        "simble-btsnoop-{}-{:?}.log",
        std::process::id(),
        std::thread::current().id()
    ));
    let file = std::fs::File::create(&path).expect("create trace file");

    let stream = DuplexMock {
        inbound: Cursor::new(enc(OPCODE_BINARY, &evt, None)),
        outbound: Vec::new(),
    };
    let mut transport = NetsimTransport::new(stream);
    transport.set_trace(file).expect("write btsnoop header");

    let channel = super::super::HciChannel::new();
    channel.send_command(&cmd[1..]).unwrap();
    transport.pump(&channel).unwrap();
    drop(transport);

    let trace = std::fs::read(&path).expect("read trace back");
    let _ = std::fs::remove_file(&path);

    // File header, then two records: the command out, the event in.
    let (file_header, mut rest) = trace.split_at(size_of::<BtsnoopFileHeader>());
    assert_eq!(file_header, BtsnoopFileHeader::new().as_bytes());

    for (expected_packet, expected_received) in [(&cmd[..], false), (&evt[..], true)] {
        let (header_bytes, body) = rest.split_at(size_of::<BtsnoopRecordHeader>());
        let header = BtsnoopRecordHeader::read_from_bytes(header_bytes).expect("record header");
        let len = header.original_length.get() as usize;
        assert_eq!(len, expected_packet.len());
        assert_eq!(header.included_length.get() as usize, len);
        assert_eq!(
            header.flags.get() == BtsnoopRecordHeader::FLAG_RECEIVED,
            expected_received
        );
        assert_eq!(header.cumulative_drops.get(), 0);
        // A live timestamp must at least be past the Unix epoch offset.
        assert!(header.timestamp_micros.get() > BtsnoopRecordHeader::UNIX_EPOCH_OFFSET);
        assert_eq!(&body[..len], expected_packet, "packet bytes are verbatim");
        rest = &body[len..];
    }
    assert!(rest.is_empty(), "no trailing bytes after the last record");
}
