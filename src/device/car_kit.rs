// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The hands-free car kit: a phone and a head unit, wired to each other
//! through the layers a real pair would use.
//!
//! A car head unit is not "an HFP device". It is one endpoint playing
//! several profile roles over one link at once — Hands-Free for telephony,
//! A2DP sink for music, AVRCP controller for the transport keys, PBAP
//! client for the phonebook. This type builds the **telephony role** end to
//! end and leaves the others out rather than pretending: see "What is not
//! here" below.
//!
//! What it owns:
//!
//! * an [`AgProtocol`] and an [`SdpServer`] — the phone, in the Audio
//!   Gateway role, which owns the calls and the indicators;
//! * an [`HfProtocol`] and an [`SdpClient`] — the head unit, in the
//!   Hands-Free role, which discovers the phone and drives the call;
//! * two RFCOMM [`Multiplexer`]s, initiator and responder, whose frames are
//!   handed to each other.
//!
//! Everything above the frame boundary is real: the AT lines in
//! [`CarKit::transcript`] are the bytes [`HfProtocol`] and [`AgProtocol`]
//! produced, the SDP exchange is real PDUs answered by a real
//! [`SdpServer`], and the DLC is opened with a real PN/SABM/UA handshake and
//! real credit accounting.
//!
//! Transport-free in the same sense as [`CisCentral`](super::CisCentral):
//! no sockets, no clock of its own, no HCI. The caller pumps it with
//! [`CarKit::tick`] and reads state back out.
//!
//! ## What is not here
//!
//! * **L2CAP and the baseband.** The two multiplexers exchange RFCOMM
//!   frames directly. On a real link those frames ride an L2CAP Basic Mode
//!   channel on PSM 3 over an ACL connection — that path is
//!   [`ClassicHost`](super::ClassicHost)'s job, not this type's.
//! * **SCO/eSCO — the call audio.** Simble has no synchronous connection
//!   at all: no `Setup Synchronous Connection`, no SCO handle, no audio
//!   path. Codec negotiation here (`AT+BAC`/`+BCS`) settles *which* codec
//!   would be used and then stops, because there is nothing to carry it.
//!   SCO is the BR/EDR counterpart of the CIS work in
//!   [`CisCentral`](super::CisCentral), and it is not implemented.
//! * **A2DP, AVRCP, PBAP.** Not wired here.

use std::collections::VecDeque;

use serde::Serialize;

use crate::classic::hfp::{
    AgConfiguration, AgIndicator, AgIndicatorState, AgProtocol, AudioCodec, CallHoldOperation,
    CallInfo, CallInfoDirection, CallInfoMode, CallInfoMultiParty, CallInfoStatus,
    CallLineIdentification, HfConfiguration, HfIndicator, HfProtocol, HfpEvent, ProfileVersion,
    VoiceRecognitionState, ag_feature, find_ag_sdp_record, hf_feature, make_ag_sdp_records,
    parse_network_operator,
};
use crate::classic::rfcomm::{
    DEFAULT_INITIAL_CREDITS, DEFAULT_L2CAP_MTU, DEFAULT_MAX_FRAME_SIZE, Multiplexer,
    MultiplexerEvent, Role,
};
use crate::classic::sdp::{SdpClient, SdpServer};

/// Server channel the phone advertises its Audio Gateway record on. Nothing
/// in the profile fixes it — the head unit learns it from SDP, which is the
/// point of doing the search at all.
pub const AG_RFCOMM_CHANNEL: u8 = 4;

/// SDP record handle for the phone's Audio Gateway record.
const AG_SERVICE_RECORD_HANDLE: u32 = 0x0001_0004;

/// L2CAP MTU claimed for the (notional) SDP channel, used to decide when an
/// SDP response needs continuation.
const SDP_MTU: u16 = 672;

/// How often the AG repeats `RING` while a call is alerting the head unit.
/// HFP does not fix the period; real AGs sit between 2 and 5 seconds.
const RING_PERIOD_MS: u64 = 2_500;

/// How long an outgoing call stays in "dialing" before the far end starts
/// ringing, and then how long it alerts before being answered. Neither is
/// Bluetooth: this stands in for the cellular network, which is outside the
/// profile entirely.
const DIALING_MS: u64 = 1_400;
/// See [`DIALING_MS`].
const ALERTING_MS: u64 = 5_000;

/// Longest transcript kept in memory.
const TRANSCRIPT_LIMIT: usize = 400;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// How far the head unit has got in reaching the phone's Hands-Free service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPhase {
    /// Nothing started.
    Down,
    /// Searching the phone's SDP server for an Audio Gateway record.
    Discovering,
    /// Opening the RFCOMM multiplexer session (DLCI 0).
    StartingMultiplexer,
    /// Opening the data link connection on the discovered channel.
    OpeningDlc,
    /// Running the Service Level Connection procedure.
    EstablishingSlc,
    /// SLC up; issuing the head unit's own post-SLC configuration.
    ConfiguringHeadUnit,
    /// Ready to place and take calls.
    Ready,
    /// Something refused; see [`CarKit::error`].
    Failed,
}

impl LinkPhase {
    /// Stable identifier for the UI.
    pub fn name(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Discovering => "discovering",
            Self::StartingMultiplexer => "starting-multiplexer",
            Self::OpeningDlc => "opening-dlc",
            Self::EstablishingSlc => "establishing-slc",
            Self::ConfiguringHeadUnit => "configuring",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }
}

/// The call, as the Audio Gateway's `call`/`callsetup` indicators describe
/// it (HFP v1.9 4.10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPhase {
    /// No call: `call = 0`, `callsetup = 0`.
    Idle,
    /// Incoming call being alerted: `callsetup = 1`.
    Incoming,
    /// Outgoing call being placed: `callsetup = 2`.
    Dialing,
    /// Outgoing call, far end alerting: `callsetup = 3`.
    Alerting,
    /// Call in progress: `call = 1`, `callsetup = 0`.
    Active,
}

impl CallPhase {
    /// Stable identifier for the UI.
    pub fn name(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Incoming => "incoming",
            Self::Dialing => "dialing",
            Self::Alerting => "alerting",
            Self::Active => "active",
        }
    }
}

/// One AT line exactly as it crossed the DLC.
#[derive(Debug, Clone, Serialize)]
pub struct AtLine {
    /// Monotonic sequence number, so a caller can ask for "everything after".
    pub seq: u64,
    /// True when the head unit sent it (a command), false when the phone did
    /// (a response or unsolicited result code).
    pub from_hf: bool,
    /// The line's printable content, framing stripped.
    pub text: String,
    /// The bytes actually written, framing included.
    pub hex: String,
}

/// Something a caller may want to react to, produced by [`CarKit::tick`].
#[derive(Debug, Clone, PartialEq)]
pub enum CarKitEvent {
    /// The link reached a new phase.
    LinkPhase(LinkPhase),
    /// The call reached a new phase.
    CallPhase(CallPhase),
    /// The phone rang the head unit.
    Ring,
    /// The head unit learned the caller's number (`+CLIP`).
    CallerId(String),
    /// The head unit asked for a speaker gain (`AT+VGS`).
    SpeakerGain(u8),
    /// The head unit asked for a microphone gain (`AT+VGM`).
    MicrophoneGain(u8),
    /// The head unit learned the network operator name (`AT+COPS?`).
    Operator(String),
    /// Voice recognition was switched on or off (`AT+BVRA`).
    VoiceRecognition(bool),
    /// A layer refused; the link is dead.
    Failed(String),
}

/// Something the head unit wants to say, held until the AT slot is free.
#[derive(Debug, Clone, PartialEq, Eq)]
enum HeadUnitCommand {
    /// `ATA`.
    Answer,
    /// `AT+CHUP`.
    HangUp,
    /// `ATD<number>;`.
    Dial(String),
    /// `AT+CLCC`.
    ListCalls,
    /// `AT+BVRA=<n>`.
    VoiceRecognition(VoiceRecognitionState),
    /// Anything without a typed helper on [`HfProtocol`].
    Raw(String),
}

// ---------------------------------------------------------------------------
// CarKit
// ---------------------------------------------------------------------------

/// A phone and a head unit on one link.
#[derive(Debug)]
pub struct CarKit {
    // --- the phone, in the Audio Gateway role ---
    ag: AgProtocol,
    ag_sdp: SdpServer,
    ag_mux: Multiplexer,

    // --- the head unit, in the Hands-Free role ---
    hf: HfProtocol,
    hf_sdp: SdpClient,
    hf_mux: Multiplexer,

    // --- the wire between them: RFCOMM frames in flight ---
    to_ag: Vec<Vec<u8>>,
    to_hf: Vec<Vec<u8>>,
    dlci: Option<u8>,
    channel: Option<u8>,

    phase: LinkPhase,
    error: Option<String>,
    now_ms: u64,
    reported_phase: LinkPhase,
    reported_call: CallPhase,

    /// Commands the head unit has asked for but not yet sent. An AT client
    /// has exactly one outstanding-command slot — the response has nothing
    /// but ordering to say which command it belongs to — so a second command
    /// waits for the first one's final status rather than racing it. A
    /// dragged volume slider is enough to hit this.
    command_queue: VecDeque<HeadUnitCommand>,

    call: CallPhase,
    caller: Option<String>,
    /// When the current call phase started, for the RING repeat and the
    /// stand-in far end.
    call_since_ms: u64,
    last_ring_ms: u64,
    last_dialed: String,

    speaker_gain: u8,
    microphone_gain: u8,
    microphone_muted: bool,
    voice_recognition: bool,
    /// Operator name as the head unit learned it, which is not the same
    /// value as the phone's until an `AT+COPS?` has actually run.
    car_operator: Option<String>,

    transcript: VecDeque<AtLine>,
    next_seq: u64,
    sdp_detail: Option<String>,
    dlc_detail: Option<String>,
}

impl Default for CarKit {
    fn default() -> Self {
        Self::new()
    }
}

impl CarKit {
    /// Builds a phone and a head unit that have never met.
    pub fn new() -> Self {
        let mut ag = AgProtocol::new(phone_configuration());
        ag.network_operator = "Simble Mobile".into();

        let mut ag_sdp = SdpServer::new();
        ag_sdp.service_records.insert(
            AG_SERVICE_RECORD_HANDLE,
            make_ag_sdp_records(
                AG_SERVICE_RECORD_HANDLE,
                AG_RFCOMM_CHANNEL,
                &phone_configuration(),
                ProfileVersion::V1_9,
            ),
        );

        Self {
            ag,
            ag_sdp,
            ag_mux: Multiplexer::new(Role::Responder, DEFAULT_L2CAP_MTU),
            hf: HfProtocol::new(head_unit_configuration()),
            hf_sdp: SdpClient::new(),
            hf_mux: Multiplexer::new(Role::Initiator, DEFAULT_L2CAP_MTU),
            to_ag: Vec::new(),
            to_hf: Vec::new(),
            dlci: None,
            channel: None,
            phase: LinkPhase::Down,
            error: None,
            now_ms: 0,
            reported_phase: LinkPhase::Down,
            reported_call: CallPhase::Idle,
            command_queue: VecDeque::new(),
            call: CallPhase::Idle,
            caller: None,
            call_since_ms: 0,
            last_ring_ms: 0,
            last_dialed: "+15550142".into(),
            speaker_gain: 9,
            microphone_gain: 12,
            microphone_muted: false,
            voice_recognition: false,
            car_operator: None,
            transcript: VecDeque::new(),
            next_seq: 0,
            sdp_detail: None,
            dlc_detail: None,
        }
    }

    // -- driving ------------------------------------------------------------

    /// Starts the head unit reaching for the phone. Idempotent.
    pub fn start(&mut self) {
        if self.phase == LinkPhase::Down {
            self.phase = LinkPhase::Discovering;
        }
    }

    /// One step. `now_ms` is a monotonic millisecond clock supplied by the
    /// caller — this type keeps no clock of its own, so it stays usable from
    /// a test with a fabricated one.
    ///
    /// Exactly one round trip of RFCOMM frames moves per call, which is what
    /// makes the AT dialogue readable as it happens rather than appearing
    /// all at once.
    pub fn tick(&mut self, now_ms: u64) -> Vec<CarKitEvent> {
        self.now_ms = now_ms;
        let mut events = Vec::new();

        if self.phase == LinkPhase::Discovering {
            self.run_sdp(&mut events);
        }
        self.service_ringing();
        self.service_outgoing();
        self.pump(&mut events);
        self.pump_command_queue();

        if self.reported_phase != self.phase {
            self.reported_phase = self.phase;
            events.push(CarKitEvent::LinkPhase(self.phase));
        }
        if self.reported_call != self.call {
            self.reported_call = self.call;
            events.push(CarKitEvent::CallPhase(self.call));
        }
        events
    }

    // -- the phone's controls ------------------------------------------------

    /// A call arrives at the phone. HFP v1.9 4.13: the AG raises
    /// `callsetup = 1`, then alerts the HF with `RING` (plus `+CLIP` when the
    /// HF asked for caller ID) until it is answered or gone.
    pub fn incoming_call(&mut self, number: &str) -> bool {
        if self.phase != LinkPhase::Ready || self.call != CallPhase::Idle {
            return false;
        }
        self.caller = Some(number.to_string());
        self.ag.calls = vec![CallInfo {
            index: 1,
            direction: CallInfoDirection::MobileTerminated,
            status: CallInfoStatus::Incoming,
            mode: CallInfoMode::Voice,
            multi_party: CallInfoMultiParty::NotInConference,
            number: Some(number.to_string()),
            kind: Some(129),
        }];
        self.set_call_phase(CallPhase::Incoming);
        self.push_indicator(AgIndicator::CallSetup, 1);
        self.alert_once();
        true
    }

    /// The phone places a call itself — dialed on its own keypad, not by the
    /// head unit. HFP v1.9 4.15: no command crosses the link, only the
    /// `callsetup` indicator, which is why the dashboard follows a call it
    /// never initiated.
    pub fn phone_dial(&mut self, number: &str) -> bool {
        if self.phase != LinkPhase::Ready || self.call != CallPhase::Idle {
            return false;
        }
        self.begin_outgoing(number);
        true
    }

    /// The phone ends the call at its own end.
    pub fn phone_end_call(&mut self) -> bool {
        if self.call == CallPhase::Idle {
            return false;
        }
        self.clear_call();
        true
    }

    /// Sets one of the indicators the AG owns and pushes down as `+CIEV`.
    pub fn set_indicator(&mut self, indicator: AgIndicator, value: u32) -> bool {
        if matches!(
            indicator,
            AgIndicator::Call | AgIndicator::CallSetup | AgIndicator::CallHeld
        ) {
            // Those belong to the call state machine, not to a slider.
            return false;
        }
        self.push_indicator(indicator, value);
        true
    }

    /// Renames the network the phone is camped on. The head unit does not
    /// find out until it asks again, so a fresh `AT+COPS?` is queued — which
    /// is what a real head unit does when the service indicator moves.
    pub fn set_operator(&mut self, name: &str) {
        self.ag.network_operator = name.to_string();
        if self.phase == LinkPhase::Ready {
            self.enqueue(HeadUnitCommand::Raw("AT+COPS?".into()));
        }
    }

    // -- the head unit's controls -------------------------------------------

    /// Green button: `ATA`.
    pub fn answer(&mut self) -> bool {
        if self.call != CallPhase::Incoming {
            return false;
        }
        self.enqueue(HeadUnitCommand::Answer)
    }

    /// Red button: `AT+CHUP`. HFP v1.9 4.16 uses the same command to reject
    /// an incoming call and to end an active one; what changes is which
    /// indicator the AG moves in reply.
    pub fn hang_up(&mut self) -> bool {
        if self.call == CallPhase::Idle {
            return false;
        }
        self.enqueue(HeadUnitCommand::HangUp)
    }

    /// The head unit places the call: `ATD<number>;`.
    pub fn car_dial(&mut self, number: &str) -> bool {
        if self.call != CallPhase::Idle {
            return false;
        }
        self.last_dialed = number.to_string();
        self.enqueue(HeadUnitCommand::Dial(number.to_string()))
    }

    /// Speaker gain knob: `AT+VGS` (HFP v1.9 4.29.2), range 0-15.
    pub fn set_speaker_gain(&mut self, level: u8) -> bool {
        let level = level.min(15);
        self.speaker_gain = level;
        self.enqueue(HeadUnitCommand::Raw(format!("AT+VGS={level}")))
    }

    /// Microphone gain knob: `AT+VGM` (HFP v1.9 4.29.1), range 0-15.
    pub fn set_microphone_gain(&mut self, level: u8) -> bool {
        let level = level.min(15);
        self.microphone_gain = level;
        self.microphone_muted = false;
        self.enqueue(HeadUnitCommand::Raw(format!("AT+VGM={level}")))
    }

    /// Mic mute. HFP has no mute command: muting is gain zero, and the
    /// stored gain is what comes back on unmute.
    pub fn set_microphone_muted(&mut self, muted: bool) -> bool {
        self.microphone_muted = muted;
        let level = if muted { 0 } else { self.microphone_gain };
        self.enqueue(HeadUnitCommand::Raw(format!("AT+VGM={level}")))
    }

    /// Voice-assistant button: `AT+BVRA` (HFP v1.9 4.25).
    pub fn set_voice_recognition(&mut self, enabled: bool) -> bool {
        let state = if enabled {
            VoiceRecognitionState::Enable
        } else {
            VoiceRecognitionState::Disable
        };
        self.enqueue(HeadUnitCommand::VoiceRecognition(state))
    }

    /// Asks the phone to enumerate its calls: `AT+CLCC` (HFP v1.9 4.32.1).
    pub fn query_calls(&mut self) -> bool {
        self.enqueue(HeadUnitCommand::ListCalls)
    }

    // -- reading ------------------------------------------------------------

    /// Where the link has got to.
    pub fn phase(&self) -> LinkPhase {
        self.phase
    }

    /// Where the call has got to.
    pub fn call_phase(&self) -> CallPhase {
        self.call
    }

    /// Why the link died, if it did.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Every AT line so far, oldest first, capped at the most recent
    /// [`TRANSCRIPT_LIMIT`].
    pub fn transcript(&self) -> impl Iterator<Item = &AtLine> {
        self.transcript.iter()
    }

    // -- the link ------------------------------------------------------------

    /// Runs the whole SDP transaction in one tick. It is request/response
    /// with no waiting in between here, so spreading it over ticks would
    /// only be theatre.
    fn run_sdp(&mut self, events: &mut Vec<CarKitEvent>) {
        let mut request_bytes = 0usize;
        let mut response_bytes = 0usize;
        let mut round_trips = 0usize;

        let found = {
            let Self { hf_sdp, ag_sdp, .. } = self;
            find_ag_sdp_record(hf_sdp, |request| {
                request_bytes += request.len();
                round_trips += 1;
                let response = ag_sdp.handle_request(request, SDP_MTU);
                response_bytes += response.len();
                response
            })
        };

        let (channel, version, features) = match found {
            Ok(Some(record)) => record,
            Ok(None) => return self.fail("the phone has no Hands-Free Audio Gateway record", events),
            Err(error) => return self.fail(&format!("SDP: {error}"), events),
        };

        self.sdp_detail = Some(format!(
            "ServiceSearchAttributeRequest for Handsfree Audio Gateway (0x111F) — \
             {round_trips} round trip(s), {request_bytes} bytes out, {response_bytes} in. \
             Answer: RFCOMM server channel {channel}, HFP v1.{}, SDP features {features:#06x}.",
            version_minor(version)
        ));
        self.channel = Some(channel);

        self.ag_mux
            .listen(channel, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS);
        match self.hf_mux.start() {
            Ok(frame) => {
                self.to_ag.push(frame);
                self.phase = LinkPhase::StartingMultiplexer;
            }
            Err(error) => self.fail(&format!("RFCOMM: {error}"), events),
        }
    }

    /// Moves one round trip of RFCOMM frames: head unit to phone, then
    /// phone back to head unit.
    fn pump(&mut self, events: &mut Vec<CarKitEvent>) {
        for frame in std::mem::take(&mut self.to_ag) {
            match self.ag_mux.receive(&frame) {
                Ok((out, mux_events)) => {
                    self.to_hf.extend(out);
                    for event in mux_events {
                        self.on_ag_mux_event(event, events);
                    }
                }
                Err(error) => return self.fail(&format!("RFCOMM (phone): {error}"), events),
            }
        }
        for frame in std::mem::take(&mut self.to_hf) {
            match self.hf_mux.receive(&frame) {
                Ok((out, mux_events)) => {
                    self.to_ag.extend(out);
                    for event in mux_events {
                        self.on_hf_mux_event(event, events);
                    }
                }
                Err(error) => return self.fail(&format!("RFCOMM (head unit): {error}"), events),
            }
        }
    }

    fn on_hf_mux_event(&mut self, event: MultiplexerEvent, events: &mut Vec<CarKitEvent>) {
        match event {
            MultiplexerEvent::Started => {
                let channel = self.channel.unwrap_or(AG_RFCOMM_CHANNEL);
                match self
                    .hf_mux
                    .open_dlc(channel, DEFAULT_MAX_FRAME_SIZE, DEFAULT_INITIAL_CREDITS)
                {
                    Ok(frame) => {
                        self.to_ag.push(frame);
                        self.phase = LinkPhase::OpeningDlc;
                    }
                    Err(error) => self.fail(&format!("RFCOMM: {error}"), events),
                }
            }
            MultiplexerEvent::DlcOpened(dlci) => {
                self.dlci = Some(dlci);
                self.record_dlc_detail();
                self.phase = LinkPhase::EstablishingSlc;
                let bytes = self.hf.start_slc();
                self.hf_send(bytes);
            }
            MultiplexerEvent::DlcOpenRejected(_) => {
                self.fail("the phone refused the data link connection", events);
            }
            MultiplexerEvent::DataReceived(_, data) => {
                let (out, hfp_events) = self.hf.receive(&data);
                for line in out {
                    self.hf_send(line);
                }
                for event in hfp_events {
                    self.on_hf_hfp_event(event, events);
                }
            }
            MultiplexerEvent::Disconnected | MultiplexerEvent::DlcClosed(_) => {
                self.fail("the link went down", events);
            }
        }
    }

    fn on_ag_mux_event(&mut self, event: MultiplexerEvent, events: &mut Vec<CarKitEvent>) {
        match event {
            MultiplexerEvent::DataReceived(_, data) => {
                let (out, hfp_events) = self.ag.receive(&data);
                for line in out {
                    self.ag_send(line);
                }
                for event in hfp_events {
                    self.on_ag_hfp_event(event, events);
                }
            }
            MultiplexerEvent::DlcOpened(_) => self.record_dlc_detail(),
            _ => {}
        }
    }

    /// What the head unit makes of what the phone said.
    fn on_hf_hfp_event(&mut self, event: HfpEvent, events: &mut Vec<CarKitEvent>) {
        match event {
            HfpEvent::SlcComplete => {
                self.phase = LinkPhase::ConfiguringHeadUnit;
                self.queue_head_unit_setup();
                self.pump_command_queue();
            }
            HfpEvent::CommandCompleted {
                command, responses, ..
            } => {
                if command == "AT+COPS?"
                    && let Some(operator) = parse_network_operator(&responses)
                {
                    self.car_operator = Some(operator.clone());
                    events.push(CarKitEvent::Operator(operator));
                }
                if self.command_queue.is_empty() {
                    if self.phase == LinkPhase::ConfiguringHeadUnit {
                        self.phase = LinkPhase::Ready;
                    }
                } else {
                    self.pump_command_queue();
                }
            }
            HfpEvent::Ring => events.push(CarKitEvent::Ring),
            HfpEvent::CliNotification(cli) => {
                self.caller = Some(cli.number.clone());
                events.push(CarKitEvent::CallerId(cli.number));
            }
            HfpEvent::AgIndicatorUpdated(_) => {}
            HfpEvent::SpeakerVolume(level) => {
                self.speaker_gain = level;
                events.push(CarKitEvent::SpeakerGain(level));
            }
            HfpEvent::MicrophoneVolume(level) => {
                self.microphone_gain = level;
                events.push(CarKitEvent::MicrophoneGain(level));
            }
            HfpEvent::VoiceRecognition(state) => {
                self.voice_recognition = state != VoiceRecognitionState::Disable;
                events.push(CarKitEvent::VoiceRecognition(self.voice_recognition));
            }
            _ => {}
        }
    }

    /// What the phone makes of what the head unit asked for. The AG owns the
    /// call, so this is where the state machine actually lives.
    fn on_ag_hfp_event(&mut self, event: HfpEvent, events: &mut Vec<CarKitEvent>) {
        match event {
            HfpEvent::Answer => {
                if matches!(self.call, CallPhase::Incoming | CallPhase::Alerting) {
                    self.activate_call();
                }
            }
            HfpEvent::HangUp => self.clear_call(),
            HfpEvent::Dial(number) => {
                if self.call == CallPhase::Idle {
                    self.caller = Some(number.clone());
                    self.last_dialed = number.clone();
                    self.begin_outgoing(&number);
                }
            }
            HfpEvent::VoiceRecognition(state) => {
                self.voice_recognition = state != VoiceRecognitionState::Disable;
                events.push(CarKitEvent::VoiceRecognition(self.voice_recognition));
            }
            HfpEvent::SpeakerVolume(level) => {
                self.speaker_gain = level;
                events.push(CarKitEvent::SpeakerGain(level));
            }
            HfpEvent::MicrophoneVolume(level) => {
                self.microphone_muted = level == 0;
                if level > 0 {
                    self.microphone_gain = level;
                }
                events.push(CarKitEvent::MicrophoneGain(level));
            }
            HfpEvent::CallHold { .. } => {}
            _ => {}
        }
    }

    // -- the call state machine ----------------------------------------------

    /// HFP v1.9 4.13.1: the AG alerts with `RING`, and repeats it, adding a
    /// `+CLIP` alongside each one when the HF turned caller ID on with
    /// `AT+CLIP=1`. Sending `+CLIP` unasked is a protocol error, so the
    /// AG's own flag decides, not the page.
    fn alert_once(&mut self) {
        let ring = self.ag.send_ring();
        self.ag_send(ring);
        if self.ag.cli_notification_enabled
            && let Some(number) = self.caller.clone()
        {
            let cli = CallLineIdentification {
                number,
                kind: 129,
                subaddr: None,
                satype: None,
                alpha: None,
                cli_validity: None,
            };
            let clip = self.ag.send_cli_notification(&cli);
            self.ag_send(clip);
        }
        self.last_ring_ms = self.now_ms;
    }

    fn service_ringing(&mut self) {
        if self.call == CallPhase::Incoming
            && self.now_ms.saturating_sub(self.last_ring_ms) >= RING_PERIOD_MS
        {
            self.alert_once();
        }
    }

    /// Walks an outgoing call through `callsetup = 2 → 3` and then into
    /// `call = 1`. The timings stand in for the cellular network, which is
    /// not Bluetooth at all; the indicator transitions are the profile's.
    fn service_outgoing(&mut self) {
        let elapsed = self.now_ms.saturating_sub(self.call_since_ms);
        match self.call {
            CallPhase::Dialing if elapsed >= DIALING_MS => {
                self.set_call_phase(CallPhase::Alerting);
                if let Some(call) = self.ag.calls.first_mut() {
                    call.status = CallInfoStatus::Alerting;
                }
                self.push_indicator(AgIndicator::CallSetup, 3);
            }
            CallPhase::Alerting if elapsed >= ALERTING_MS => self.activate_call(),
            _ => {}
        }
    }

    fn begin_outgoing(&mut self, number: &str) {
        self.caller = Some(number.to_string());
        self.ag.calls = vec![CallInfo {
            index: 1,
            direction: CallInfoDirection::MobileOriginated,
            status: CallInfoStatus::Dialing,
            mode: CallInfoMode::Voice,
            multi_party: CallInfoMultiParty::NotInConference,
            number: Some(number.to_string()),
            kind: Some(129),
        }];
        self.set_call_phase(CallPhase::Dialing);
        self.push_indicator(AgIndicator::CallSetup, 2);
    }

    /// HFP v1.9 4.13.1 and 4.14: `call` goes to 1 first, then `callsetup`
    /// drops to 0. The order matters — a head unit that sees `callsetup = 0`
    /// first has a moment in which no call exists at all.
    fn activate_call(&mut self) {
        self.set_call_phase(CallPhase::Active);
        if let Some(call) = self.ag.calls.first_mut() {
            call.status = CallInfoStatus::Active;
        }
        self.push_indicator(AgIndicator::Call, 1);
        self.push_indicator(AgIndicator::CallSetup, 0);
    }

    fn clear_call(&mut self) {
        let was = self.call;
        self.ag.calls.clear();
        self.caller = None;
        self.set_call_phase(CallPhase::Idle);
        match was {
            CallPhase::Active => self.push_indicator(AgIndicator::Call, 0),
            CallPhase::Incoming | CallPhase::Dialing | CallPhase::Alerting => {
                self.push_indicator(AgIndicator::CallSetup, 0)
            }
            CallPhase::Idle => {}
        }
    }

    fn set_call_phase(&mut self, phase: CallPhase) {
        if self.call != phase {
            self.call = phase;
            self.call_since_ms = self.now_ms;
        }
    }

    /// Moves an AG indicator and sends the `+CIEV` for it — but only if the
    /// HF turned reporting on with `AT+CMER`. An unasked-for `+CIEV` is a
    /// protocol error, and suppressing it here is what makes the indicator
    /// mirror on the dashboard mean something.
    fn push_indicator(&mut self, indicator: AgIndicator, value: u32) {
        match self.ag.update_ag_indicator(indicator, value) {
            Ok(line) => {
                if self.ag.indicator_report_enabled {
                    self.ag_send(line);
                }
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    // -- head-unit setup -----------------------------------------------------

    /// What a head unit asks for once the SLC is up, in the order a real one
    /// does: extended errors, caller ID, call waiting, the operator name,
    /// then its own gains. None of this is part of the SLC procedure — it is
    /// the HF configuring the AG for its own display, and until `AT+CLIP=1`
    /// has run the AG is not allowed to send caller ID at all.
    fn queue_head_unit_setup(&mut self) {
        for command in [
            "AT+CMEE=1".to_string(),
            "AT+CLIP=1".to_string(),
            "AT+CCWA=1".to_string(),
            "AT+COPS=3,0".to_string(),
            "AT+COPS?".to_string(),
            format!("AT+VGS={}", self.speaker_gain),
            format!("AT+VGM={}", self.microphone_gain),
        ] {
            self.command_queue.push_back(HeadUnitCommand::Raw(command));
        }
    }

    /// Queues one head-unit command, refusing before the link is usable.
    fn enqueue(&mut self, command: HeadUnitCommand) -> bool {
        if !matches!(
            self.phase,
            LinkPhase::ConfiguringHeadUnit | LinkPhase::Ready
        ) {
            return false;
        }
        self.command_queue.push_back(command);
        self.pump_command_queue();
        true
    }

    /// Sends the next queued command if the head unit's single
    /// outstanding-command slot is free.
    fn pump_command_queue(&mut self) {
        if self.hf.has_pending_command() {
            return;
        }
        let Some(command) = self.command_queue.pop_front() else {
            return;
        };
        let bytes = match command {
            HeadUnitCommand::Answer => self.hf.answer_incoming_call(),
            HeadUnitCommand::HangUp => self.hf.terminate_call(),
            HeadUnitCommand::Dial(number) => self.hf.dial(&number),
            HeadUnitCommand::ListCalls => self.hf.query_current_calls(),
            HeadUnitCommand::VoiceRecognition(state) => self.hf.set_voice_recognition(state),
            HeadUnitCommand::Raw(command) => self.hf.send_command(command),
        };
        self.hf_send(bytes);
    }

    // -- byte plumbing --------------------------------------------------------

    /// Sends one AT command line from the head unit, recording exactly what
    /// crossed the DLC.
    fn hf_send(&mut self, line: Vec<u8>) {
        self.log(true, &line);
        let Some(dlci) = self.dlci else { return };
        match self.hf_mux.write(dlci, &line) {
            Ok(frames) => self.to_ag.extend(frames),
            Err(error) => self.error = Some(format!("RFCOMM (head unit): {error}")),
        }
    }

    /// Sends one AT response line from the phone.
    fn ag_send(&mut self, line: Vec<u8>) {
        self.log(false, &line);
        let Some(dlci) = self.dlci else { return };
        match self.ag_mux.write(dlci, &line) {
            Ok(frames) => self.to_hf.extend(frames),
            Err(error) => self.error = Some(format!("RFCOMM (phone): {error}")),
        }
    }

    fn log(&mut self, from_hf: bool, line: &[u8]) {
        let text = String::from_utf8_lossy(line)
            .trim_matches(|c| c == '\r' || c == '\n')
            .to_string();
        if text.is_empty() {
            return;
        }
        let entry = AtLine {
            seq: self.next_seq,
            from_hf,
            text,
            hex: line
                .iter()
                .map(|b| format!("{b:02X}"))
                .collect::<Vec<_>>()
                .join(" "),
        };
        self.next_seq += 1;
        self.transcript.push_back(entry);
        while self.transcript.len() > TRANSCRIPT_LIMIT {
            self.transcript.pop_front();
        }
    }

    fn fail(&mut self, message: &str, events: &mut Vec<CarKitEvent>) {
        self.error = Some(message.to_string());
        self.phase = LinkPhase::Failed;
        events.push(CarKitEvent::Failed(message.to_string()));
    }

    /// Snapshots the credit window the DLC actually negotiated, which is the
    /// part of RFCOMM that is easiest to claim and hardest to see.
    fn record_dlc_detail(&mut self) {
        let Some(dlci) = self.dlci.or(self.channel.map(|c| c << 1)) else {
            return;
        };
        let Some(dlc) = self.hf_mux.dlcs.get(&dlci) else {
            return;
        };
        self.dlc_detail = Some(format!(
            "DLCI {dlci} — frame size {} out / {} in, {} credits granted at open",
            dlc.tx_max_frame_size, dlc.rx_max_frame_size, dlc.rx_initial_credits
        ));
    }
}

fn version_minor(version: ProfileVersion) -> u8 {
    match version {
        ProfileVersion::V1_5 => 5,
        ProfileVersion::V1_6 => 6,
        ProfileVersion::V1_7 => 7,
        ProfileVersion::V1_8 => 8,
        ProfileVersion::V1_9 => 9,
    }
}

// ---------------------------------------------------------------------------
// Configurations
// ---------------------------------------------------------------------------

/// The head unit's Hands-Free feature set. Codec negotiation is advertised
/// because the AT handshake for it is real; the audio it would select is
/// not (see the module doc comment).
pub fn head_unit_configuration() -> HfConfiguration {
    HfConfiguration {
        supported_hf_features: hf_feature::THREE_WAY_CALLING
            | hf_feature::CLI_PRESENTATION_CAPABILITY
            | hf_feature::ENHANCED_CALL_STATUS
            | hf_feature::CODEC_NEGOTIATION
            | hf_feature::HF_INDICATORS
            | hf_feature::ESCO_S4_SETTINGS_SUPPORTED,
        supported_hf_indicators: vec![HfIndicator::EnhancedSafety, HfIndicator::BatteryLevel],
        supported_audio_codecs: vec![AudioCodec::Cvsd, AudioCodec::Msbc],
    }
}

/// The phone's Audio Gateway feature set and indicator list.
///
/// The indicator **order** is load-bearing: `+CIEV` identifies an indicator
/// by its one-based position in the `+CIND=?` list, so this order is the
/// only thing that makes `+CIEV: 2,1` mean "call is up". This is the order
/// the common AGs use.
pub fn phone_configuration() -> AgConfiguration {
    let mut indicators = vec![
        AgIndicatorState::service(),
        AgIndicatorState::call(),
        AgIndicatorState::callsetup(),
        AgIndicatorState::callheld(),
        AgIndicatorState::signal(),
        AgIndicatorState::roam(),
        AgIndicatorState::battchg(),
    ];
    indicators[0].current_status = 1; // registered
    indicators[4].current_status = 4; // signal, of 5
    indicators[6].current_status = 4; // battery, of 5

    AgConfiguration {
        supported_ag_features: ag_feature::THREE_WAY_CALLING
            | ag_feature::IN_BAND_RING_TONE_CAPABILITY
            | ag_feature::REJECT_CALL
            | ag_feature::ENHANCED_CALL_STATUS
            | ag_feature::EXTENDED_ERROR_RESULT_CODES
            | ag_feature::CODEC_NEGOTIATION
            | ag_feature::HF_INDICATORS
            | ag_feature::ESCO_S4_SETTINGS_SUPPORTED,
        supported_ag_indicators: indicators,
        supported_hf_indicators: vec![HfIndicator::EnhancedSafety, HfIndicator::BatteryLevel],
        supported_ag_call_hold_operations: vec![
            CallHoldOperation::ReleaseAllHeldCalls,
            CallHoldOperation::ReleaseAllActiveCalls,
            CallHoldOperation::HoldAllActiveCalls,
            CallHoldOperation::AddHeldCall,
        ],
        supported_audio_codecs: vec![AudioCodec::Cvsd, AudioCodec::Msbc],
    }
}

// ---------------------------------------------------------------------------
// JSON for the page
// ---------------------------------------------------------------------------

/// One entry in the stack step-list.
#[derive(Serialize)]
struct StepJson {
    id: &'static str,
    label: &'static str,
    state: &'static str,
    detail: String,
}

/// One AG indicator as the phone holds it and as the head unit mirrors it.
#[derive(Serialize)]
struct IndicatorJson {
    index: usize,
    name: &'static str,
    value: u32,
    max: u32,
    mirrored: Option<u32>,
}

#[derive(Serialize)]
struct CarKitStatusJson {
    link: &'static str,
    error: Option<String>,
    steps: Vec<StepJson>,
    call: &'static str,
    caller: Option<String>,
    operator: String,
    car_operator: Option<String>,
    indicators: Vec<IndicatorJson>,
    speaker_gain: u8,
    microphone_gain: u8,
    microphone_muted: bool,
    voice_recognition: bool,
    last_dialed: String,
    codec: &'static str,
    ag_features: u32,
    hf_features: u32,
    clip_enabled: bool,
    ciev_enabled: bool,
    credits_out: u8,
    credits_in: u8,
    at: Vec<AtLine>,
    next_seq: u64,
}

impl CarKit {
    /// Everything the page renders, as JSON. `since_seq` selects the
    /// transcript tail the caller has not seen yet, so a page can append
    /// rather than redraw.
    pub fn status_json(&self, since_seq: u64) -> String {
        let dlc = self.dlci.and_then(|dlci| self.hf_mux.dlcs.get(&dlci));
        let status = CarKitStatusJson {
            link: self.phase.name(),
            error: self.error.clone(),
            steps: self.steps(),
            call: self.call.name(),
            caller: self.caller.clone(),
            operator: self.ag.network_operator.clone(),
            car_operator: self.car_operator.clone(),
            indicators: self.indicators(),
            speaker_gain: self.speaker_gain,
            microphone_gain: self.microphone_gain,
            microphone_muted: self.microphone_muted,
            voice_recognition: self.voice_recognition,
            last_dialed: self.last_dialed.clone(),
            codec: match self.hf.active_codec {
                AudioCodec::Cvsd => "CVSD",
                AudioCodec::Msbc => "mSBC",
                AudioCodec::Lc3Swb => "LC3-SWB",
            },
            ag_features: self.hf.supported_ag_features,
            hf_features: self.ag.supported_hf_features,
            clip_enabled: self.ag.cli_notification_enabled,
            ciev_enabled: self.ag.indicator_report_enabled,
            credits_out: dlc.map(|d| d.tx_credits).unwrap_or(0),
            credits_in: dlc.map(|d| d.rx_credits).unwrap_or(0),
            at: self
                .transcript
                .iter()
                .filter(|line| line.seq >= since_seq)
                .cloned()
                .collect(),
            next_seq: self.next_seq,
        };
        serde_json::to_string(&status).unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
    }

    fn indicators(&self) -> Vec<IndicatorJson> {
        self.ag
            .ag_indicators
            .iter()
            .enumerate()
            .map(|(index, state)| IndicatorJson {
                index: index + 1,
                name: indicator_name(state.indicator),
                value: state.current_status,
                max: indicator_max(state.indicator),
                mirrored: self.hf.ag_indicators.get(index).map(|s| s.current_status),
            })
            .collect()
    }

    fn steps(&self) -> Vec<StepJson> {
        let phase = self.phase;
        let order = |p: LinkPhase| match p {
            LinkPhase::Down => 0,
            LinkPhase::Discovering => 1,
            LinkPhase::StartingMultiplexer => 2,
            LinkPhase::OpeningDlc => 3,
            LinkPhase::EstablishingSlc => 4,
            LinkPhase::ConfiguringHeadUnit => 5,
            LinkPhase::Ready | LinkPhase::Failed => 6,
        };
        let reached = order(phase);
        let step = |id, label, at: usize, detail: String| StepJson {
            id,
            label,
            state: if phase == LinkPhase::Failed && at == reached {
                "failed"
            } else if reached > at {
                "done"
            } else if reached == at {
                "active"
            } else {
                "pending"
            },
            detail,
        };
        vec![
            step(
                "sdp",
                "SDP — find the phone's Audio Gateway record",
                1,
                self.sdp_detail.clone().unwrap_or_default(),
            ),
            step(
                "mux",
                "RFCOMM multiplexer — SABM/UA on DLCI 0",
                2,
                if self.hf_mux.is_connected() {
                    "session up; the control channel carries PN and MSC".into()
                } else {
                    String::new()
                },
            ),
            step(
                "dlc",
                "Data link connection — PN, SABM/UA, credit grant",
                3,
                self.dlc_detail.clone().unwrap_or_default(),
            ),
            step(
                "slc",
                "Service Level Connection — BRSF, BAC, CIND, CMER, CHLD, BIND",
                4,
                if self.hf.supported_ag_features != 0 {
                    format!(
                        "AG features {:#06x}, {} indicators discovered, codec {}",
                        self.hf.supported_ag_features,
                        self.hf.ag_indicators.len(),
                        match self.hf.active_codec {
                            AudioCodec::Cvsd => "CVSD",
                            AudioCodec::Msbc => "mSBC",
                            AudioCodec::Lc3Swb => "LC3-SWB",
                        }
                    )
                } else {
                    String::new()
                },
            ),
            step(
                "setup",
                "Head-unit setup — CMEE, CLIP, CCWA, COPS, VGS/VGM",
                5,
                if self.command_queue.is_empty() && reached >= 5 {
                    format!(
                        "caller ID {}, indicator reporting {}",
                        if self.ag.cli_notification_enabled {
                            "on"
                        } else {
                            "off"
                        },
                        if self.ag.indicator_report_enabled {
                            "on"
                        } else {
                            "off"
                        }
                    )
                } else {
                    String::new()
                },
            ),
            step(
                "call",
                "Call state machine — call / callsetup over +CIEV",
                6,
                match self.call {
                    CallPhase::Idle => "idle".into(),
                    other => other.name().to_string(),
                },
            ),
        ]
    }
}

fn indicator_name(indicator: AgIndicator) -> &'static str {
    match indicator {
        AgIndicator::Service => "service",
        AgIndicator::Call => "call",
        AgIndicator::CallSetup => "callsetup",
        AgIndicator::CallHeld => "callheld",
        AgIndicator::Signal => "signal",
        AgIndicator::Roam => "roam",
        AgIndicator::BatteryCharge => "battchg",
    }
}

fn indicator_max(indicator: AgIndicator) -> u32 {
    match indicator {
        AgIndicator::Service | AgIndicator::Call | AgIndicator::Roam => 1,
        AgIndicator::CallSetup => 3,
        AgIndicator::CallHeld => 2,
        AgIndicator::Signal | AgIndicator::BatteryCharge => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs ticks until `ready`, or panics. Ticks advance 100 ms each, the
    /// same cadence the page uses.
    fn drive(kit: &mut CarKit, mut until: impl FnMut(&CarKit) -> bool) -> Vec<CarKitEvent> {
        let mut events = Vec::new();
        for step in 0..400 {
            events.extend(kit.tick(step * 100));
            if until(kit) {
                return events;
            }
        }
        panic!(
            "car kit stalled at {:?} / {:?}: {:?}",
            kit.phase(),
            kit.call_phase(),
            kit.error()
        );
    }

    fn connected() -> CarKit {
        let mut kit = CarKit::new();
        kit.start();
        drive(&mut kit, |k| k.phase() == LinkPhase::Ready);
        kit
    }

    fn said(kit: &CarKit, text: &str) -> bool {
        kit.transcript().any(|line| line.text == text)
    }

    #[test]
    fn test_the_head_unit_discovers_the_channel_rather_than_assuming_it() {
        let kit = connected();
        let detail = kit
            .steps()
            .into_iter()
            .find(|s| s.id == "sdp")
            .expect("sdp step")
            .detail;
        assert!(
            detail.contains(&format!("server channel {AG_RFCOMM_CHANNEL}")),
            "the SDP answer should name the channel: {detail}"
        );
    }

    #[test]
    fn test_the_service_level_connection_runs_in_the_order_the_profile_specifies() {
        let kit = connected();
        let commands: Vec<&str> = kit
            .transcript()
            .filter(|line| line.from_hf)
            .map(|line| line.text.as_str())
            .collect();
        let expected = [
            "AT+BRSF=",
            "AT+BAC=",
            "AT+CIND=?",
            "AT+CIND?",
            "AT+CMER=",
            "AT+CHLD=?",
            "AT+BIND=",
            "AT+BIND=?",
            "AT+BIND?",
        ];
        let mut cursor = 0;
        for prefix in expected {
            let found = commands[cursor..]
                .iter()
                .position(|c| c.starts_with(prefix))
                .unwrap_or_else(|| panic!("{prefix} missing after {cursor}: {commands:?}"));
            cursor += found + 1;
        }
    }

    #[test]
    fn test_an_incoming_call_rings_the_head_unit_with_the_caller_id() {
        let mut kit = connected();
        assert!(kit.incoming_call("+15551234"));
        drive(&mut kit, |k| said(k, "RING"));

        assert!(said(&kit, "RING"));
        assert!(
            said(&kit, "+CLIP: \"+15551234\",129"),
            "the +CLIP line should carry the number: {:?}",
            kit.transcript().map(|l| &l.text).collect::<Vec<_>>()
        );
        // callsetup is the third indicator, so +CIEV names index 3.
        assert!(said(&kit, "+CIEV: 3,1"));
    }

    #[test]
    fn test_answering_sends_ata_and_flips_the_call_indicator() {
        let mut kit = connected();
        kit.incoming_call("+15551234");
        drive(&mut kit, |k| said(k, "RING"));

        assert!(kit.answer());
        drive(&mut kit, |k| k.call_phase() == CallPhase::Active);

        assert!(said(&kit, "ATA"));
        // call is the second indicator; it goes up before callsetup goes down.
        let seq_call = kit
            .transcript()
            .find(|l| l.text == "+CIEV: 2,1")
            .expect("call = 1")
            .seq;
        let seq_setup = kit
            .transcript()
            .find(|l| l.text == "+CIEV: 3,0" && l.seq > seq_call)
            .expect("callsetup = 0 after call = 1")
            .seq;
        assert!(seq_setup > seq_call);
    }

    #[test]
    fn test_the_head_unit_hangs_up_with_chup() {
        let mut kit = connected();
        kit.incoming_call("+15551234");
        drive(&mut kit, |k| said(k, "RING"));
        kit.answer();
        drive(&mut kit, |k| k.call_phase() == CallPhase::Active);

        assert!(kit.hang_up());
        drive(&mut kit, |k| k.call_phase() == CallPhase::Idle);
        assert!(said(&kit, "AT+CHUP"));
        assert!(said(&kit, "+CIEV: 2,0"));
    }

    #[test]
    fn test_the_phone_can_end_the_call_without_the_head_unit_asking() {
        let mut kit = connected();
        kit.incoming_call("+15551234");
        drive(&mut kit, |k| said(k, "RING"));
        kit.answer();
        drive(&mut kit, |k| k.call_phase() == CallPhase::Active);

        let before = kit.transcript().filter(|l| l.text == "AT+CHUP").count();
        assert!(kit.phone_end_call());
        drive(&mut kit, |k| said(k, "+CIEV: 2,0"));
        assert_eq!(
            kit.transcript().filter(|l| l.text == "AT+CHUP").count(),
            before,
            "an AG-side hangup puts no command on the wire, only an indicator"
        );
    }

    #[test]
    fn test_dialing_from_the_dashboard_uses_the_voice_call_form_of_atd() {
        let mut kit = connected();
        assert!(kit.car_dial("5550142"));
        drive(&mut kit, |k| k.call_phase() == CallPhase::Dialing);
        assert!(
            said(&kit, "ATD5550142;"),
            "HFP 4.19.1 requires the trailing semicolon"
        );
        drive(&mut kit, |k| k.call_phase() == CallPhase::Alerting);
        assert!(said(&kit, "+CIEV: 3,2"));
        assert!(said(&kit, "+CIEV: 3,3"));
    }

    #[test]
    fn test_a_call_the_phone_placed_reaches_the_dashboard_with_no_command_at_all() {
        let mut kit = connected();
        assert!(kit.phone_dial("+15559876"));
        drive(&mut kit, |k| said(k, "+CIEV: 3,2"));
        assert!(!said(&kit, "ATD+15559876;"));
    }

    #[test]
    fn test_the_head_unit_reads_the_operator_name_off_the_wire() {
        let kit = connected();
        assert!(said(&kit, "AT+COPS=3,0"));
        assert!(said(&kit, "AT+COPS?"));
        assert!(said(&kit, "+COPS: 0,0,\"Simble Mobile\""));
        assert_eq!(kit.car_operator.as_deref(), Some("Simble Mobile"));
    }

    #[test]
    fn test_the_gain_knobs_are_the_profiles_own_commands() {
        let mut kit = connected();
        assert!(kit.set_speaker_gain(13));
        assert!(kit.set_microphone_muted(true));
        drive(&mut kit, |k| said(k, "AT+VGM=0"));
        assert!(said(&kit, "AT+VGS=13"));
    }

    #[test]
    fn test_an_indicator_the_phone_moves_reaches_the_head_units_mirror() {
        let mut kit = connected();
        assert!(kit.set_indicator(AgIndicator::Signal, 1));
        drive(&mut kit, |k| {
            k.hf.ag_indicators
                .iter()
                .any(|s| s.indicator == AgIndicator::Signal && s.current_status == 1)
        });
        // signal is the fifth indicator.
        assert!(said(&kit, "+CIEV: 5,1"));
    }

    #[test]
    fn test_the_call_indicators_are_not_settable_by_hand() {
        let mut kit = connected();
        assert!(!kit.set_indicator(AgIndicator::Call, 1));
        assert_eq!(kit.call_phase(), CallPhase::Idle);
    }

    #[test]
    fn test_nothing_can_happen_before_the_link_is_up() {
        let mut kit = CarKit::new();
        assert!(!kit.incoming_call("+15551234"));
        assert!(!kit.answer());
        assert!(!kit.car_dial("123"));
        assert!(kit.transcript().next().is_none());
    }

    #[test]
    fn test_the_dlc_negotiates_a_credit_window_rather_than_flowing_freely() {
        let kit = connected();
        let dlci = kit.dlci.expect("dlc open");
        let dlc = kit.hf_mux.dlcs.get(&dlci).expect("dlc");
        assert!(
            dlc.tx_credits > 0,
            "the head unit may only write while it holds credits"
        );
        assert_eq!(dlc.rx_initial_credits, DEFAULT_INITIAL_CREDITS);
    }

    #[test]
    fn test_every_transcript_line_is_the_bytes_that_were_written() {
        let kit = connected();
        for line in kit.transcript() {
            let bytes: Vec<u8> = line
                .hex
                .split(' ')
                .map(|b| u8::from_str_radix(b, 16).expect("hex"))
                .collect();
            let decoded = String::from_utf8_lossy(&bytes);
            assert!(
                decoded.contains(&line.text),
                "{:?} is not in {decoded:?}",
                line.text
            );
            // Commands are \r-terminated, responses \r\n-wrapped: HFP 4.34.
            if line.from_hf {
                assert_eq!(*bytes.last().expect("nonempty"), b'\r');
            } else {
                assert!(bytes.starts_with(b"\r\n") && bytes.ends_with(b"\r\n"));
            }
        }
    }
}
