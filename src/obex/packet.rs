// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! OBEX packets: requests, responses, and the fixed fields CONNECT and
//! SETPATH carry ahead of their headers (IrOBEX 1.3, Section 3.3).
//!
//! Every packet is `opcode, 16-bit big-endian length, [fixed fields],
//! headers…`, where the length counts the whole packet including the 3-byte
//! prefix. The high bit of the opcode is the **Final bit**: a request with
//! it clear says "more packets follow", which is what makes a multi-packet
//! PUT possible and what a naive implementation forgets.

use zerocopy::byteorder::big_endian::U16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

use super::header::{Header, HeaderError};

/// Request opcodes (IrOBEX 1.3, Section 3.4). Values here are the *base*
/// opcodes; [`FINAL_BIT`] is set separately.
pub mod opcode {
    /// Start a session.
    pub const CONNECT: u8 = 0x00;
    /// End a session.
    pub const DISCONNECT: u8 = 0x01;
    /// Send an object.
    pub const PUT: u8 = 0x02;
    /// Retrieve an object.
    pub const GET: u8 = 0x03;
    /// Change folder.
    pub const SETPATH: u8 = 0x05;
    /// Abandon the operation in progress.
    pub const ABORT: u8 = 0xFF;
}

/// Set on the final packet of a request or response (IrOBEX 1.3, Section
/// 3.1). CONNECT, DISCONNECT and ABORT always set it.
pub const FINAL_BIT: u8 = 0x80;

/// Response codes (IrOBEX 1.3, Section 3.2). These mirror HTTP status
/// codes shifted into a byte, which is why `Success` is 0xA0 (HTTP 200) and
/// `Continue` is 0x90 (HTTP 100).
pub mod response {
    /// Keep going — the server consumed this packet and wants the next.
    pub const CONTINUE: u8 = 0x90;
    /// The operation completed.
    pub const SUCCESS: u8 = 0xA0;
    /// Malformed request.
    pub const BAD_REQUEST: u8 = 0xC0;
    /// The server will not perform this operation.
    pub const FORBIDDEN: u8 = 0xC3;
    /// No object by that name.
    pub const NOT_FOUND: u8 = 0xC4;
    /// The opcode is not supported here.
    pub const NOT_IMPLEMENTED: u8 = 0xD1;
    /// The request would exceed a limit the server enforces.
    pub const ENTITY_TOO_LARGE: u8 = 0xCD;
    /// Catch-all failure.
    pub const INTERNAL_SERVER_ERROR: u8 = 0xD0;
    /// The service is unavailable (e.g. no session established).
    pub const SERVICE_UNAVAILABLE: u8 = 0xD3;
}

/// The 3-byte prefix every OBEX packet starts with.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct PacketPrefix {
    /// Opcode (requests) or response code, with the Final bit in bit 7.
    pub code: u8,
    /// Total packet length, this prefix included.
    pub length: U16,
}

/// The fixed fields CONNECT carries before its headers (IrOBEX 1.3,
/// Section 3.3.1). A CONNECT *response* uses the identical layout, which is
/// how both sides learn each other's maximum packet size.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct ConnectFields {
    /// OBEX version, encoded as major/minor nibbles: 0x10 is version 1.0.
    pub version: u8,
    /// Connect flags; bit 0 requests multiple simultaneous sessions.
    pub flags: u8,
    /// Largest packet this side will accept, headers included.
    pub max_packet_length: U16,
}

/// The fixed fields SETPATH carries before its headers (IrOBEX 1.3,
/// Section 3.3.6).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct SetPathFields {
    /// Bit 0: go to the parent first. Bit 1: do not create the folder.
    pub flags: u8,
    /// Reserved; senders write zero.
    pub constants: u8,
}

/// Why a packet could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacketError {
    /// The buffer is shorter than the packet's declared length, or too
    /// short to hold the fields its opcode requires.
    Truncated,
    /// The declared length is smaller than the 3-byte prefix it includes.
    BadLength(u16),
    /// A header inside the packet was malformed.
    Header(HeaderError),
}

impl From<HeaderError> for PacketError {
    fn from(error: HeaderError) -> Self {
        Self::Header(error)
    }
}

/// One parsed OBEX request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The base opcode, Final bit masked off.
    pub opcode: u8,
    /// Whether the Final bit was set.
    pub is_final: bool,
    /// CONNECT's fixed fields, when this is a CONNECT.
    pub connect: Option<ConnectFields>,
    /// SETPATH's fixed fields, when this is a SETPATH.
    pub set_path: Option<SetPathFields>,
    /// The headers that followed.
    pub headers: Vec<Header>,
}

/// One parsed OBEX response.
///
/// Unlike a request opcode, a response code is stored **whole**. The
/// conventional constants (`0xA0` Success, `0x90` Continue, `0xD1` Not
/// Implemented) already carry the high bit, so masking it off and re-setting
/// it would turn Success into 0x20 and compare unequal against every named
/// constant. `is_final` is derived from that same bit rather than removed
/// from the code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// The response code, high bit included (compare against [`response`]).
    pub code: u8,
    /// Whether the Final bit is set — the same bit, read rather than removed.
    pub is_final: bool,
    /// CONNECT's fixed fields, when this answers a CONNECT.
    pub connect: Option<ConnectFields>,
    /// The headers that followed.
    pub headers: Vec<Header>,
}

/// Splits the prefix off a packet and checks its declared length against the
/// buffer, returning `(code, is_final, body)`.
fn split_packet(bytes: &[u8]) -> Result<(u8, bool, &[u8]), PacketError> {
    let (prefix, rest) =
        PacketPrefix::ref_from_prefix(bytes).map_err(|_| PacketError::Truncated)?;
    let total = usize::from(prefix.length.get());
    if total < 3 {
        return Err(PacketError::BadLength(prefix.length.get()));
    }
    let body_len = total - 3;
    if rest.len() < body_len {
        return Err(PacketError::Truncated);
    }
    Ok((
        prefix.code & !FINAL_BIT,
        prefix.code & FINAL_BIT != 0,
        &rest[..body_len],
    ))
}

/// Builds a packet from a code, optional fixed fields, and headers.
fn build_packet(code: u8, fixed: &[u8], headers: &[Header]) -> Vec<u8> {
    let body = Header::encode_all(headers);
    let total = 3 + fixed.len() + body.len();
    let prefix = PacketPrefix {
        code,
        length: U16::new(total as u16),
    };
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(prefix.as_bytes());
    out.extend_from_slice(fixed);
    out.extend_from_slice(&body);
    out
}

impl Request {
    /// Parses one request packet.
    pub fn parse(bytes: &[u8]) -> Result<Self, PacketError> {
        let (opcode, is_final, body) = split_packet(bytes)?;
        // ABORT's opcode is 0xFF, so masking the Final bit leaves 0x7F;
        // recover it before dispatching on the opcode.
        let opcode = if bytes[0] == opcode::ABORT {
            opcode::ABORT
        } else {
            opcode
        };

        let (connect, set_path, header_bytes) = match opcode {
            opcode::CONNECT => {
                let (fields, rest) =
                    ConnectFields::ref_from_prefix(body).map_err(|_| PacketError::Truncated)?;
                (Some(*fields), None, rest)
            }
            opcode::SETPATH => {
                let (fields, rest) =
                    SetPathFields::ref_from_prefix(body).map_err(|_| PacketError::Truncated)?;
                (None, Some(*fields), rest)
            }
            _ => (None, None, body),
        };

        Ok(Self {
            opcode,
            is_final,
            connect,
            set_path,
            headers: Header::parse_all(header_bytes)?,
        })
    }

    /// Encodes this request.
    pub fn to_bytes(&self) -> Vec<u8> {
        let code = if self.is_final {
            self.opcode | FINAL_BIT
        } else {
            self.opcode
        };
        let mut fixed = Vec::new();
        if let Some(connect) = &self.connect {
            fixed.extend_from_slice(connect.as_bytes());
        }
        if let Some(set_path) = &self.set_path {
            fixed.extend_from_slice(set_path.as_bytes());
        }
        build_packet(code, &fixed, &self.headers)
    }

    /// A CONNECT request advertising `max_packet_length`.
    pub fn connect(max_packet_length: u16, headers: Vec<Header>) -> Self {
        Self {
            opcode: opcode::CONNECT,
            is_final: true,
            connect: Some(ConnectFields {
                version: 0x10,
                flags: 0x00,
                max_packet_length: U16::new(max_packet_length),
            }),
            set_path: None,
            headers,
        }
    }

    /// A DISCONNECT request.
    pub fn disconnect(headers: Vec<Header>) -> Self {
        Self::simple(opcode::DISCONNECT, true, headers)
    }

    /// A PUT request. `is_final` distinguishes the last packet of a
    /// multi-packet transfer from the ones before it.
    pub fn put(is_final: bool, headers: Vec<Header>) -> Self {
        Self::simple(opcode::PUT, is_final, headers)
    }

    /// A GET request.
    pub fn get(is_final: bool, headers: Vec<Header>) -> Self {
        Self::simple(opcode::GET, is_final, headers)
    }

    /// An ABORT request.
    pub fn abort() -> Self {
        Self::simple(opcode::ABORT, true, Vec::new())
    }

    fn simple(opcode: u8, is_final: bool, headers: Vec<Header>) -> Self {
        Self {
            opcode,
            is_final,
            connect: None,
            set_path: None,
            headers,
        }
    }

    /// Finds the first header matching `predicate`.
    pub fn header(&self, identifier: u8) -> Option<&Header> {
        self.headers.iter().find(|h| h.identifier() == identifier)
    }
}

impl Response {
    /// Parses one response packet. `to_connect` tells the parser whether
    /// this answers a CONNECT, since the fixed fields are otherwise
    /// indistinguishable from headers.
    pub fn parse(bytes: &[u8], to_connect: bool) -> Result<Self, PacketError> {
        let (_, is_final, body) = split_packet(bytes)?;
        // Keep the code whole; see the note on `Response::code`.
        let code = bytes[0];
        let (connect, header_bytes) = if to_connect && code == response::SUCCESS {
            let (fields, rest) =
                ConnectFields::ref_from_prefix(body).map_err(|_| PacketError::Truncated)?;
            (Some(*fields), rest)
        } else {
            (None, body)
        };
        Ok(Self {
            code,
            is_final,
            connect,
            headers: Header::parse_all(header_bytes)?,
        })
    }

    /// Encodes this response. The code is written as-is — it already
    /// carries its high bit.
    pub fn to_bytes(&self) -> Vec<u8> {
        let code = self.code;
        let mut fixed = Vec::new();
        if let Some(connect) = &self.connect {
            fixed.extend_from_slice(connect.as_bytes());
        }
        build_packet(code, &fixed, &self.headers)
    }

    /// A plain response carrying `code` and no headers.
    pub fn status(code: u8) -> Self {
        Self {
            code,
            is_final: code & FINAL_BIT != 0,
            connect: None,
            headers: Vec::new(),
        }
    }

    /// A `Continue`, asking the peer for the next packet of a transfer.
    pub fn cont() -> Self {
        Self::status(response::CONTINUE)
    }

    /// A `Success` carrying `headers`.
    pub fn success(headers: Vec<Header>) -> Self {
        Self {
            code: response::SUCCESS,
            is_final: true,
            connect: None,
            headers,
        }
    }

    /// A CONNECT response advertising this side's `max_packet_length`.
    pub fn connect_success(max_packet_length: u16, headers: Vec<Header>) -> Self {
        Self {
            code: response::SUCCESS,
            is_final: true,
            connect: Some(ConnectFields {
                version: 0x10,
                flags: 0x00,
                max_packet_length: U16::new(max_packet_length),
            }),
            headers,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec-derived vector (IrOBEX 1.3, Section 3.3.1): a minimal CONNECT is
    /// seven bytes — opcode with the Final bit, a length of 7, version 1.0,
    /// no flags, and a 16-bit maximum packet size. This layout is the most
    /// widely reproduced OBEX byte sequence there is.
    ///
    /// No foreign implementation was available to check against: Bumble
    /// carries only OBEX UUID constants, not the protocol.
    #[test]
    fn test_connect_matches_the_spec_layout() {
        let request = Request::connect(0x2000, Vec::new());
        assert_eq!(
            request.to_bytes(),
            vec![0x80, 0x00, 0x07, 0x10, 0x00, 0x20, 0x00]
        );

        let parsed = Request::parse(&request.to_bytes()).unwrap();
        assert_eq!(parsed.opcode, opcode::CONNECT);
        assert!(parsed.is_final);
        let fields = parsed.connect.unwrap();
        assert_eq!(fields.version, 0x10);
        assert_eq!(fields.max_packet_length.get(), 0x2000);
    }

    #[test]
    fn test_connect_response_matches_the_spec_layout() {
        let response = Response::connect_success(0x2000, Vec::new());
        assert_eq!(
            response.to_bytes(),
            vec![0xA0, 0x00, 0x07, 0x10, 0x00, 0x20, 0x00]
        );
        let parsed = Response::parse(&response.to_bytes(), true).unwrap();
        assert_eq!(parsed.code, response::SUCCESS);
        assert_eq!(parsed.connect.unwrap().max_packet_length.get(), 0x2000);
    }

    /// The Final bit is what separates a mid-transfer PUT from the last
    /// one; losing it turns a multi-packet transfer into a truncated object.
    #[test]
    fn test_final_bit_distinguishes_put_from_put_final() {
        let mid = Request::put(false, vec![Header::Body(vec![1, 2])]);
        let last = Request::put(true, vec![Header::EndOfBody(vec![3])]);
        assert_eq!(mid.to_bytes()[0], 0x02, "PUT without the Final bit");
        assert_eq!(last.to_bytes()[0], 0x82, "PUT-Final");

        assert!(!Request::parse(&mid.to_bytes()).unwrap().is_final);
        assert!(Request::parse(&last.to_bytes()).unwrap().is_final);
    }

    /// ABORT is 0xFF, so the Final bit overlaps its opcode — masking it off
    /// naively yields 0x7F and the request is misrouted.
    #[test]
    fn test_abort_survives_final_bit_masking() {
        let parsed = Request::parse(&Request::abort().to_bytes()).unwrap();
        assert_eq!(parsed.opcode, opcode::ABORT);
        assert!(parsed.is_final);
    }

    #[test]
    fn test_setpath_fixed_fields_round_trip() {
        let request = Request {
            opcode: opcode::SETPATH,
            is_final: true,
            connect: None,
            set_path: Some(SetPathFields {
                flags: 0x01, // to parent
                constants: 0x00,
            }),
            headers: vec![Header::Name(String::new())],
        };
        let parsed = Request::parse(&request.to_bytes()).unwrap();
        assert_eq!(parsed.set_path.unwrap().flags, 0x01);
        assert_eq!(parsed.headers, vec![Header::Name(String::new())]);
    }

    #[test]
    fn test_truncated_and_malformed_packets_are_rejected() {
        assert_eq!(Request::parse(&[0x80, 0x00]), Err(PacketError::Truncated));
        // Declares 20 bytes, carries 4.
        assert_eq!(
            Request::parse(&[0x82, 0x00, 0x14, 0x01]),
            Err(PacketError::Truncated)
        );
        // A length below the 3-byte prefix it must cover.
        assert_eq!(
            Request::parse(&[0x80, 0x00, 0x01]),
            Err(PacketError::BadLength(1))
        );
        // CONNECT without room for its fixed fields.
        assert_eq!(
            Request::parse(&[0x80, 0x00, 0x05, 0x10, 0x00]),
            Err(PacketError::Truncated)
        );
        // A well-formed packet wrapping a malformed header.
        assert!(matches!(
            Request::parse(&[0x82, 0x00, 0x06, 0x01, 0x00, 0x02]),
            Err(PacketError::Header(_))
        ));
    }

    /// A header declaring a length that overruns the packet must not read
    /// into whatever follows in the buffer.
    #[test]
    fn test_a_header_cannot_read_past_its_packet() {
        let mut packet = vec![0x82, 0x00, 0x08, 0x01, 0x00, 0x0A, 0xAA, 0xBB];
        packet.extend_from_slice(b"trailing bytes that must stay unread");
        assert!(matches!(
            Request::parse(&packet),
            Err(PacketError::Header(HeaderError::Truncated))
        ));
    }
}
