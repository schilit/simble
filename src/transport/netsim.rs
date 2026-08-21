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
//! Each WebSocket message is exactly one complete H4-framed HCI packet —
//! WebSocket's own message framing (RFC 6455 Section 5) gives packet
//! boundaries for free, so unlike `rootcanal::H4FrameReader` there's no need
//! to parse H4 headers to find packet lengths, only to buffer partial
//! WebSocket frames until a complete one has arrived.
//!
//! This module hand-rolls the minimal pieces of RFC 6455 needed to talk to
//! netsim: the opening handshake (including a small SHA-1 and base64
//! encoder, since the handshake's `Sec-WebSocket-Accept` check needs both
//! and neither exists elsewhere in this codebase), and binary data-frame
//! encoding/decoding. No `wss://` (TLS) support, since netsim is always a
//! local connection.

use crate::types::SimbleError;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned,
    byteorder::big_endian::{U16 as BeU16, U64 as BeU64},
};

/// Fixed by RFC 6455 Section 1.3: concatenated with the client's
/// `Sec-WebSocket-Key` and SHA-1/base64'd to derive `Sec-WebSocket-Accept`.
const WS_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

// --- SHA-1 (FIPS 180-4), scoped to exactly what the handshake needs. ---

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];

    let bit_len = (data.len() as u64) * 8;
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.as_chunks::<64>().0 {
        let mut w = [0u32; 80];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }

        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

// --- base64 (RFC 4648), encode-only, needed for the handshake key/accept. ---

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        let n = (u32::from(b0) << 16) | (u32::from(b1) << 8) | u32::from(b2);
        out.push(BASE64_ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(BASE64_ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(if chunk.len() > 1 {
            BASE64_ALPHABET[((n >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            BASE64_ALPHABET[(n & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// A minimal, non-cryptographically-secure PRNG (xorshift), sufficient for
/// generating a `Sec-WebSocket-Key` nonce and per-frame masking keys — RFC
/// 6455 doesn't require these be unpredictable to an attacker, only unlikely
/// to collide / trivially compressible (Section 5.3).
fn pseudo_random_bytes(n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    crate::types::rng::fill_pseudo_random(&n as *const usize as u64, &mut out);
    out
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

fn expected_accept(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(WS_GUID.as_bytes());
    base64_encode(&sha1(&input))
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

/// Reads a `\r\n\r\n`-terminated HTTP response header block one byte at a
/// time (simplest correct approach — handshake responses are tiny and this
/// only runs once per connection, so it doesn't need to be fast).
fn read_http_headers<R: Read>(reader: &mut R) -> Result<String, SimbleError> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = reader
            .read(&mut byte)
            .map_err(|e| SimbleError::Transport(e.to_string()))?;
        if n == 0 {
            return Err(SimbleError::Transport(
                "connection closed during handshake".to_string(),
            ));
        }
        buf.push(byte[0]);
        if buf.ends_with(b"\r\n\r\n") {
            return String::from_utf8(buf)
                .map_err(|e| SimbleError::Transport(format!("non-UTF8 handshake response: {e}")));
        }
        if buf.len() > 16 * 1024 {
            return Err(SimbleError::Transport(
                "handshake response too large".to_string(),
            ));
        }
    }
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
    let key = base64_encode(&pseudo_random_bytes(16));
    let request = build_handshake_request(host, port, path, &key);
    stream
        .write_all(request.as_bytes())
        .map_err(|e| SimbleError::Transport(e.to_string()))?;
    let response = read_http_headers(stream)?;
    validate_handshake_response(&response, &key)
}

// --- RFC 6455 Section 5: WebSocket data framing. ---

const OPCODE_CONTINUATION: u8 = 0x0;
const OPCODE_BINARY: u8 = 0x2;
const OPCODE_CLOSE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xA;

/// 2-byte fixed WebSocket frame header (RFC 6455 Section 5.2): FIN/RSV/opcode
/// in the first byte, MASK bit + 7-bit length (or length-class selector) in
/// the second. `len7() == 126` or `127` means the real length instead lives
/// in a following [`WsExtLen16`]/[`WsExtLen64`], mirroring how `HciAclHeader`
/// in `l2cap_frame.rs` keeps packed bit fields behind accessor methods
/// rather than unpacking them into separate struct fields.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
struct WsFrameHeader {
    fin_rsv_opcode: u8,
    mask_len7: u8,
}

impl WsFrameHeader {
    fn new(fin: bool, opcode: u8, masked: bool, len7: u8) -> Self {
        Self {
            fin_rsv_opcode: (if fin { 0x80 } else { 0 }) | (opcode & 0x0F),
            mask_len7: (if masked { 0x80 } else { 0 }) | (len7 & 0x7F),
        }
    }

    fn fin(&self) -> bool {
        self.fin_rsv_opcode & 0x80 != 0
    }

    fn opcode(&self) -> u8 {
        self.fin_rsv_opcode & 0x0F
    }

    fn is_masked(&self) -> bool {
        self.mask_len7 & 0x80 != 0
    }

    fn len7(&self) -> u8 {
        self.mask_len7 & 0x7F
    }

    fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        Ref::<&[u8], Self>::from_prefix(bytes).ok()
    }
}

/// Extended 16-bit payload length (RFC 6455 Section 5.2), sent when
/// `len7() == 126`. Big-endian ("network byte order") per the spec — unlike
/// this codebase's Bluetooth packets, which are little-endian throughout.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
struct WsExtLen16 {
    len: BeU16,
}

impl WsExtLen16 {
    fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        Ref::<&[u8], Self>::from_prefix(bytes).ok()
    }
}

/// Extended 64-bit payload length (RFC 6455 Section 5.2), sent when
/// `len7() == 127`. Big-endian, as with [`WsExtLen16`].
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
struct WsExtLen64 {
    len: BeU64,
}

impl WsExtLen64 {
    fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        Ref::<&[u8], Self>::from_prefix(bytes).ok()
    }
}

/// 4-byte client-to-server masking key (RFC 6455 Section 5.2), present only
/// when [`WsFrameHeader::is_masked`] is set.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
struct WsMaskKey {
    key: [u8; 4],
}

impl WsMaskKey {
    fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
        Ref::<&[u8], Self>::from_prefix(bytes).ok()
    }
}

/// Encodes `payload` as a single unfragmented frame with the given opcode.
/// `mask` is `Some` for client-to-server frames (mandatory per Section 5.1 —
/// masking client frames stops cache-poisoning attacks against intermediary
/// proxies that can't tell WebSocket traffic from the HTTP it's tunneled
/// over) and `None` for server-to-client frames, which MUST NOT be masked.
fn encode_frame(opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + 14);
    let masked = mask.is_some();
    let len = payload.len();

    if len <= 125 {
        out.extend_from_slice(WsFrameHeader::new(true, opcode, masked, len as u8).as_bytes());
    } else if len <= 0xFFFF {
        out.extend_from_slice(WsFrameHeader::new(true, opcode, masked, 126).as_bytes());
        out.extend_from_slice(
            WsExtLen16 {
                len: BeU16::from_bytes((len as u16).to_be_bytes()),
            }
            .as_bytes(),
        );
    } else {
        out.extend_from_slice(WsFrameHeader::new(true, opcode, masked, 127).as_bytes());
        out.extend_from_slice(
            WsExtLen64 {
                len: BeU64::from_bytes((len as u64).to_be_bytes()),
            }
            .as_bytes(),
        );
    }

    match mask {
        Some(key) => {
            out.extend_from_slice(WsMaskKey { key }.as_bytes());
            out.extend(payload.iter().enumerate().map(|(i, b)| b ^ key[i % 4]));
        }
        None => out.extend_from_slice(payload),
    }
    out
}

fn encode_masked_binary_frame(payload: &[u8]) -> Vec<u8> {
    let key: [u8; 4] = pseudo_random_bytes(4).try_into().expect("4 bytes");
    encode_frame(OPCODE_BINARY, payload, Some(key))
}

/// One decoded WebSocket frame's header, plus how many bytes of `buffer` it
/// occupies before the payload starts.
struct FrameHeader {
    fin: bool,
    opcode: u8,
    masked: bool,
    mask_key: [u8; 4],
    payload_len: usize,
    header_len: usize,
}

/// Parses a frame header from the front of `buffer` if enough bytes have
/// arrived to do so (fixed header, then whichever extended-length and/or
/// mask-key struct applies). Does not require the payload itself to be
/// present yet — a short read on any layer just yields `None`, and the
/// caller retries once more bytes have arrived.
fn parse_frame_header(buffer: &[u8]) -> Option<FrameHeader> {
    let (header, rest) = WsFrameHeader::parse(buffer)?;
    let mut header_len = size_of::<WsFrameHeader>();

    let (payload_len, rest): (usize, &[u8]) = match header.len7() {
        126 => {
            let (ext, rest) = WsExtLen16::parse(rest)?;
            header_len += size_of::<WsExtLen16>();
            (ext.len.get() as usize, rest)
        }
        127 => {
            let (ext, rest) = WsExtLen64::parse(rest)?;
            header_len += size_of::<WsExtLen64>();
            (ext.len.get() as usize, rest)
        }
        n => (n as usize, rest),
    };

    let mask_key = if header.is_masked() {
        let (mask, _rest) = WsMaskKey::parse(rest)?;
        header_len += size_of::<WsMaskKey>();
        mask.key
    } else {
        [0u8; 4]
    };

    Some(FrameHeader {
        fin: header.fin(),
        opcode: header.opcode(),
        masked: header.is_masked(),
        mask_key,
        payload_len,
        header_len,
    })
}

/// One fully-decoded frame, with the mask (if any) already undone.
struct DecodedFrame {
    fin: bool,
    opcode: u8,
    payload: Vec<u8>,
}

/// Incrementally reassembles complete WebSocket frames from an arbitrarily
/// chunked byte stream, the WS-frame analogue of `rootcanal::H4FrameReader`.
/// Unlike H4 reassembly, the frame header always states the exact payload
/// length up front, so there's no packet-type-specific length table needed.
#[derive(Debug, Default)]
struct WsFrameReader {
    buffer: Vec<u8>,
    /// Read cursor: bytes before it belong to already-returned frames.
    /// Compacted away in [`feed`](Self::feed) rather than per frame, so
    /// popping a burst of frames stays O(n) instead of O(n^2).
    offset: usize,
}

impl WsFrameReader {
    fn feed(&mut self, bytes: &[u8]) {
        if self.offset > 0 {
            self.buffer.drain(..self.offset);
            self.offset = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    fn next_frame(&mut self) -> Option<DecodedFrame> {
        let buffer = &self.buffer[self.offset..];
        let header = parse_frame_header(buffer)?;
        let total_len = header.header_len + header.payload_len;
        if buffer.len() < total_len {
            return None;
        }
        let mut payload = buffer[header.header_len..total_len].to_vec();
        if header.masked {
            for (i, b) in payload.iter_mut().enumerate() {
                *b ^= header.mask_key[i % 4];
            }
        }
        self.offset += total_len;
        if self.offset == self.buffer.len() {
            self.buffer.clear();
            self.offset = 0;
        }
        Some(DecodedFrame {
            fin: header.fin,
            opcode: header.opcode,
            payload,
        })
    }
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

fn mask_key() -> [u8; 4] {
    pseudo_random_bytes(4).try_into().expect("4 bytes")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::h4_type;
    use std::io::Cursor;

    #[test]
    fn test_sha1_matches_known_vector() {
        // "abc" -> a9993e364706816aba3e25717850c26c9cd0d89d (FIPS 180-4 Appendix A.1)
        let digest = sha1(b"abc");
        assert_eq!(
            digest,
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
    fn test_parse_ws_url() {
        let url = parse_ws_url("ws://localhost:9922/hci").unwrap();
        assert_eq!(url.host, "localhost");
        assert_eq!(url.port, 9922);
        assert_eq!(url.path, "/hci");

        let default_port = parse_ws_url("ws://example.com/hci").unwrap();
        assert_eq!(default_port.port, 80);

        let root_path = parse_ws_url("ws://example.com:1234").unwrap();
        assert_eq!(root_path.path, "/");

        assert!(parse_ws_url("http://example.com/hci").is_err());
    }

    #[test]
    fn test_build_handshake_request_contains_required_headers() {
        let req = build_handshake_request("localhost", 9922, "/hci", "dGVzdGtleQ==");
        assert!(req.starts_with("GET /hci HTTP/1.1\r\n"));
        assert!(req.contains("Host: localhost:9922\r\n"));
        assert!(req.contains("Upgrade: websocket\r\n"));
        assert!(req.contains("Connection: Upgrade\r\n"));
        assert!(req.contains("Sec-WebSocket-Key: dGVzdGtleQ==\r\n"));
        assert!(req.contains("Sec-WebSocket-Version: 13\r\n"));
        assert!(req.ends_with("\r\n\r\n"));
    }

    #[test]
    fn test_validate_handshake_response_accepts_correct_accept() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {}\r\n\r\n",
            expected_accept(key)
        );
        assert!(validate_handshake_response(&response, key).is_ok());
    }

    #[test]
    fn test_validate_handshake_response_rejects_wrong_accept() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        let response = "HTTP/1.1 101 Switching Protocols\r\n\
             Sec-WebSocket-Accept: not-the-right-value\r\n\r\n";
        let err = validate_handshake_response(response, key).unwrap_err();
        assert!(matches!(err, SimbleError::Transport(_)));
    }

    #[test]
    fn test_validate_handshake_response_rejects_missing_accept_header() {
        let response = "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\n\r\n";
        assert!(validate_handshake_response(response, "anykey==").is_err());
    }

    #[test]
    fn test_validate_handshake_response_rejects_non_101_status() {
        let response = "HTTP/1.1 404 Not Found\r\n\r\n";
        assert!(validate_handshake_response(response, "anykey==").is_err());
    }

    #[test]
    fn test_read_http_headers_stops_at_blank_line() {
        let mut cursor = Cursor::new(b"HTTP/1.1 101 Switching Protocols\r\n\r\nEXTRA".to_vec());
        let headers = read_http_headers(&mut cursor).unwrap();
        assert_eq!(headers, "HTTP/1.1 101 Switching Protocols\r\n\r\n");
    }

    #[test]
    fn test_encode_frame_short_length_is_masked_and_round_trips() {
        let payload = vec![h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00];
        let frame = encode_masked_binary_frame(&payload);

        assert_eq!(frame[0], 0x80 | OPCODE_BINARY);
        assert_eq!(frame[1] & 0x80, 0x80, "client frame must be masked");
        assert_eq!(frame[1] & 0x7F, payload.len() as u8);

        let mut reader = WsFrameReader::default();
        reader.feed(&frame);
        let decoded = reader.next_frame().unwrap();
        assert!(decoded.fin);
        assert_eq!(decoded.opcode, OPCODE_BINARY);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_encode_frame_16_bit_extended_length() {
        let payload = vec![0xAB; 200];
        let frame = encode_frame(OPCODE_BINARY, &payload, None);
        assert_eq!(frame[1] & 0x7F, 126);
        let len = u16::from_be_bytes([frame[2], frame[3]]) as usize;
        assert_eq!(len, 200);

        let mut reader = WsFrameReader::default();
        reader.feed(&frame);
        let decoded = reader.next_frame().unwrap();
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_encode_frame_64_bit_extended_length() {
        let payload = vec![0xCD; 70_000];
        let frame = encode_frame(OPCODE_BINARY, &payload, None);
        assert_eq!(frame[1] & 0x7F, 127);
        let mut len_bytes = [0u8; 8];
        len_bytes.copy_from_slice(&frame[2..10]);
        assert_eq!(u64::from_be_bytes(len_bytes), 70_000);

        let mut reader = WsFrameReader::default();
        reader.feed(&frame);
        let decoded = reader.next_frame().unwrap();
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_frame_reader_handles_message_split_across_multiple_reads() {
        let payload = vec![h4_type::HCI_EVENT, 0x03, 0x02, 0xAA, 0xBB];
        let frame = encode_frame(OPCODE_BINARY, &payload, None);

        let mut reader = WsFrameReader::default();
        for chunk in frame.chunks(3) {
            reader.feed(chunk);
            if reader.buffer.len() < frame.len() {
                assert!(reader.next_frame().is_none());
            }
        }
        let decoded = reader.next_frame().unwrap();
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_masking_round_trip_unmasks_correctly() {
        let payload = vec![1, 2, 3, 4, 5, 6, 7];
        let key = [0xDE, 0xAD, 0xBE, 0xEF];
        let frame = encode_frame(OPCODE_BINARY, &payload, Some(key));

        let mut reader = WsFrameReader::default();
        reader.feed(&frame);
        let decoded = reader.next_frame().unwrap();
        assert_eq!(decoded.payload, payload);
    }

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
        let evt_from_controller = [h4_type::HCI_EVENT, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00];
        let inbound_frame = encode_frame(OPCODE_BINARY, &evt_from_controller, None);
        let stream = DuplexMock {
            inbound: Cursor::new(inbound_frame),
            outbound: Vec::new(),
        };
        let mut transport = NetsimTransport::new(stream);

        let channel = super::super::HciChannel::new();
        channel.send_command(&[0x03, 0x0C, 0x00]).unwrap();

        transport.pump(&channel).unwrap();

        let mut reader = WsFrameReader::default();
        reader.feed(&transport.stream.outbound);
        let sent = reader.next_frame().unwrap();
        assert_eq!(sent.opcode, OPCODE_BINARY);
        assert_eq!(sent.payload, vec![h4_type::HCI_COMMAND, 0x03, 0x0C, 0x00]);

        assert_eq!(
            channel.poll_controller_packet().unwrap(),
            evt_from_controller
        );
    }

    #[test]
    fn test_pump_replies_to_ping_with_pong() {
        let ping_payload = b"keepalive".to_vec();
        let ping_frame = encode_frame(OPCODE_PING, &ping_payload, None);
        let stream = DuplexMock {
            inbound: Cursor::new(ping_frame),
            outbound: Vec::new(),
        };
        let mut transport = NetsimTransport::new(stream);
        let channel = super::super::HciChannel::new();

        transport.pump(&channel).unwrap();

        let mut reader = WsFrameReader::default();
        reader.feed(&transport.stream.outbound);
        let pong = reader.next_frame().unwrap();
        assert_eq!(pong.opcode, OPCODE_PONG);
        assert_eq!(pong.payload, ping_payload);
        assert!(channel.poll_controller_packet().is_none());
    }

    #[test]
    fn test_pump_errors_on_close_frame() {
        let close_frame = encode_frame(OPCODE_CLOSE, &[], None);
        let stream = DuplexMock {
            inbound: Cursor::new(close_frame),
            outbound: Vec::new(),
        };
        let mut transport = NetsimTransport::new(stream);
        let channel = super::super::HciChannel::new();
        assert!(transport.pump(&channel).is_err());
    }
}
