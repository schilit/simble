// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! WebSocket HCI transport to Android's netsim (Simble's primary client),
//! carrying the same H4-framed packets as `rootcanal::RootcanalTransport`
//! but over a WebSocket connection instead of a raw TCP pipe. netsim's real
//! endpoint, confirmed against its own source
//! (`rust/daemon/src/transport/websocket.rs`), is
//! `ws://localhost:7681/v1/websocket/bt?name=<device-name>&address=<mac>` —
//! the device's name and address are read straight from the connection URI's
//! query string, no separate handshake message. See the README's "Testing
//! Against netsim" section: this endpoint requires Android Studio Canary,
//! not Stable, whose bundled `netsimd` doesn't start the WebSocket frontend
//! server at all.
//!
//! This is the *client* half of the WebSocket link; the RFC 6455 codec it
//! shares with the `usb-ble-ws` *server* ([`super::ws`]) lives there. All this
//! module adds is the client-side opening handshake and the [`NetsimTransport`]
//! pump (which masks its outgoing frames, as clients must).

use super::ws::{
    DecodedFrame, OPCODE_BINARY, OPCODE_CLOSE, OPCODE_CONTINUATION, OPCODE_PING, OPCODE_PONG,
    WsFrameReader, encode_frame, encode_masked_binary_frame, expected_accept, mask_key,
    read_http_headers,
};
use crate::types::SimbleError;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::big_endian::{U32, U64},
};

/// btsnoop file header: the identification pattern, then the format version
/// and datalink type.
///
/// Every multi-byte field in btsnoop is **big-endian**, unlike the
/// little-endian HCI structures everywhere else in this crate. Writing these
/// fields little-endian yields a file that tools reject or, worse, misread —
/// so the byte order is pinned by test rather than left to inspection.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
struct BtsnoopFileHeader {
    /// Always `b"btsnoop\0"`.
    identification: [u8; 8],
    /// Format version number; 1 is the only version in use.
    version: U32,
    /// Datalink type; 1002 is HCI UART (H4).
    datalink: U32,
}

impl BtsnoopFileHeader {
    /// Datalink type for HCI UART (H4) — the framing this transport carries.
    const DATALINK_H4: u32 = 1002;

    /// Builds the fixed 16-byte header written once at the start of a trace.
    fn new() -> Self {
        Self {
            identification: *b"btsnoop\0",
            version: U32::new(1),
            datalink: U32::new(Self::DATALINK_H4),
        }
    }
}

/// btsnoop per-packet record header, immediately followed by the packet bytes.
/// Big-endian throughout, like [`BtsnoopFileHeader`].
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
struct BtsnoopRecordHeader {
    /// Length of the original packet.
    original_length: U32,
    /// Length actually captured; equal to `original_length` (never truncated).
    included_length: U32,
    /// Per-packet flags; see [`Self::FLAG_RECEIVED`].
    flags: U32,
    /// Packets dropped since the previous record; always 0 here.
    cumulative_drops: U32,
    /// Capture timestamp, microseconds since year 0.
    timestamp_micros: U64,
}

impl BtsnoopRecordHeader {
    /// btsnoop timestamps are microseconds since year 0; this constant is the
    /// Unix epoch on that scale (what Wireshark expects).
    const UNIX_EPOCH_OFFSET: u64 = 0x00DC_DDB3_0F2F_8000;
    /// Flags bit 0: set when the packet travelled controller→host.
    const FLAG_RECEIVED: u32 = 0x01;

    /// Builds a record header for a packet of `len` bytes captured at
    /// `micros` (already offset to the btsnoop epoch).
    fn new(len: u32, received: bool, micros: u64) -> Self {
        Self {
            original_length: U32::new(len),
            included_length: U32::new(len),
            flags: U32::new(if received { Self::FLAG_RECEIVED } else { 0 }),
            cumulative_drops: U32::new(0),
            timestamp_micros: U64::new(micros),
        }
    }
}

/// A `ws://host:port/path` URL, split into the pieces the handshake needs.
struct WsUrl {
    host: String,
    port: u16,
    path: String,
}

fn parse_ws_url(url: &str) -> Result<WsUrl, SimbleError> {
    let rest = url
        .strip_prefix("ws://")
        .ok_or_else(|| SimbleError::Transport(format!("unsupported WebSocket URL: {url}")))?;
    let (authority, path) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| SimbleError::Transport(format!("invalid port in URL: {url}")))?,
        ),
        None => (authority.to_string(), 80),
    };
    if host.is_empty() {
        return Err(SimbleError::Transport(format!(
            "missing host in URL: {url}"
        )));
    }
    Ok(WsUrl {
        host,
        port,
        path: path.to_string(),
    })
}

fn build_handshake_request(host: &str, port: u16, path: &str, key: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\n\
         Host: {host}:{port}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Key: {key}\r\n\
         Sec-WebSocket-Version: 13\r\n\
         \r\n"
    )
}

fn validate_handshake_response(response: &str, key: &str) -> Result<(), SimbleError> {
    let mut lines = response.split("\r\n");
    let status_line = lines
        .next()
        .ok_or_else(|| SimbleError::Transport("empty handshake response".to_string()))?;
    if !status_line.contains("101") {
        return Err(SimbleError::Transport(format!(
            "handshake rejected: {status_line}"
        )));
    }

    let mut accept = None;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("Sec-WebSocket-Accept")
        {
            accept = Some(value.trim().to_string());
        }
    }
    let accept = accept.ok_or_else(|| {
        SimbleError::Transport("handshake response missing Sec-WebSocket-Accept".to_string())
    })?;
    let expected = expected_accept(key);
    if accept != expected {
        return Err(SimbleError::Transport(format!(
            "Sec-WebSocket-Accept mismatch: got {accept}, expected {expected}"
        )));
    }
    Ok(())
}

fn perform_handshake<S: Read + Write>(
    stream: &mut S,
    host: &str,
    port: u16,
    path: &str,
) -> Result<(), SimbleError> {
    let key = super::ws::base64_encode(&super::ws::pseudo_random_bytes(16));
    let request = build_handshake_request(host, port, path, &key);
    stream
        .write_all(request.as_bytes())
        .map_err(|e| SimbleError::Transport(e.to_string()))?;
    let response = read_http_headers(stream)?;
    validate_handshake_response(&response, &key)
}

/// Bidirectional HCI transport to netsim over a hand-rolled WebSocket
/// client, generic over any `Read + Write` stream (a `TcpStream` in
/// practice, matching netsim's `ws://host:port/hci` interface).
pub struct NetsimTransport<S: Read + Write> {
    stream: S,
    reader: WsFrameReader,
    /// Accumulates a message's payload across `Continuation` frames — not
    /// expected from netsim in practice (H4 packets are sent as single
    /// unfragmented binary frames both ways) but handled for spec fidelity.
    fragment: Vec<u8>,
    fragment_opcode: Option<u8>,
    /// Optional btsnoop capture of every H4 packet both ways (Wireshark
    /// opens it directly) — the HCI-level equivalent of a sniffer, for
    /// debugging exchanges against real stacks. See [`Self::set_trace`].
    trace: Option<std::fs::File>,
}

impl NetsimTransport<TcpStream> {
    /// Connects to netsim at `url` (e.g. `"ws://localhost:9922/hci"`):
    /// opens a TCP connection, performs the WebSocket opening handshake,
    /// then puts the socket in non-blocking mode so `pump` never stalls.
    pub fn connect(url: &str) -> Result<Self, SimbleError> {
        let parsed = parse_ws_url(url)?;
        let mut stream = TcpStream::connect((parsed.host.as_str(), parsed.port)).map_err(|e| {
            // A refused connection almost always means netsimd isn't running
            // (or was started without its WebSocket frontend). Turn the bare
            // OS error into an actionable hint rather than "os error 61".
            if e.kind() == ErrorKind::ConnectionRefused {
                SimbleError::Transport(format!(
                    "could not reach netsim at {}:{} — is netsimd running with its \
                     WebSocket frontend enabled? Start it with:\n    \
                     netsimd --logtostderr --no-shutdown --ws-port {}\n  \
                     (needs the canary-channel emulator; see the README's \"Testing \
                     Against netsim\" section). Underlying error: {e}",
                    parsed.host, parsed.port, parsed.port
                ))
            } else {
                SimbleError::Transport(e.to_string())
            }
        })?;
        perform_handshake(&mut stream, &parsed.host, parsed.port, &parsed.path)?;
        stream
            .set_nonblocking(true)
            .map_err(|e| SimbleError::Transport(e.to_string()))?;
        Ok(Self::new(stream))
    }
}

impl<S: Read + Write> NetsimTransport<S> {
    /// Wraps an already-connected WebSocket stream as a netsim transport.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            reader: WsFrameReader::default(),
            fragment: Vec::new(),
            fragment_opcode: None,
            trace: None,
        }
    }

    /// Starts capturing every H4 packet (both directions) into `file` in
    /// btsnoop format (datalink 1002, HCI UART/H4) — the format Wireshark
    /// and `tshark` read natively. The header is written immediately.
    pub fn set_trace(&mut self, mut file: std::fs::File) -> std::io::Result<()> {
        file.write_all(BtsnoopFileHeader::new().as_bytes())?;
        self.trace = Some(file);
        Ok(())
    }

    /// Appends one btsnoop record; `received` is the direction flag
    /// (false = host→controller). Trace failures are swallowed — capture
    /// must never break the transport.
    fn trace_record(&mut self, packet: &[u8], received: bool) {
        let Some(file) = self.trace.as_mut() else {
            return;
        };
        let micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as u64)
            .unwrap_or(0)
            + BtsnoopRecordHeader::UNIX_EPOCH_OFFSET;
        let header = BtsnoopRecordHeader::new(packet.len() as u32, received, micros);
        let mut record = Vec::with_capacity(size_of::<BtsnoopRecordHeader>() + packet.len());
        record.extend_from_slice(header.as_bytes());
        record.extend_from_slice(packet);
        let _ = file.write_all(&record);
        let _ = file.flush();
    }

    fn handle_frame(&mut self, frame: DecodedFrame) -> Result<Option<Vec<u8>>, SimbleError> {
        match frame.opcode {
            OPCODE_BINARY | OPCODE_CONTINUATION => {
                if frame.opcode == OPCODE_BINARY {
                    self.fragment_opcode = Some(OPCODE_BINARY);
                    self.fragment.clear();
                }
                self.fragment.extend_from_slice(&frame.payload);
                if frame.fin && self.fragment_opcode.is_some() {
                    self.fragment_opcode = None;
                    return Ok(Some(std::mem::take(&mut self.fragment)));
                }
                Ok(None)
            }
            OPCODE_PING => {
                let pong = encode_frame(OPCODE_PONG, &frame.payload, Some(mask_key()));
                self.stream
                    .write_all(&pong)
                    .map_err(|e| SimbleError::Transport(e.to_string()))?;
                Ok(None)
            }
            OPCODE_PONG => Ok(None),
            OPCODE_CLOSE => Err(SimbleError::Transport(
                "netsim closed the WebSocket connection".to_string(),
            )),
            other => Err(SimbleError::Transport(format!(
                "unsupported WebSocket opcode {other:#x}"
            ))),
        }
    }

    /// Moves packets in both directions between netsim and `channel`,
    /// mirroring `RootcanalTransport::pump`: drains every packet `channel`
    /// has queued for the controller and sends it as one masked binary WS
    /// frame, then reads whatever bytes are currently available from the
    /// socket, decodes any complete WS frames they form (replying to Pings
    /// automatically), and hands each complete binary message to `channel`
    /// as-is — it's already a complete H4 packet, no further framing needed.
    pub fn pump(&mut self, channel: &super::HciChannel) -> Result<(), SimbleError> {
        while let Some(packet) = channel.poll_host_packet() {
            self.trace_record(&packet, false);
            let frame = encode_masked_binary_frame(&packet);
            self.stream
                .write_all(&frame)
                .map_err(|e| SimbleError::Transport(e.to_string()))?;
        }

        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    return Err(SimbleError::Transport(
                        "netsim WebSocket connection closed".to_string(),
                    ));
                }
                Ok(n) => self.reader.feed(&chunk[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => return Err(SimbleError::Transport(e.to_string())),
            }
        }

        while let Some(frame) = self.reader.next_frame() {
            if let Some(packet) = self.handle_frame(frame)? {
                self.trace_record(&packet, true);
                channel.receive_from_controller(packet)?;
            }
        }
        Ok(())
    }
}

// --- Scene over netsim ------------------------------------------------------

/// The WebSocket frontend of a netsimd started by Android Studio's emulator
/// (or by hand: `netsimd --logtostderr --no-shutdown --ws-port 7681`).
pub const DEFAULT_WS_URL: &str = "ws://127.0.0.1:7681";

/// A [`LiveScene`] whose backend is netsim: each peripheral is its own
/// WebSocket connection to netsimd, which routes advertising and data
/// between them, the Android emulator, and any other netsim clients. The
/// emulator (or another netsim device) plays the central.
pub struct NetsimScene {
    ws_url: String,
    scene: super::live_scene::LiveScene<NetsimTransport<TcpStream>>,
}

impl NetsimScene {
    /// Creates an empty netsim scene targeting `ws_url` (no connection is
    /// made until the first peripheral is added).
    pub fn new(ws_url: &str) -> Self {
        Self {
            ws_url: ws_url.trim_end_matches('/').to_string(),
            scene: super::live_scene::LiveScene::new(),
        }
    }

    /// Runs `script` and registers the resulting device with netsimd under
    /// its script name and `address` (netsim reads both from the connection
    /// URI's query string). A connection failure returns the "is netsimd
    /// running" hint from [`NetsimTransport::connect`].
    pub fn add_peripheral(
        &mut self,
        address: crate::types::Address,
        script: &str,
    ) -> Result<usize, String> {
        self.add_peripheral_named(address, script, None)
    }

    /// As [`Self::add_peripheral`], but registers the device under
    /// `node_name` instead of the name its script gave the GATT server.
    ///
    /// The node name is *placement*, not identity: it is the label netsim
    /// lists the device under (and the only handle a human has on it in
    /// `netsim devices`), and a scene file names it so two devices built from
    /// the same catalog script are still tellable apart. It does not change
    /// what the device advertises, which still comes from the script.
    pub fn add_peripheral_named(
        &mut self,
        address: crate::types::Address,
        script: &str,
        node_name: Option<&str>,
    ) -> Result<usize, String> {
        let ws_url = self.ws_url.clone();
        let node_name = node_name.map(str::to_string);
        self.scene.add_peripheral(address, script, |peripheral| {
            // The name goes verbatim into the query string; keep it URL-safe
            // (spaces are the only realistic offender in a device name).
            let name = node_name
                .unwrap_or_else(|| peripheral.device_name())
                .replace(' ', "%20");
            let addr_lsb = address.to_netsim_wire_string();
            let url = format!("{ws_url}/v1/websocket/bt?name={name}&address={addr_lsb}");
            let mut transport = NetsimTransport::connect(&url).map_err(|e| e.to_string())?;
            // SIMBLE_BTSNOOP=<dir> captures this device's HCI traffic to
            // <dir>/<name>.btsnoop (Wireshark-readable) for debugging
            // exchanges against real stacks.
            if let Ok(dir) = std::env::var("SIMBLE_BTSNOOP") {
                let path = std::path::Path::new(&dir).join(format!("{name}.btsnoop"));
                match std::fs::File::create(&path) {
                    Ok(file) => {
                        let _ = transport.set_trace(file);
                    }
                    Err(e) => eprintln!("SIMBLE_BTSNOOP: cannot create {path:?}: {e}"),
                }
            }
            Ok(transport)
        })
    }

    /// See [`LiveScene::pump`].
    pub fn pump(&mut self) {
        self.scene.pump();
    }

    /// See [`LiveScene::tick`].
    pub fn tick(&mut self, seconds: f64) {
        self.scene.tick(seconds);
    }

    /// The current script-clock time in seconds.
    pub fn now(&self) -> f64 {
        self.scene.now()
    }

    /// The number of peripherals in the scene.
    pub fn device_count(&self) -> usize {
        self.scene.device_count()
    }

    /// The GATT status JSON of peripheral `index`, or `None` for an unknown
    /// index.
    pub fn peripheral_status_json(&self, index: usize) -> Option<String> {
        self.scene.peripheral_status_json(index)
    }
}

#[cfg(test)]
mod tests {
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
}
