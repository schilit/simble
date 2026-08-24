// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The OBEX server: bytes in, bytes out.
//!
//! Deliberately transport-free. OBEX rides on RFCOMM for OPP, PBAP and MAP,
//! and on L2CAP for OBEX-over-L2CAP, but none of that is visible here — a
//! caller hands over one received packet and writes back whatever comes out.
//! That keeps this module mergeable with whatever wires RFCOMM to a device,
//! and testable without a radio.
//!
//! The interesting part is **continuation** (IrOBEX 1.3, Section 3.1): an
//! object larger than one packet arrives as a run of PUTs with the Final bit
//! clear, each answered `0x90 Continue`, ending with a PUT-Final answered
//! `0xA0 Success`. Getting that wrong is the classic OBEX bug — a server
//! that answers Success too early truncates every large object it receives.

use super::header::Header;
use super::packet::{PacketError, Request, Response, opcode, response};

/// An object a peer pushed to us, reassembled from however many packets it
/// arrived in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ReceivedObject {
    /// The `NAME` header, if the sender supplied one.
    pub name: Option<String>,
    /// The `TYPE` header (a MIME type), if supplied.
    pub mime_type: Option<Vec<u8>>,
    /// The `LENGTH` header the sender declared, if any. Advisory: the
    /// authoritative size is `body.len()` once the transfer completes.
    pub declared_length: Option<u32>,
    /// The reassembled object.
    pub body: Vec<u8>,
}

/// What the server decided to do with a request, alongside the bytes to send
/// back. Callers that only relay bytes can ignore this; a profile uses it to
/// notice a completed transfer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    /// A session was established.
    Connected,
    /// The session ended.
    Disconnected,
    /// A packet was consumed and more are expected.
    Continued,
    /// An object finished arriving.
    ObjectReceived(Box<ReceivedObject>),
    /// The peer abandoned the transfer in progress.
    Aborted,
    /// The request was rejected; the response carries the reason.
    Rejected(u8),
}

/// Whether a server requires a CONNECT before it will accept operations.
///
/// OPP explicitly permits a bare PUT with no session (Object Push Profile
/// 1.2, Section 4.3) — "push a vCard at a device you have never met" is the
/// whole point — whereas PBAP and MAP require a session because they carry a
/// Target header identifying which service is being addressed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPolicy {
    /// Operations are accepted with or without a preceding CONNECT.
    Optional,
    /// Operations before a successful CONNECT are refused.
    Required,
}

/// Limits a server enforces so a peer cannot exhaust memory by streaming an
/// unbounded object at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerLimits {
    /// Largest packet this server will accept, advertised in its CONNECT
    /// response.
    pub max_packet_length: u16,
    /// Largest object it will reassemble before answering
    /// `Entity Too Large`.
    pub max_object_bytes: usize,
}

impl Default for ServerLimits {
    fn default() -> Self {
        Self {
            // 0x2000 is the conventional OBEX maximum packet size and what
            // most stacks advertise.
            max_packet_length: 0x2000,
            max_object_bytes: 8 * 1024 * 1024,
        }
    }
}

/// A transport-agnostic OBEX server.
#[derive(Debug, Clone)]
pub struct ObexServer {
    limits: ServerLimits,
    policy: SessionPolicy,
    connected: bool,
    /// The transfer being reassembled, if a multi-packet PUT is in flight.
    in_progress: Option<ReceivedObject>,
    /// Objects that finished arriving and have not been collected.
    completed: Vec<ReceivedObject>,
    /// The peer's maximum packet length, learned from its CONNECT.
    peer_max_packet_length: Option<u16>,
}

impl Default for ObexServer {
    fn default() -> Self {
        Self::new(SessionPolicy::Optional, ServerLimits::default())
    }
}

impl ObexServer {
    /// Creates a server with the given session policy and limits.
    pub fn new(policy: SessionPolicy, limits: ServerLimits) -> Self {
        Self {
            limits,
            policy,
            connected: false,
            in_progress: None,
            completed: Vec::new(),
            peer_max_packet_length: None,
        }
    }

    /// Whether a session is currently established.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// The peer's advertised maximum packet length, once it has connected.
    pub fn peer_max_packet_length(&self) -> Option<u16> {
        self.peer_max_packet_length
    }

    /// Takes the objects that have finished arriving.
    pub fn take_objects(&mut self) -> Vec<ReceivedObject> {
        std::mem::take(&mut self.completed)
    }

    /// Handles one received packet, returning the response bytes to send and
    /// what the packet meant.
    ///
    /// A packet that cannot be parsed is answered `Bad Request` rather than
    /// returning an error: OBEX has no way to say "I could not read that"
    /// other than a response, and dropping the packet would hang the peer.
    pub fn handle_packet(&mut self, packet: &[u8]) -> (Vec<u8>, ServerEvent) {
        match Request::parse(packet) {
            Ok(request) => self.handle_request(request),
            Err(PacketError::Header(_) | PacketError::BadLength(_) | PacketError::Truncated) => (
                Response::status(response::BAD_REQUEST).to_bytes(),
                ServerEvent::Rejected(response::BAD_REQUEST),
            ),
        }
    }

    fn handle_request(&mut self, request: Request) -> (Vec<u8>, ServerEvent) {
        // Everything except CONNECT needs a session when the policy demands
        // one. ABORT is exempt so a peer can always tear down a transfer.
        if self.policy == SessionPolicy::Required
            && !self.connected
            && !matches!(request.opcode, opcode::CONNECT | opcode::ABORT)
        {
            return self.reject(response::SERVICE_UNAVAILABLE);
        }

        match request.opcode {
            opcode::CONNECT => self.on_connect(&request),
            opcode::DISCONNECT => {
                self.connected = false;
                self.in_progress = None;
                (
                    Response::success(Vec::new()).to_bytes(),
                    ServerEvent::Disconnected,
                )
            }
            opcode::PUT => self.on_put(request),
            opcode::ABORT => {
                self.in_progress = None;
                (
                    Response::success(Vec::new()).to_bytes(),
                    ServerEvent::Aborted,
                )
            }
            // GET and SETPATH are parsed and answered honestly rather than
            // silently: OPP needs neither, and a peer deserves to be told
            // so instead of waiting.
            _ => self.reject(response::NOT_IMPLEMENTED),
        }
    }

    fn on_connect(&mut self, request: &Request) -> (Vec<u8>, ServerEvent) {
        self.connected = true;
        self.in_progress = None;
        self.peer_max_packet_length = request
            .connect
            .as_ref()
            .map(|fields| fields.max_packet_length.get());
        (
            Response::connect_success(self.limits.max_packet_length, Vec::new()).to_bytes(),
            ServerEvent::Connected,
        )
    }

    fn on_put(&mut self, request: Request) -> (Vec<u8>, ServerEvent) {
        let mut object = self.in_progress.take().unwrap_or_default();

        // A PUT carrying no body headers at all is a delete request in OPP's
        // sibling profiles; here it simply contributes metadata.
        for header in request.headers {
            match header {
                Header::Name(name) => object.name = Some(name),
                Header::Type(mime) => object.mime_type = Some(mime),
                Header::Length(length) => object.declared_length = Some(length),
                Header::Body(chunk) | Header::EndOfBody(chunk) => {
                    if object.body.len() + chunk.len() > self.limits.max_object_bytes {
                        // Drop the partial object rather than holding it:
                        // the transfer is over and the peer is told why.
                        return self.reject(response::ENTITY_TOO_LARGE);
                    }
                    object.body.extend_from_slice(&chunk);
                }
                _ => {}
            }
        }

        if request.is_final {
            let event = ServerEvent::ObjectReceived(Box::new(object.clone()));
            self.completed.push(object);
            (Response::success(Vec::new()).to_bytes(), event)
        } else {
            // Continue: consumed, send the next one. Answering Success here
            // is the bug this whole state machine exists to avoid.
            self.in_progress = Some(object);
            (Response::cont().to_bytes(), ServerEvent::Continued)
        }
    }

    fn reject(&mut self, code: u8) -> (Vec<u8>, ServerEvent) {
        self.in_progress = None;
        (
            Response::status(code).to_bytes(),
            ServerEvent::Rejected(code),
        )
    }
}

/// Splits `body` into the PUT packets needed to carry it, given the peer's
/// maximum packet length.
///
/// The last packet uses `END_OF_BODY` and sets the Final bit; the rest use
/// `BODY` and clear it. Exposed because both the client and tests need to
/// produce a correctly chunked transfer, and because getting the per-packet
/// overhead right (3-byte packet prefix plus a 3-byte header prefix) is
/// exactly the arithmetic that goes wrong by hand.
pub fn put_packets(
    name: Option<&str>,
    mime_type: Option<&[u8]>,
    body: &[u8],
    max_packet_length: u16,
) -> Vec<Vec<u8>> {
    let mut leading = Vec::new();
    if let Some(name) = name {
        leading.push(Header::Name(name.to_string()));
    }
    if let Some(mime) = mime_type {
        leading.push(Header::Type(mime.to_vec()));
    }
    leading.push(Header::Length(body.len() as u32));

    let leading_len: usize = leading.iter().map(Header::encoded_len).sum();
    let max = usize::from(max_packet_length);
    // Room left in the first packet after the packet prefix, the metadata
    // headers, and the body header's own 3-byte prefix.
    let first_capacity = max.saturating_sub(3 + leading_len + 3);
    let later_capacity = max.saturating_sub(3 + 3);
    if first_capacity == 0 || later_capacity == 0 {
        // A peer advertising a packet size this small cannot carry a
        // transfer; send the metadata and let it answer.
        return vec![Request::put(true, leading).to_bytes()];
    }

    if body.len() <= first_capacity {
        leading.push(Header::EndOfBody(body.to_vec()));
        return vec![Request::put(true, leading).to_bytes()];
    }

    let mut packets = Vec::new();
    let (first, mut rest) = body.split_at(first_capacity);
    leading.push(Header::Body(first.to_vec()));
    packets.push(Request::put(false, leading).to_bytes());

    while rest.len() > later_capacity {
        let (chunk, remainder) = rest.split_at(later_capacity);
        packets.push(Request::put(false, vec![Header::Body(chunk.to_vec())]).to_bytes());
        rest = remainder;
    }
    packets.push(Request::put(true, vec![Header::EndOfBody(rest.to_vec())]).to_bytes());
    packets
}

/// Finds a header by identifier in a parsed request.
pub fn find_header(headers: &[Header], identifier: u8) -> Option<&Header> {
    headers.iter().find(|h| h.identifier() == identifier)
}

#[cfg(test)]
#[path = "server_tests.rs"]
mod tests;
