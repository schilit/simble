// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! RTP media packets, the format A2DP audio actually travels in.
//!
//! AVDTP's signaling layer (`crate::classic::avdtp`) negotiates *what* will
//! be streamed; the bytes themselves cross a separate L2CAP transport
//! channel as RTP packets (AVDTP spec 7.3, which defers to RFC 3550 for the
//! header). Every field wider than a byte is network byte order —
//! big-endian — unlike the rest of Bluetooth, which is little-endian
//! throughout. That single inconsistency is the most common way a hand-rolled
//! A2DP implementation ends up sending audio a real sink discards.
//!
//! **Codec payloads are opaque here.** Simble models the transport, not the
//! codec: an [`SbcPayload`] tells you how many SBC frames a packet carries
//! and hands them back as byte slices, and nothing in this module decodes
//! audio — exactly as the LE Audio ISO path treats LC3 frames as opaque
//! SDUs. A sink that wants sound decodes above this layer.

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Unaligned,
    byteorder::big_endian::{U16, U32},
};

/// RTP version 2, the only version RFC 3550 defines and the one AVDTP
/// requires (AVDTP spec 7.3).
pub const RTP_VERSION: u8 = 2;

/// The dynamic payload type A2DP implementations conventionally use for
/// SBC. RFC 3551 leaves 96..=127 dynamic; A2DP does not mandate a value, so
/// a sink must not rely on it — but sources overwhelmingly send 96.
pub const DEFAULT_PAYLOAD_TYPE: u8 = 96;

/// The fixed part of an RTP header (RFC 3550 section 5.1). A CSRC list of
/// `csrc_count` 32-bit identifiers may follow before the payload.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct RtpHeader {
    /// Bits 7-6 version, bit 5 padding, bit 4 extension, bits 3-0 CSRC count.
    pub flags: u8,
    /// Bit 7 marker, bits 6-0 payload type.
    pub marker_and_payload_type: u8,
    /// Increments by one per packet; wraps. A sink uses it to spot loss.
    pub sequence_number: U16,
    /// Sampling instant of the first octet, in codec sample units.
    pub timestamp: U32,
    /// Synchronization source — identifies the stream.
    pub ssrc: U32,
}

impl RtpHeader {
    /// The header's wire length, before any CSRC identifiers.
    pub const LEN: usize = 12;

    /// Builds a header for a non-padded, non-extended packet with no CSRCs —
    /// what an A2DP source sends.
    pub fn new(sequence_number: u16, timestamp: u32, ssrc: u32, payload_type: u8) -> Self {
        Self {
            flags: RTP_VERSION << 6,
            marker_and_payload_type: payload_type & 0x7F,
            sequence_number: U16::new(sequence_number),
            timestamp: U32::new(timestamp),
            ssrc: U32::new(ssrc),
        }
    }

    /// RTP version; anything but [`RTP_VERSION`] is not a packet we can read.
    pub fn version(&self) -> u8 {
        self.flags >> 6
    }

    /// Whether the payload carries trailing padding octets.
    pub fn padding(&self) -> bool {
        self.flags & 0x20 != 0
    }

    /// Whether a header extension follows the CSRC list.
    pub fn extension(&self) -> bool {
        self.flags & 0x10 != 0
    }

    /// How many 32-bit CSRC identifiers follow the fixed header.
    pub fn csrc_count(&self) -> usize {
        usize::from(self.flags & 0x0F)
    }

    /// The marker bit — frame boundary hint, unused by A2DP/SBC.
    pub fn marker(&self) -> bool {
        self.marker_and_payload_type & 0x80 != 0
    }

    /// The payload type (see [`DEFAULT_PAYLOAD_TYPE`]).
    pub fn payload_type(&self) -> u8 {
        self.marker_and_payload_type & 0x7F
    }
}

/// One received media packet: its header fields and the codec payload.
///
/// The payload is returned exactly as it arrived, including any codec
/// framing header (for SBC, see [`SbcPayload`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPacket {
    /// Sequence number, for loss and reordering detection.
    pub sequence_number: u16,
    /// Media timestamp in codec sample units.
    pub timestamp: u32,
    /// Synchronization source identifier.
    pub ssrc: u32,
    /// Payload type as sent.
    pub payload_type: u8,
    /// Marker bit.
    pub marker: bool,
    /// The codec payload, opaque to this layer.
    pub payload: Vec<u8>,
}

/// Why a media packet could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaPacketError {
    /// Shorter than a fixed RTP header, or truncated inside the CSRC list.
    Truncated,
    /// The version field is not 2, so this is not an RTP packet at all.
    UnsupportedVersion(u8),
    /// The declared padding length is zero or exceeds the payload.
    BadPadding,
}

impl std::fmt::Display for MediaPacketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Truncated => write!(f, "media packet is truncated"),
            Self::UnsupportedVersion(v) => write!(f, "unsupported RTP version {v}"),
            Self::BadPadding => write!(f, "media packet declares invalid padding"),
        }
    }
}

impl std::error::Error for MediaPacketError {}

impl MediaPacket {
    /// Serializes a packet: fixed header then payload, no CSRCs.
    pub fn to_bytes(&self) -> Vec<u8> {
        let header = RtpHeader::new(
            self.sequence_number,
            self.timestamp,
            self.ssrc,
            self.payload_type,
        );
        let mut out = Vec::with_capacity(RtpHeader::LEN + self.payload.len());
        out.extend_from_slice(header.as_bytes());
        if self.marker {
            out[1] |= 0x80;
        }
        out.extend_from_slice(&self.payload);
        out
    }

    /// Parses one media packet off the transport channel.
    ///
    /// Rejects rather than guessing: a truncated packet or a non-RTP version
    /// is an error, because a sink that silently accepts malformed media
    /// plays noise instead of reporting a problem.
    pub fn parse(data: &[u8]) -> Result<Self, MediaPacketError> {
        let (header, rest) =
            RtpHeader::ref_from_prefix(data).map_err(|_| MediaPacketError::Truncated)?;
        if header.version() != RTP_VERSION {
            return Err(MediaPacketError::UnsupportedVersion(header.version()));
        }
        // The CSRC list sits between the fixed header and the payload.
        let csrc_bytes = header.csrc_count() * 4;
        let payload = rest.get(csrc_bytes..).ok_or(MediaPacketError::Truncated)?;

        // RFC 3550 section 5.1: with the padding bit set, the last octet
        // counts the padding, itself included.
        let payload = if header.padding() {
            let pad = usize::from(*payload.last().ok_or(MediaPacketError::BadPadding)?);
            if pad == 0 || pad > payload.len() {
                return Err(MediaPacketError::BadPadding);
            }
            &payload[..payload.len() - pad]
        } else {
            payload
        };

        Ok(Self {
            sequence_number: header.sequence_number.get(),
            timestamp: header.timestamp.get(),
            ssrc: header.ssrc.get(),
            payload_type: header.payload_type(),
            marker: header.marker(),
            payload: payload.to_vec(),
        })
    }
}

/// The one-byte SBC media payload header that precedes SBC frames in an RTP
/// payload (A2DP spec 4.3.4).
///
/// Two distinct meanings share the low nibble: in an unfragmented payload it
/// counts the whole frames present; in a fragment it counts the fragments
/// still to come, this one included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SbcPayloadHeader {
    /// This payload is part of a fragmented frame.
    pub fragmented: bool,
    /// This is the first fragment of a frame.
    pub start: bool,
    /// This is the last fragment of a frame.
    pub last: bool,
    /// Whole frames present, or remaining fragments when `fragmented`.
    pub frame_count: u8,
}

impl SbcPayloadHeader {
    /// Header for a payload carrying `frames` complete SBC frames.
    ///
    /// A2DP allows at most 15, since the count is a nibble.
    pub fn unfragmented(frames: u8) -> Self {
        Self {
            fragmented: false,
            start: false,
            last: false,
            frame_count: frames & 0x0F,
        }
    }

    /// Encodes the header byte.
    pub fn to_byte(self) -> u8 {
        (u8::from(self.fragmented) << 7)
            | (u8::from(self.start) << 6)
            | (u8::from(self.last) << 5)
            | (self.frame_count & 0x0F)
    }

    /// Decodes a header byte.
    pub fn from_byte(byte: u8) -> Self {
        Self {
            fragmented: byte & 0x80 != 0,
            start: byte & 0x40 != 0,
            last: byte & 0x20 != 0,
            frame_count: byte & 0x0F,
        }
    }
}

/// The maximum SBC frames one RTP payload may carry: the count field is four
/// bits (A2DP spec 4.3.4).
pub const SBC_MAX_FRAMES_PER_PAYLOAD: u8 = 15;

/// An SBC RTP payload split into its header and frame bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SbcPayload {
    /// The payload header.
    pub header: SbcPayloadHeader,
    /// Everything after it — whole frames, or one fragment of a frame.
    /// Opaque: this module never decodes SBC.
    pub data: Vec<u8>,
}

impl SbcPayload {
    /// Reads the payload header off an RTP payload.
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let (&first, rest) = payload.split_first()?;
        Some(Self {
            header: SbcPayloadHeader::from_byte(first),
            data: rest.to_vec(),
        })
    }

    /// Builds a payload carrying `frames` complete SBC frames.
    pub fn unfragmented(frames: &[Vec<u8>]) -> Self {
        Self {
            header: SbcPayloadHeader::unfragmented(frames.len() as u8),
            data: frames.concat(),
        }
    }

    /// Serializes header plus data.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + self.data.len());
        out.push(self.header.to_byte());
        out.extend_from_slice(&self.data);
        out
    }
}

/// Reassembles SBC frames that were split across several RTP payloads
/// (A2DP spec 4.3.4).
///
/// A frame too large for one L2CAP MTU is sent as fragments flagged
/// start/…/last; anything else is delivered straight through. Bumble's sink
/// does not implement this at all (`speaker.py` drops the header and hands
/// on the rest with a `TODO: support fragmented payloads`), so a fragmenting
/// source loses audio there — which is why this is worth modeling.
#[derive(Debug, Default)]
pub struct SbcReassembler {
    partial: Vec<u8>,
    in_progress: bool,
}

impl SbcReassembler {
    /// Creates an empty reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one RTP payload, returning the complete frame data it
    /// completed, if any.
    ///
    /// Returns `None` while a fragmented frame is still arriving. A fragment
    /// that arrives out of order (a continuation with no start, or a start
    /// while another frame is mid-flight) discards the partial frame rather
    /// than splicing unrelated audio together.
    pub fn push(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        let parsed = SbcPayload::parse(payload)?;
        if !parsed.header.fragmented {
            self.reset();
            return Some(parsed.data);
        }
        if parsed.header.start {
            // A start while another frame is in flight means the previous
            // one was lost; keep the new frame, drop the stale bytes.
            self.partial.clear();
            self.in_progress = true;
        } else if !self.in_progress {
            // A continuation with no start: the beginning was lost, so this
            // frame can never be completed.
            return None;
        }
        self.partial.extend_from_slice(&parsed.data);
        if parsed.header.last {
            self.in_progress = false;
            return Some(std::mem::take(&mut self.partial));
        }
        None
    }

    /// Whether a fragmented frame is partially received.
    pub fn is_reassembling(&self) -> bool {
        self.in_progress
    }

    /// Drops any partial frame (used when a stream restarts).
    pub fn reset(&mut self) {
        self.partial.clear();
        self.in_progress = false;
    }
}

/// Splits SBC frames into RTP payloads that fit `max_payload` bytes.
///
/// Frames are packed whole where they fit, up to
/// [`SBC_MAX_FRAMES_PER_PAYLOAD`]; a frame too large for one payload on its
/// own is fragmented (A2DP spec 4.3.4).
pub fn packetize_sbc(frames: &[Vec<u8>], max_payload: usize) -> Vec<Vec<u8>> {
    // One byte of every payload belongs to the SBC header.
    let Some(budget) = max_payload.checked_sub(1).filter(|b| *b > 0) else {
        return Vec::new();
    };
    let mut payloads = Vec::new();
    let mut batch: Vec<Vec<u8>> = Vec::new();
    let mut batch_len = 0;

    for frame in frames {
        if frame.len() > budget {
            // Flush what is queued, then fragment this frame on its own.
            if !batch.is_empty() {
                payloads.push(SbcPayload::unfragmented(&batch).to_bytes());
                batch.clear();
                batch_len = 0;
            }
            let chunks: Vec<&[u8]> = frame.chunks(budget).collect();
            for (i, chunk) in chunks.iter().enumerate() {
                let header = SbcPayloadHeader {
                    fragmented: true,
                    start: i == 0,
                    last: i == chunks.len() - 1,
                    // A fragment counts the fragments still to come,
                    // itself included (A2DP spec 4.3.4).
                    frame_count: (chunks.len() - i).min(0x0F) as u8,
                };
                let mut payload = Vec::with_capacity(1 + chunk.len());
                payload.push(header.to_byte());
                payload.extend_from_slice(chunk);
                payloads.push(payload);
            }
            continue;
        }
        let would_overflow =
            batch_len + frame.len() > budget || batch.len() as u8 >= SBC_MAX_FRAMES_PER_PAYLOAD;
        if would_overflow && !batch.is_empty() {
            payloads.push(SbcPayload::unfragmented(&batch).to_bytes());
            batch.clear();
            batch_len = 0;
        }
        batch_len += frame.len();
        batch.push(frame.clone());
    }
    if !batch.is_empty() {
        payloads.push(SbcPayload::unfragmented(&batch).to_bytes());
    }
    payloads
}

#[cfg(test)]
#[path = "rtp_tests.rs"]
mod tests;
