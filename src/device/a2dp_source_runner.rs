// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The A2DP source's whole climb — inquiry, paging, pairing, encryption,
//! SDP, AVDTP, streaming — as one synchronous driver, shared between
//! `examples/a2dp_source.rs` (native, against a real speaker) and the wasm
//! `WebA2dpSource` (a browser page driving a dongle through the
//! `simble --usb` bridge).
//!
//! Extracted from the example rather than written fresh: the example's
//! ladder is the code that actually streamed to consumer hardware, and two
//! implementations of the same climb would disagree exactly where it
//! matters. Three things changed in the move:
//!
//! - **No wall clock.** `std::time::Instant` panics on
//!   `wasm32-unknown-unknown`, so every step takes `now_ms` from the caller
//!   — the same clock-passing shape `CarKit::tick` uses.
//! - **No `println!`.** Progress lands in a log the caller drains
//!   ([`Self::take_log`]); the example prints it, the worker posts it.
//! - **No built-in melody.** PCM comes from the caller via
//!   [`Self::queue_pcm`] and is metered out at real time by
//!   [`Self::feed`], because a source that dumps its whole track at once
//!   hands the controller more ACL data than its buffers hold.

use std::collections::VecDeque;

use crate::classic::avdtp::AvdtpEvent;
use crate::classic::sdp::SdpUuid;
use crate::device::a2dp::{A2dpSource, SourcePhase};
use crate::device::classic_host::{
    ClassicHost, DiscoveredDevice, SdpQueryHandler, SharedSdpQueryResults,
    authentication_requirements,
};
use crate::types::Address;

/// A2DP Audio Sink service class (Assigned Numbers): what the SDP query
/// searches the peer for.
const AUDIO_SINK_SERVICE_CLASS: SdpUuid = SdpUuid::Uuid16(0x110B);

/// How an inquiry result is recognised as a speaker rather than a laptop.
const AUDIO_VIDEO_MAJOR_CLASS: u8 = 0x04;

/// AVDTP's assigned PSM, for the "the peer named something else" note.
const AVDTP_PSM: u16 = 0x0019;

/// SDP's assigned PSM.
const SDP_PSM: u16 = 0x0001;

/// The sample rate PCM is expected at and the stream is negotiated at.
pub const SAMPLE_RATE: u32 = 44_100;

/// Class of Device 0x5A020C: Phone major class, Smartphone minor class —
/// what this end looks like to the speaker's own inquiry, if it runs one.
pub const PHONE_CLASS_OF_DEVICE: [u8; 3] = [0x0C, 0x02, 0x5A];

/// How far the climb has got. Naming every stage is the point: the useful
/// report is "it stopped *here*", not "it timed out".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SourceRung {
    /// Nothing sent yet.
    Starting,
    /// Inquiry running, looking for an Audio/Video device (or the target).
    Inquiring,
    /// The target answered without an EIR name; asking for one.
    ResolvingName,
    /// Create Connection sent, waiting for the ACL.
    Paging,
    /// SSP in progress.
    Pairing,
    /// Authentication done, Set Connection Encryption sent.
    Encrypting,
    /// The speaker's SDP server is being read for its Audio Sink record.
    QueryingSdp,
    /// AVDTP discovery/configuration against the PSM that record named.
    Avdtp,
    /// Media is leaving.
    Streaming,
    /// The caller ended the run.
    Done,
}

impl SourceRung {
    /// The stage as a person would read it in a report.
    pub fn label(self) -> &'static str {
        match self {
            SourceRung::Starting => "0. starting",
            SourceRung::Inquiring => "1. inquiry",
            SourceRung::ResolvingName => "1b. remote name",
            SourceRung::Paging => "2a. create connection",
            SourceRung::Pairing => "2. pairing (SSP)",
            SourceRung::Encrypting => "2b. encryption",
            SourceRung::QueryingSdp => "3. SDP (Audio Sink record)",
            SourceRung::Avdtp => "4. AVDTP (discover, capabilities, configure)",
            SourceRung::Streaming => "5. streaming SBC media",
            SourceRung::Done => "done",
        }
    }
}

/// The driver. Owns the [`ClassicHost`]; the caller owns the transport and
/// the clock, and moves packets both ways every turn:
///
/// 1. inbound packets → [`Self::handle_packet`]
/// 2. [`Self::step`] → outbound packets
/// 3. [`Self::feed`], then [`Self::poll`] → outbound packets
pub struct A2dpSourceRunner {
    host: ClassicHost,
    target: Option<Address>,
    target_was_given: bool,
    rung: SourceRung,
    highest: SourceRung,
    sdp_results: SharedSdpQueryResults,
    avdtp_psm: Option<u16>,
    pair: bool,
    streaming_since_ms: Option<f64>,
    /// Interleaved stereo PCM waiting to be metered into the encoder.
    pcm: VecDeque<i16>,
    samples_queued: usize,
    capabilities: Vec<(u8, u8, Vec<u8>)>,
    endpoints_reported: bool,
    negotiated: Option<String>,
    log: Vec<String>,
}

impl A2dpSourceRunner {
    /// Creates the driver. `target` is the speaker's address if known;
    /// `None` inquires and takes the first Audio/Video device that answers.
    pub fn new(target: Option<Address>, io_capability: u8, pair: bool) -> Self {
        let mut host = ClassicHost::new("simble-a2dp-source", PHONE_CLASS_OF_DEVICE);
        let (sdp, sdp_results) = SdpQueryHandler::searching(AUDIO_SINK_SERVICE_CLASS);
        host.register_handler(Box::new(sdp))
            .expect("the SDP client registers");
        host.set_io_capability(io_capability, authentication_requirements::GENERAL_BONDING);
        Self {
            host,
            target_was_given: target.is_some(),
            target,
            rung: SourceRung::Starting,
            highest: SourceRung::Starting,
            sdp_results,
            avdtp_psm: None,
            pair,
            streaming_since_ms: None,
            pcm: VecDeque::new(),
            samples_queued: 0,
            capabilities: Vec::new(),
            endpoints_reported: false,
            negotiated: None,
            log: Vec::new(),
        }
    }

    /// The host, for bring-up (`start_commands`, scan enable, inquiry mode).
    pub fn host(&self) -> &ClassicHost {
        &self.host
    }

    /// Routes one controller packet into the host.
    pub fn handle_packet(&mut self, packet: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        self.host.handle_packet(packet).map_err(|e| e.to_string())
    }

    /// The host's unprompted output: the SDP query, AVDTP signalling and
    /// every media packet leave this way rather than from [`Self::step`].
    pub fn poll(&mut self) -> Vec<Vec<u8>> {
        self.host.poll()
    }

    /// Progress lines since the last call.
    pub fn take_log(&mut self) -> Vec<String> {
        std::mem::take(&mut self.log)
    }

    /// The current stage.
    pub fn rung(&self) -> SourceRung {
        self.rung
    }

    /// The furthest stage reached, for the final report.
    pub fn highest(&self) -> SourceRung {
        self.highest
    }

    /// The SBC operating point the stream settled on, once it has.
    pub fn negotiated(&self) -> Option<&str> {
        self.negotiated.as_deref()
    }

    /// The AVDTP PSM the peer's own service record named.
    pub fn avdtp_psm(&self) -> Option<u16> {
        self.avdtp_psm
    }

    /// The peer's AVDTP capability bytes, as `(seid, category, data)`.
    pub fn capabilities(&self) -> &[(u8, u8, Vec<u8>)] {
        &self.capabilities
    }

    /// RTP media packets handed to the controller so far.
    pub fn packets_sent(&self) -> usize {
        self.host
            .handler::<A2dpSource>()
            .map(A2dpSource::packets_sent)
            .unwrap_or(0)
    }

    /// Milliseconds of streaming so far, by the caller's clock.
    pub fn streaming_ms(&self, now_ms: f64) -> Option<f64> {
        self.streaming_since_ms.map(|since| now_ms - since)
    }

    /// Everything the inquiry has turned up, for a caller offering a choice.
    pub fn discovered(&self) -> &[DiscoveredDevice] {
        self.host.discovered()
    }

    /// Appends PCM (interleaved stereo, [`SAMPLE_RATE`]) for the stream.
    pub fn queue_pcm(&mut self, samples: &[i16]) {
        self.pcm.extend(samples);
    }

    /// Samples queued and not yet metered out — the caller's low-water mark.
    pub fn pending_samples(&self) -> usize {
        self.pcm.len()
    }

    /// Ends the run (closing is the caller's decision, not a timeout here).
    pub fn finish(&mut self) {
        self.enter(SourceRung::Done);
    }

    fn enter(&mut self, rung: SourceRung) {
        self.log.push(format!("-> {}", rung.label()));
        self.rung = rung;
        self.highest = self.highest.max(rung);
    }

    fn found(&self) -> Option<&DiscoveredDevice> {
        let target = self.target?;
        self.host.discovered().iter().find(|d| d.address == target)
    }

    /// The first device whose Class of Device says Audio/Video. Guessing is
    /// only acceptable because the alternative is making the caller read an
    /// address off a different report first.
    fn pick_speaker(&self) -> Option<Address> {
        self.host
            .discovered()
            .iter()
            .find(|d| (d.class_of_device[1] & 0x1F) == AUDIO_VIDEO_MAJOR_CLASS)
            .map(|d| d.address)
    }

    /// Advance one step. `Err` is a stage that failed outright, as opposed
    /// to one still waiting for the peer.
    pub fn step(&mut self, now_ms: f64, inquiry_length: u8) -> Result<Vec<Vec<u8>>, String> {
        match self.rung {
            SourceRung::Starting => {
                self.enter(SourceRung::Inquiring);
                Ok(self.host.start_inquiry(inquiry_length))
            }
            SourceRung::Inquiring => {
                if self.target.is_none() {
                    // Take the first Audio/Video device rather than waiting
                    // out the whole inquiry: a speaker in pairing mode
                    // answers early, and the remaining seconds only collect
                    // laptops.
                    if let Some(address) = self.pick_speaker() {
                        self.log.push(format!("found a speaker at {address}"));
                        self.target = Some(address);
                    }
                }
                if let Some(device) = self.found() {
                    let name = device.name.clone();
                    let cod = device.class_of_device;
                    self.log.push(format!(
                        "inquiry result: {} class {:#08x}{}",
                        device.address,
                        u32::from_le_bytes([cod[0], cod[1], cod[2], 0]),
                        match &name {
                            Some(name) => format!(" name {name:?} (from EIR)"),
                            None => String::new(),
                        }
                    ));
                    let target = self.target.expect("found implies a target");
                    if name.is_some() {
                        self.enter(SourceRung::Paging);
                        return Ok(self.host.create_connection(target));
                    }
                    self.enter(SourceRung::ResolvingName);
                    return Ok(self.host.request_remote_name(target));
                }
                if self.host.inquiry_finished() {
                    let seen: Vec<String> = self
                        .host
                        .discovered()
                        .iter()
                        .map(|d| {
                            format!(
                                "{} (class {:#08x})",
                                d.address,
                                u32::from_le_bytes([
                                    d.class_of_device[0],
                                    d.class_of_device[1],
                                    d.class_of_device[2],
                                    0
                                ])
                            )
                        })
                        .collect();
                    return Err(if self.target_was_given {
                        format!("inquiry finished without finding the speaker; it found {seen:?}")
                    } else {
                        format!(
                            "inquiry finished with no Audio/Video device in range — is the \
                             speaker in pairing mode? it found {seen:?}"
                        )
                    });
                }
                Ok(Vec::new())
            }
            SourceRung::ResolvingName => {
                let target = self.target.expect("a name is only resolved for a target");
                match self.host.name_of(target) {
                    Some(name) => self.log.push(format!("the speaker calls itself {name:?}")),
                    None => return Ok(Vec::new()),
                }
                self.enter(SourceRung::Paging);
                Ok(self.host.create_connection(target))
            }
            SourceRung::Paging => {
                if let Some(status) = self.host.connection_failure() {
                    return Err(format!("the page was refused: status {status:#04x}"));
                }
                if self.host.connection().is_none() {
                    return Ok(Vec::new());
                }
                self.log.push("ACL connected".to_string());
                if self.pair {
                    self.enter(SourceRung::Pairing);
                    return Ok(self.host.authenticate());
                }
                self.enter(SourceRung::QueryingSdp);
                self.host.open_channel(SDP_PSM).map_err(|e| e.to_string())
            }
            SourceRung::Pairing => {
                let security = self.host.security();
                if let Some(status) = security.pairing_status.filter(|s| *s != 0x00) {
                    return Err(format!(
                        "the speaker refused pairing: Simple Pairing Complete status \
                         {status:#04x}"
                    ));
                }
                if !security.authenticated {
                    return Ok(Vec::new());
                }
                if let Some(capability) = security.peer_io_capability {
                    self.log
                        .push(format!("the speaker's IO capability is {capability:#04x}"));
                }
                let target = self.target.expect("pairing implies a target");
                if let Some(key) = self.host.link_key(target) {
                    self.log.push(format!(
                        "bonded: link key type {:#04x} ({})",
                        key.key_type,
                        if key.is_authenticated() {
                            "authenticated"
                        } else {
                            "unauthenticated"
                        }
                    ));
                }
                self.enter(SourceRung::Encrypting);
                Ok(self.host.encrypt(true))
            }
            SourceRung::Encrypting => {
                if !self.host.security().encrypted {
                    return Ok(Vec::new());
                }
                self.log.push("link encrypted".to_string());
                self.enter(SourceRung::QueryingSdp);
                self.host.open_channel(SDP_PSM).map_err(|e| e.to_string())
            }
            SourceRung::QueryingSdp => self.advance_sdp(),
            SourceRung::Avdtp => {
                self.observe_source();
                let Some(source) = self.host.handler::<A2dpSource>() else {
                    return Err("the A2DP source handler vanished".to_string());
                };
                match source.phase() {
                    SourcePhase::Failed => Err(format!(
                        "AVDTP setup failed: {}",
                        source.error().unwrap_or("(no reason given)")
                    )),
                    SourcePhase::Streaming => {
                        self.enter(SourceRung::Streaming);
                        self.streaming_since_ms = Some(now_ms);
                        Ok(Vec::new())
                    }
                    _ => Ok(Vec::new()),
                }
            }
            SourceRung::Streaming => {
                self.observe_source();
                let Some(source) = self.host.handler::<A2dpSource>() else {
                    return Err("the A2DP source handler vanished".to_string());
                };
                if source.phase() == SourcePhase::Failed {
                    return Err(format!(
                        "the stream stopped: {}",
                        source.error().unwrap_or("(no reason given)")
                    ));
                }
                Ok(Vec::new())
            }
            SourceRung::Done => Ok(Vec::new()),
        }
    }

    /// The SDP stage: wait for the speaker's answer, read the AVDTP PSM out
    /// of its Audio Sink record, and hand that PSM to a fresh [`A2dpSource`].
    ///
    /// The PSM is read rather than assumed: 0x0019 is the assigned number
    /// and every speaker uses it, but a source that opens a hardcoded PSM
    /// has not proved it can read a service record — and reading one is the
    /// rung.
    fn advance_sdp(&mut self) -> Result<Vec<Vec<u8>>, String> {
        let psm = {
            let Ok(results) = self.sdp_results.lock() else {
                return Ok(Vec::new());
            };
            if !results.answered {
                return Ok(Vec::new());
            }
            if let Some(code) = results.error {
                return Err(format!(
                    "the speaker's SDP server answered error {code:#06x}"
                ));
            }
            self.log.push(format!(
                "SDP answered: {} bytes, {} L2CAP service record(s), {} RFCOMM",
                results.response_bytes,
                results.l2cap_psms.len(),
                results.rfcomm_channels.len(),
            ));
            for (psm, classes) in &results.l2cap_psms {
                self.log
                    .push(format!("  PSM {psm:#06x} for service classes {classes:?}"));
            }
            if results.truncated {
                self.log
                    .push("  (the answer was truncated — the speaker kept continuing)".to_string());
            }
            match results.psm_for(AUDIO_SINK_SERVICE_CLASS) {
                Some(psm) => psm,
                None => {
                    return Err(format!(
                        "the speaker's SDP advertises no Audio Sink service; it offered \
                         PSMs {:?}",
                        results.l2cap_psms
                    ));
                }
            }
        };
        self.log.push(format!(
            "the speaker's Audio Sink record names AVDTP PSM {psm:#06x}"
        ));
        if psm != AVDTP_PSM {
            self.log.push(format!(
                "  (note: that is not the assigned AVDTP PSM {AVDTP_PSM:#06x})"
            ));
        }
        self.avdtp_psm = Some(psm);

        self.host
            .register_handler(Box::new(A2dpSource::new()))
            .map_err(|e| e.to_string())?;
        self.enter(SourceRung::Avdtp);
        // The source asks for its own signalling channel from
        // `poll_channel_requests`, so opening one here as well would give it
        // two and make the second look like a media transport.
        Ok(Vec::new())
    }

    /// Records anything new the AVDTP exchange has revealed about the peer.
    /// Capabilities especially: they are the bytes worth writing down, and
    /// they exist only for as long as the handler does.
    fn observe_source(&mut self) {
        let mut discovered = Vec::new();
        let mut capabilities = Vec::new();
        let mut negotiated = None;
        if let Some(source) = self.host.handler::<A2dpSource>() {
            for event in source.events() {
                match event {
                    AvdtpEvent::EndpointsDiscovered(seps) => {
                        discovered = seps.clone();
                    }
                    AvdtpEvent::CapabilitiesReceived {
                        seid,
                        capabilities: caps,
                    } => {
                        for capability in caps {
                            capabilities.push((
                                *seid,
                                capability.service_category,
                                capability.data.clone(),
                            ));
                        }
                    }
                    _ => {}
                }
            }
            negotiated = source.parameters().map(|p| format!("{p:?}"));
        }
        if !discovered.is_empty() && !self.endpoints_reported {
            self.endpoints_reported = true;
            self.log.push(format!(
                "the speaker offers {} endpoint(s):",
                discovered.len()
            ));
            for sep in &discovered {
                self.log.push(format!(
                    "  SEID {} — {:?} {:?}{}",
                    sep.seid,
                    sep.media_type,
                    sep.tsep,
                    if sep.in_use { " (in use)" } else { "" }
                ));
            }
        }
        if capabilities.len() > self.capabilities.len() {
            for (seid, category, data) in &capabilities[self.capabilities.len()..] {
                self.log.push(format!(
                    "  SEID {seid} capability {category:#04x}: {data:02X?}"
                ));
            }
            self.capabilities = capabilities;
        }
        if negotiated.is_some() && self.negotiated.is_none() {
            self.log.push(format!(
                "negotiated SBC parameters: {}",
                negotiated.as_deref().unwrap_or("")
            ));
            self.negotiated = negotiated;
        }
    }

    /// Meters queued PCM into the encoder at real time, staying a fifth of
    /// a second ahead. A source that handed over everything at once would
    /// encode it all in one poll and swamp the controller's buffers — which
    /// the credit queue now survives, but a prompt stop still wants a short
    /// pipeline.
    pub fn feed(&mut self, now_ms: f64) {
        let Some(since) = self.streaming_since_ms else {
            return;
        };
        let channels = 2usize;
        let elapsed_s = ((now_ms - since) / 1000.0).max(0.0);
        let wanted = ((elapsed_s + 0.2) * SAMPLE_RATE as f64) as usize * channels;
        if wanted <= self.samples_queued {
            return;
        }
        // Rounded down to whole stereo frames, so a channel never slips.
        let take = ((wanted - self.samples_queued).min(self.pcm.len())) & !1;
        if take == 0 {
            return;
        }
        let block: Vec<i16> = self.pcm.drain(..take).collect();
        self.samples_queued += take;
        if let Some(source) = self.host.handler_mut::<A2dpSource>() {
            source.queue_pcm(&block);
        }
    }
}
