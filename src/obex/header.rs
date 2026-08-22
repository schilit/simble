// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! OBEX headers (IrOBEX 1.3, Section 2.1).
//!
//! A header is an identifier byte followed by a value whose *encoding is
//! carried in the identifier's top two bits* — so the parser never needs a
//! table of known headers to walk a packet, and an unrecognised header can
//! be skipped rather than aborting the transfer. That property is why
//! `Header::Other` exists instead of an error case.
//!
//! Note the byte order: OBEX predates Bluetooth and uses **network byte
//! order (big-endian)** for its length and 4-byte value fields, unlike
//! nearly everything else in this crate.

use zerocopy::byteorder::big_endian::{U16, U32};
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned};

/// Header identifiers (IrOBEX 1.3, Section 2.2). The low six bits name the
/// header; the top two give its encoding, so the same name can appear with
/// different encodings across versions.
pub mod header_id {
    /// Number of objects (4-byte).
    pub const COUNT: u8 = 0xC0;
    /// Object name, e.g. a file name (unicode).
    pub const NAME: u8 = 0x01;
    /// MIME type (byte sequence).
    pub const TYPE: u8 = 0x42;
    /// Object length in bytes (4-byte).
    pub const LENGTH: u8 = 0xC3;
    /// A chunk of the object (byte sequence).
    pub const BODY: u8 = 0x48;
    /// The final chunk of the object (byte sequence).
    pub const END_OF_BODY: u8 = 0x49;
    /// Service identifier for the conversation (byte sequence).
    pub const TARGET: u8 = 0x46;
    /// Answers `TARGET` (byte sequence).
    pub const WHO: u8 = 0x4A;
    /// Identifies the OBEX connection (4-byte).
    pub const CONNECTION_ID: u8 = 0xCB;
    /// Profile-defined parameters, tag-length-value inside (byte sequence).
    pub const APP_PARAMETERS: u8 = 0x4C;
    /// Authentication digest challenge (byte sequence).
    pub const AUTH_CHALLENGE: u8 = 0x4D;
    /// Authentication digest response (byte sequence).
    pub const AUTH_RESPONSE: u8 = 0x4E;
    /// Single Response Mode (1-byte).
    pub const SINGLE_RESPONSE_MODE: u8 = 0x97;
    /// Single Response Mode parameters (1-byte).
    pub const SRM_PARAMETERS: u8 = 0x98;
    /// Object class (byte sequence).
    pub const OBJECT_CLASS: u8 = 0x51;
    /// Descriptive text (unicode).
    pub const DESCRIPTION: u8 = 0x05;
}

/// The encoding named by a header identifier's top two bits (IrOBEX 1.3,
/// Section 2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeaderEncoding {
    /// Null-terminated UTF-16BE text, preceded by a 2-byte length.
    Unicode,
    /// Raw bytes, preceded by a 2-byte length.
    ByteSequence,
    /// A single byte, with no length prefix.
    Byte,
    /// Four big-endian bytes, with no length prefix.
    FourByte,
}

impl HeaderEncoding {
    /// Reads the encoding out of a header identifier.
    pub fn of(identifier: u8) -> Self {
        match identifier & 0xC0 {
            0x00 => Self::Unicode,
            0x40 => Self::ByteSequence,
            0x80 => Self::Byte,
            _ => Self::FourByte,
        }
    }
}

/// The 3-byte prefix on a length-prefixed header: its identifier and the
/// total length *including this prefix*.
///
/// Counting the prefix in the length is the classic off-by-three in OBEX
/// implementations; keeping it in a typed view means the arithmetic is
/// written once.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct HeaderPrefix {
    /// Header identifier.
    pub identifier: u8,
    /// Total header length in bytes, this 3-byte prefix included.
    pub length: U16,
}

/// A four-byte header's value, big-endian.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct FourByteValue {
    /// The value.
    pub value: U32,
}

/// One OBEX header.
///
/// The named variants are the ones simble acts on; anything else round-trips
/// through [`Header::Other`] so an unknown header is preserved rather than
/// dropped or fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Header {
    /// Object name (`NAME`).
    Name(String),
    /// MIME type (`TYPE`), carried as bytes because it is conventionally
    /// null-terminated ASCII and callers differ on whether to keep the null.
    Type(Vec<u8>),
    /// Total object length (`LENGTH`).
    Length(u32),
    /// A non-final chunk of the object (`BODY`).
    Body(Vec<u8>),
    /// The final chunk (`END_OF_BODY`).
    EndOfBody(Vec<u8>),
    /// Service identifier (`TARGET`).
    Target(Vec<u8>),
    /// Response to a target (`WHO`).
    Who(Vec<u8>),
    /// Connection identifier (`CONNECTION_ID`).
    ConnectionId(u32),
    /// Profile-specific parameters (`APP_PARAMETERS`).
    AppParameters(Vec<u8>),
    /// Authentication challenge (`AUTH_CHALLENGE`).
    AuthChallenge(Vec<u8>),
    /// Authentication response (`AUTH_RESPONSE`).
    AuthResponse(Vec<u8>),
    /// Single Response Mode (`SINGLE_RESPONSE_MODE`).
    SingleResponseMode(u8),
    /// Number of objects (`COUNT`).
    Count(u32),
    /// Human-readable description (`DESCRIPTION`).
    Description(String),
    /// Any header this build does not model, kept verbatim.
    Other {
        /// The header identifier.
        identifier: u8,
        /// The value, decoded according to the identifier's encoding.
        value: HeaderValue,
    },
}

/// A header value in whichever shape its encoding implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderValue {
    /// Decoded UTF-16BE text.
    Text(String),
    /// Raw bytes.
    Bytes(Vec<u8>),
    /// A single byte.
    Byte(u8),
    /// A four-byte integer.
    FourByte(u32),
}

/// Why a header could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HeaderError {
    /// The buffer ended mid-header.
    Truncated,
    /// A length-prefixed header declared a length smaller than its own
    /// 3-byte prefix, or one that runs past the end of the packet.
    BadLength(u16),
}

/// Encodes a Rust string as the null-terminated UTF-16BE OBEX uses.
///
/// An empty string encodes as an empty value, *not* as a lone null: IrOBEX
/// 1.3 Section 2.2.2 distinguishes "no name" (used by SETPATH to mean the
/// parent folder) from a name that happens to be empty.
fn encode_unicode(text: &str) -> Vec<u8> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    bytes.extend_from_slice(&[0x00, 0x00]); // null terminator
    bytes
}

/// Decodes null-terminated UTF-16BE. A trailing null and an odd trailing
/// byte are both tolerated — peers differ, and refusing a transfer over a
/// stray byte helps nobody.
fn decode_unicode(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_be_bytes(*pair))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

impl Header {
    /// The header's identifier byte.
    pub fn identifier(&self) -> u8 {
        match self {
            Self::Name(_) => header_id::NAME,
            Self::Type(_) => header_id::TYPE,
            Self::Length(_) => header_id::LENGTH,
            Self::Body(_) => header_id::BODY,
            Self::EndOfBody(_) => header_id::END_OF_BODY,
            Self::Target(_) => header_id::TARGET,
            Self::Who(_) => header_id::WHO,
            Self::ConnectionId(_) => header_id::CONNECTION_ID,
            Self::AppParameters(_) => header_id::APP_PARAMETERS,
            Self::AuthChallenge(_) => header_id::AUTH_CHALLENGE,
            Self::AuthResponse(_) => header_id::AUTH_RESPONSE,
            Self::SingleResponseMode(_) => header_id::SINGLE_RESPONSE_MODE,
            Self::Count(_) => header_id::COUNT,
            Self::Description(_) => header_id::DESCRIPTION,
            Self::Other { identifier, .. } => *identifier,
        }
    }

    /// Appends this header's wire encoding to `out`.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        let identifier = self.identifier();
        match self {
            Self::Name(text) | Self::Description(text) => {
                push_length_prefixed(out, identifier, &encode_unicode(text));
            }
            Self::Type(bytes)
            | Self::Body(bytes)
            | Self::EndOfBody(bytes)
            | Self::Target(bytes)
            | Self::Who(bytes)
            | Self::AppParameters(bytes)
            | Self::AuthChallenge(bytes)
            | Self::AuthResponse(bytes) => push_length_prefixed(out, identifier, bytes),
            Self::Length(value) | Self::ConnectionId(value) | Self::Count(value) => {
                out.push(identifier);
                out.extend_from_slice(&value.to_be_bytes());
            }
            Self::SingleResponseMode(value) => {
                out.push(identifier);
                out.push(*value);
            }
            Self::Other { value, .. } => match value {
                HeaderValue::Text(text) => {
                    push_length_prefixed(out, identifier, &encode_unicode(text))
                }
                HeaderValue::Bytes(bytes) => push_length_prefixed(out, identifier, bytes),
                HeaderValue::Byte(byte) => {
                    out.push(identifier);
                    out.push(*byte);
                }
                HeaderValue::FourByte(value) => {
                    out.push(identifier);
                    out.extend_from_slice(&value.to_be_bytes());
                }
            },
        }
    }

    /// This header's encoded length in bytes.
    pub fn encoded_len(&self) -> usize {
        let mut buf = Vec::new();
        self.encode_into(&mut buf);
        buf.len()
    }

    /// Parses one header from the front of `bytes`, returning it and the
    /// number of bytes consumed.
    pub fn parse(bytes: &[u8]) -> Result<(Self, usize), HeaderError> {
        let &identifier = bytes.first().ok_or(HeaderError::Truncated)?;
        match HeaderEncoding::of(identifier) {
            HeaderEncoding::Byte => {
                let &value = bytes.get(1).ok_or(HeaderError::Truncated)?;
                let header = match identifier {
                    header_id::SINGLE_RESPONSE_MODE => Self::SingleResponseMode(value),
                    _ => Self::Other {
                        identifier,
                        value: HeaderValue::Byte(value),
                    },
                };
                Ok((header, 2))
            }
            HeaderEncoding::FourByte => {
                let (view, _) =
                    FourByteValue::ref_from_prefix(&bytes[1..]).map_err(|_| HeaderError::Truncated)?;
                let value = view.value.get();
                let header = match identifier {
                    header_id::LENGTH => Self::Length(value),
                    header_id::CONNECTION_ID => Self::ConnectionId(value),
                    header_id::COUNT => Self::Count(value),
                    _ => Self::Other {
                        identifier,
                        value: HeaderValue::FourByte(value),
                    },
                };
                Ok((header, 5))
            }
            encoding => {
                let (prefix, rest) =
                    HeaderPrefix::ref_from_prefix(bytes).map_err(|_| HeaderError::Truncated)?;
                let total = usize::from(prefix.length.get());
                // The declared length covers the prefix, so anything under
                // three bytes is malformed rather than merely empty.
                if total < 3 {
                    return Err(HeaderError::BadLength(prefix.length.get()));
                }
                let value_len = total - 3;
                if rest.len() < value_len {
                    return Err(HeaderError::Truncated);
                }
                let value = &rest[..value_len];
                let header = match (identifier, encoding) {
                    (header_id::NAME, _) => Self::Name(decode_unicode(value)),
                    (header_id::DESCRIPTION, _) => Self::Description(decode_unicode(value)),
                    (header_id::TYPE, _) => Self::Type(value.to_vec()),
                    (header_id::BODY, _) => Self::Body(value.to_vec()),
                    (header_id::END_OF_BODY, _) => Self::EndOfBody(value.to_vec()),
                    (header_id::TARGET, _) => Self::Target(value.to_vec()),
                    (header_id::WHO, _) => Self::Who(value.to_vec()),
                    (header_id::APP_PARAMETERS, _) => Self::AppParameters(value.to_vec()),
                    (header_id::AUTH_CHALLENGE, _) => Self::AuthChallenge(value.to_vec()),
                    (header_id::AUTH_RESPONSE, _) => Self::AuthResponse(value.to_vec()),
                    (_, HeaderEncoding::Unicode) => Self::Other {
                        identifier,
                        value: HeaderValue::Text(decode_unicode(value)),
                    },
                    _ => Self::Other {
                        identifier,
                        value: HeaderValue::Bytes(value.to_vec()),
                    },
                };
                Ok((header, total))
            }
        }
    }

    /// Parses every header in `bytes`.
    pub fn parse_all(bytes: &[u8]) -> Result<Vec<Self>, HeaderError> {
        let mut headers = Vec::new();
        let mut rest = bytes;
        while !rest.is_empty() {
            let (header, used) = Self::parse(rest)?;
            headers.push(header);
            rest = &rest[used..];
        }
        Ok(headers)
    }

    /// Encodes a list of headers.
    pub fn encode_all(headers: &[Self]) -> Vec<u8> {
        let mut out = Vec::new();
        for header in headers {
            header.encode_into(&mut out);
        }
        out
    }
}

/// Writes `identifier`, the 2-byte total length, then `value`.
fn push_length_prefixed(out: &mut Vec<u8>, identifier: u8, value: &[u8]) {
    let prefix = HeaderPrefix {
        identifier,
        length: U16::new((value.len() + 3) as u16),
    };
    out.extend_from_slice(prefix.as_bytes());
    out.extend_from_slice(value);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec-derived vector, hand-computed from IrOBEX 1.3 Section 2.2.2: a
    /// Name header for "x" is the identifier, a length of 7 covering the
    /// 3-byte prefix plus UTF-16BE 'x' and its null terminator.
    ///
    /// Bumble has no OBEX implementation (only three UUID constants in
    /// `core.py`), so there is no foreign stack to check these against —
    /// these vectors are derived from the specification by hand and are
    /// weaker evidence than a cross-implementation check would be.
    #[test]
    fn test_name_header_matches_the_spec_layout() {
        let encoded = Header::encode_all(&[Header::Name("x".to_string())]);
        assert_eq!(encoded, vec![0x01, 0x00, 0x07, 0x00, 0x78, 0x00, 0x00]);

        let (parsed, used) = Header::parse(&encoded).unwrap();
        assert_eq!(parsed, Header::Name("x".to_string()));
        assert_eq!(used, 7);
    }

    #[test]
    fn test_every_encoding_round_trips() {
        let headers = vec![
            Header::Name("photo.jpg".to_string()),
            Header::Type(b"image/jpeg\0".to_vec()),
            Header::Length(4096),
            Header::Body(vec![1, 2, 3]),
            Header::EndOfBody(vec![4, 5]),
            Header::Target(vec![0xF9, 0xEC, 0x7B, 0xC4]),
            Header::Who(vec![0xAA]),
            Header::ConnectionId(0x1234_5678),
            Header::AppParameters(vec![0x01, 0x02, 0xFF]),
            Header::AuthChallenge(vec![0x10]),
            Header::AuthResponse(vec![0x20]),
            Header::SingleResponseMode(0x01),
            Header::Count(7),
            Header::Description("done".to_string()),
        ];
        let encoded = Header::encode_all(&headers);
        assert_eq!(Header::parse_all(&encoded).unwrap(), headers);
    }

    #[test]
    fn test_encoding_is_taken_from_the_identifier_not_a_table() {
        assert_eq!(HeaderEncoding::of(header_id::NAME), HeaderEncoding::Unicode);
        assert_eq!(
            HeaderEncoding::of(header_id::BODY),
            HeaderEncoding::ByteSequence
        );
        assert_eq!(
            HeaderEncoding::of(header_id::SINGLE_RESPONSE_MODE),
            HeaderEncoding::Byte
        );
        assert_eq!(
            HeaderEncoding::of(header_id::CONNECTION_ID),
            HeaderEncoding::FourByte
        );
    }

    /// An unknown header must survive the round trip: a peer may send
    /// headers this build predates, and dropping them silently corrupts a
    /// transfer that a later version would understand.
    #[test]
    fn test_unknown_headers_are_preserved_in_each_encoding() {
        let headers = vec![
            Header::Other {
                identifier: 0x4F, // unknown byte sequence
                value: HeaderValue::Bytes(vec![9, 9]),
            },
            Header::Other {
                identifier: 0x0F, // unknown unicode
                value: HeaderValue::Text("hi".to_string()),
            },
            Header::Other {
                identifier: 0x9F, // unknown 1-byte
                value: HeaderValue::Byte(3),
            },
            Header::Other {
                identifier: 0xCF, // unknown 4-byte
                value: HeaderValue::FourByte(9),
            },
        ];
        let encoded = Header::encode_all(&headers);
        assert_eq!(Header::parse_all(&encoded).unwrap(), headers);
    }

    #[test]
    fn test_truncated_and_malformed_headers_are_rejected() {
        // A length-prefixed header cut short mid-value.
        assert_eq!(
            Header::parse(&[0x01, 0x00, 0x09, 0x00]),
            Err(HeaderError::Truncated)
        );
        // A declared length smaller than the 3-byte prefix it must include.
        assert_eq!(
            Header::parse(&[0x01, 0x00, 0x02]),
            Err(HeaderError::BadLength(2))
        );
        // A 4-byte header with only two bytes of value.
        assert_eq!(
            Header::parse(&[header_id::CONNECTION_ID, 0x00, 0x01]),
            Err(HeaderError::Truncated)
        );
        // A 1-byte header with no value at all.
        assert_eq!(
            Header::parse(&[header_id::SINGLE_RESPONSE_MODE]),
            Err(HeaderError::Truncated)
        );
        assert_eq!(Header::parse(&[]), Err(HeaderError::Truncated));
    }

    /// SETPATH uses an empty Name to mean "the parent folder", which is a
    /// different thing from a zero-length name — so an empty string must not
    /// encode a lone null terminator.
    #[test]
    fn test_empty_name_carries_no_null_terminator() {
        let encoded = Header::encode_all(&[Header::Name(String::new())]);
        assert_eq!(encoded, vec![0x01, 0x00, 0x03]);
        assert_eq!(
            Header::parse(&encoded).unwrap().0,
            Header::Name(String::new())
        );
    }

    #[test]
    fn test_non_ascii_names_survive_utf16_encoding() {
        // Two code units plus a null: 3 + 4 + 2 = 9 bytes.
        let header = Header::Name("é☃".to_string());
        let encoded = Header::encode_all(std::slice::from_ref(&header));
        assert_eq!(encoded.len(), 9);
        assert_eq!(Header::parse(&encoded).unwrap().0, header);
    }
}
