// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Live A2DP **source** against a *real Bluetooth speaker*, over a real USB
//! dongle: the phone's half of the sequence, with consumer kit on the other
//! end and no simulator anywhere in the path.
//!
//! [`examples/a2dp_sink`](a2dp_sink) is the mirror of this and answers a
//! foreign source over netsim. Both of those still have software on both
//! ends. This one does not: the peer is a speaker somebody bought, whose
//! firmware nobody here can read, and whose refusals are therefore evidence
//! rather than a modelling choice. Every rung it climbs is a fact about
//! simble's bytes that a simulator cannot establish, because a simulator
//! shares simble's assumptions.
//!
//! # The ladder
//!
//! Each stage is reported as it is reached, and the run's summary names the
//! highest one — a stall at AVDTP with the speaker's real capability bytes
//! in hand is a better outcome than a green run that proves nothing.
//!
//! 1. **Inquiry** — find the speaker. It must be in pairing mode.
//! 2. **Pairing** — SSP against a peer that has never met simble.
//! 3. **SDP** — read the Audio Sink record's AVDTP L2CAP PSM.
//! 4. **AVDTP** — discover its endpoints and negotiate an SBC configuration
//!    it will accept, intersecting with what it offers rather than assuming.
//! 5. **Stream** — open the media transport channel, encode PCM to SBC, send
//!    RTP-framed media, and make a noise.
//!
//! # Running it
//!
//! ```sh
//! cargo run --example usb_list                # which dongle is which
//! cargo run --example a2dp_source -- 00:11:22:33:44:55
//! cargo run --example a2dp_source             # inquire and take the first speaker
//! ```
//!
//! **Use a dual-mode dongle.** A2DP is Classic, and a controller built
//! without BR/EDR support (a Zephyr `hci_usb` nRF52840, say) cannot inquire
//! at all — the run fails at rung 1 for a reason that is not the speaker's.
//!
//! | Variable | Meaning |
//! |---|---|
//! | `SIMBLE_HCI` | which controller (default `usb` — the only dongle present) |
//! | `SIMBLE_TIMEOUT` | seconds before a stalled stage is a failure (default 60) |
//! | `SIMBLE_STREAM_SECS` | how long to play for once streaming (default 10) |
//! | `SIMBLE_INQUIRY_SECS` | inquiry duration, in 1.28 s units (default 8) |
//! | `SIMBLE_NO_PAIR` | `1` to skip pairing, for a speaker already bonded |
//! | `SIMBLE_IO_CAPABILITY` | `noinputnooutput` (default), `displayyesno`, … |
//!
//! The default IO capability is No Input No Output, which is what makes SSP
//! pick Just Works: nobody is standing at this end to confirm a six-digit
//! number, and a speaker has no keypad either.

use std::str::FromStr;
use std::time::{Duration, Instant};

use simble::classic::avdtp::AVDTP_PSM;
use simble::classic::sdp::SdpUuid;
use simble::device::a2dp::{A2dpSource, SourcePhase};
use simble::device::classic_host::{
    SdpQueryHandler, SharedSdpQueryResults, authentication_requirements, inquiry_mode,
    io_capability, scan_enable,
};
use simble::device::{ClassicHost, DiscoveredDevice};
use simble::transport::{HciChannel, HciTransport, LiveTransport};
use simble::types::Address;

/// A2DP Audio Sink service class (Assigned Numbers) — what a speaker's SDP
/// record advertises itself as, and what the SDP search asks for.
const AUDIO_SINK_SERVICE_CLASS: SdpUuid = SdpUuid::Uuid16(0x110B);

/// Class of Device 0x5A020C: Phone / Smartphone. What the speaker's pairing
/// list should render this as, and what makes it recognisably the *source*.
const PHONE_CLASS_OF_DEVICE: [u8; 3] = [0x0C, 0x02, 0x5A];

/// Major device class Audio/Video, from the middle Class of Device octet.
/// How an inquiry result is recognised as a speaker rather than a laptop.
const AUDIO_VIDEO_MAJOR_CLASS: u8 = 0x04;

/// The sample rate the stream is negotiated at and the tone generated at.
/// These must agree, and they are the same constant so they cannot drift.
const SAMPLE_RATE: u32 = 44_100;

// ---------------------------------------------------------------------------
// The ladder
// ---------------------------------------------------------------------------

/// How far the run got. The point of naming every stage is that the useful
/// report is "it stopped *here*, and here is what the peer said", not "it
/// timed out".
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Rung {
    Starting,
    Inquiring,
    ResolvingName,
    Paging,
    Pairing,
    Encrypting,
    QueryingSdp,
    Avdtp,
    Streaming,
    Done,
}

impl Rung {
    fn label(self) -> &'static str {
        match self {
            Rung::Starting => "bring-up",
            Rung::Inquiring => "1. inquiry",
            Rung::ResolvingName => "1b. remote name request",
            Rung::Paging => "2a. create connection",
            Rung::Pairing => "2. pairing (SSP)",
            Rung::Encrypting => "2b. encryption",
            Rung::QueryingSdp => "3. SDP (Audio Sink record)",
            Rung::Avdtp => "4. AVDTP (discover, capabilities, configure)",
            Rung::Streaming => "5. streaming SBC media",
            Rung::Done => "done",
        }
    }
}

// ---------------------------------------------------------------------------
// The tone
// ---------------------------------------------------------------------------

/// One note of the melody: a frequency in Hz (0 for a rest) and a duration
/// in milliseconds.
struct Note(f32, u32);

/// A short, unmistakably deliberate phrase — a major arpeggio up, an octave
/// above, then down. Chosen over a single steady tone because a steady tone
/// is what a broken stream *also* sounds like when a buffer repeats, while a
/// melody that plays in order and in tune says the frames arrived in order
/// and decoded correctly. The pitches are A4 and the major triad above it.
const MELODY: &[Note] = &[
    Note(440.00, 260), // A4
    Note(554.37, 260), // C#5
    Note(659.25, 260), // E5
    Note(880.00, 420), // A5
    Note(659.25, 260), // E5
    Note(554.37, 260), // C#5
    Note(440.00, 520), // A4
    Note(0.0, 360),    // rest, so a loop is audibly a loop
];

/// Renders [`MELODY`] to interleaved stereo PCM at [`SAMPLE_RATE`].
///
/// Each note gets a short linear fade in and out. Without it every note
/// boundary is a step discontinuity, which SBC faithfully encodes as a click
/// — and a run full of clicks is indistinguishable from a run with real
/// framing bugs, which is exactly the confusion this example exists to avoid.
fn render_melody() -> Vec<i16> {
    let mut pcm = Vec::new();
    for Note(frequency, milliseconds) in MELODY {
        let samples = (SAMPLE_RATE as u64 * *milliseconds as u64 / 1000) as usize;
        // 5 ms of fade at each end, or a quarter of a very short note.
        let fade = (SAMPLE_RATE as usize / 200).min(samples / 4).max(1);
        for n in 0..samples {
            let amplitude = if *frequency == 0.0 {
                0.0
            } else {
                let envelope = if n < fade {
                    n as f32 / fade as f32
                } else if n + fade >= samples {
                    (samples - n) as f32 / fade as f32
                } else {
                    1.0
                };
                // 0.35 of full scale: loud enough to hear across a room,
                // quiet enough that nothing clips once SBC has rounded it.
                let phase = std::f32::consts::TAU * *frequency * n as f32 / SAMPLE_RATE as f32;
                phase.sin() * envelope * 0.35
            };
            let sample = (amplitude * i16::MAX as f32) as i16;
            pcm.push(sample);
            pcm.push(sample);
        }
    }
    pcm
}

// ---------------------------------------------------------------------------
// Reporting
// ---------------------------------------------------------------------------

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Decodes an SBC codec-specific information element into the words a
/// person reads, so the report says "44.1 kHz joint stereo" and not only
/// four bytes. The bit layouts are A2DP spec §4.3.2.
fn describe_sbc(data: &[u8]) -> String {
    let Some(&[b0, b1, min, max]) = data.get(..4) else {
        return format!("(not a 4-byte SBC element: {})", hex(data));
    };
    let mut parts = Vec::new();
    let mut flags = |value: u8, names: [(u8, &str); 4]| {
        let set: Vec<&str> = names
            .iter()
            .filter(|(bit, _)| value & bit != 0)
            .map(|(_, name)| *name)
            .collect();
        parts.push(if set.is_empty() {
            "none".to_string()
        } else {
            set.join("/")
        });
    };
    flags(
        b0 >> 4,
        [(0x8, "16k"), (0x4, "32k"), (0x2, "44.1k"), (0x1, "48k")],
    );
    flags(
        b0 & 0x0F,
        [
            (0x8, "mono"),
            (0x4, "dual"),
            (0x2, "stereo"),
            (0x1, "joint"),
        ],
    );
    flags(
        b1 >> 4,
        [(0x8, "blk4"), (0x4, "blk8"), (0x2, "blk12"), (0x1, "blk16")],
    );
    let subbands = match (b1 >> 2) & 0x03 {
        0x2 => "4sb",
        0x1 => "8sb",
        0x3 => "4sb/8sb",
        _ => "no subbands",
    };
    let allocation = match b1 & 0x03 {
        0x2 => "SNR",
        0x1 => "loudness",
        0x3 => "SNR/loudness",
        _ => "no allocation",
    };
    format!(
        "freq {}, channels {}, blocks {}, {subbands}, {allocation}, bitpool {min}..{max}",
        parts[0], parts[1], parts[2],
    )
}

/// Names an AVDTP service category (AVDTP §8.21).
fn service_category_name(category: u8) -> &'static str {
    match category {
        0x01 => "Media Transport",
        0x02 => "Reporting",
        0x03 => "Recovery",
        0x04 => "Content Protection",
        0x05 => "Header Compression",
        0x06 => "Multiplexing",
        0x07 => "Media Codec",
        0x08 => "Delay Reporting",
        _ => "unknown",
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn parse_io_capability(text: &str) -> Result<u8, String> {
    match text.trim().to_ascii_lowercase().as_str() {
        "displayonly" => Ok(io_capability::DISPLAY_ONLY),
        "displayyesno" => Ok(io_capability::DISPLAY_YES_NO),
        "keyboardonly" => Ok(io_capability::KEYBOARD_ONLY),
        "none" | "noinputnooutput" => Ok(io_capability::NO_INPUT_NO_OUTPUT),
        other => Err(format!("unknown SIMBLE_IO_CAPABILITY: {other}")),
    }
}

// ---------------------------------------------------------------------------
// The run
// ---------------------------------------------------------------------------

struct Run {
    host: ClassicHost,
    /// The speaker, once chosen — either given on the command line or
    /// picked out of the inquiry results by its Class of Device.
    target: Option<Address>,
    /// Whether the target was named by the caller, in which case an inquiry
    /// that does not find it is a failure rather than a reason to look on.
    target_was_given: bool,
    rung: Rung,
    highest: Rung,
    rung_since: Instant,
    sdp_results: SharedSdpQueryResults,
    /// The AVDTP PSM the speaker's own SDP record named.
    avdtp_psm: Option<u16>,
    pair: bool,
    /// Wall clock at the moment STREAMING was reached, which is when audio
    /// starts being metered out.
    streaming_since: Option<Instant>,
    /// Interleaved stereo PCM, looped for as long as the run plays.
    melody: Vec<i16>,
    /// How many samples have been handed to the encoder, so the next feed
    /// picks up where the last left off.
    samples_queued: usize,
    /// The peer's AVDTP capability bytes, kept for the report.
    capabilities: Vec<(u8, u8, Vec<u8>)>,
    /// Whether the endpoint list has been printed. The events vector is
    /// replayed from the start on every poll, so without this the same
    /// Discover response is announced once per turn of the loop.
    endpoints_reported: bool,
    /// The SBC operating point the stream settled on.
    negotiated: Option<String>,
}

impl Run {
    fn new(target: Option<Address>, io_capability: u8, pair: bool) -> Self {
        let mut host = ClassicHost::new("simble-a2dp-source", PHONE_CLASS_OF_DEVICE);
        let (sdp, sdp_results) = SdpQueryHandler::searching(AUDIO_SINK_SERVICE_CLASS);
        host.register_handler(Box::new(sdp))
            .expect("the SDP client registers");
        host.set_io_capability(io_capability, authentication_requirements::GENERAL_BONDING);
        Self {
            host,
            target_was_given: target.is_some(),
            target,
            rung: Rung::Starting,
            highest: Rung::Starting,
            rung_since: Instant::now(),
            sdp_results,
            avdtp_psm: None,
            pair,
            streaming_since: None,
            melody: render_melody(),
            samples_queued: 0,
            capabilities: Vec::new(),
            endpoints_reported: false,
            negotiated: None,
        }
    }

    fn enter(&mut self, rung: Rung) {
        println!("  -> {}", rung.label());
        self.rung = rung;
        self.highest = self.highest.max(rung);
        self.rung_since = Instant::now();
    }

    /// The inquiry result for the chosen target, if it has been seen.
    fn found(&self) -> Option<&DiscoveredDevice> {
        let target = self.target?;
        self.host.discovered().iter().find(|d| d.address == target)
    }

    /// Picks a speaker out of whatever the inquiry has turned up: the first
    /// device whose Class of Device says Audio/Video. Guessing is only
    /// acceptable because the alternative is making the caller run
    /// `classic_initiator` first to read an address off a different report.
    fn pick_speaker(&self) -> Option<Address> {
        self.host
            .discovered()
            .iter()
            .find(|d| (d.class_of_device[1] & 0x1F) == AUDIO_VIDEO_MAJOR_CLASS)
            .map(|d| d.address)
    }

    /// Advance one step. `Err` is a stage that failed outright, as opposed
    /// to one still waiting for the peer.
    fn step(&mut self, inquiry_length: u8) -> Result<Vec<Vec<u8>>, String> {
        match self.rung {
            Rung::Starting => {
                self.enter(Rung::Inquiring);
                Ok(self.host.start_inquiry(inquiry_length))
            }
            Rung::Inquiring => {
                if self.target.is_none() {
                    // Take the first Audio/Video device that appears rather
                    // than waiting out the whole inquiry: a speaker in
                    // pairing mode answers early, and the remaining seconds
                    // only collect laptops.
                    if let Some(address) = self.pick_speaker() {
                        println!("  found a speaker at {address}");
                        self.target = Some(address);
                    }
                }
                if let Some(device) = self.found() {
                    let name = device.name.clone();
                    let cod = device.class_of_device;
                    println!(
                        "  inquiry result: {} class {:#08x}{}",
                        device.address,
                        u32::from_le_bytes([cod[0], cod[1], cod[2], 0]),
                        match &name {
                            Some(name) => format!(" name {name:?} (from EIR)"),
                            None => String::new(),
                        }
                    );
                    if name.is_some() {
                        self.enter(Rung::Paging);
                        let target = self.target.expect("found implies a target");
                        return Ok(self.host.create_connection(target));
                    }
                    self.enter(Rung::ResolvingName);
                    let target = self.target.expect("found implies a target");
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
            Rung::ResolvingName => {
                let target = self.target.expect("a name is only resolved for a target");
                match self.host.name_of(target) {
                    Some(name) => println!("  the speaker calls itself {name:?}"),
                    None => return Ok(Vec::new()),
                }
                self.enter(Rung::Paging);
                Ok(self.host.create_connection(target))
            }
            Rung::Paging => {
                if self.host.connection().is_none() {
                    return Ok(Vec::new());
                }
                println!("  ACL connected");
                if self.pair {
                    self.enter(Rung::Pairing);
                    return Ok(self.host.authenticate());
                }
                self.enter(Rung::QueryingSdp);
                self.host.open_channel(SDP_PSM).map_err(|e| e.to_string())
            }
            Rung::Pairing => {
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
                    println!("  the speaker's IO capability is {capability:#04x}");
                }
                let target = self.target.expect("pairing implies a target");
                if let Some(key) = self.host.link_key(target) {
                    println!(
                        "  bonded: link key type {:#04x} ({})",
                        key.key_type,
                        if key.is_authenticated() {
                            "authenticated"
                        } else {
                            "unauthenticated"
                        }
                    );
                }
                self.enter(Rung::Encrypting);
                Ok(self.host.encrypt(true))
            }
            Rung::Encrypting => {
                if !self.host.security().encrypted {
                    return Ok(Vec::new());
                }
                println!("  link encrypted");
                self.enter(Rung::QueryingSdp);
                self.host.open_channel(SDP_PSM).map_err(|e| e.to_string())
            }
            Rung::QueryingSdp => self.advance_sdp(),
            Rung::Avdtp => {
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
                        self.enter(Rung::Streaming);
                        self.streaming_since = Some(Instant::now());
                        Ok(Vec::new())
                    }
                    _ => Ok(Vec::new()),
                }
            }
            Rung::Streaming => {
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
            Rung::Done => Ok(Vec::new()),
        }
    }

    /// The SDP stage: wait for the speaker's answer, read the AVDTP PSM out
    /// of its Audio Sink record, and hand that PSM to a fresh [`A2dpSource`].
    ///
    /// The PSM is read rather than assumed. 0x0019 is the assigned number
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
            println!(
                "  SDP answered: {} bytes, {} L2CAP service record(s), {} RFCOMM",
                results.response_bytes,
                results.l2cap_psms.len(),
                results.rfcomm_channels.len(),
            );
            for (psm, classes) in &results.l2cap_psms {
                println!("    PSM {psm:#06x} for service classes {classes:?}");
            }
            if results.truncated {
                println!("    (the answer was truncated — the speaker kept continuing)");
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
        println!("  the speaker's Audio Sink record names AVDTP PSM {psm:#06x}");
        if psm != AVDTP_PSM {
            println!("    (note: that is not the assigned AVDTP PSM {AVDTP_PSM:#06x})");
        }
        self.avdtp_psm = Some(psm);

        self.host
            .register_handler(Box::new(A2dpSource::new()))
            .map_err(|e| e.to_string())?;
        self.enter(Rung::Avdtp);
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
                    simble::classic::avdtp::AvdtpEvent::EndpointsDiscovered(seps) => {
                        discovered = seps.clone();
                    }
                    simble::classic::avdtp::AvdtpEvent::CapabilitiesReceived {
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
            println!("  the speaker offers {} endpoint(s):", discovered.len());
            for sep in &discovered {
                println!(
                    "    SEID {} — {:?} {:?}{}",
                    sep.seid,
                    sep.media_type,
                    sep.tsep,
                    if sep.in_use { " (in use)" } else { "" }
                );
            }
        }
        if capabilities.len() > self.capabilities.len() {
            for (seid, category, data) in &capabilities[self.capabilities.len()..] {
                let name = service_category_name(*category);
                println!(
                    "    SEID {seid} capability {category:#04x} ({name}): {}",
                    hex(data)
                );
                // Category 0x07 is Media Codec: media type, codec type, then
                // the codec element. Codec type 0x00 is SBC.
                if *category == 0x07 && data.get(1) == Some(&0x00) {
                    println!("      SBC: {}", describe_sbc(&data[2..]));
                }
            }
            self.capabilities = capabilities;
        }
        if negotiated.is_some() && self.negotiated.is_none() {
            println!(
                "  negotiated SBC parameters: {}",
                negotiated.as_deref().unwrap_or("")
            );
            self.negotiated = negotiated;
        }
    }

    /// Meters PCM into the source at real time. A source that dumped the
    /// whole melody at once would encode it all in one poll and hand the
    /// controller more ACL data than its buffers hold, which on real
    /// silicon means dropped packets rather than fast music.
    fn feed_audio(&mut self) {
        let Some(since) = self.streaming_since else {
            return;
        };
        let channels = 2;
        // Stay a fifth of a second ahead: enough that a slow poll does not
        // starve the speaker, little enough that stopping is prompt.
        let wanted = ((since.elapsed().as_secs_f64() + 0.2) * SAMPLE_RATE as f64) as usize;
        if wanted <= self.samples_queued {
            return;
        }
        let mut block = Vec::new();
        for index in self.samples_queued..wanted {
            let frame = index % (self.melody.len() / channels);
            block.push(self.melody[frame * channels]);
            block.push(self.melody[frame * channels + 1]);
        }
        self.samples_queued = wanted;
        if let Some(source) = self.host.handler_mut::<A2dpSource>() {
            source.queue_pcm(&block);
        }
    }
}

/// SDP's assigned PSM. Named here rather than imported so the two PSMs this
/// example opens read alike.
const SDP_PSM: u16 = 0x0001;

fn main() {
    let mut args = std::env::args().skip(1);
    let target = match args.next() {
        Some(text) => match Address::from_str(&text) {
            Ok(address) => Some(address),
            Err(e) => {
                eprintln!("{e}");
                eprintln!(
                    "usage: a2dp_source [<speaker-address>]\n\
                     (omit the address to inquire and take the first Audio/Video device)"
                );
                std::process::exit(2);
            }
        },
        None => None,
    };
    let io_capability = match std::env::var("SIMBLE_IO_CAPABILITY") {
        Ok(text) => match parse_io_capability(&text) {
            Ok(capability) => capability,
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        },
        // Just Works: nobody is standing here to compare a number.
        Err(_) => io_capability::NO_INPUT_NO_OUTPUT,
    };
    let timeout = Duration::from_secs(env_u64("SIMBLE_TIMEOUT", 60));
    let stream_secs = env_u64("SIMBLE_STREAM_SECS", 10);
    let inquiry_length = env_u64("SIMBLE_INQUIRY_SECS", 8).clamp(1, 48) as u8;
    let pair = !env_flag("SIMBLE_NO_PAIR");

    // A dongle by default: this example's entire reason to exist is the peer
    // being real, and neither simulated controller has a speaker on it.
    let spec = std::env::var("SIMBLE_HCI").unwrap_or_else(|_| "usb".to_string());
    let mut transport = match LiveTransport::open(&spec, "simble-a2dp-source", Address::ANY) {
        Ok(transport) => transport,
        Err(e) => {
            eprintln!("cannot reach a controller ({spec}): {e}");
            eprintln!("try `cargo run --example usb_list` to see what is plugged in");
            std::process::exit(1);
        }
    };
    println!("connected to {} via {spec}", transport.describe());
    match target {
        Some(address) => println!("target: {address}"),
        None => println!("target: the first Audio/Video device the inquiry finds"),
    }

    let mut run = Run::new(target, io_capability, pair);
    let channel = HciChannel::new();
    for packet in run.host.start_commands() {
        channel.inject_host_packet(packet).expect("queue bring-up");
    }
    // A source is neither discoverable nor connectable: it does the finding.
    // `start_commands` ends by enabling both scans, so this must follow it.
    for packet in run.host.set_scan_enable(scan_enable::NONE) {
        channel
            .inject_host_packet(packet)
            .expect("queue scan enable");
    }
    // Extended results carry the peer's name in the EIR, which saves a
    // Remote Name Request — and a speaker's name is how a person confirms
    // the run found the right box.
    for packet in run.host.set_inquiry_mode(inquiry_mode::WITH_EXTENDED) {
        channel
            .inject_host_packet(packet)
            .expect("queue inquiry mode");
    }

    let mut failure: Option<String> = None;
    let deadline = Instant::now() + timeout;

    while run.rung != Rung::Done && failure.is_none() {
        if let Err(e) = transport.pump(&channel) {
            failure = Some(format!("transport: {e}"));
            break;
        }
        while let Some(packet) = channel.poll_controller_packet() {
            match run.host.handle_packet(&packet) {
                Ok(outgoing) => {
                    for out in outgoing {
                        let _ = channel.inject_host_packet(out);
                    }
                }
                Err(e) => eprintln!("host: {e}"),
            }
        }
        match run.step(inquiry_length) {
            Ok(packets) => {
                for packet in packets {
                    let _ = channel.inject_host_packet(packet);
                }
            }
            Err(e) => failure = Some(e),
        }
        run.feed_audio();
        // Profiles speak unprompted: the SDP query, every AVDTP signalling
        // PDU and every media packet leave this way rather than from `step`.
        for packet in run.host.poll() {
            let _ = channel.inject_host_packet(packet);
        }
        if let Some(since) = run.streaming_since
            && since.elapsed() >= Duration::from_secs(stream_secs)
        {
            run.enter(Rung::Done);
        }
        if Instant::now() > deadline && failure.is_none() {
            failure = Some(format!(
                "timed out after {timeout:?} at stage: {}",
                run.rung.label()
            ));
        }
        // 5 ms rather than the 20 ms an SPP example can afford: an SBC frame
        // at 44.1 kHz is about 3 ms of audio, so a slower loop cannot keep a
        // stream fed.
        std::thread::sleep(Duration::from_millis(5));
    }

    // --- the verdict ------------------------------------------------------
    println!();
    println!("--- how far up the ladder ---");
    println!("highest stage reached: {}", run.highest.label());
    let packets_sent = run
        .host
        .handler::<A2dpSource>()
        .map(A2dpSource::packets_sent)
        .unwrap_or(0);
    if let Some(psm) = run.avdtp_psm {
        println!("the speaker's AVDTP PSM: {psm:#06x}");
    }
    if let Some(parameters) = run.negotiated.as_deref() {
        println!("negotiated SBC: {parameters}");
    }
    if !run.capabilities.is_empty() {
        println!("the speaker's AVDTP capabilities, as bytes:");
        for (seid, category, data) in &run.capabilities {
            println!(
                "  SEID {seid} category {category:#04x} ({}): {}",
                service_category_name(*category),
                hex(data)
            );
        }
    }
    println!("RTP media packets written: {packets_sent}");
    if run.highest >= Rung::Streaming {
        println!(
            "\nThe stream reached STREAMING and {packets_sent} media packets went out. \
             Whether sound came out of the speaker is a question only a person in the \
             room can answer — this program cannot hear."
        );
    }
    match failure {
        Some(e) => {
            println!("\nFAIL at {}: {e}", run.rung.label());
            std::process::exit(1);
        }
        None => {
            println!("\nok");
        }
    }
}
