// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! A2DP as [`ProtocolHandler`]s — the two ends of a Bluetooth speaker.
//!
//! [`crate::classic::avdtp`] has been a complete signalling state machine
//! and [`crate::audio::sbc`] a `libsbc`-verified codec for as long as either
//! has existed, and nothing could host them: a scene's BR/EDR device
//! registers `Box<dyn ProtocolHandler>`s, and neither module was one. These
//! two types are that seam, and nothing else — every protocol decision below
//! them still belongs to `avdtp`.
//!
//! ## The two channels
//!
//! AVDTP runs **signalling** and **media transport** on the same PSM
//! (0x0019) as separate L2CAP channels. That is why [`ProtocolHandler`]
//! carries a [`HandlerChannel`] rather than only a PSM: the first channel to
//! open is signalling, the second is the media transport for whichever
//! stream just went OPEN, and no byte on the wire says so. The sequence is
//! fixed by the profile — AVDTP §5.4.6: OPEN succeeds, *then* the initiator
//! opens the transport channel — so "the next 0x0019 channel after an OPEN"
//! is not a guess, it is the specification.
//!
//! ## What is here and what is not
//!
//! [`A2dpSink`] renders: it answers Discover / Get_Capabilities /
//! Set_Configuration / Open / Start, accepts the transport channel, and
//! hands whole SBC frames up through [`A2dpSink::take_frames`]. It decodes
//! them with [`crate::audio::sbc::SbcDecoder`] when asked to
//! ([`A2dpSink::decode`]), which is the only thing in simble that turns a
//! Bluetooth payload back into audio.
//!
//! [`A2dpSource`] is the phone: it opens signalling, discovers the peer's
//! endpoints, configures the one advertising SBC, opens it, opens the
//! transport channel, starts the stream, and packetises whatever PCM it is
//! given.
//!
//! Not modelled: content protection (SCMS-T), delay reporting as anything
//! but an event, codec fallback (a source that finds no SBC sink gives up
//! rather than trying AAC), and any notion of real time — a source sends
//! when it is polled, not on a clock.

use std::collections::HashMap;

use crate::classic::a2dp::{MediaCodecInformation, SbcMediaCodecInformation, codec_type, sbc};
use crate::classic::avdtp::{
    AVDTP_PSM, AvdtpEvent, MediaCodecCapabilities, MediaFrame, MediaType, Protocol,
    ServiceCapability, StreamEndPointType, StreamState,
};
use crate::device::classic_host::{HandlerChannel, ProtocolHandler};

/// The L2CAP MTU an A2DP endpoint offers. AVDTP needs room for a whole SBC
/// frame plus RTP and A2DP headers; 672 is the Classic default and holds
/// several 44.1 kHz joint-stereo frames.
const A2DP_MTU: u16 = 672;

/// The SBC capability an endpoint advertises when it is willing to take
/// anything the codec can do: every sampling frequency, every channel mode,
/// every block length and subband count, both allocation methods, and the
/// bitpool range A2DP §4.3.2.6 recommends for 44.1 kHz.
pub fn sbc_full_capability() -> SbcMediaCodecInformation {
    use sbc::{allocation_method, block_length, channel_mode, sampling_frequency, subbands};
    SbcMediaCodecInformation {
        sampling_frequency: sampling_frequency::SF_16000
            | sampling_frequency::SF_32000
            | sampling_frequency::SF_44100
            | sampling_frequency::SF_48000,
        channel_mode: channel_mode::MONO
            | channel_mode::DUAL_CHANNEL
            | channel_mode::STEREO
            | channel_mode::JOINT_STEREO,
        block_length: block_length::BL_4
            | block_length::BL_8
            | block_length::BL_12
            | block_length::BL_16,
        subbands: subbands::S_4 | subbands::S_8,
        allocation_method: allocation_method::SNR | allocation_method::LOUDNESS,
        minimum_bitpool_value: 2,
        maximum_bitpool_value: 53,
    }
}

/// The single operating point a source configures a stream at: 44.1 kHz
/// joint stereo, 16 blocks, 8 subbands, loudness allocation, bitpool 53.
/// This is what essentially every phone picks, and what
/// [`crate::audio::sbc::SbcParameters::joint_stereo_44100`] encodes at.
fn sbc_high_quality_configuration() -> SbcMediaCodecInformation {
    use sbc::{allocation_method, block_length, channel_mode, sampling_frequency, subbands};
    SbcMediaCodecInformation {
        sampling_frequency: sampling_frequency::SF_44100,
        channel_mode: channel_mode::JOINT_STEREO,
        block_length: block_length::BL_16,
        subbands: subbands::S_8,
        allocation_method: allocation_method::LOUDNESS,
        minimum_bitpool_value: 2,
        maximum_bitpool_value: 53,
    }
}

fn sbc_capabilities(information: SbcMediaCodecInformation) -> MediaCodecCapabilities {
    MediaCodecCapabilities {
        media_type: MediaType::Audio,
        media_codec_type: codec_type::SBC,
        media_codec_information: information.to_bytes().to_vec(),
    }
}

/// What came back out of [`A2dpSink::decode`].
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DecodedAudio {
    /// Interleaved PCM, one `i16` per sample per channel.
    pub pcm: Vec<i16>,
    /// Whole SBC frames decoded.
    pub frames: usize,
    /// Bytes that were not the start of a whole SBC frame. Non-zero means
    /// the stream was truncated or mis-framed, which is worth being able to
    /// see rather than inferring from a short PCM buffer.
    pub undecodable_bytes: usize,
}

// ---------------------------------------------------------------------------
// Sink
// ---------------------------------------------------------------------------

/// An A2DP **sink** — the speaker. Registered on [`AVDTP_PSM`], it accepts
/// the signalling channel, answers the whole AVDTP acceptor sequence, takes
/// the media transport channel that follows OPEN, and collects the SBC
/// frames that arrive on it.
///
/// It initiates nothing. Everything it does is an answer, which is what
/// makes it the honest half of the pair to test a foreign source against.
#[derive(Debug)]
pub struct A2dpSink {
    avdtp: Protocol,
    /// The local SEID of the one sink endpoint this speaker offers.
    seid: u8,
    /// The signalling channel, once it has opened. The *first* 0x0019
    /// channel is signalling by definition — there is no other way to tell.
    signalling_cid: Option<u16>,
    /// CID of the media transport channel, once attached.
    media_cid: Option<u16>,
    /// The endpoint whose transport channel is expected next: set when OPEN
    /// succeeds, cleared when the channel arrives. Without it the second
    /// 0x0019 channel could not be bound to any stream.
    awaiting_media_for: Option<u8>,
    /// Frames received and not yet taken.
    frames: Vec<MediaFrame>,
    /// Every AVDTP event this endpoint has seen, in order — the sequence a
    /// test asserts on.
    events: Vec<AvdtpEvent>,
}

impl Default for A2dpSink {
    fn default() -> Self {
        Self::new()
    }
}

impl A2dpSink {
    /// A speaker offering one SBC sink endpoint that accepts anything the
    /// codec can express.
    pub fn new() -> Self {
        Self::with_capability(sbc_full_capability())
    }

    /// A speaker offering one SBC sink endpoint with `capability` — the way
    /// to build a fussy sink, which is what a Set_Configuration rejection
    /// test needs.
    pub fn with_capability(capability: SbcMediaCodecInformation) -> Self {
        let mut avdtp = Protocol::new(A2DP_MTU);
        let seid = avdtp.add_sink(sbc_capabilities(capability));
        Self {
            avdtp,
            seid,
            signalling_cid: None,
            media_cid: None,
            awaiting_media_for: None,
            frames: Vec::new(),
            events: Vec::new(),
        }
    }

    /// The SEID of this sink's endpoint.
    pub fn seid(&self) -> u8 {
        self.seid
    }

    /// The endpoint's AVDTP stream state — IDLE, CONFIGURED, OPEN,
    /// STREAMING. This is the assertion a rejection test makes: a refused
    /// command must leave it where it was.
    pub fn state(&self) -> StreamState {
        self.avdtp
            .get_local_endpoint_by_seid(self.seid)
            .map(|endpoint| endpoint.state)
            .unwrap_or(StreamState::Idle)
    }

    /// The codec configuration the peer set, once Set_Configuration has been
    /// accepted. `None` before that, and after a Close.
    pub fn configuration(&self) -> Option<SbcMediaCodecInformation> {
        let endpoint = self.avdtp.get_local_endpoint_by_seid(self.seid)?;
        if endpoint.state == StreamState::Idle {
            return None;
        }
        endpoint
            .configuration
            .iter()
            .filter_map(sbc_information_of)
            .next()
    }

    /// Whether a media transport channel is attached — i.e. whether media
    /// *could* arrive. A stream that is STREAMING with no transport channel
    /// is the failure this distinguishes from silence.
    pub fn has_media_channel(&self) -> bool {
        self.media_cid.is_some() && self.avdtp.has_media_channel(self.seid)
    }

    /// Drains the SBC frames received so far, oldest first.
    pub fn take_frames(&mut self) -> Vec<MediaFrame> {
        std::mem::take(&mut self.frames)
    }

    /// How many frames have been received in total, taken or not.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Every AVDTP event this endpoint has seen, in order.
    pub fn events(&self) -> &[AvdtpEvent] {
        &self.events
    }

    /// Commands this endpoint refused, as `(signal identifier, error code)`,
    /// read back out of the event log.
    pub fn rejections(&self) -> Vec<(u8, u8)> {
        self.events
            .iter()
            .filter_map(|event| match event {
                AvdtpEvent::CommandRefused {
                    signal_identifier,
                    error_code,
                } => Some((*signal_identifier, *error_code)),
                _ => None,
            })
            .collect()
    }

    /// Decodes everything taken from [`Self::take_frames`] to interleaved
    /// PCM.
    ///
    /// This is the end of the audio path, and the reason it is worth having:
    /// the decoder is verified against bluez's `libsbc` in both directions,
    /// so PCM coming out of here is evidence the bytes that crossed the link
    /// were a real SBC stream rather than plausible-looking noise.
    pub fn decode(frames: &[MediaFrame]) -> DecodedAudio {
        let mut decoder = crate::audio::sbc::SbcDecoder::new();
        let mut audio = DecodedAudio::default();
        for frame in frames {
            let mut rest = frame.payload.as_slice();
            while let Ok((_, samples, remainder)) = decoder.decode(rest) {
                audio.pcm.extend_from_slice(&samples);
                audio.frames += 1;
                rest = remainder;
            }
            // Whatever is left is not the start of a whole frame. On a
            // lossless link that is a packetiser bug, so it is counted
            // rather than dropped quietly.
            audio.undecodable_bytes += rest.len();
        }
        audio
    }

    /// Notes what an AVDTP event means for this handler's own bookkeeping.
    fn absorb(&mut self, events: Vec<AvdtpEvent>) {
        for event in &events {
            match event {
                AvdtpEvent::StreamOpened { seid } => self.awaiting_media_for = Some(*seid),
                AvdtpEvent::StreamClosed { seid } | AvdtpEvent::StreamAborted { seid } => {
                    if self.awaiting_media_for == Some(*seid) {
                        self.awaiting_media_for = None;
                    }
                    self.media_cid = None;
                }
                _ => {}
            }
        }
        self.events.extend(events);
    }
}

impl ProtocolHandler for A2dpSink {
    fn psm(&self) -> u16 {
        AVDTP_PSM
    }

    /// Never called: AVDTP runs two channels on one PSM, so every SDU is
    /// routed by CID through [`ProtocolHandler::on_channel_data`]. A reply
    /// built without knowing which channel spoke would be an answer to
    /// signalling put on the media transport, or the reverse.
    fn on_data(&mut self, _data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        Vec::new()
    }

    fn on_channel_open(&mut self, channel: HandlerChannel) {
        if channel.psm != AVDTP_PSM {
            return;
        }
        if self.signalling_cid.is_none() {
            self.signalling_cid = Some(channel.cid);
            return;
        }
        // The second channel is the transport for the stream that just went
        // OPEN. If nothing is OPEN, the peer has opened a channel AVDTP does
        // not permit yet; leave it unattached rather than binding it to a
        // stream it does not belong to, and media on it will be refused.
        let Some(seid) = self.awaiting_media_for.take() else {
            return;
        };
        if self.avdtp.attach_media_channel(seid, channel.cid).is_ok() {
            self.media_cid = Some(channel.cid);
        }
    }

    fn on_channel_lost(&mut self, cid: u16) {
        if self.media_cid == Some(cid) {
            self.avdtp.detach_media_channel(cid);
            self.media_cid = None;
            return;
        }
        if self.signalling_cid == Some(cid) {
            self.signalling_cid = None;
        }
    }

    fn on_channel_data(&mut self, channel: HandlerChannel, data: &[u8]) -> Vec<Vec<u8>> {
        if Some(channel.cid) == self.media_cid {
            // Media never draws a reply: RTP over AVDTP is one-way, and a
            // malformed packet is dropped by the layer that knows the
            // framing rather than guessed at here.
            if self.avdtp.on_media_pdu(channel.cid, data).is_ok() {
                self.frames.extend(self.avdtp.take_media());
            }
            return Vec::new();
        }
        let (out, events) = self.avdtp.receive(data);
        self.absorb(events);
        out
    }

    fn on_channel_closed(&mut self) {
        // The link is gone: the whole session goes with it, including the
        // stream state. A speaker that stayed STREAMING after its peer
        // walked away would refuse the next peer's Set_Configuration with
        // BAD_STATE, which is the shape of bug this exists to prevent.
        let capability = self
            .avdtp
            .get_local_endpoint_by_seid(self.seid)
            .and_then(|endpoint| endpoint.capabilities.iter().find_map(sbc_information_of))
            .unwrap_or_else(sbc_full_capability);
        let mut fresh = Self::with_capability(capability);
        std::mem::swap(self, &mut fresh);
        // Keep the record: what happened to the previous peer is evidence,
        // and a test that reconnects wants to see both halves.
        self.events = fresh.events;
        self.frames = fresh.frames;
    }
}

// ---------------------------------------------------------------------------
// Source
// ---------------------------------------------------------------------------

/// Where an [`A2dpSource`] has got to. Each step is entered only when the
/// AVDTP *response* for the previous one arrived, so a source stuck in
/// `Configuring` has been left without a Set_Configuration response — not
/// refused, which is `Failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourcePhase {
    /// No signalling channel yet.
    Connecting,
    /// Signalling is open; Discover has been sent.
    Discovering,
    /// A remote sink was found; Get_Capabilities has been sent.
    ReadingCapabilities,
    /// Capabilities arrived; Set_Configuration has been sent.
    Configuring,
    /// Configured; Open has been sent.
    Opening,
    /// Open succeeded; the media transport channel has been asked for.
    OpeningTransport,
    /// The transport channel is attached; Start has been sent.
    Starting,
    /// STREAMING. Media queued with [`A2dpSource::queue_pcm`] leaves here.
    Streaming,
    /// The peer refused something, or offered no SBC sink; see
    /// [`A2dpSource::error`].
    Failed,
}

impl SourcePhase {
    /// Stable identifier for a status document.
    pub fn name(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Discovering => "discovering",
            Self::ReadingCapabilities => "reading-capabilities",
            Self::Configuring => "configuring",
            Self::Opening => "opening",
            Self::OpeningTransport => "opening-transport",
            Self::Starting => "starting",
            Self::Streaming => "streaming",
            Self::Failed => "failed",
        }
    }
}

/// An A2DP **source** — the phone. It asks the host for its own signalling
/// channel, runs the initiator sequence to STREAMING, and then encodes
/// whatever PCM it is handed into SBC and sends it.
pub struct A2dpSource {
    avdtp: Protocol,
    /// The local SEID of the one source endpoint.
    seid: u8,
    phase: SourcePhase,
    signalling_cid: Option<u16>,
    media_cid: Option<u16>,
    /// The peer endpoint being configured, once Discover has answered.
    remote_seid: Option<u8>,
    /// PSMs still to ask the host for. Drained by `poll_channel_requests`.
    wanted_channels: Vec<u16>,
    /// SDUs waiting to go out, keyed by the CID they belong on. Signalling
    /// PDUs and media packets are both here and must not be confused: this
    /// is the whole reason `poll_channel_output` is told which channel it is
    /// being asked about.
    outbound: HashMap<u16, Vec<Vec<u8>>>,
    /// PCM waiting to be encoded, interleaved.
    pcm: Vec<i16>,
    /// The encoder, built once the configuration is settled.
    encoder: Option<crate::audio::sbc::SbcEncoder>,
    /// RTP timestamp in codec sample units — one frame's worth of samples
    /// per frame sent, which is what a real source advances by.
    timestamp: u32,
    /// A configuration to propose without negotiating; see
    /// [`Self::misconfigure_with`].
    forced_configuration: Option<SbcMediaCodecInformation>,
    /// Media packets actually written to the transport channel.
    packets_sent: usize,
    events: Vec<AvdtpEvent>,
    error: Option<String>,
}

impl Default for A2dpSource {
    fn default() -> Self {
        Self::new()
    }
}

/// Hand-written because [`crate::audio::sbc::SbcEncoder`] carries kilobytes
/// of filter history that nobody wants printed — and `ProtocolHandler`
/// requires `Debug`.
impl std::fmt::Debug for A2dpSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("A2dpSource")
            .field("phase", &self.phase.name())
            .field("seid", &self.seid)
            .field("remote_seid", &self.remote_seid)
            .field("signalling_cid", &self.signalling_cid)
            .field("media_cid", &self.media_cid)
            .field("pcm_queued", &self.pcm.len())
            .field("packets_sent", &self.packets_sent)
            .field("error", &self.error)
            .finish()
    }
}

impl A2dpSource {
    /// A phone offering one SBC source endpoint, with delay reporting.
    pub fn new() -> Self {
        let mut avdtp = Protocol::new(A2DP_MTU);
        let seid = avdtp.add_source(sbc_capabilities(sbc_full_capability()), true);
        Self {
            avdtp,
            seid,
            phase: SourcePhase::Connecting,
            signalling_cid: None,
            media_cid: None,
            remote_seid: None,
            // The signalling channel is the first thing it needs, and it
            // cannot open one itself — the host owns L2CAP.
            wanted_channels: vec![AVDTP_PSM],
            outbound: HashMap::new(),
            pcm: Vec::new(),
            encoder: None,
            timestamp: 0,
            forced_configuration: None,
            packets_sent: 0,
            events: Vec::new(),
            error: None,
        }
    }

    /// How far the stream setup has got.
    pub fn phase(&self) -> SourcePhase {
        self.phase
    }

    /// Why setup stopped, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Every AVDTP event this endpoint has seen, in order.
    pub fn events(&self) -> &[AvdtpEvent] {
        &self.events
    }

    /// Media packets written to the transport channel so far.
    pub fn packets_sent(&self) -> usize {
        self.packets_sent
    }

    /// Queues interleaved stereo PCM to be encoded and sent. Anything queued
    /// before the stream is STREAMING waits; nothing is dropped, because a
    /// source that silently discarded the first second of a track would look
    /// exactly like one whose stream started late.
    pub fn queue_pcm(&mut self, samples: &[i16]) {
        self.pcm.extend_from_slice(samples);
    }

    /// Interleaved PCM samples still waiting to be encoded.
    pub fn pcm_queued(&self) -> usize {
        self.pcm.len()
    }

    /// Sends `configuration` regardless of what the peer said it can do,
    /// instead of intersecting first.
    ///
    /// A well-behaved source never needs this: it reads Get_Capabilities and
    /// proposes something inside them. Real ones are not all well-behaved,
    /// and a sink's refusal path is unreachable from a peer that never asks
    /// for anything wrong — which is how a sink that accepted *any*
    /// configuration went unnoticed. This is the peer that asks.
    pub fn misconfigure_with(&mut self, configuration: SbcMediaCodecInformation) {
        self.forced_configuration = Some(configuration);
    }

    /// The SBC parameters this stream settled on, once configured.
    pub fn parameters(&self) -> Option<crate::audio::sbc::SbcParameters> {
        self.encoder.as_ref().map(|encoder| *encoder.parameters())
    }

    fn fail(&mut self, reason: impl Into<String>) {
        self.error = Some(reason.into());
        self.phase = SourcePhase::Failed;
    }

    /// Queues signalling PDUs on the signalling channel.
    fn send_signalling(&mut self, result: Result<Vec<Vec<u8>>, crate::types::SimbleError>) {
        let Some(cid) = self.signalling_cid else {
            self.fail("A2DP source: no signalling channel to send on");
            return;
        };
        match result {
            Ok(pdus) => self.outbound.entry(cid).or_default().extend(pdus),
            Err(e) => self.fail(e.to_string()),
        }
    }

    /// Drives the initiator sequence off one batch of AVDTP events.
    fn advance(&mut self, events: Vec<AvdtpEvent>) {
        for event in events {
            self.events.push(event.clone());
            match event {
                AvdtpEvent::CommandRejected {
                    signal_identifier,
                    error_code,
                } => {
                    self.fail(format!(
                        "peer rejected AVDTP signal {signal_identifier:#04x} with error \
                         {error_code:#04x}"
                    ));
                    return;
                }
                AvdtpEvent::EndpointsDiscovered(_) => {
                    // A Discover response carries no capabilities, so the
                    // choice here can only be made on role: take the lowest
                    // free audio sink and ask what codecs it has. Deciding
                    // on the codec first is impossible, however much
                    // `find_remote_sink_by_codec` looks like it would.
                    let candidate = self
                        .avdtp
                        .discovered_endpoints(MediaType::Audio, StreamEndPointType::Sink)
                        .first()
                        .copied();
                    let Some(remote) = candidate else {
                        self.fail("peer advertises no free audio sink endpoint");
                        return;
                    };
                    self.remote_seid = Some(remote);
                    self.phase = SourcePhase::ReadingCapabilities;
                    let pdus = self.avdtp.get_capabilities(remote);
                    self.send_signalling(pdus);
                }
                AvdtpEvent::CapabilitiesReceived { seid, capabilities } => {
                    let configured = match self.forced_configuration {
                        Some(forced) => forced,
                        None => {
                            let Some(peer_sbc) = capabilities.iter().find_map(sbc_information_of)
                            else {
                                self.fail(format!("peer endpoint {seid} offers no SBC codec"));
                                return;
                            };
                            let Some(agreed) =
                                sbc_high_quality_configuration().intersect(&peer_sbc)
                            else {
                                self.fail(format!(
                                    "no common SBC operating point with peer endpoint {seid}"
                                ));
                                return;
                            };
                            // `intersect` leaves a *set*; a configuration must
                            // name exactly one of each, so keep the single
                            // bits asked for and take the agreed bitpool range.
                            let mut configured = sbc_high_quality_configuration();
                            configured.minimum_bitpool_value = agreed.minimum_bitpool_value;
                            configured.maximum_bitpool_value = agreed.maximum_bitpool_value;
                            configured
                        }
                    };
                    match crate::audio::sbc::SbcEncoder::new(
                        crate::audio::sbc::SbcParameters::joint_stereo_44100(
                            configured.maximum_bitpool_value,
                        ),
                    ) {
                        Ok(encoder) => self.encoder = Some(encoder),
                        Err(e) => {
                            self.fail(format!("SBC encoder: {e}"));
                            return;
                        }
                    }
                    self.phase = SourcePhase::Configuring;
                    let int_seid = self.seid;
                    let pdus = self.avdtp.set_configuration(
                        seid,
                        int_seid,
                        vec![
                            ServiceCapability::media_transport(),
                            sbc_capabilities(configured).to_capability(),
                        ],
                    );
                    self.send_signalling(pdus);
                }
                AvdtpEvent::StreamConfigured { .. } => {
                    let Some(remote) = self.remote_seid else {
                        self.fail("configured with no remote endpoint recorded");
                        return;
                    };
                    self.phase = SourcePhase::Opening;
                    let pdus = self.avdtp.open(remote);
                    self.send_signalling(pdus);
                }
                AvdtpEvent::StreamOpened { .. } => {
                    // AVDTP 5.4.6: the transport channel is opened *after*
                    // OPEN succeeds, and by the initiator. Ask the host for
                    // it; the CID comes back at `on_channel_open`.
                    self.phase = SourcePhase::OpeningTransport;
                    self.wanted_channels.push(AVDTP_PSM);
                }
                AvdtpEvent::StreamStarted { .. } => self.phase = SourcePhase::Streaming,
                AvdtpEvent::StreamSuspended { .. } | AvdtpEvent::StreamClosed { .. } => {
                    self.phase = SourcePhase::Opening;
                }
                _ => {}
            }
        }
    }

    /// Encodes as much queued PCM as makes whole SBC frames and turns it
    /// into RTP packets on the transport channel.
    fn produce_media(&mut self) {
        let (Some(cid), Some(encoder)) = (self.media_cid, self.encoder.as_mut()) else {
            return;
        };
        if self.phase != SourcePhase::Streaming {
            return;
        }
        let pcm_len = encoder.pcm_len();
        if pcm_len == 0 {
            return;
        }
        let samples_per_frame = (pcm_len / encoder.parameters().channels().max(1)).max(1) as u32;
        let mut frames = Vec::new();
        let mut consumed = 0;
        while self.pcm.len() - consumed >= pcm_len {
            match encoder.encode(&self.pcm[consumed..consumed + pcm_len]) {
                Ok(frame) => frames.push(frame),
                Err(e) => {
                    self.error = Some(format!("SBC encode: {e}"));
                    break;
                }
            }
            consumed += pcm_len;
        }
        self.pcm.drain(..consumed);
        if frames.is_empty() {
            return;
        }
        let frame_count = frames.len() as u32;
        match self.avdtp.send_media(self.seid, &frames, self.timestamp) {
            Ok(packets) => {
                self.packets_sent += packets.len();
                self.timestamp = self.timestamp.wrapping_add(samples_per_frame * frame_count);
                self.outbound.entry(cid).or_default().extend(packets);
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }
}

impl ProtocolHandler for A2dpSource {
    fn psm(&self) -> u16 {
        AVDTP_PSM
    }

    /// Never called; see [`A2dpSink::on_data`].
    fn on_data(&mut self, _data: &[u8], _peer_mtu: u16) -> Vec<Vec<u8>> {
        Vec::new()
    }

    fn poll_channel_requests(&mut self) -> Vec<u16> {
        std::mem::take(&mut self.wanted_channels)
    }

    fn on_channel_open(&mut self, channel: HandlerChannel) {
        if channel.psm != AVDTP_PSM {
            return;
        }
        if self.signalling_cid.is_none() {
            self.signalling_cid = Some(channel.cid);
            self.phase = SourcePhase::Discovering;
            let pdus = self.avdtp.discover();
            self.send_signalling(pdus);
            return;
        }
        let Some(remote) = self.remote_seid else {
            return;
        };
        if self
            .avdtp
            .attach_media_channel(self.seid, channel.cid)
            .is_err()
        {
            self.fail("AVDTP refused the media transport channel");
            return;
        }
        self.media_cid = Some(channel.cid);
        self.phase = SourcePhase::Starting;
        let pdus = self.avdtp.start(&[remote]);
        self.send_signalling(pdus);
    }

    fn on_channel_lost(&mut self, cid: u16) {
        if self.media_cid == Some(cid) {
            self.avdtp.detach_media_channel(cid);
            self.media_cid = None;
        } else if self.signalling_cid == Some(cid) {
            self.signalling_cid = None;
        }
        self.outbound.remove(&cid);
    }

    fn on_channel_data(&mut self, channel: HandlerChannel, data: &[u8]) -> Vec<Vec<u8>> {
        if Some(channel.cid) == self.media_cid {
            // A source can legitimately receive media on a bidirectional
            // stream; this one is send-only, so anything arriving is noted
            // and dropped rather than fed to a decoder that has no sink.
            let _ = self.avdtp.on_media_pdu(channel.cid, data);
            self.avdtp.take_media();
            return Vec::new();
        }
        let (out, events) = self.avdtp.receive(data);
        self.advance(events);
        // Responses this endpoint owes the peer go straight back; anything
        // the sequence queued leaves through `poll_channel_output`.
        out
    }

    fn poll_channel_output(&mut self, channel: HandlerChannel) -> Vec<Vec<u8>> {
        if Some(channel.cid) == self.media_cid {
            self.produce_media();
        }
        self.outbound.remove(&channel.cid).unwrap_or_default()
    }

    fn on_channel_closed(&mut self) {
        let mut fresh = Self::new();
        std::mem::swap(self, &mut fresh);
        self.events = fresh.events;
        self.error = fresh.error;
        self.packets_sent = fresh.packets_sent;
        self.forced_configuration = fresh.forced_configuration;
        // Whatever was queued for the departed peer is still this device's
        // audio; it belongs to the next stream, not to the last one.
        self.pcm = fresh.pcm;
    }
}

/// Reads an SBC codec information element out of a service capability, if it
/// is one.
fn sbc_information_of(capability: &ServiceCapability) -> Option<SbcMediaCodecInformation> {
    let codec = MediaCodecCapabilities::from_capability(capability)?;
    if codec.media_codec_type != codec_type::SBC {
        return None;
    }
    match MediaCodecInformation::parse(codec.media_codec_type, &codec.media_codec_information)? {
        MediaCodecInformation::Sbc(information) => Some(information),
        _ => None,
    }
}
