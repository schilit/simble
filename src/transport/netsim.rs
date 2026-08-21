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
        }
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
                channel.receive_from_controller(packet)?;
            }
        }
        Ok(())
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
}
