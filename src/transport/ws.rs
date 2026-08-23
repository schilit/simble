// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Shared, hand-rolled RFC 6455 WebSocket pieces used by the netsim *client*
//! ([`super::netsim`]), the `usb-ble-ws` *server* ([`WsServerConn::pump`]), and
//! the MCP server's `--ws-server` transport ([`WsServerConn::poll_messages`]).
//!
//! The HCI users carry the same payload as the raw-TCP `rootcanal` transport —
//! one complete H4-framed HCI packet per WebSocket message — so WebSocket's own
//! message framing (Section 5) gives packet boundaries for free. MCP puts
//! JSON-RPC in text frames instead; the framing is the same either way. Only
//! the minimum is implemented: the opening handshake (with a small SHA-1 and
//! base64 for `Sec-WebSocket-Accept`), binary and text data frames, and
//! Ping/Pong. No `wss://` (TLS) — these are always local connections.
//!
//! The one asymmetry RFC 6455 imposes is masking: client-to-server frames
//! MUST be masked, server-to-client frames MUST NOT be (Section 5.1). The
//! frame *reader* handles either, so the only role-specific choice is whether
//! [`encode_frame`] is called with a mask.

use crate::types::SimbleError;
use std::io::Read;
use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned,
    byteorder::big_endian::{U16 as BeU16, U64 as BeU64},
};

/// Fixed by RFC 6455 Section 1.3: concatenated with the peer's
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

pub(crate) fn base64_encode(data: &[u8]) -> String {
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
pub(crate) fn pseudo_random_bytes(n: usize) -> Vec<u8> {
    let mut out = vec![0u8; n];
    crate::types::rng::fill_pseudo_random(&n as *const usize as u64, &mut out);
    out
}

pub(crate) fn mask_key() -> [u8; 4] {
    pseudo_random_bytes(4).try_into().expect("4 bytes")
}

/// The `Sec-WebSocket-Accept` value a server returns for a client's
/// `Sec-WebSocket-Key` (RFC 6455 Section 4.2.2), and what a client checks the
/// response against.
pub(crate) fn expected_accept(key: &str) -> String {
    let mut input = key.as_bytes().to_vec();
    input.extend_from_slice(WS_GUID.as_bytes());
    base64_encode(&sha1(&input))
}

/// Reads a `\r\n\r\n`-terminated HTTP header block one byte at a time
/// (simplest correct approach — handshake messages are tiny and this only
/// runs once per connection). Used for both the client's response and the
/// server's request.
pub(crate) fn read_http_headers<R: Read>(reader: &mut R) -> Result<String, SimbleError> {
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
                .map_err(|e| SimbleError::Transport(format!("non-UTF8 handshake header: {e}")));
        }
        if buf.len() > 16 * 1024 {
            return Err(SimbleError::Transport(
                "handshake header too large".to_string(),
            ));
        }
    }
}

/// Finds a header's value in a raw `\r\n`-delimited header block,
/// case-insensitively by name (RFC 7230 makes header names case-insensitive).
fn header_value<'a>(headers: &'a str, name: &str) -> Option<&'a str> {
    headers.split("\r\n").find_map(|line| {
        let (n, v) = line.split_once(':')?;
        n.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

// --- RFC 6455 Section 5: WebSocket data framing. ---

pub(crate) const OPCODE_CONTINUATION: u8 = 0x0;
/// UTF-8 text messages (Section 5.6). The HCI transports never use it — an H4
/// packet is binary — but the MCP-over-WebSocket server does: JSON-RPC is
/// text, and browser `WebSocket` clients send strings as text frames.
pub(crate) const OPCODE_TEXT: u8 = 0x1;
pub(crate) const OPCODE_BINARY: u8 = 0x2;
pub(crate) const OPCODE_CLOSE: u8 = 0x8;
pub(crate) const OPCODE_PING: u8 = 0x9;
pub(crate) const OPCODE_PONG: u8 = 0xA;

/// 2-byte fixed WebSocket frame header (RFC 6455 Section 5.2): FIN/RSV/opcode
/// in the first byte, MASK bit + 7-bit length (or length-class selector) in
/// the second. `len7() == 126` or `127` means the real length instead lives
/// in a following [`WsExtLen16`]/[`WsExtLen64`], mirroring how `HciAclHeader`
/// in `l2cap_frame.rs` keeps packed bit fields behind accessor methods.
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
/// masking stops cache-poisoning attacks against intermediary proxies that
/// can't tell WebSocket traffic from the HTTP it's tunneled over) and `None`
/// for server-to-client frames, which MUST NOT be masked.
pub(crate) fn encode_frame(opcode: u8, payload: &[u8], mask: Option<[u8; 4]>) -> Vec<u8> {
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

/// A single masked binary frame — the client-to-server form used by the
/// netsim transport.
pub(crate) fn encode_masked_binary_frame(payload: &[u8]) -> Vec<u8> {
    encode_frame(OPCODE_BINARY, payload, Some(mask_key()))
}

/// One decoded WebSocket frame's header, plus how many bytes of the buffer it
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
/// arrived (fixed header, then whichever extended-length and/or mask-key
/// struct applies). A short read on any layer yields `None`, and the caller
/// retries once more bytes have arrived.
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
pub(crate) struct DecodedFrame {
    pub(crate) fin: bool,
    pub(crate) opcode: u8,
    pub(crate) payload: Vec<u8>,
}

/// Incrementally reassembles complete WebSocket frames from an arbitrarily
/// chunked byte stream, the WS-frame analogue of `rootcanal::H4FrameReader`.
/// The frame header always states the exact payload length up front, so no
/// packet-type-specific length table is needed. Unmasks masked frames.
#[derive(Debug, Default)]
pub(crate) struct WsFrameReader {
    buffer: Vec<u8>,
    /// Read cursor: bytes before it belong to already-returned frames.
    /// Compacted in [`feed`](Self::feed) rather than per frame, so popping a
    /// burst of frames stays O(n) instead of O(n^2).
    offset: usize,
}

impl WsFrameReader {
    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        if self.offset > 0 {
            self.buffer.drain(..self.offset);
            self.offset = 0;
        }
        self.buffer.extend_from_slice(bytes);
    }

    pub(crate) fn next_frame(&mut self) -> Option<DecodedFrame> {
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

// --- Server side (the `usb-ble-ws` bridge). ---

use std::io::Write;
use std::net::TcpStream;

/// Completes the RFC 6455 server handshake on `stream`: reads the client's
/// HTTP upgrade request, echoes the derived `Sec-WebSocket-Accept`, and
/// returns the request's query string (netsim puts `name`/`address` there;
/// the bridge only logs it, since the real dongle has its own identity).
pub(crate) fn server_handshake<S: Read + Write>(stream: &mut S) -> Result<String, SimbleError> {
    let request = read_http_headers(stream)?;
    let request_line = request
        .split("\r\n")
        .next()
        .ok_or_else(|| SimbleError::Transport("empty handshake request".to_string()))?;
    // "GET /path?query HTTP/1.1" -> the target between the verb and version.
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");

    let key = header_value(&request, "Sec-WebSocket-Key").ok_or_else(|| {
        SimbleError::Transport("handshake request missing Sec-WebSocket-Key".to_string())
    })?;
    let response = format!(
        "HTTP/1.1 101 Switching Protocols\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Accept: {}\r\n\
         \r\n",
        expected_accept(key)
    );
    stream
        .write_all(response.as_bytes())
        .map_err(|e| SimbleError::Transport(e.to_string()))?;
    Ok(query.to_string())
}

/// The server end of one accepted WebSocket connection.
///
/// Two layers sit here. The **message** layer — `poll_messages` /
/// `send_text` — is protocol-agnostic: complete application messages in and out, with Ping
/// answered and fragmentation reassembled. The **HCI** layer,
/// [`pump`](Self::pump), is that message layer bound to an
/// [`HciChannel`](super::HciChannel), where the *WebSocket peer is the host*:
/// incoming binary messages are complete host→controller H4 packets, and
/// controller→host packets go back as **unmasked** binary frames (Section
/// 5.1). Pair `pump` with a [`UsbTransport`](super::UsbTransport) on the
/// *same* channel and the two carry HCI between a browser and a physical
/// dongle; `poll_messages`/`send_text` instead carry newline-free JSON-RPC
/// for `simble mcp --ws-server`.
pub struct WsServerConn<S: Read + Write> {
    stream: S,
    reader: WsFrameReader,
    /// Accumulates a message's payload across `Continuation` frames — not
    /// expected in practice (H4 packets are single unfragmented binary frames)
    /// but handled for spec fidelity.
    fragment: Vec<u8>,
    fragment_opcode: Option<u8>,
    /// Why the connection is finished (peer Close, EOF, or a read error),
    /// recorded rather than returned immediately so messages decoded in the
    /// same batch are still delivered first. Once set, every later
    /// `poll_messages` returns it.
    closed: Option<String>,
}

impl WsServerConn<TcpStream> {
    /// Accepts a freshly-connected TCP client: completes the WebSocket server
    /// handshake, switches the socket to non-blocking so [`pump`](Self::pump)
    /// never stalls, and returns the connection plus its request query string.
    pub fn accept(mut stream: TcpStream) -> Result<(Self, String), SimbleError> {
        let query = server_handshake(&mut stream)?;
        stream
            .set_nonblocking(true)
            .map_err(|e| SimbleError::Transport(e.to_string()))?;
        Ok((Self::new(stream), query))
    }
}

impl<S: Read + Write> WsServerConn<S> {
    /// Wraps an already-handshaken WebSocket stream as a server connection.
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            reader: WsFrameReader::default(),
            fragment: Vec::new(),
            fragment_opcode: None,
            closed: None,
        }
    }

    fn handle_frame(&mut self, frame: DecodedFrame) -> Result<Option<Vec<u8>>, SimbleError> {
        match frame.opcode {
            OPCODE_BINARY | OPCODE_TEXT | OPCODE_CONTINUATION => {
                if frame.opcode != OPCODE_CONTINUATION {
                    self.fragment_opcode = Some(frame.opcode);
                    self.fragment.clear();
                }
                self.fragment.extend_from_slice(&frame.payload);
                if frame.fin && self.fragment_opcode.is_some() {
                    self.fragment_opcode = None;
                    return Ok(Some(std::mem::take(&mut self.fragment)));
                }
                Ok(None)
            }
            // Server-to-client frames MUST NOT be masked, so the Pong is unmasked.
            OPCODE_PING => {
                let pong = encode_frame(OPCODE_PONG, &frame.payload, None);
                self.stream
                    .write_all(&pong)
                    .map_err(|e| SimbleError::Transport(e.to_string()))?;
                Ok(None)
            }
            OPCODE_PONG => Ok(None),
            OPCODE_CLOSE => Err(SimbleError::Transport(
                "WebSocket peer closed the connection".to_string(),
            )),
            other => Err(SimbleError::Transport(format!(
                "unsupported WebSocket opcode {other:#x}"
            ))),
        }
    }

    /// Sends one complete application message as a single unmasked frame
    /// (server-to-client frames MUST NOT be masked, Section 5.1).
    pub(crate) fn send_message(&mut self, opcode: u8, payload: &[u8]) -> Result<(), SimbleError> {
        let frame = encode_frame(opcode, payload, None);
        self.stream
            .write_all(&frame)
            .map_err(|e| SimbleError::Transport(e.to_string()))
    }

    /// Sends one complete text message (Section 5.6) — what a JSON-RPC
    /// response or notification travels in.
    pub(crate) fn send_text(&mut self, text: &str) -> Result<(), SimbleError> {
        self.send_message(OPCODE_TEXT, text.as_bytes())
    }

    /// Reads whatever bytes have arrived (never blocking on a non-blocking
    /// stream) and returns every complete application message they finished,
    /// auto-replying to Pings.
    ///
    /// A peer Close, an EOF, or a read error does **not** discard messages
    /// already decoded: the reason is recorded and returned from the *next*
    /// call, so a client that sends a request and immediately closes still
    /// gets that request handled. (Returning the error straight away is how
    /// the previous version silently dropped a final batch of packets.)
    pub(crate) fn poll_messages(&mut self) -> Result<Vec<Vec<u8>>, SimbleError> {
        if let Some(closed) = &self.closed {
            return Err(SimbleError::Transport(closed.clone()));
        }

        let mut chunk = [0u8; 4096];
        loop {
            match self.stream.read(&mut chunk) {
                Ok(0) => {
                    self.closed = Some("WebSocket connection closed".to_string());
                    break;
                }
                Ok(n) => self.reader.feed(&chunk[..n]),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => {
                    self.closed = Some(e.to_string());
                    break;
                }
            }
        }

        let mut messages = Vec::new();
        while let Some(frame) = self.reader.next_frame() {
            match self.handle_frame(frame) {
                Ok(Some(message)) => messages.push(message),
                Ok(None) => {}
                Err(e) => {
                    self.closed.get_or_insert_with(|| e.to_string());
                    break;
                }
            }
        }

        match (&self.closed, messages.is_empty()) {
            (Some(closed), true) => Err(SimbleError::Transport(closed.clone())),
            _ => Ok(messages),
        }
    }

    /// Moves packets in both directions between the WebSocket peer and
    /// `channel`: drains every controller→host packet the channel has queued
    /// and sends it as one unmasked binary frame, then injects each complete
    /// inbound message into the channel's host side as a ready-made H4 packet.
    pub fn pump(&mut self, channel: &super::HciChannel) -> Result<(), SimbleError> {
        while let Some(packet) = channel.poll_controller_packet() {
            self.send_message(OPCODE_BINARY, &packet)?;
        }
        for packet in self.poll_messages()? {
            channel.inject_host_packet(packet)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::HciChannel;
    use crate::transport::h4_type;
    use std::io::Cursor;

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
}
