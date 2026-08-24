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
    finish_server_handshake(stream, &request)
}

/// Completes a WebSocket handshake whose headers have already been read.
///
/// Split out because a caller may need to look at the request first: the same
/// port can answer a plain `GET` (a page asking what devices exist) and a
/// WebSocket upgrade (a page opening one), and by the time the key is missing
/// it is too late to tell those apart.
pub(crate) fn finish_server_handshake<S: Read + Write>(
    stream: &mut S,
    request: &str,
) -> Result<String, SimbleError> {
    let request_line = request
        .split("\r\n")
        .next()
        .ok_or_else(|| SimbleError::Transport("empty handshake request".to_string()))?;
    // "GET /path?query HTTP/1.1" -> the target between the verb and version.
    let target = request_line.split_whitespace().nth(1).unwrap_or("/");
    let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");

    let key = header_value(request, "Sec-WebSocket-Key").ok_or_else(|| {
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

/// What an inbound connection turned out to want.
///
/// One port answers both: a page asking *what devices exist* sends a plain
/// `GET`, and a page opening one sends a WebSocket upgrade. netsim works this
/// way — one port, one connection per device, each naming its device in the
/// URL query — and this mirrors it.
pub enum Inbound {
    /// A completed WebSocket handshake, with the request's query string.
    WebSocket(WsServerConn<TcpStream>, String),
    /// A plain HTTP request the caller should answer and close.
    Request {
        /// `GET`, `POST`, …
        method: String,
        /// The request target, query string included.
        target: String,
    },
}

/// Reads one inbound request and either completes a WebSocket handshake or
/// hands back the plain HTTP request for the caller to answer.
///
/// The two cannot be told apart after the fact: by the time a missing
/// `Sec-WebSocket-Key` is noticed, the headers are consumed and the reply is
/// already wrong. So the decision is made here, before committing.
pub fn accept_inbound(mut stream: TcpStream) -> Result<Inbound, SimbleError> {
    let request = read_http_headers(&mut stream)?;
    let line = request.split("\r\n").next().unwrap_or("");
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();
    if header_value(&request, "Sec-WebSocket-Key").is_none() {
        return Ok(Inbound::Request { method, target });
    }
    let query = finish_server_handshake(&mut stream, &request)?;
    Ok(Inbound::WebSocket(WsServerConn::new(stream), query))
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
#[path = "ws_tests.rs"]
mod tests;
