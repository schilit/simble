// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio/Video Distribution Transport Protocol (AVDTP), the signaling and
//! stream-management layer A2DP (`crate::classic::a2dp`) rides on.
//!
//! Wire format: every signaling PDU starts with a 1-byte header packing the
//! transaction label, packet type, and message type, followed by the signal
//! identifier (single/start packets only) and a per-signal payload. Messages
//! larger than the peer's L2CAP MTU are fragmented into
//! start/continue/end packets and reassembled by [`MessageAssembler`].
//!
//! [`Protocol`] is the per-connection state machine over one Classic L2CAP
//! channel (PSM [`AVDTP_PSM`]): it owns the local stream endpoints (SEPs),
//! tracks discovered remote endpoints, and drives each endpoint's stream
//! state machine (AVDTP spec 9.1: IDLE -> CONFIGURED -> OPEN -> STREAMING).
//! It is a plain synchronous state machine in the same style as
//! `rfcomm::Multiplexer`: [`Protocol::receive`] takes one incoming signaling
//! PDU and returns `(outgoing PDUs, events)`.
//!
//! Scope: signaling plus the **media transport channel**. Once a stream is
//! OPENed, a second L2CAP channel — same PSM [`AVDTP_PSM`], distinct CID —
//! carries RTP packets ([`crate::classic::rtp`]); [`Protocol::attach_media_channel`]
//! registers it and [`Protocol::on_media_pdu`] delivers what arrives into a
//! bounded queue a device drains with [`Protocol::take_media`], mirroring
//! how the LE Audio path hands ISO SDUs to a sink.
//!
//! Codec payloads stay opaque: Simble models the transport, never the audio.
//! The CLOSING and ABORTING states are still passed through instantly rather
//! than draining the transport channel over time.

use crate::classic::a2dp::codec_type;
use crate::classic::rtp::{
    MediaPacket, MediaPacketError, RtpHeader, SbcReassembler, packetize_sbc,
};
use crate::classic::sdp::{AttributeIdSpec, DataElement, SdpClient, SdpUuid, attribute_id};
use crate::l2cap::classic::{ClassicChannelManager, ClassicChannelSpec};
use crate::packets::l2cap_signaling::ConnectionRequestHeader;
use crate::types::SimbleError;
use std::collections::HashMap;

/// Well-known PSM for AVDTP signaling (Bluetooth Assigned Numbers).
pub const AVDTP_PSM: u16 = 0x0019;
/// A2DP (Advanced Audio Distribution) service class UUID (0x110D).
pub(crate) const ADVANCED_AUDIO_DISTRIBUTION_SERVICE_UUID: SdpUuid = SdpUuid::Uuid16(0x110D);
/// AVDTP protocol UUID (0x0019) for SDP protocol descriptor lists.
pub(crate) const AVDTP_PROTOCOL_UUID: SdpUuid = SdpUuid::Uuid16(0x0019);
/// Default AVDTP version advertised/assumed: 1.3.
pub(crate) const DEFAULT_VERSION: (u8, u8) = (1, 3);

/// Signal identifiers (AVDTP spec 8.5, Signal Command Set).
pub mod signal_identifier {
    /// Discover signal.
    pub(crate) const DISCOVER: u8 = 0x01;
    /// Get_Capabilities signal.
    pub const GET_CAPABILITIES: u8 = 0x02;
    /// Set_Configuration signal.
    pub(crate) const SET_CONFIGURATION: u8 = 0x03;
    /// Get_Configuration signal.
    pub const GET_CONFIGURATION: u8 = 0x04;
    /// Reconfigure signal.
    pub(crate) const RECONFIGURE: u8 = 0x05;
    /// Open signal.
    pub const OPEN: u8 = 0x06;
    /// Start signal.
    pub const START: u8 = 0x07;
    /// Close signal.
    pub const CLOSE: u8 = 0x08;
    /// Suspend signal.
    pub(crate) const SUSPEND: u8 = 0x09;
    /// Abort signal.
    pub(crate) const ABORT: u8 = 0x0A;
    /// Security_Control signal.
    pub const SECURITY_CONTROL: u8 = 0x0B;
    /// Get_All_Capabilities signal.
    pub const GET_ALL_CAPABILITIES: u8 = 0x0C;
    /// DelayReport signal.
    pub const DELAYREPORT: u8 = 0x0D;
}

/// Error codes (AVDTP spec 8.20.6.2, ERROR_CODE tables).
pub mod error_code {
    /// Bad header format.
    pub const BAD_HEADER_FORMAT: u8 = 0x01;
    /// Bad length.
    pub const BAD_LENGTH: u8 = 0x11;
    /// Bad ACP SEID.
    pub const BAD_ACP_SEID: u8 = 0x12;
    /// Stream endpoint in use.
    pub const SEP_IN_USE: u8 = 0x13;
    /// Stream endpoint not in use.
    pub const SEP_NOT_IN_USE: u8 = 0x14;
    /// Bad service category.
    pub const BAD_SERV_CATEGORY: u8 = 0x17;
    /// Bad payload format.
    pub const BAD_PAYLOAD_FORMAT: u8 = 0x18;
    /// Command not supported.
    pub const NOT_SUPPORTED_COMMAND: u8 = 0x19;
    /// Invalid capabilities.
    pub const INVALID_CAPABILITIES: u8 = 0x1A;
    /// Bad recovery type.
    pub const BAD_RECOVERY_TYPE: u8 = 0x22;
    /// Bad media transport format.
    pub const BAD_MEDIA_TRANSPORT_FORMAT: u8 = 0x23;
    /// Bad recovery format.
    pub const BAD_RECOVERY_FORMAT: u8 = 0x25;
    /// Bad ROHC format.
    pub const BAD_ROHC_FORMAT: u8 = 0x26;
    /// Bad content-protection format.
    pub const BAD_CP_FORMAT: u8 = 0x27;
    /// Bad multiplexing format.
    pub const BAD_MULTIPLEXING_FORMAT: u8 = 0x28;
    /// Unsupported configuration.
    pub const UNSUPPORTED_CONFIGURATION: u8 = 0x29;
    /// Bad state for this command.
    pub const BAD_STATE: u8 = 0x31;
}

/// Service categories (AVDTP spec Table 8.47).
pub mod service_category {
    /// Media Transport service category.
    pub const MEDIA_TRANSPORT: u8 = 0x01;
    /// Reporting service category.
    pub const REPORTING: u8 = 0x02;
    /// Recovery service category.
    pub const RECOVERY: u8 = 0x03;
    /// Content Protection service category.
    pub const CONTENT_PROTECTION: u8 = 0x04;
    /// Header Compression service category.
    pub const HEADER_COMPRESSION: u8 = 0x05;
    /// Multiplexing service category.
    pub const MULTIPLEXING: u8 = 0x06;
    /// Media Codec service category.
    pub(crate) const MEDIA_CODEC: u8 = 0x07;
    /// Delay Reporting service category.
    pub(crate) const DELAY_REPORTING: u8 = 0x08;
}

/// Media type of a stream endpoint (AVDTP spec 8.20.6.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaType {
    /// Audio.
    Audio,
    /// Video.
    Video,
    /// Multimedia.
    Multimedia,
}

impl MediaType {
    /// Decodes from its wire value.
    pub(crate) fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(MediaType::Audio),
            0x01 => Some(MediaType::Video),
            0x02 => Some(MediaType::Multimedia),
            _ => None,
        }
    }

    /// Encodes to its wire value.
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            MediaType::Audio => 0x00,
            MediaType::Video => 0x01,
            MediaType::Multimedia => 0x02,
        }
    }
}

/// TSEP (AVDTP spec 8.20.3, Stream End-point Type, Source or Sink).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamEndPointType {
    /// Source (produces media).
    Source,
    /// Sink (consumes media).
    Sink,
}

impl StreamEndPointType {
    /// Decodes from its wire value.
    pub(crate) fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(StreamEndPointType::Source),
            0x01 => Some(StreamEndPointType::Sink),
            _ => None,
        }
    }

    /// Encodes to its wire value.
    pub(crate) fn as_u8(self) -> u8 {
        match self {
            StreamEndPointType::Source => 0x00,
            StreamEndPointType::Sink => 0x01,
        }
    }
}

/// Stream states (AVDTP spec 9.1, State Definitions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamState {
    /// IDLE.
    Idle,
    /// CONFIGURED.
    Configured,
    /// OPEN.
    Open,
    /// STREAMING.
    Streaming,
    /// CLOSING.
    Closing,
    /// ABORTING.
    Aborting,
}

/// AVDTP signaling message type (packet header bits).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MessageType {
    #[default]
    /// A command.
    Command,
    /// A general reject.
    GeneralReject,
    /// An accept response.
    ResponseAccept,
    /// A reject response.
    ResponseReject,
}

impl MessageType {
    fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => MessageType::Command,
            1 => MessageType::GeneralReject,
            2 => MessageType::ResponseAccept,
            _ => MessageType::ResponseReject,
        }
    }

    fn as_bits(self) -> u8 {
        match self {
            MessageType::Command => 0,
            MessageType::GeneralReject => 1,
            MessageType::ResponseAccept => 2,
            MessageType::ResponseReject => 3,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PacketType {
    Single,
    Start,
    Continue,
    End,
}

impl PacketType {
    fn from_bits(bits: u8) -> Self {
        match bits & 3 {
            0 => PacketType::Single,
            1 => PacketType::Start,
            2 => PacketType::Continue,
            _ => PacketType::End,
        }
    }

    fn as_bits(self) -> u8 {
        match self {
            PacketType::Single => 0,
            PacketType::Start => 1,
            PacketType::Continue => 2,
            PacketType::End => 3,
        }
    }
}

// ---------------------------------------------------------------------------
// Capabilities
// ---------------------------------------------------------------------------

/// One service capability entry: a category byte plus category-specific
/// bytes (AVDTP spec 8.21). Media codec entries can be viewed through
/// [`MediaCodecCapabilities`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceCapability {
    /// Service category byte (`service_category::*`).
    pub service_category: u8,
    /// Category-specific capability bytes.
    pub data: Vec<u8>,
}

impl ServiceCapability {
    /// Creates an empty capability for `service_category`.
    pub fn new(service_category: u8) -> Self {
        Self {
            service_category,
            data: Vec::new(),
        }
    }

    /// Media Transport capability (no parameters).
    pub fn media_transport() -> Self {
        Self::new(service_category::MEDIA_TRANSPORT)
    }

    /// Delay Reporting capability (no parameters).
    pub fn delay_reporting() -> Self {
        Self::new(service_category::DELAY_REPORTING)
    }

    /// Parses a concatenated capability list (category, length, data ...).
    pub(crate) fn parse_list(payload: &[u8]) -> Option<Vec<ServiceCapability>> {
        let mut capabilities = Vec::new();
        let mut offset = 0;
        while offset < payload.len() {
            let category = *payload.get(offset)?;
            let length = *payload.get(offset + 1)? as usize;
            let data = payload.get(offset + 2..offset + 2 + length)?.to_vec();
            capabilities.push(ServiceCapability {
                service_category: category,
                data,
            });
            offset += 2 + length;
        }
        Some(capabilities)
    }

    /// Serializes a capability list to its wire form.
    pub(crate) fn serialize_list(capabilities: &[ServiceCapability]) -> Vec<u8> {
        let mut out = Vec::new();
        for capability in capabilities {
            out.push(capability.service_category);
            out.push(capability.data.len() as u8);
            out.extend_from_slice(&capability.data);
        }
        out
    }
}

/// Structured view of a Media Codec service capability (AVDTP spec 8.21.5).
/// The codec-specific information element stays as raw bytes here; the A2DP
/// layer (`crate::classic::a2dp::MediaCodecInformation`) interprets it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaCodecCapabilities {
    /// Media type.
    pub media_type: MediaType,
    /// Media codec type byte (A2DP `codec_type::*`).
    pub media_codec_type: u8,
    /// Raw codec-specific information element.
    pub media_codec_information: Vec<u8>,
}

impl MediaCodecCapabilities {
    /// Wraps this into a Media Codec service capability.
    pub fn to_capability(&self) -> ServiceCapability {
        let mut data = vec![self.media_type.as_u8(), self.media_codec_type];
        data.extend_from_slice(&self.media_codec_information);
        ServiceCapability {
            service_category: service_category::MEDIA_CODEC,
            data,
        }
    }

    /// Extracts a Media Codec capability, or `None` for other categories.
    pub(crate) fn from_capability(capability: &ServiceCapability) -> Option<Self> {
        if capability.service_category != service_category::MEDIA_CODEC {
            return None;
        }
        Some(Self {
            media_type: MediaType::from_u8(*capability.data.first()?)?,
            media_codec_type: *capability.data.get(1)?,
            media_codec_information: capability.data.get(2..)?.to_vec(),
        })
    }
}

/// A media frame delivered up from the transport channel.
///
/// `payload` is codec data and stays opaque — Simble never decodes audio.
/// For an SBC stream the A2DP payload header has been consumed and any
/// fragmented frame reassembled, so this is whole codec frames; for other
/// codecs it is the RTP payload exactly as it arrived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaFrame {
    /// The local stream endpoint the frame arrived on.
    pub seid: u8,
    /// RTP sequence number, so a sink can detect loss or reordering.
    pub sequence_number: u16,
    /// RTP timestamp in codec sample units.
    pub timestamp: u32,
    /// Codec payload, opaque to this layer.
    pub payload: Vec<u8>,
}

/// The state one open media transport channel carries.
///
/// A stream's transport channel is a second L2CAP channel to the same PSM as
/// signaling; the CID is how incoming media is matched back to a stream.
#[derive(Debug)]
pub(crate) struct MediaTransport {
    /// The local endpoint this channel serves.
    seid: u8,
    /// Outgoing RTP sequence number; wraps at 16 bits.
    sequence_number: u16,
    /// Synchronization source for packets we send.
    ssrc: u32,
    /// Set when the configured codec is SBC, whose payloads carry an A2DP
    /// framing header and may fragment one frame across packets.
    sbc: bool,
    /// Reassembles fragmented SBC frames.
    reassembler: SbcReassembler,
}

/// How many media frames a sink may fall behind before the oldest are
/// dropped. An unbounded queue would grow without limit when nothing
/// drains it; dropping the oldest is what a real audio pipeline does on
/// overrun.
const MAX_QUEUED_MEDIA_FRAMES: usize = 256;

/// One entry of a Discover response (AVDTP spec 8.6.2, SEP information).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SepInfo {
    /// Stream endpoint ID (SEID).
    pub seid: u8,
    /// Whether the endpoint is currently in a stream.
    pub in_use: bool,
    /// Media type.
    pub media_type: MediaType,
    /// Source or Sink.
    pub tsep: StreamEndPointType,
}

impl SepInfo {
    /// Encodes to the 2-byte SEP info.
    pub fn to_bytes(self) -> [u8; 2] {
        [
            (self.seid << 2) | ((self.in_use as u8) << 1),
            (self.media_type.as_u8() << 4) | (self.tsep.as_u8() << 3),
        ]
    }

    /// Decodes the 2-byte SEP info.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < 2 {
            return None;
        }
        Some(Self {
            seid: bytes[0] >> 2,
            in_use: (bytes[0] >> 1) & 1 != 0,
            media_type: MediaType::from_u8(bytes[1] >> 4)?,
            tsep: StreamEndPointType::from_u8((bytes[1] >> 3) & 1)?,
        })
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// One AVDTP signaling message (AVDTP spec 8.6-8.19). `Reject` covers every
/// signal whose reject carries only an error code; signals with richer
/// reject payloads (Set_Configuration/Reconfigure, Start/Suspend) have
/// dedicated variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Discover command.
    DiscoverCommand,
    /// Discover response: the peer's SEP list.
    DiscoverResponse(Vec<SepInfo>),
    /// Get_Capabilities command.
    GetCapabilitiesCommand {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
    },
    /// Get_Capabilities response.
    GetCapabilitiesResponse(Vec<ServiceCapability>),
    /// Get_All_Capabilities command.
    GetAllCapabilitiesCommand {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
    },
    /// Get_All_Capabilities response.
    GetAllCapabilitiesResponse(Vec<ServiceCapability>),
    /// Set_Configuration command.
    SetConfigurationCommand {
        /// Acp seid.
        acp_seid: u8,
        /// Int seid.
        int_seid: u8,
        /// Capabilities.
        capabilities: Vec<ServiceCapability>,
    },
    /// Set_Configuration accept.
    SetConfigurationResponse,
    /// Set_Configuration reject (failing category and error code).
    SetConfigurationReject {
        /// Service category.
        service_category: u8,
        /// Error code.
        error_code: u8,
    },
    /// Get_Configuration command.
    GetConfigurationCommand {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
    },
    /// Get_Configuration response.
    GetConfigurationResponse(Vec<ServiceCapability>),
    /// Reconfigure command.
    ReconfigureCommand {
        /// Acp seid.
        acp_seid: u8,
        /// Capabilities.
        capabilities: Vec<ServiceCapability>,
    },
    /// Reconfigure accept.
    ReconfigureResponse,
    /// Reconfigure reject (failing category and error code).
    ReconfigureReject {
        /// Service category.
        service_category: u8,
        /// Error code.
        error_code: u8,
    },
    /// Open command.
    OpenCommand {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
    },
    /// Open accept.
    OpenResponse,
    /// Start command (one or more SEIDs).
    StartCommand {
        /// ACP stream endpoint identifiers.
        acp_seids: Vec<u8>,
    },
    /// Start accept.
    StartResponse,
    /// Start reject (failing SEID and error code).
    StartReject {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
        /// Error code.
        error_code: u8,
    },
    /// Suspend command (one or more SEIDs).
    SuspendCommand {
        /// ACP stream endpoint identifiers.
        acp_seids: Vec<u8>,
    },
    /// Suspend accept.
    SuspendResponse,
    /// Suspend reject (failing SEID and error code).
    SuspendReject {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
        /// Error code.
        error_code: u8,
    },
    /// Close command.
    CloseCommand {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
    },
    /// Close accept.
    CloseResponse,
    /// Abort command.
    AbortCommand {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
    },
    /// Abort accept.
    AbortResponse,
    /// Security_Control command.
    SecurityControlCommand {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
        /// Payload data.
        data: Vec<u8>,
    },
    /// Security_Control accept.
    SecurityControlResponse,
    /// DelayReport command (rendering delay).
    DelayReportCommand {
        /// ACP stream endpoint identifier.
        acp_seid: u8,
        /// Delay, in milliseconds.
        delay: u16,
    },
    /// DelayReport accept.
    DelayReportResponse,
    /// A reject whose payload is a single error code, for any signal.
    Reject {
        /// Signal identifier.
        signal_identifier: u8,
        /// Error code.
        error_code: u8,
    },
    /// General Reject (AVDTP spec 8.18), echoing the offending signal.
    GeneralReject {
        /// Signal identifier.
        signal_identifier: u8,
    },
}

// SEIDs travel in the top 6 bits of their byte (AVDTP spec 8.20.5).
fn seid_to_byte(seid: u8) -> u8 {
    seid << 2
}

fn seid_from_byte(byte: u8) -> u8 {
    byte >> 2
}

impl Message {
    /// Returns the AVDTP signal identifier for this message.
    pub fn signal_identifier(&self) -> u8 {
        use signal_identifier as sig;
        match self {
            Message::DiscoverCommand | Message::DiscoverResponse(..) => sig::DISCOVER,
            Message::GetCapabilitiesCommand { .. } | Message::GetCapabilitiesResponse(..) => {
                sig::GET_CAPABILITIES
            }
            Message::GetAllCapabilitiesCommand { .. } | Message::GetAllCapabilitiesResponse(..) => {
                sig::GET_ALL_CAPABILITIES
            }
            Message::SetConfigurationCommand { .. }
            | Message::SetConfigurationResponse
            | Message::SetConfigurationReject { .. } => sig::SET_CONFIGURATION,
            Message::GetConfigurationCommand { .. } | Message::GetConfigurationResponse(..) => {
                sig::GET_CONFIGURATION
            }
            Message::ReconfigureCommand { .. }
            | Message::ReconfigureResponse
            | Message::ReconfigureReject { .. } => sig::RECONFIGURE,
            Message::OpenCommand { .. } | Message::OpenResponse => sig::OPEN,
            Message::StartCommand { .. } | Message::StartResponse | Message::StartReject { .. } => {
                sig::START
            }
            Message::SuspendCommand { .. }
            | Message::SuspendResponse
            | Message::SuspendReject { .. } => sig::SUSPEND,
            Message::CloseCommand { .. } | Message::CloseResponse => sig::CLOSE,
            Message::AbortCommand { .. } | Message::AbortResponse => sig::ABORT,
            Message::SecurityControlCommand { .. } | Message::SecurityControlResponse => {
                sig::SECURITY_CONTROL
            }
            Message::DelayReportCommand { .. } | Message::DelayReportResponse => sig::DELAYREPORT,
            Message::Reject {
                signal_identifier, ..
            }
            | Message::GeneralReject { signal_identifier } => *signal_identifier,
        }
    }

    /// Returns the message type (command, accept, or reject).
    pub fn message_type(&self) -> MessageType {
        match self {
            Message::DiscoverCommand
            | Message::GetCapabilitiesCommand { .. }
            | Message::GetAllCapabilitiesCommand { .. }
            | Message::SetConfigurationCommand { .. }
            | Message::GetConfigurationCommand { .. }
            | Message::ReconfigureCommand { .. }
            | Message::OpenCommand { .. }
            | Message::StartCommand { .. }
            | Message::SuspendCommand { .. }
            | Message::CloseCommand { .. }
            | Message::AbortCommand { .. }
            | Message::SecurityControlCommand { .. }
            | Message::DelayReportCommand { .. } => MessageType::Command,
            Message::SetConfigurationReject { .. }
            | Message::ReconfigureReject { .. }
            | Message::StartReject { .. }
            | Message::SuspendReject { .. }
            | Message::Reject { .. } => MessageType::ResponseReject,
            Message::GeneralReject { .. } => MessageType::GeneralReject,
            _ => MessageType::ResponseAccept,
        }
    }

    /// Serializes the signal-specific payload (excluding the signaling header).
    pub fn to_payload(&self) -> Vec<u8> {
        match self {
            Message::DiscoverCommand
            | Message::SetConfigurationResponse
            | Message::ReconfigureResponse
            | Message::OpenResponse
            | Message::StartResponse
            | Message::SuspendResponse
            | Message::CloseResponse
            | Message::AbortResponse
            | Message::SecurityControlResponse
            | Message::DelayReportResponse
            | Message::GeneralReject { .. } => Vec::new(),
            Message::DiscoverResponse(endpoints) => {
                endpoints.iter().flat_map(|e| e.to_bytes()).collect()
            }
            Message::GetCapabilitiesCommand { acp_seid }
            | Message::GetAllCapabilitiesCommand { acp_seid }
            | Message::GetConfigurationCommand { acp_seid }
            | Message::OpenCommand { acp_seid }
            | Message::CloseCommand { acp_seid }
            | Message::AbortCommand { acp_seid } => vec![seid_to_byte(*acp_seid)],
            Message::GetCapabilitiesResponse(capabilities)
            | Message::GetAllCapabilitiesResponse(capabilities)
            | Message::GetConfigurationResponse(capabilities) => {
                ServiceCapability::serialize_list(capabilities)
            }
            Message::SetConfigurationCommand {
                acp_seid,
                int_seid,
                capabilities,
            } => {
                let mut out = vec![seid_to_byte(*acp_seid), seid_to_byte(*int_seid)];
                out.extend(ServiceCapability::serialize_list(capabilities));
                out
            }
            Message::ReconfigureCommand {
                acp_seid,
                capabilities,
            } => {
                let mut out = vec![seid_to_byte(*acp_seid)];
                out.extend(ServiceCapability::serialize_list(capabilities));
                out
            }
            Message::SetConfigurationReject {
                service_category,
                error_code,
            }
            | Message::ReconfigureReject {
                service_category,
                error_code,
            } => vec![*service_category, *error_code],
            Message::StartCommand { acp_seids } | Message::SuspendCommand { acp_seids } => {
                acp_seids.iter().map(|s| seid_to_byte(*s)).collect()
            }
            Message::StartReject {
                acp_seid,
                error_code,
            }
            | Message::SuspendReject {
                acp_seid,
                error_code,
            } => vec![seid_to_byte(*acp_seid), *error_code],
            Message::SecurityControlCommand { acp_seid, data } => {
                let mut out = vec![seid_to_byte(*acp_seid)];
                out.extend_from_slice(data);
                out
            }
            Message::DelayReportCommand { acp_seid, delay } => {
                // The delay field is big-endian (AVDTP spec 8.19.1).
                vec![seid_to_byte(*acp_seid), (delay >> 8) as u8, *delay as u8]
            }
            Message::Reject { error_code, .. } => vec![*error_code],
        }
    }

    /// Parses a payload for the given signal identifier and message type.
    pub fn parse(
        signal_identifier: u8,
        message_type: MessageType,
        payload: &[u8],
    ) -> Option<Message> {
        use self::signal_identifier as sig;
        if message_type == MessageType::GeneralReject {
            return Some(Message::GeneralReject { signal_identifier });
        }
        match (signal_identifier, message_type) {
            (sig::DISCOVER, MessageType::Command) => Some(Message::DiscoverCommand),
            (sig::DISCOVER, MessageType::ResponseAccept) => Some(Message::DiscoverResponse(
                payload
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| SepInfo::parse(pair))
                    .collect::<Option<Vec<_>>>()?,
            )),
            (sig::GET_CAPABILITIES, MessageType::Command) => {
                Some(Message::GetCapabilitiesCommand {
                    acp_seid: seid_from_byte(*payload.first()?),
                })
            }
            (sig::GET_CAPABILITIES, MessageType::ResponseAccept) => Some(
                Message::GetCapabilitiesResponse(ServiceCapability::parse_list(payload)?),
            ),
            (sig::GET_ALL_CAPABILITIES, MessageType::Command) => {
                Some(Message::GetAllCapabilitiesCommand {
                    acp_seid: seid_from_byte(*payload.first()?),
                })
            }
            (sig::GET_ALL_CAPABILITIES, MessageType::ResponseAccept) => Some(
                Message::GetAllCapabilitiesResponse(ServiceCapability::parse_list(payload)?),
            ),
            (sig::SET_CONFIGURATION, MessageType::Command) => {
                Some(Message::SetConfigurationCommand {
                    acp_seid: seid_from_byte(*payload.first()?),
                    int_seid: seid_from_byte(*payload.get(1)?),
                    capabilities: ServiceCapability::parse_list(payload.get(2..)?)?,
                })
            }
            (sig::SET_CONFIGURATION, MessageType::ResponseAccept) => {
                Some(Message::SetConfigurationResponse)
            }
            (sig::SET_CONFIGURATION, MessageType::ResponseReject) => {
                Some(Message::SetConfigurationReject {
                    service_category: *payload.first()?,
                    error_code: *payload.get(1)?,
                })
            }
            (sig::GET_CONFIGURATION, MessageType::Command) => {
                Some(Message::GetConfigurationCommand {
                    acp_seid: seid_from_byte(*payload.first()?),
                })
            }
            (sig::GET_CONFIGURATION, MessageType::ResponseAccept) => Some(
                Message::GetConfigurationResponse(ServiceCapability::parse_list(payload)?),
            ),
            (sig::RECONFIGURE, MessageType::Command) => Some(Message::ReconfigureCommand {
                acp_seid: seid_from_byte(*payload.first()?),
                capabilities: ServiceCapability::parse_list(payload.get(1..)?)?,
            }),
            (sig::RECONFIGURE, MessageType::ResponseAccept) => Some(Message::ReconfigureResponse),
            (sig::RECONFIGURE, MessageType::ResponseReject) => Some(Message::ReconfigureReject {
                service_category: *payload.first()?,
                error_code: *payload.get(1)?,
            }),
            (sig::OPEN, MessageType::Command) => Some(Message::OpenCommand {
                acp_seid: seid_from_byte(*payload.first()?),
            }),
            (sig::OPEN, MessageType::ResponseAccept) => Some(Message::OpenResponse),
            (sig::START, MessageType::Command) => Some(Message::StartCommand {
                acp_seids: payload.iter().map(|b| seid_from_byte(*b)).collect(),
            }),
            (sig::START, MessageType::ResponseAccept) => Some(Message::StartResponse),
            (sig::START, MessageType::ResponseReject) => Some(Message::StartReject {
                acp_seid: seid_from_byte(*payload.first()?),
                error_code: *payload.get(1)?,
            }),
            (sig::SUSPEND, MessageType::Command) => Some(Message::SuspendCommand {
                acp_seids: payload.iter().map(|b| seid_from_byte(*b)).collect(),
            }),
            (sig::SUSPEND, MessageType::ResponseAccept) => Some(Message::SuspendResponse),
            (sig::SUSPEND, MessageType::ResponseReject) => Some(Message::SuspendReject {
                acp_seid: seid_from_byte(*payload.first()?),
                error_code: *payload.get(1)?,
            }),
            (sig::CLOSE, MessageType::Command) => Some(Message::CloseCommand {
                acp_seid: seid_from_byte(*payload.first()?),
            }),
            (sig::CLOSE, MessageType::ResponseAccept) => Some(Message::CloseResponse),
            (sig::ABORT, MessageType::Command) => Some(Message::AbortCommand {
                acp_seid: seid_from_byte(*payload.first()?),
            }),
            (sig::ABORT, MessageType::ResponseAccept) => Some(Message::AbortResponse),
            (sig::SECURITY_CONTROL, MessageType::Command) => {
                Some(Message::SecurityControlCommand {
                    acp_seid: seid_from_byte(*payload.first()?),
                    data: payload.get(1..)?.to_vec(),
                })
            }
            (sig::SECURITY_CONTROL, MessageType::ResponseAccept) => {
                Some(Message::SecurityControlResponse)
            }
            (sig::DELAYREPORT, MessageType::Command) => Some(Message::DelayReportCommand {
                acp_seid: seid_from_byte(*payload.first()?),
                delay: ((*payload.get(1)? as u16) << 8) | *payload.get(2)? as u16,
            }),
            (sig::DELAYREPORT, MessageType::ResponseAccept) => Some(Message::DelayReportResponse),
            (_, MessageType::ResponseReject) => Some(Message::Reject {
                signal_identifier,
                error_code: *payload.first()?,
            }),
            _ => None,
        }
    }
}

/// Serializes one message into signaling PDUs, fragmenting into
/// start/continue/end packets when the payload exceeds `peer_mtu`
/// (AVDTP spec 8.4, signaling header formats).
pub fn write_message(transaction_label: u8, message: &Message, peer_mtu: u16) -> Vec<Vec<u8>> {
    let mut payload = message.to_payload();
    let signal = message.signal_identifier();
    let message_type = message.message_type();
    // Every fragment leaves room for a worst-case 3-byte start header.
    let max_fragment_size = (peer_mtu as usize).saturating_sub(3).max(1);

    let mut packet_type = if payload.len() + 2 <= peer_mtu as usize {
        PacketType::Single
    } else {
        PacketType::Start
    };

    let mut pdus = Vec::new();
    loop {
        let first_byte =
            (transaction_label << 4) | (packet_type.as_bits() << 2) | message_type.as_bits();
        let mut pdu = match packet_type {
            PacketType::Single => vec![first_byte, signal],
            PacketType::Start => {
                let packet_count = payload.len().div_ceil(max_fragment_size) as u8;
                vec![first_byte, signal, packet_count]
            }
            _ => vec![first_byte],
        };
        let take = max_fragment_size.min(payload.len());
        pdu.extend(payload.drain(..take));
        pdus.push(pdu);

        if payload.is_empty() {
            return pdus;
        }
        packet_type = if payload.len() > max_fragment_size {
            PacketType::Continue
        } else {
            PacketType::End
        };
    }
}

// ---------------------------------------------------------------------------
// Message assembler
// ---------------------------------------------------------------------------

/// A fully reassembled signaling message, still in wire form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledMessage {
    /// Transaction label from the signaling header.
    pub transaction_label: u8,
    /// AVDTP signal identifier.
    pub signal_identifier: u8,
    /// Message type (command, accept, or reject).
    pub message_type: MessageType,
    /// Reassembled message payload.
    pub payload: Vec<u8>,
}

/// Reassembles fragmented signaling messages (single or
/// start/continue/end packet sequences). Malformed or out-of-sequence PDUs
/// are dropped, never panicking, so a hostile peer cannot take the
/// signaling channel down.
#[derive(Debug, Default)]
pub struct MessageAssembler {
    transaction_label: u8,
    message: Option<Vec<u8>>,
    message_type: MessageType,
    signal_identifier: u8,
    number_of_signal_packets: u8,
    packet_count: u8,
}

impl MessageAssembler {
    /// Creates an empty reassembler.
    pub fn new() -> Self {
        Self::default()
    }

    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feeds one signaling PDU; returns the completed message once the last
    /// fragment arrives, or `None` while incomplete (or on a dropped PDU).
    pub fn on_pdu(&mut self, pdu: &[u8]) -> Option<AssembledMessage> {
        self.packet_count += 1;

        let first_byte = *pdu.first()?;
        let transaction_label = first_byte >> 4;
        let packet_type = PacketType::from_bits(first_byte >> 2);
        let message_type = MessageType::from_bits(first_byte);

        match packet_type {
            PacketType::Single | PacketType::Start => {
                let signal = *pdu.get(1)? & 0x3F;
                if self.message.is_some() {
                    // The previous fragmented message was never terminated.
                    self.reset();
                    self.packet_count = 1;
                }
                self.transaction_label = transaction_label;
                self.signal_identifier = signal;
                self.message_type = message_type;
                if packet_type == PacketType::Single {
                    self.message = Some(pdu[2..].to_vec());
                    return self.complete();
                }
                self.number_of_signal_packets = *pdu.get(2)?;
                self.message = Some(pdu[3..].to_vec());
                None
            }
            PacketType::Continue | PacketType::End => {
                if self.message.is_none()
                    || transaction_label != self.transaction_label
                    || message_type != self.message_type
                {
                    return None;
                }
                self.message.as_mut()?.extend_from_slice(&pdu[1..]);
                if packet_type == PacketType::End {
                    if self.packet_count != self.number_of_signal_packets {
                        self.reset();
                        return None;
                    }
                    return self.complete();
                }
                if self.packet_count > self.number_of_signal_packets {
                    self.reset();
                }
                None
            }
        }
    }

    fn complete(&mut self) -> Option<AssembledMessage> {
        let assembled = AssembledMessage {
            transaction_label: self.transaction_label,
            signal_identifier: self.signal_identifier,
            message_type: self.message_type,
            payload: self.message.take().unwrap_or_default(),
        };
        self.reset();
        Some(assembled)
    }
}

// ---------------------------------------------------------------------------
// Endpoints
// ---------------------------------------------------------------------------

/// A local stream endpoint (SEP) with its stream state machine. The stream
/// state lives on the endpoint: one SEP can be part of at most one stream
/// (AVDTP spec 5.3), so a separate stream object would be 1:1 with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalStreamEndPoint {
    /// Local stream endpoint ID (SEID).
    pub seid: u8,
    /// Media type.
    pub media_type: MediaType,
    /// Source or Sink.
    pub tsep: StreamEndPointType,
    /// Advertised service capabilities.
    pub capabilities: Vec<ServiceCapability>,
    /// Configured service capabilities, once set.
    pub configuration: Vec<ServiceCapability>,
    /// Current stream state.
    pub state: StreamState,
    /// The peer SEID this endpoint is streaming with, once configured.
    pub remote_seid: Option<u8>,
}

impl LocalStreamEndPoint {
    /// Whether the endpoint is part of an active stream (not IDLE).
    pub fn in_use(&self) -> bool {
        self.state != StreamState::Idle
    }

    fn sep_info(&self) -> SepInfo {
        SepInfo {
            seid: self.seid,
            in_use: self.in_use(),
            media_type: self.media_type,
            tsep: self.tsep,
        }
    }
}

/// A remote stream endpoint learned through Discover and Get_Capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredStreamEndPoint {
    /// Remote stream endpoint ID (SEID).
    pub seid: u8,
    /// Media type.
    pub media_type: MediaType,
    /// Source or Sink.
    pub tsep: StreamEndPointType,
    /// Whether the remote endpoint is currently in use.
    pub in_use: bool,
    /// Capabilities learned via Get_Capabilities.
    pub capabilities: Vec<ServiceCapability>,
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// Result of feeding one incoming PDU into [`Protocol::receive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvdtpEvent {
    /// A Discover response arrived; remote endpoints are now known.
    EndpointsDiscovered(Vec<SepInfo>),
    /// A Get_Capabilities/Get_All_Capabilities response arrived for `seid`.
    CapabilitiesReceived {
        /// Seid.
        seid: u8,
        /// Capabilities.
        capabilities: Vec<ServiceCapability>,
    },
    /// A Get_Configuration response arrived for remote endpoint `seid`.
    ConfigurationReceived {
        /// Seid.
        seid: u8,
        /// Capabilities.
        capabilities: Vec<ServiceCapability>,
    },
    /// The local endpoint `seid` entered CONFIGURED (either side).
    StreamConfigured {
        /// Stream endpoint identifier.
        seid: u8,
    },
    /// The local endpoint `seid` was reconfigured while OPEN (either side).
    StreamReconfigured {
        /// Stream endpoint identifier.
        seid: u8,
    },
    /// The local endpoint `seid` entered OPEN (either side).
    StreamOpened {
        /// Stream endpoint identifier.
        seid: u8,
    },
    /// The local endpoint `seid` entered STREAMING (either side).
    StreamStarted {
        /// Stream endpoint identifier.
        seid: u8,
    },
    /// The local endpoint `seid` suspended back to OPEN (either side).
    StreamSuspended {
        /// Stream endpoint identifier.
        seid: u8,
    },
    /// The local endpoint `seid` closed back to IDLE (either side).
    StreamClosed {
        /// Stream endpoint identifier.
        seid: u8,
    },
    /// The local endpoint `seid` aborted back to IDLE (either side).
    StreamAborted {
        /// Stream endpoint identifier.
        seid: u8,
    },
    /// The peer reported its rendering delay for local endpoint `seid`.
    DelayReport {
        /// Stream endpoint identifier.
        seid: u8,
        /// Delay, in milliseconds.
        delay: u16,
    },
    /// The peer sent content-protection data for local endpoint `seid`.
    SecurityControl {
        /// Stream endpoint identifier.
        seid: u8,
        /// Payload data.
        data: Vec<u8>,
    },
    /// A command we sent was rejected. `error_code` is 0 for a General
    /// Reject, which carries none.
    CommandRejected {
        /// Signal identifier.
        signal_identifier: u8,
        /// Error code.
        error_code: u8,
    },
}

#[derive(Debug, Clone)]
enum Pending {
    Discover,
    GetCapabilities {
        seid: u8,
    },
    SetConfiguration {
        acp_seid: u8,
        int_seid: u8,
        capabilities: Vec<ServiceCapability>,
    },
    GetConfiguration {
        seid: u8,
    },
    Reconfigure {
        int_seid: u8,
        capabilities: Vec<ServiceCapability>,
    },
    Open {
        int_seid: u8,
    },
    Start {
        int_seids: Vec<u8>,
    },
    Suspend {
        int_seids: Vec<u8>,
    },
    Close {
        int_seid: u8,
    },
    Abort {
        int_seid: u8,
    },
}

impl Pending {
    fn signal_identifier(&self, version: (u8, u8)) -> u8 {
        use signal_identifier as sig;
        match self {
            Pending::Discover => sig::DISCOVER,
            Pending::GetCapabilities { .. } => {
                if version > (1, 2) {
                    sig::GET_ALL_CAPABILITIES
                } else {
                    sig::GET_CAPABILITIES
                }
            }
            Pending::SetConfiguration { .. } => sig::SET_CONFIGURATION,
            Pending::GetConfiguration { .. } => sig::GET_CONFIGURATION,
            Pending::Reconfigure { .. } => sig::RECONFIGURE,
            Pending::Open { .. } => sig::OPEN,
            Pending::Start { .. } => sig::START,
            Pending::Suspend { .. } => sig::SUSPEND,
            Pending::Close { .. } => sig::CLOSE,
            Pending::Abort { .. } => sig::ABORT,
        }
    }
}

/// The AVDTP signaling state machine for one connection: local SEPs,
/// discovered remote SEPs, outstanding transactions, and per-endpoint
/// stream states. Synchronous: [`Protocol::receive`] takes one incoming
/// signaling PDU (an L2CAP SDU on the AVDTP channel) and returns the PDUs
/// to send back plus events for the caller, mirroring `rfcomm::Multiplexer`.
#[derive(Debug)]
pub struct Protocol {
    /// Negotiated AVDTP version.
    pub version: (u8, u8),
    /// Local stream endpoints, indexed by SEID minus one.
    pub(crate) local_endpoints: Vec<LocalStreamEndPoint>,
    /// Discovered remote endpoints, keyed by SEID.
    pub(crate) remote_endpoints: HashMap<u8, DiscoveredStreamEndPoint>,
    peer_mtu: u16,
    assembler: MessageAssembler,
    pending: HashMap<u8, Pending>,
    next_transaction: u8,
    /// Open media transport channels, keyed by their L2CAP CID.
    media_transports: HashMap<u16, MediaTransport>,
    /// Frames received on any transport channel, oldest first.
    media_rx: std::collections::VecDeque<MediaFrame>,
}

impl Protocol {
    /// Creates a protocol instance at the default AVDTP version.
    pub fn new(peer_mtu: u16) -> Self {
        Self::with_version(DEFAULT_VERSION, peer_mtu)
    }

    /// Creates a protocol instance at a specific AVDTP version.
    pub(crate) fn with_version(version: (u8, u8), peer_mtu: u16) -> Self {
        Self {
            version,
            local_endpoints: Vec::new(),
            remote_endpoints: HashMap::new(),
            peer_mtu,
            assembler: MessageAssembler::new(),
            pending: HashMap::new(),
            next_transaction: 0,
            media_transports: HashMap::new(),
            media_rx: std::collections::VecDeque::new(),
        }
    }

    /// Adds a local source endpoint, returning its SEID (contiguous from 1).
    pub fn add_source(&mut self, codec: MediaCodecCapabilities, delay_reporting: bool) -> u8 {
        let mut capabilities = vec![ServiceCapability::media_transport(), codec.to_capability()];
        if delay_reporting {
            capabilities.push(ServiceCapability::delay_reporting());
        }
        self.add_endpoint(codec.media_type, StreamEndPointType::Source, capabilities)
    }

    /// Adds a local sink endpoint, returning its SEID (contiguous from 1).
    pub fn add_sink(&mut self, codec: MediaCodecCapabilities) -> u8 {
        let capabilities = vec![ServiceCapability::media_transport(), codec.to_capability()];
        self.add_endpoint(codec.media_type, StreamEndPointType::Sink, capabilities)
    }

    fn add_endpoint(
        &mut self,
        media_type: MediaType,
        tsep: StreamEndPointType,
        capabilities: Vec<ServiceCapability>,
    ) -> u8 {
        let seid = self.local_endpoints.len() as u8 + 1;
        self.local_endpoints.push(LocalStreamEndPoint {
            seid,
            media_type,
            tsep,
            configuration: capabilities.clone(),
            capabilities,
            state: StreamState::Idle,
            remote_seid: None,
        });
        seid
    }

    /// Returns the local endpoint with the given SEID, if any.
    pub fn get_local_endpoint_by_seid(&self, seid: u8) -> Option<&LocalStreamEndPoint> {
        (seid > 0)
            .then(|| self.local_endpoints.get(seid as usize - 1))
            .flatten()
    }

    fn get_local_endpoint_mut(&mut self, seid: u8) -> Option<&mut LocalStreamEndPoint> {
        (seid > 0)
            .then(|| self.local_endpoints.get_mut(seid as usize - 1))
            .flatten()
    }

    fn local_endpoint_by_remote_seid(&self, remote_seid: u8) -> Option<&LocalStreamEndPoint> {
        self.local_endpoints
            .iter()
            .find(|e| e.remote_seid == Some(remote_seid))
    }

    /// Finds an unused remote sink endpoint advertising the given codec.
    /// For `NON_A2DP` (vendor-specific) codecs, `vendor_id`/`codec_id` are
    /// matched against the codec information prefix; otherwise ignored.
    pub fn find_remote_sink_by_codec(
        &self,
        media_type: MediaType,
        codec_type: u8,
        vendor_id: u32,
        codec_id: u16,
    ) -> Option<&DiscoveredStreamEndPoint> {
        self.remote_endpoints.values().find(|endpoint| {
            if endpoint.in_use
                || endpoint.media_type != media_type
                || endpoint.tsep != StreamEndPointType::Sink
            {
                return false;
            }
            let has_media_transport = endpoint
                .capabilities
                .iter()
                .any(|c| c.service_category == service_category::MEDIA_TRANSPORT);
            let has_codec = endpoint
                .capabilities
                .iter()
                .filter_map(MediaCodecCapabilities::from_capability)
                .any(|codec| {
                    if codec.media_type != MediaType::Audio || codec.media_codec_type != codec_type
                    {
                        return false;
                    }
                    if codec_type != 0xFF {
                        return true;
                    }
                    let info = &codec.media_codec_information;
                    info.len() >= 6
                        && u32::from_le_bytes([info[0], info[1], info[2], info[3]]) == vendor_id
                        && u16::from_le_bytes([info[4], info[5]]) == codec_id
                });
            has_media_transport && has_codec
        })
    }

    // -- Media transport channel -----------------------------------------

    /// Registers the L2CAP channel that will carry media for `seid`.
    ///
    /// AVDTP opens the transport channel *after* the OPEN signal succeeds,
    /// on the same PSM as signaling but with its own CID, so the caller
    /// (which owns L2CAP) tells the protocol which CID belongs to which
    /// stream. Fails if the endpoint is unknown or not yet OPEN, because a
    /// transport channel for an unconfigured stream has nowhere to deliver.
    pub fn attach_media_channel(&mut self, seid: u8, cid: u16) -> Result<(), SimbleError> {
        let endpoint = self
            .get_local_endpoint_by_seid(seid)
            .ok_or_else(|| SimbleError::DeviceError(format!("AVDTP: unknown SEID {seid}")))?;
        if !matches!(endpoint.state, StreamState::Open | StreamState::Streaming) {
            return Err(SimbleError::DeviceError(format!(
                "AVDTP: SEID {seid} is {:?}, not OPEN - no media channel yet",
                endpoint.state
            )));
        }
        // An SBC stream's payloads carry the A2DP framing header; other
        // codecs are passed through untouched.
        let sbc = endpoint
            .configuration
            .iter()
            .filter_map(MediaCodecCapabilities::from_capability)
            .any(|codec| codec.media_codec_type == codec_type::SBC);
        self.media_transports.insert(
            cid,
            MediaTransport {
                seid,
                sequence_number: 0,
                // A single stream per endpoint, so the SEID identifies the
                // source well enough for a simulation.
                ssrc: u32::from(seid),
                sbc,
                reassembler: SbcReassembler::new(),
            },
        );
        Ok(())
    }

    /// Drops the media transport channel belonging to `seid`, if any.
    ///
    /// AVDTP tears the transport channel down with the stream (spec 9.1):
    /// leaving it registered would let media for a closed stream keep
    /// arriving, and would leak a half-reassembled frame into the next
    /// stream that reused the CID.
    fn detach_media_by_seid(&mut self, seid: u8) {
        self.media_transports.retain(|_, t| t.seid != seid);
    }

    /// Forgets the media channel with `cid` (the transport channel closed).
    pub fn detach_media_channel(&mut self, cid: u16) -> Option<u8> {
        self.media_transports.remove(&cid).map(|t| t.seid)
    }

    /// Whether `seid` currently has a media transport channel.
    pub fn has_media_channel(&self, seid: u8) -> bool {
        self.media_transports.values().any(|t| t.seid == seid)
    }

    /// Delivers one PDU received on media transport channel `cid`.
    ///
    /// A packet for an unknown CID, or one that is not well-formed RTP, is
    /// rejected rather than guessed at: a sink that accepts malformed media
    /// plays noise instead of reporting a fault.
    pub fn on_media_pdu(&mut self, cid: u16, pdu: &[u8]) -> Result<(), SimbleError> {
        let transport = self.media_transports.get_mut(&cid).ok_or_else(|| {
            SimbleError::DeviceError(format!("AVDTP: no media channel with CID {cid:#06X}"))
        })?;
        let packet = MediaPacket::parse(pdu).map_err(|e: MediaPacketError| {
            SimbleError::PacketParseError(format!("AVDTP media: {e}"))
        })?;

        // SBC payloads carry an A2DP header and may fragment one frame over
        // several packets; anything else is delivered as it arrived.
        let payload = if transport.sbc {
            match transport.reassembler.push(&packet.payload) {
                Some(frame) => frame,
                // Still collecting fragments - nothing to deliver yet.
                None => return Ok(()),
            }
        } else {
            packet.payload
        };

        let frame = MediaFrame {
            seid: transport.seid,
            sequence_number: packet.sequence_number,
            timestamp: packet.timestamp,
            payload,
        };
        if self.media_rx.len() >= MAX_QUEUED_MEDIA_FRAMES {
            self.media_rx.pop_front();
        }
        self.media_rx.push_back(frame);
        Ok(())
    }

    /// Drains received media frames, oldest first - the sink's accessor.
    pub fn take_media(&mut self) -> Vec<MediaFrame> {
        self.media_rx.drain(..).collect()
    }

    /// Packetizes `frames` for the stream on `seid` and returns the RTP
    /// packets to write to its transport channel.
    ///
    /// `timestamp` is in codec sample units and is the caller's business:
    /// only a source that knows the codec's frame duration can advance it
    /// correctly. Sequence numbers are stamped here and wrap at 16 bits.
    /// Returns an error if the stream has no media channel or is not
    /// STREAMING - media before START is a protocol violation.
    pub fn send_media(
        &mut self,
        seid: u8,
        frames: &[Vec<u8>],
        timestamp: u32,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        let state = self
            .get_local_endpoint_by_seid(seid)
            .map(|e| e.state)
            .ok_or_else(|| SimbleError::DeviceError(format!("AVDTP: unknown SEID {seid}")))?;
        if state != StreamState::Streaming {
            return Err(SimbleError::DeviceError(format!(
                "AVDTP: SEID {seid} is {state:?}, not STREAMING"
            )));
        }
        let peer_mtu = self.peer_mtu;
        let transport = self
            .media_transports
            .values_mut()
            .find(|t| t.seid == seid)
            .ok_or_else(|| {
                SimbleError::DeviceError(format!("AVDTP: SEID {seid} has no media channel"))
            })?;

        // Every packet spends 12 bytes on the RTP header before any payload.
        let max_payload = usize::from(peer_mtu).saturating_sub(RtpHeader::LEN);
        let payloads = if transport.sbc {
            packetize_sbc(frames, max_payload)
        } else {
            // Without codec framing each frame is its own payload; one too
            // large for the MTU cannot be sent, and silently truncating it
            // would corrupt the stream.
            frames
                .iter()
                .filter(|f| f.len() <= max_payload)
                .cloned()
                .collect()
        };

        Ok(payloads
            .into_iter()
            .map(|payload| {
                let packet = MediaPacket {
                    sequence_number: transport.sequence_number,
                    timestamp,
                    ssrc: transport.ssrc,
                    payload_type: crate::classic::rtp::DEFAULT_PAYLOAD_TYPE,
                    marker: false,
                    payload,
                };
                transport.sequence_number = transport.sequence_number.wrapping_add(1);
                packet.to_bytes()
            })
            .collect())
    }

    // -- Initiator (INT) command builders --------------------------------

    fn send_command(
        &mut self,
        pending: Pending,
        message: Message,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        for offset in 0..16u8 {
            let label = (self.next_transaction + offset) % 16;
            if let std::collections::hash_map::Entry::Vacant(entry) = self.pending.entry(label) {
                entry.insert(pending);
                self.next_transaction = (label + 1) % 16;
                return Ok(write_message(label, &message, self.peer_mtu));
            }
        }
        Err(SimbleError::DeviceError(
            "AVDTP: all 16 transaction labels are in flight".into(),
        ))
    }

    /// Sends a Discover command to enumerate the peer's endpoints.
    pub fn discover(&mut self) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_command(Pending::Discover, Message::DiscoverCommand)
    }

    /// Sends Get_Capabilities (Get_All_Capabilities on AVDTP >= 1.3) for a
    /// remote endpoint.
    pub fn get_capabilities(&mut self, seid: u8) -> Result<Vec<Vec<u8>>, SimbleError> {
        let message = if self.version > (1, 2) {
            Message::GetAllCapabilitiesCommand { acp_seid: seid }
        } else {
            Message::GetCapabilitiesCommand { acp_seid: seid }
        };
        self.send_command(Pending::GetCapabilities { seid }, message)
    }

    /// Configures remote endpoint `acp_seid` for streaming with our local
    /// endpoint `int_seid` (which must be IDLE).
    pub fn set_configuration(
        &mut self,
        acp_seid: u8,
        int_seid: u8,
        capabilities: Vec<ServiceCapability>,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.require_local_state(int_seid, &[StreamState::Idle])?;
        self.send_command(
            Pending::SetConfiguration {
                acp_seid,
                int_seid,
                capabilities: capabilities.clone(),
            },
            Message::SetConfigurationCommand {
                acp_seid,
                int_seid,
                capabilities,
            },
        )
    }

    /// Requests the current configuration of remote endpoint `seid`.
    /// Sends a Get_Configuration command for remote endpoint `seid`.
    pub fn get_configuration(&mut self, seid: u8) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_command(
            Pending::GetConfiguration { seid },
            Message::GetConfigurationCommand { acp_seid: seid },
        )
    }

    /// Reconfigures the stream with remote endpoint `acp_seid` (local side
    /// must be OPEN).
    pub fn reconfigure(
        &mut self,
        acp_seid: u8,
        capabilities: Vec<ServiceCapability>,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        let int_seid = self.require_stream(acp_seid, &[StreamState::Open])?;
        self.send_command(
            Pending::Reconfigure {
                int_seid,
                capabilities: capabilities.clone(),
            },
            Message::ReconfigureCommand {
                acp_seid,
                capabilities,
            },
        )
    }

    /// Opens the stream with remote endpoint `acp_seid` (local side must be
    /// CONFIGURED).
    pub fn open(&mut self, acp_seid: u8) -> Result<Vec<Vec<u8>>, SimbleError> {
        let int_seid = self.require_stream(acp_seid, &[StreamState::Configured])?;
        self.send_command(
            Pending::Open { int_seid },
            Message::OpenCommand { acp_seid },
        )
    }

    /// Starts the streams with the given remote endpoints (local sides must
    /// be OPEN).
    pub fn start(&mut self, acp_seids: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let int_seids = acp_seids
            .iter()
            .map(|seid| self.require_stream(*seid, &[StreamState::Open]))
            .collect::<Result<Vec<_>, _>>()?;
        self.send_command(
            Pending::Start { int_seids },
            Message::StartCommand {
                acp_seids: acp_seids.to_vec(),
            },
        )
    }

    /// Suspends the streams with the given remote endpoints (local sides
    /// must be STREAMING).
    pub fn suspend(&mut self, acp_seids: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        let int_seids = acp_seids
            .iter()
            .map(|seid| self.require_stream(*seid, &[StreamState::Streaming]))
            .collect::<Result<Vec<_>, _>>()?;
        self.send_command(
            Pending::Suspend { int_seids },
            Message::SuspendCommand {
                acp_seids: acp_seids.to_vec(),
            },
        )
    }

    /// Closes the stream with remote endpoint `acp_seid` (local side must
    /// be OPEN or STREAMING).
    pub fn close(&mut self, acp_seid: u8) -> Result<Vec<Vec<u8>>, SimbleError> {
        let int_seid =
            self.require_stream(acp_seid, &[StreamState::Open, StreamState::Streaming])?;
        self.send_command(
            Pending::Close { int_seid },
            Message::CloseCommand { acp_seid },
        )
    }

    /// Aborts the stream with remote endpoint `acp_seid` from any non-IDLE
    /// state.
    pub fn abort(&mut self, acp_seid: u8) -> Result<Vec<Vec<u8>>, SimbleError> {
        let int_seid = self.require_stream(
            acp_seid,
            &[
                StreamState::Configured,
                StreamState::Open,
                StreamState::Streaming,
            ],
        )?;
        self.send_command(
            Pending::Abort { int_seid },
            Message::AbortCommand { acp_seid },
        )
    }

    fn require_local_state(&self, seid: u8, states: &[StreamState]) -> Result<(), SimbleError> {
        let endpoint = self
            .get_local_endpoint_by_seid(seid)
            .ok_or_else(|| unknown_seid(seid))?;
        if !states.contains(&endpoint.state) {
            return Err(bad_local_state(seid, endpoint.state));
        }
        Ok(())
    }

    /// Finds the local endpoint streaming with `remote_seid` and checks its
    /// state, returning its SEID.
    fn require_stream(&self, remote_seid: u8, states: &[StreamState]) -> Result<u8, SimbleError> {
        let endpoint = self
            .local_endpoint_by_remote_seid(remote_seid)
            .ok_or_else(|| {
                SimbleError::DeviceError(format!(
                    "AVDTP: no stream configured with remote SEID {remote_seid}"
                ))
            })?;
        if !states.contains(&endpoint.state) {
            return Err(bad_local_state(endpoint.seid, endpoint.state));
        }
        Ok(endpoint.seid)
    }

    // -- Receive path ------------------------------------------------------

    /// Processes one incoming signaling PDU, returning outgoing PDUs plus
    /// any events. Malformed PDUs are dropped without a response, matching
    /// the assembler's tolerance policy (the spec defines no recovery for
    /// them below the message level).
    pub fn receive(&mut self, pdu: &[u8]) -> (Vec<Vec<u8>>, Vec<AvdtpEvent>) {
        let Some(assembled) = self.assembler.on_pdu(pdu) else {
            return (Vec::new(), Vec::new());
        };
        match assembled.message_type {
            MessageType::Command => self.on_command(&assembled),
            _ => (Vec::new(), self.on_response(&assembled)),
        }
    }

    fn respond(&self, transaction_label: u8, message: Message) -> Vec<Vec<u8>> {
        write_message(transaction_label, &message, self.peer_mtu)
    }

    fn on_command(&mut self, assembled: &AssembledMessage) -> (Vec<Vec<u8>>, Vec<AvdtpEvent>) {
        use signal_identifier as sig;
        // Signal identifier 0 is reserved; above DELAYREPORT is unknown and
        // draws a General Reject (AVDTP spec 8.18).
        if assembled.signal_identifier == 0 {
            return (Vec::new(), Vec::new());
        }
        if assembled.signal_identifier > sig::DELAYREPORT {
            return (
                self.respond(
                    assembled.transaction_label,
                    Message::GeneralReject {
                        signal_identifier: assembled.signal_identifier,
                    },
                ),
                Vec::new(),
            );
        }
        let Some(message) = Message::parse(
            assembled.signal_identifier,
            MessageType::Command,
            &assembled.payload,
        ) else {
            return (Vec::new(), Vec::new());
        };

        let mut events = Vec::new();
        let response = match message {
            Message::DiscoverCommand => Message::DiscoverResponse(
                self.local_endpoints.iter().map(|e| e.sep_info()).collect(),
            ),
            Message::GetCapabilitiesCommand { acp_seid } => {
                match self.get_local_endpoint_by_seid(acp_seid) {
                    None => reject(sig::GET_CAPABILITIES, error_code::BAD_ACP_SEID),
                    Some(endpoint) => {
                        Message::GetCapabilitiesResponse(endpoint.capabilities.clone())
                    }
                }
            }
            Message::GetAllCapabilitiesCommand { acp_seid } => {
                match self.get_local_endpoint_by_seid(acp_seid) {
                    None => reject(sig::GET_ALL_CAPABILITIES, error_code::BAD_ACP_SEID),
                    Some(endpoint) => {
                        Message::GetAllCapabilitiesResponse(endpoint.capabilities.clone())
                    }
                }
            }
            Message::SetConfigurationCommand {
                acp_seid,
                int_seid,
                capabilities,
            } => self.on_set_configuration_command(acp_seid, int_seid, capabilities, &mut events),
            Message::GetConfigurationCommand { acp_seid } => {
                match self.get_local_endpoint_by_seid(acp_seid) {
                    None => reject(sig::GET_CONFIGURATION, error_code::BAD_ACP_SEID),
                    Some(endpoint)
                        if matches!(
                            endpoint.state,
                            StreamState::Configured | StreamState::Open | StreamState::Streaming
                        ) =>
                    {
                        Message::GetConfigurationResponse(endpoint.configuration.clone())
                    }
                    Some(_) => reject(sig::GET_CONFIGURATION, error_code::BAD_STATE),
                }
            }
            Message::ReconfigureCommand {
                acp_seid,
                capabilities,
            } => self.on_reconfigure_command(acp_seid, capabilities, &mut events),
            Message::OpenCommand { acp_seid } => match self.get_local_endpoint_mut(acp_seid) {
                None => reject(sig::OPEN, error_code::BAD_ACP_SEID),
                Some(endpoint) if endpoint.state == StreamState::Configured => {
                    endpoint.state = StreamState::Open;
                    events.push(AvdtpEvent::StreamOpened { seid: acp_seid });
                    Message::OpenResponse
                }
                Some(_) => reject(sig::OPEN, error_code::BAD_STATE),
            },
            Message::StartCommand { acp_seids } => self.on_start_or_suspend_command(
                &acp_seids,
                StreamState::Open,
                StreamState::Streaming,
                &mut events,
            ),
            Message::SuspendCommand { acp_seids } => self.on_start_or_suspend_command(
                &acp_seids,
                StreamState::Streaming,
                StreamState::Open,
                &mut events,
            ),
            Message::CloseCommand { acp_seid } => match self.get_local_endpoint_mut(acp_seid) {
                None => reject(sig::CLOSE, error_code::BAD_ACP_SEID),
                Some(endpoint)
                    if matches!(endpoint.state, StreamState::Open | StreamState::Streaming) =>
                {
                    endpoint.state = StreamState::Idle;
                    endpoint.remote_seid = None;
                    self.detach_media_by_seid(acp_seid);
                    events.push(AvdtpEvent::StreamClosed { seid: acp_seid });
                    Message::CloseResponse
                }
                Some(_) => reject(sig::CLOSE, error_code::BAD_STATE),
            },
            Message::AbortCommand { acp_seid } => {
                // Abort always accepts, even for an unknown SEID
                // (AVDTP spec 8.16: no reject is defined for Abort).
                if let Some(endpoint) = self.get_local_endpoint_mut(acp_seid)
                    && endpoint.in_use()
                {
                    endpoint.state = StreamState::Idle;
                    endpoint.remote_seid = None;
                    self.detach_media_by_seid(acp_seid);
                    events.push(AvdtpEvent::StreamAborted { seid: acp_seid });
                }
                Message::AbortResponse
            }
            Message::SecurityControlCommand { acp_seid, data } => {
                match self.get_local_endpoint_by_seid(acp_seid) {
                    None => reject(sig::SECURITY_CONTROL, error_code::BAD_ACP_SEID),
                    Some(_) => {
                        events.push(AvdtpEvent::SecurityControl {
                            seid: acp_seid,
                            data,
                        });
                        Message::SecurityControlResponse
                    }
                }
            }
            Message::DelayReportCommand { acp_seid, delay } => {
                match self.get_local_endpoint_by_seid(acp_seid) {
                    None => reject(sig::DELAYREPORT, error_code::BAD_ACP_SEID),
                    Some(_) => {
                        events.push(AvdtpEvent::DelayReport {
                            seid: acp_seid,
                            delay,
                        });
                        Message::DelayReportResponse
                    }
                }
            }
            _ => return (Vec::new(), Vec::new()),
        };
        (self.respond(assembled.transaction_label, response), events)
    }

    fn on_set_configuration_command(
        &mut self,
        acp_seid: u8,
        int_seid: u8,
        capabilities: Vec<ServiceCapability>,
        events: &mut Vec<AvdtpEvent>,
    ) -> Message {
        let Some(endpoint) = self.get_local_endpoint_mut(acp_seid) else {
            return Message::SetConfigurationReject {
                service_category: 0,
                error_code: error_code::BAD_ACP_SEID,
            };
        };
        if endpoint.in_use() {
            return Message::SetConfigurationReject {
                service_category: 0,
                error_code: error_code::SEP_IN_USE,
            };
        }
        endpoint.configuration = capabilities;
        endpoint.remote_seid = Some(int_seid);
        endpoint.state = StreamState::Configured;
        events.push(AvdtpEvent::StreamConfigured { seid: acp_seid });
        Message::SetConfigurationResponse
    }

    fn on_reconfigure_command(
        &mut self,
        acp_seid: u8,
        capabilities: Vec<ServiceCapability>,
        events: &mut Vec<AvdtpEvent>,
    ) -> Message {
        let Some(endpoint) = self.get_local_endpoint_mut(acp_seid) else {
            return Message::ReconfigureReject {
                service_category: 0,
                error_code: error_code::BAD_ACP_SEID,
            };
        };
        // Reconfigure is only legal in the OPEN state (AVDTP spec 8.11).
        if endpoint.state != StreamState::Open {
            return Message::ReconfigureReject {
                service_category: 0,
                error_code: error_code::BAD_STATE,
            };
        }
        apply_reconfiguration(&mut endpoint.configuration, capabilities);
        events.push(AvdtpEvent::StreamReconfigured { seid: acp_seid });
        Message::ReconfigureResponse
    }

    fn on_start_or_suspend_command(
        &mut self,
        acp_seids: &[u8],
        expected: StreamState,
        next: StreamState,
        events: &mut Vec<AvdtpEvent>,
    ) -> Message {
        let signal = if expected == StreamState::Open {
            signal_identifier::START
        } else {
            signal_identifier::SUSPEND
        };
        let make_reject = |acp_seid, error| {
            if signal == signal_identifier::START {
                Message::StartReject {
                    acp_seid,
                    error_code: error,
                }
            } else {
                Message::SuspendReject {
                    acp_seid,
                    error_code: error,
                }
            }
        };
        for &seid in acp_seids {
            match self.get_local_endpoint_by_seid(seid) {
                None => return make_reject(seid, error_code::BAD_ACP_SEID),
                Some(endpoint) if endpoint.state != expected => {
                    return make_reject(seid, error_code::BAD_STATE);
                }
                Some(_) => {}
            }
        }
        for &seid in acp_seids {
            if let Some(endpoint) = self.get_local_endpoint_mut(seid) {
                endpoint.state = next;
            }
            events.push(if next == StreamState::Streaming {
                AvdtpEvent::StreamStarted { seid }
            } else {
                AvdtpEvent::StreamSuspended { seid }
            });
        }
        if signal == signal_identifier::START {
            Message::StartResponse
        } else {
            Message::SuspendResponse
        }
    }

    fn on_response(&mut self, assembled: &AssembledMessage) -> Vec<AvdtpEvent> {
        let Some(pending) = self.pending.remove(&assembled.transaction_label) else {
            return Vec::new();
        };
        if assembled.signal_identifier != pending.signal_identifier(self.version)
            && assembled.message_type != MessageType::GeneralReject
        {
            return Vec::new();
        }
        if assembled.message_type != MessageType::ResponseAccept {
            let error = Message::parse(
                assembled.signal_identifier,
                assembled.message_type,
                &assembled.payload,
            );
            let error_code = match error {
                Some(Message::Reject { error_code, .. })
                | Some(Message::SetConfigurationReject { error_code, .. })
                | Some(Message::ReconfigureReject { error_code, .. })
                | Some(Message::StartReject { error_code, .. })
                | Some(Message::SuspendReject { error_code, .. }) => error_code,
                _ => 0,
            };
            return vec![AvdtpEvent::CommandRejected {
                signal_identifier: pending.signal_identifier(self.version),
                error_code,
            }];
        }
        let Some(message) = Message::parse(
            assembled.signal_identifier,
            MessageType::ResponseAccept,
            &assembled.payload,
        ) else {
            return Vec::new();
        };
        match (pending, message) {
            (Pending::Discover, Message::DiscoverResponse(endpoints)) => {
                self.remote_endpoints = endpoints
                    .iter()
                    .map(|info| {
                        (
                            info.seid,
                            DiscoveredStreamEndPoint {
                                seid: info.seid,
                                media_type: info.media_type,
                                tsep: info.tsep,
                                in_use: info.in_use,
                                capabilities: Vec::new(),
                            },
                        )
                    })
                    .collect();
                vec![AvdtpEvent::EndpointsDiscovered(endpoints)]
            }
            (
                Pending::GetCapabilities { seid },
                Message::GetCapabilitiesResponse(capabilities)
                | Message::GetAllCapabilitiesResponse(capabilities),
            ) => {
                if let Some(endpoint) = self.remote_endpoints.get_mut(&seid) {
                    endpoint.capabilities = capabilities.clone();
                }
                vec![AvdtpEvent::CapabilitiesReceived { seid, capabilities }]
            }
            (
                Pending::SetConfiguration {
                    acp_seid,
                    int_seid,
                    capabilities,
                },
                Message::SetConfigurationResponse,
            ) => {
                if let Some(endpoint) = self.get_local_endpoint_mut(int_seid) {
                    endpoint.configuration = capabilities;
                    endpoint.remote_seid = Some(acp_seid);
                    endpoint.state = StreamState::Configured;
                }
                vec![AvdtpEvent::StreamConfigured { seid: int_seid }]
            }
            (
                Pending::GetConfiguration { seid },
                Message::GetConfigurationResponse(capabilities),
            ) => {
                vec![AvdtpEvent::ConfigurationReceived { seid, capabilities }]
            }
            (
                Pending::Reconfigure {
                    int_seid,
                    capabilities,
                },
                Message::ReconfigureResponse,
            ) => {
                if let Some(endpoint) = self.get_local_endpoint_mut(int_seid) {
                    apply_reconfiguration(&mut endpoint.configuration, capabilities);
                }
                vec![AvdtpEvent::StreamReconfigured { seid: int_seid }]
            }
            (Pending::Open { int_seid }, Message::OpenResponse) => {
                if let Some(endpoint) = self.get_local_endpoint_mut(int_seid) {
                    endpoint.state = StreamState::Open;
                }
                vec![AvdtpEvent::StreamOpened { seid: int_seid }]
            }
            (Pending::Start { int_seids }, Message::StartResponse) => int_seids
                .into_iter()
                .map(|seid| {
                    if let Some(endpoint) = self.get_local_endpoint_mut(seid) {
                        endpoint.state = StreamState::Streaming;
                    }
                    AvdtpEvent::StreamStarted { seid }
                })
                .collect(),
            (Pending::Suspend { int_seids }, Message::SuspendResponse) => int_seids
                .into_iter()
                .map(|seid| {
                    if let Some(endpoint) = self.get_local_endpoint_mut(seid) {
                        endpoint.state = StreamState::Open;
                    }
                    AvdtpEvent::StreamSuspended { seid }
                })
                .collect(),
            (Pending::Close { int_seid }, Message::CloseResponse) => {
                if let Some(endpoint) = self.get_local_endpoint_mut(int_seid) {
                    endpoint.state = StreamState::Idle;
                    endpoint.remote_seid = None;
                }
                self.detach_media_by_seid(int_seid);
                vec![AvdtpEvent::StreamClosed { seid: int_seid }]
            }
            (Pending::Abort { int_seid }, Message::AbortResponse) => {
                if let Some(endpoint) = self.get_local_endpoint_mut(int_seid) {
                    endpoint.state = StreamState::Idle;
                    endpoint.remote_seid = None;
                }
                self.detach_media_by_seid(int_seid);
                vec![AvdtpEvent::StreamAborted { seid: int_seid }]
            }
            _ => Vec::new(),
        }
    }
}

fn reject(signal_identifier: u8, error: u8) -> Message {
    Message::Reject {
        signal_identifier,
        error_code: error,
    }
}

/// Replaces the reconfigured categories in `configuration`, leaving other
/// categories (notably Media Transport) untouched, per AVDTP spec 8.11.1
/// (only application service capabilities may be reconfigured).
fn apply_reconfiguration(
    configuration: &mut Vec<ServiceCapability>,
    capabilities: Vec<ServiceCapability>,
) {
    for capability in capabilities {
        if let Some(existing) = configuration
            .iter_mut()
            .find(|c| c.service_category == capability.service_category)
        {
            *existing = capability;
        } else {
            configuration.push(capability);
        }
    }
}

fn unknown_seid(seid: u8) -> SimbleError {
    SimbleError::DeviceError(format!("AVDTP: unknown local SEID {seid}"))
}

fn bad_local_state(seid: u8, state: StreamState) -> SimbleError {
    SimbleError::DeviceError(format!(
        "AVDTP: local SEID {seid} is in state {state:?}, invalid for this operation"
    ))
}

// ---------------------------------------------------------------------------
// L2CAP / SDP integration
// ---------------------------------------------------------------------------

/// Allocates the AVDTP signaling L2CAP channel (initiator role), returning
/// its local CID plus the `Connection Request` to send.
pub fn connect_channel(
    manager: &mut ClassicChannelManager,
    mtu: u16,
) -> Result<(u16, ConnectionRequestHeader), SimbleError> {
    manager.connect(&ClassicChannelSpec::with_mtu(AVDTP_PSM, mtu))
}

/// Registers the AVDTP PSM as accepting incoming connections.
pub fn register_server(manager: &mut ClassicChannelManager) -> Result<(), SimbleError> {
    manager.register_server(AVDTP_PSM)
}

/// Searches for an A2DP service via `sdp_client`/`transport` and returns
/// its advertised AVDTP version, or `None` if the peer has none.
pub fn find_avdtp_service(
    sdp_client: &mut SdpClient,
    mut transport: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<Option<(u8, u8)>, SimbleError> {
    let results = sdp_client.search_attributes(
        &[ADVANCED_AUDIO_DISTRIBUTION_SERVICE_UUID],
        &[AttributeIdSpec::Single(
            attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
        )],
        &mut transport,
    )?;
    for attributes in results {
        for attribute in &attributes {
            if attribute.id != attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST {
                continue;
            }
            let Some(descriptors) = attribute.value.as_sequence() else {
                continue;
            };
            for descriptor in descriptors {
                if let Some(items) = descriptor.as_sequence()
                    && let Some((version, _)) =
                        items.get(1).and_then(DataElement::as_unsigned_integer)
                {
                    return Ok(Some(((version >> 8) as u8, version as u8)));
                }
            }
        }
    }
    Ok(None)
}
