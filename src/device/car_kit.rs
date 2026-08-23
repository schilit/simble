// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The hands-free car kit: a phone and a head unit, on the simulated
//! BR/EDR link a real pair would use.
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
//! * a [`SceneEngine`] holding two BR/EDR devices — a phone with a real
//!   BD_ADDR that is discoverable and connectable, and a head unit that
//!   inquires for it, resolves its name, pages it, and opens L2CAP;
//! * an [`AgProtocol`] — the phone, in the Audio Gateway role, which owns
//!   the calls and the indicators;
//! * an [`HfProtocol`] — the head unit, in the Hands-Free role, which
//!   drives the call;
//! * an **audio connection** — a real SCO/eSCO link, set up by the phone
//!   over HCI when the call needs it and torn down when the call ends,
//!   carrying payload both ways on a handle of its own. The Service Level
//!   Connection above it survives the audio coming and going, which is what
//!   makes the second call cost one setup rather than a whole bring-up.
//!
//! The two protocol objects are attached to the ends of one RFCOMM data link
//! by a pair of [`SharedRfcommPort`]s: AT bytes written to a port become UIH
//! frames, ride an L2CAP Basic Mode channel on PSM 3, ride an ACL connection,
//! and cross the simulated controller in [`crate::controller::sim`] before
//! the far end's [`ClassicHost`](super::ClassicHost) hands them up. There is
//! nothing wired directly together: unplug the phone's inquiry scan and the
//! head unit never finds it.
//!
//! Transport-free in the same sense as [`CisCentral`](super::CisCentral):
//! no sockets and no clock of its own. The caller pumps it with
//! [`CarKit::tick`] and reads state back out.
//!
//! ## What is not here
//!
//! * **Any codec.** The call audio has a real path — see below — but
//!   nothing on it is encoded. Codec negotiation (`AT+BAC`/`+BCS`) settles
//!   *which* codec, that choice becomes a Voice Setting and a packet-type
//!   mask on the wire, and then a counter pattern crosses the link in place
//!   of speech. Nothing transcodes; `crate::audio`'s SBC and LC3 encoders
//!   are not wired in.
//! * **Pairing.** The link is unauthenticated and unencrypted: no Secure
//!   Simple Pairing, no link key. A real head unit pairs once and bonds.
//! * **A2DP, AVRCP, PBAP.** Not wired here.

use std::collections::VecDeque;

use serde::Serialize;

use crate::classic::hfp::{
    AgConfiguration, AgIndicator, AgIndicatorState, AgProtocol, AudioCodec, AudioConnectionState,
    CallHoldOperation, CallInfo, CallInfoDirection, CallInfoMode, CallInfoMultiParty,
    CallInfoStatus, CallLineIdentification, HfConfiguration, HfIndicator, HfProtocol, HfpEvent,
    ProfileVersion, VoiceRecognitionState, ag_feature, hf_feature, make_ag_sdp_records,
    parse_network_operator,
};
use crate::classic::sdp::SdpUuid;
use crate::device::SharedRfcommPort;
use crate::transport::wasm_ws::{ClassicDevice, ClassicPhase, SceneEngine};
use crate::types::Address;

/// Server channel the phone advertises its Audio Gateway record on. Nothing
/// in the profile fixes it — the head unit learns it from SDP, which is the
/// point of doing the search at all.
pub const AG_RFCOMM_CHANNEL: u8 = 4;

/// SDP record handle for the phone's Audio Gateway record.
const AG_SERVICE_RECORD_HANDLE: u32 = 0x0001_0004;

/// The Handsfree Audio Gateway service class (Assigned Numbers) — what the
/// phone's record advertises itself as, and what the head unit searches the
/// phone's SDP server for.
const HANDSFREE_AUDIO_GATEWAY: SdpUuid = SdpUuid::Uuid16(0x111F);

/// The phone's BD_ADDR. A BR/EDR address is a fixed public identity — there
/// is no privacy, no resolvable address and no rotation on this transport,
/// which is exactly why a car remembers a phone for years.
pub const PHONE_ADDRESS: Address = Address::new([0x0A, 0x11, 0x00, 0xCC, 0xBB, 0xAA]);

/// The head unit's BD_ADDR.
pub const HEAD_UNIT_ADDRESS: Address = Address::new([0x0C, 0x22, 0x00, 0xCC, 0xBB, 0xAA]);

/// The phone's Class of Device, 0x5A020C: major class Phone, minor class
/// Smartphone, with the Networking / Capturing / Object Transfer / Telephony
/// service bits set. This is the number a car's pairing list turns into a
/// phone icon, and the reason a head unit can filter its inquiry results
/// before any name has been resolved.
const PHONE_CLASS_OF_DEVICE: [u8; 3] = [0x0C, 0x02, 0x5A];

/// The head unit's Class of Device, 0x240420: major class Audio/Video,
/// minor class Car audio, Audio + Rendering service bits.
const HEAD_UNIT_CLASS_OF_DEVICE: [u8; 3] = [0x20, 0x04, 0x24];

/// Simulated seconds one scene step advances. The BR/EDR half of the
/// simulated controller counts *ticks* rather than reading this clock, so
/// this value only has to be monotonic; it is here so the LE devices that
/// may share a scene see a sensible rate.
const SCENE_STEP_SECONDS: f64 = 0.01;

/// Scene steps one [`CarKit::tick`] may spend.
///
/// Bring-up — inquiry, Remote Name Request, page, L2CAP, SDP, RFCOMM — is
/// dozens of HCI round trips with nothing to print, so a page ticking at
/// 8 Hz would take ten seconds to reach the first AT line if it spent one
/// step per frame. The loop stops early as soon as an AT line is produced,
/// which keeps the dialogue paced at roughly a line per frame once the
/// silent part is over.
const SCENE_STEPS_PER_TICK: usize = 24;

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

/// Bytes in one synthetic audio frame put on the synchronous link while a
/// call is up.
///
/// 60 is the payload a real HV3 (CVSD) or EV3 (mSBC) packet carries, and it
/// is what the controller reports at setup. The *contents* are a counter
/// pattern, not speech: simble transcodes nothing, and a frame of fake PCM
/// would be a claim about a codec that is not implemented. What this proves
/// is the path — that bytes written at one end come out of the other, on the
/// audio handle, byte for byte.
const AUDIO_FRAME_BYTES: usize = 60;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// How far the head unit has got in reaching the phone's Hands-Free service.
///
/// The first three are the BR/EDR link coming up and belong to
/// [`ClassicDevice`]; the rest are HFP's own and belong to this type. They
/// are one list because from the dashboard's point of view there is one
/// question — can I make a call yet — and the interesting failures are in
/// the half nobody usually shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkPhase {
    /// Nothing started.
    Down,
    /// Inquiring on the GIAC for the phone, then asking it its name.
    Inquiring,
    /// Paging the phone: the ACL connection is being made.
    Paging,
    /// Searching the phone's SDP server for an Audio Gateway record.
    Discovering,
    /// Opening L2CAP PSM 3, the RFCOMM multiplexer session, and the data
    /// link connection on the channel SDP named.
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
            Self::Inquiring => "inquiring",
            Self::Paging => "paging",
            Self::Discovering => "discovering",
            Self::OpeningDlc => "opening-dlc",
            Self::EstablishingSlc => "establishing-slc",
            Self::ConfiguringHeadUnit => "configuring",
            Self::Ready => "ready",
            Self::Failed => "failed",
        }
    }

    /// Whether HFP, rather than the BR/EDR link underneath it, is what the
    /// phase is now reporting on.
    fn is_profile_phase(self) -> bool {
        matches!(
            self,
            Self::EstablishingSlc | Self::ConfiguringHeadUnit | Self::Ready | Self::Failed
        )
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
    /// The audio connection came up: the SCO/eSCO link is carrying the call.
    AudioConnected(AudioCodec),
    /// The audio connection went away, leaving the SLC up.
    AudioDisconnected,
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
pub struct CarKit {
    // --- the BR/EDR link: two devices, one simulated controller ---
    scene: SceneEngine,
    /// Index of the phone (the acceptor) in `scene`.
    phone: usize,
    /// Index of the head unit (the initiator) in `scene`.
    head_unit: usize,
    /// Monotonic simulated time handed to `scene.tick`.
    scene_time: f64,
    /// Whether [`CarKit::start`] has been called.
    started: bool,

    // --- the phone, in the Audio Gateway role ---
    ag: AgProtocol,
    /// The phone's end of the RFCOMM data link: AT responses go in here and
    /// come out of the head unit's port, having crossed L2CAP, the ACL
    /// connection, and the simulated controller.
    ag_port: SharedRfcommPort,

    // --- the head unit, in the Hands-Free role ---
    hf: HfProtocol,
    /// The head unit's end of the same data link. It exists before the DLC
    /// does, because the profile has to have somewhere to write.
    hf_port: SharedRfcommPort,

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

    // --- the audio connection ---
    /// Whether this type has told the two protocol objects the synchronous
    /// link is up. Derived from the controller, never assumed: the AT
    /// handshake settling a codec is not the same event as a link existing.
    audio_up: bool,
    /// Frames the phone has put on the link, and the head unit has taken off
    /// it — counted at both ends so a page can show that audio is moving
    /// rather than merely that a handle exists.
    audio_frames_from_phone: u64,
    audio_frames_from_car: u64,
    /// Frames each end has *received*, which is the number that proves the
    /// routing rather than the writing.
    audio_frames_to_phone: u64,
    audio_frames_to_car: u64,
    /// The counter stamped into the next synthetic frame.
    audio_frame_seq: u64,
    /// What the audio connection is, once there is one, for the page.
    audio_detail: Option<String>,
}

impl Default for CarKit {
    fn default() -> Self {
        Self::new()
    }
}

/// A [`SceneEngine`] is not `Debug` — it holds boxed protocol handlers — so
/// this reports what is actually worth seeing in a panic message.
impl std::fmt::Debug for CarKit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CarKit")
            .field("link", &self.phase)
            .field("classic", &self.classic_phase())
            .field("call", &self.call)
            .field("error", &self.error)
            .field("at_lines", &self.next_seq)
            .finish()
    }
}

impl CarKit {
    /// Builds a phone and a head unit that have never met, in a scene of
    /// their own.
    pub fn new() -> Self {
        let mut ag = AgProtocol::new(phone_configuration());
        ag.network_operator = "Simble Mobile".into();

        let mut scene = SceneEngine::new();
        // The phone is the acceptor: discoverable, connectable, and serving
        // the one SDP record that says where its Audio Gateway lives.
        let (phone_device, ag_port) = ClassicDevice::serving(
            "Simble Phone",
            PHONE_CLASS_OF_DEVICE,
            AG_RFCOMM_CHANNEL,
            vec![(
                AG_SERVICE_RECORD_HANDLE,
                make_ag_sdp_records(
                    AG_SERVICE_RECORD_HANDLE,
                    AG_RFCOMM_CHANNEL,
                    &phone_configuration(),
                    ProfileVersion::V1_9,
                ),
            )],
        );
        let phone = scene.add_classic_device(PHONE_ADDRESS, phone_device);

        // The head unit is the initiator, and it is told *the address*, not
        // the channel: which RFCOMM channel to open is what the SDP search
        // is for.
        let (car_device, hf_port) = ClassicDevice::client(
            "Simble Head Unit",
            HEAD_UNIT_CLASS_OF_DEVICE,
            PHONE_ADDRESS,
            HANDSFREE_AUDIO_GATEWAY,
        );
        let head_unit = scene.add_classic_device(HEAD_UNIT_ADDRESS, car_device);

        Self {
            scene,
            phone,
            head_unit,
            scene_time: 0.0,
            started: false,
            ag,
            ag_port,
            hf: HfProtocol::new(head_unit_configuration()),
            hf_port,
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
            audio_up: false,
            audio_frames_from_phone: 0,
            audio_frames_from_car: 0,
            audio_frames_to_phone: 0,
            audio_frames_to_car: 0,
            audio_frame_seq: 0,
            audio_detail: None,
        }
    }

    // -- driving ------------------------------------------------------------

    /// Starts the head unit reaching for the phone. Idempotent.
    ///
    /// The phase stays [`LinkPhase::Down`] until the first [`Self::tick`]:
    /// the head unit has not inquired yet, and saying it has before any HCI
    /// has moved would cost the caller the one frame in which it could draw
    /// the inquiry.
    pub fn start(&mut self) {
        self.started = true;
    }

    /// One step. `now_ms` is a monotonic millisecond clock supplied by the
    /// caller — this type keeps no clock of its own, so it stays usable from
    /// a test with a fabricated one.
    ///
    /// The simulated stack is pumped until something a caller can see has
    /// happened — an AT line crossed the data link, or the link reached a new
    /// phase — or [`SCENE_STEPS_PER_TICK`] steps are spent.
    ///
    /// Stopping on a phase change is what makes the BR/EDR bring-up visible
    /// at all: inquiry, paging, SDP and the DLC are dozens of HCI round trips
    /// with nothing to print, and a pump that only watched the transcript
    /// would run the lot inside one tick and leave a page that has been given
    /// the phases with no frame in which to draw them.
    pub fn tick(&mut self, now_ms: u64) -> Vec<CarKitEvent> {
        self.now_ms = now_ms;
        let mut events = Vec::new();
        if !self.started {
            return events;
        }

        self.service_ringing();
        self.service_outgoing();
        self.pump_command_queue();

        let (seq, phase) = (self.next_seq, self.phase);
        for _ in 0..SCENE_STEPS_PER_TICK {
            self.step_scene(&mut events);
            if self.next_seq != seq || self.phase != phase {
                break;
            }
        }

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

    /// One step of the simulated stack: advance the scene, then move
    /// whatever came out of each RFCOMM port up into the AT layer.
    fn step_scene(&mut self, events: &mut Vec<CarKitEvent>) {
        self.scene_time += SCENE_STEP_SECONDS;
        self.scene.tick(self.scene_time);
        self.follow_link(events);

        // Bytes that arrived at the phone were written by the head unit, and
        // vice versa. This is the whole of the attachment between HFP and
        // RFCOMM: two byte queues, one at each end of a real data link.
        for data in drain(&self.ag_port) {
            let (out, hfp_events) = self.ag.receive(&data);
            for line in out {
                self.ag_send(line);
            }
            for event in hfp_events {
                self.on_ag_hfp_event(event, events);
            }
        }
        for data in drain(&self.hf_port) {
            let (out, hfp_events) = self.hf.receive(&data);
            for line in out {
                self.hf_send(line);
            }
            for event in hfp_events {
                self.on_hf_hfp_event(event, events);
            }
        }

        self.follow_audio(events);
    }

    // -- the audio connection ------------------------------------------------

    /// The SCO/eSCO link's half of a step: notice when the controller has
    /// brought it up or taken it away, and carry a frame each way while it
    /// is up.
    ///
    /// Both ends are consulted, not one. A handle at the phone alone would
    /// mean the phone believes there is audio and the head unit does not —
    /// which is the failure this checks for rather than the one it hides.
    fn follow_audio(&mut self, events: &mut Vec<CarKitEvent>) {
        let phone_sco = self
            .scene
            .classic_device(self.phone)
            .and_then(ClassicDevice::sco);
        let car_sco = self
            .scene
            .classic_device(self.head_unit)
            .and_then(ClassicDevice::sco);
        let up = phone_sco.is_some() && car_sco.is_some();

        if up && !self.audio_up {
            self.audio_up = true;
            for event in self.ag.on_audio_connected() {
                let _ = event;
            }
            for event in self.hf.on_audio_connected() {
                let _ = event;
            }
            if let Some(sco) = phone_sco {
                self.audio_detail = Some(format!(
                    "{} link on handle {:#06X}, air mode {}, {} — {} bytes per frame. \
                     The payload crosses untouched: nothing here encodes or decodes it.",
                    if sco.link_type == 0x02 { "eSCO" } else { "SCO" },
                    sco.handle,
                    match sco.air_mode {
                        0x02 => "CVSD",
                        0x03 => "transparent",
                        other => {
                            let _ = other;
                            "log-PCM"
                        }
                    },
                    self.ag.negotiated_codec().name(),
                    AUDIO_FRAME_BYTES,
                ));
            }
            events.push(CarKitEvent::AudioConnected(self.ag.negotiated_codec()));
        } else if !up && self.audio_up {
            self.audio_up = false;
            let _ = self.ag.on_audio_disconnected();
            let _ = self.hf.on_audio_disconnected();
            self.audio_detail = None;
            events.push(CarKitEvent::AudioDisconnected);
        }

        if !self.audio_up {
            return;
        }
        // A frame each way per step. There is no sample rate here and no
        // packet interval: a tick is not a unit of time in this simulator,
        // and the reserved slots a real SCO link runs on are exactly what
        // rootcanal/netsim exist to model.
        let frame = self.next_audio_frame();
        if let Some(device) = self.scene.classic_device_mut(self.phone) {
            device.send_sco(frame.clone());
            self.audio_frames_from_phone += 1;
        }
        if let Some(device) = self.scene.classic_device_mut(self.head_unit) {
            device.send_sco(frame);
            self.audio_frames_from_car += 1;
        }
        let received_at_car = self
            .scene
            .classic_device_mut(self.head_unit)
            .map(ClassicDevice::take_sco_received)
            .unwrap_or_default();
        self.audio_frames_to_car += received_at_car.len() as u64;
        let received_at_phone = self
            .scene
            .classic_device_mut(self.phone)
            .map(ClassicDevice::take_sco_received)
            .unwrap_or_default();
        self.audio_frames_to_phone += received_at_phone.len() as u64;
    }

    /// One synthetic audio frame: a sequence number and a counter pattern.
    /// Deliberately not speech — see [`AUDIO_FRAME_BYTES`].
    fn next_audio_frame(&mut self) -> Vec<u8> {
        let seq = self.audio_frame_seq;
        self.audio_frame_seq += 1;
        let mut frame = Vec::with_capacity(AUDIO_FRAME_BYTES);
        frame.extend_from_slice(&seq.to_le_bytes());
        while frame.len() < AUDIO_FRAME_BYTES {
            frame.push(frame.len() as u8);
        }
        frame
    }

    /// Tell both ends what the settled codec asks the controller for, then
    /// ask the phone — the Audio Gateway — to open the link.
    ///
    /// Both ends are given the parameters: the acceptor states its own
    /// bandwidth, Voice Setting and packet types in Accept Synchronous
    /// Connection Request rather than inheriting the initiator's, so a head
    /// unit left on the CVSD defaults would be answering a wideband request
    /// with narrowband terms.
    fn open_audio_connection(&mut self, codec: AudioCodec) {
        let (voice_setting, packet_type) = (codec.voice_setting(), codec.esco_packet_type());
        for index in [self.phone, self.head_unit] {
            if let Some(device) = self.scene.classic_device_mut(index) {
                device.set_sco_parameters(voice_setting, packet_type);
            }
        }
        if let Some(device) = self.scene.classic_device_mut(self.phone) {
            device.request_sco();
        }
    }

    /// The AG decides there is audio: run the Codec Connection procedure if
    /// the two ends negotiated one, and open the link when it settles.
    ///
    /// Idempotent — [`AgProtocol::start_audio_connection`] returns nothing
    /// when audio is already up or already coming up, so answering a call
    /// whose in-band ring tone is already playing does not start a second
    /// procedure.
    fn start_ag_audio(&mut self) {
        let (outgoing, hfp_events) = self.ag.start_audio_connection();
        for line in outgoing {
            self.ag_send(line);
        }
        let mut ignored = Vec::new();
        for event in hfp_events {
            self.on_ag_hfp_event(event, &mut ignored);
        }
    }

    /// Hang up the audio, leaving the Service Level Connection alone. The
    /// call ends; the phone stays paired and the AT link stays open, which
    /// is what makes the next call cost one SCO setup instead of a whole
    /// bring-up.
    fn close_audio_connection(&mut self) {
        if let Some(device) = self.scene.classic_device_mut(self.phone) {
            device.release_sco();
        }
    }

    /// The BR/EDR half of [`LinkPhase`]: mirror the head unit's
    /// [`ClassicPhase`] until the data link opens, then hand over to HFP.
    fn follow_link(&mut self, events: &mut Vec<CarKitEvent>) {
        let classic = self.classic_phase();
        if classic == ClassicPhase::Failed && self.phase != LinkPhase::Failed {
            let reason = self
                .scene
                .classic_device(self.head_unit)
                .and_then(ClassicDevice::error)
                .unwrap_or("the BR/EDR link could not be established")
                .to_string();
            return self.fail(&reason, events);
        }
        if self.phase.is_profile_phase() {
            return;
        }

        // The DLC opening is the handover: everything below it was the link,
        // everything above it is the profile.
        if self.dlc_is_open() {
            self.record_details();
            self.phase = LinkPhase::EstablishingSlc;
            let bytes = self.hf.start_slc();
            self.hf_send(bytes);
            return;
        }
        self.phase = match classic {
            ClassicPhase::Starting | ClassicPhase::Inquiring | ClassicPhase::ResolvingNames => {
                LinkPhase::Inquiring
            }
            ClassicPhase::Paging => LinkPhase::Paging,
            ClassicPhase::QueryingSdp => LinkPhase::Discovering,
            ClassicPhase::OpeningRfcomm | ClassicPhase::Exchanging | ClassicPhase::Done => {
                LinkPhase::OpeningDlc
            }
            ClassicPhase::Failed => LinkPhase::Failed,
            ClassicPhase::Accepting => self.phase,
        };
    }

    /// How far the head unit's BR/EDR link has got.
    fn classic_phase(&self) -> ClassicPhase {
        self.scene
            .classic_device(self.head_unit)
            .map(ClassicDevice::phase)
            .unwrap_or(ClassicPhase::Failed)
    }

    /// Whether the RFCOMM data link carrying AT is open at the head unit.
    fn dlc_is_open(&self) -> bool {
        self.hf_port
            .lock()
            .ok()
            .is_some_and(|port| port.window().is_some())
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
        // In-band ring tone (HFP v1.9 4.13.1): the ring the driver hears is
        // the phone's, carried over the audio connection, so the AG brings
        // the synchronous link up *before* the first `RING`. Without in-band
        // ringing the head unit makes its own noise and audio waits for the
        // answer.
        if self.ag.inband_ringtone_enabled {
            self.start_ag_audio();
        }
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

    /// Where the audio connection has got to, as the Audio Gateway sees it.
    pub fn audio_state(&self) -> AudioConnectionState {
        self.ag.audio_state()
    }

    /// The audio connection itself, once the controller has made one.
    pub fn audio_connection(&self) -> Option<crate::device::ScoConnection> {
        self.scene
            .classic_device(self.phone)
            .and_then(ClassicDevice::sco)
    }

    /// Audio frames received at the head unit and at the phone. These count
    /// what came *off* the link, not what was written to it — which is the
    /// half that proves the routing.
    pub fn audio_frames_received(&self) -> (u64, u64) {
        (self.audio_frames_to_car, self.audio_frames_to_phone)
    }

    /// Every AT line so far, oldest first, capped at the most recent
    /// `TRANSCRIPT_LIMIT`.
    pub fn transcript(&self) -> impl Iterator<Item = &AtLine> {
        self.transcript.iter()
    }

    // -- the link ------------------------------------------------------------

    /// Snapshots what SDP answered and what the DLC negotiated, once both
    /// have happened. These are the two facts about the link a dashboard can
    /// show and a spec-reader will check.
    fn record_details(&mut self) {
        if let Some(results) = self
            .scene
            .classic_device(self.head_unit)
            .and_then(ClassicDevice::sdp_results)
            .and_then(|r| r.lock().ok())
            && results.answered
        {
            let channel = results
                .channel_for(HANDSFREE_AUDIO_GATEWAY)
                .map(|c| c.to_string())
                .unwrap_or_else(|| "none".into());
            // A BluetoothProfileDescriptorList version is `major << 8 | minor`
            // with both in *decimal*, so HFP 1.9 is 0x0109 — not a nibble
            // pair, which is what turns v1.9 into "v1.0" if you assume one.
            let version = results
                .profile_version
                .map(|v| format!("HFP v{}.{}", v >> 8, v & 0xFF))
                .unwrap_or_else(|| "no profile descriptor".into());
            self.sdp_detail = Some(format!(
                "ServiceSearchAttributeRequest for Handsfree Audio Gateway (0x111F) on \
                 L2CAP PSM 1 — {} bytes out, {} in. Answer: RFCOMM server channel {channel}, \
                 {version}.",
                results.request_bytes, results.response_bytes
            ));
        }
        if let Some(window) = self.hf_port.lock().ok().and_then(|p| p.window()) {
            self.dlc_detail = Some(format!(
                "DLCI {} — frame size {} out / {} in, {} credits granted at open",
                window.dlci,
                window.tx_max_frame_size,
                window.rx_max_frame_size,
                window.rx_initial_credits
            ));
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
            // The AG has settled the codec and wants the synchronous link.
            // This is the whole seam between HFP and HCI: everything above
            // it is AT commands, everything below it is a SCO handle.
            HfpEvent::AudioConnectionRequested(codec) => self.open_audio_connection(codec),
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
        // A connected call always has audio, whether or not the ring did.
        self.start_ag_audio();
    }

    fn clear_call(&mut self) {
        let was = self.call;
        self.ag.calls.clear();
        self.caller = None;
        // The audio goes and the Service Level Connection stays. A head unit
        // that tore down the ACL here would pay for a full inquiry, page,
        // SDP search and RFCOMM handshake on the next call.
        self.close_audio_connection();
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

    /// Sends one AT command line from the head unit into its RFCOMM port.
    ///
    /// This is the attachment, seen from above: HFP hands a line of bytes to
    /// the serial port and is done with it. What happens next — a UIH frame,
    /// a credit, an L2CAP SDU on PSM 3, an ACL packet, the controller — is
    /// none of the profile's business, which is exactly the property that
    /// makes an AT profile portable across transports in the first place.
    fn hf_send(&mut self, line: Vec<u8>) {
        self.log(true, &line);
        if let Ok(mut port) = self.hf_port.lock() {
            port.write(line);
        }
    }

    /// Sends one AT response line from the phone.
    fn ag_send(&mut self, line: Vec<u8>) {
        self.log(false, &line);
        if let Ok(mut port) = self.ag_port.lock() {
            port.write(line);
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
}

/// Takes everything that arrived at one end of the data link.
fn drain(port: &SharedRfcommPort) -> Vec<Vec<u8>> {
    port.lock()
        .map(|mut port| port.take_received())
        .unwrap_or_default()
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

/// One device the head unit's inquiry turned up.
#[derive(Serialize)]
struct FoundJson {
    address: String,
    class_of_device: String,
    name: Option<String>,
}

#[derive(Serialize)]
struct CarKitStatusJson {
    link: &'static str,
    /// The head unit's BR/EDR phase, which is where `link` comes from until
    /// the data link opens. Shown separately so a page can say *inquiry*
    /// rather than only "connecting".
    classic: &'static str,
    phone_address: String,
    head_unit_address: String,
    /// What the head unit's inquiry found — the acid test that this is a
    /// link and not a wire: a phone that stops inquiry-scanning vanishes.
    discovered: Vec<FoundJson>,
    acl_handle: Option<u16>,
    /// Whether the *phone* also has the ACL connection. Both ends agreeing
    /// is what separates a link from a page that drew one.
    phone_linked: bool,
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
    /// Where the audio connection has got to: disconnected, negotiating,
    /// connecting, connected.
    audio: &'static str,
    /// The SCO/eSCO handle, once the controller has made one. Distinct from
    /// `acl_handle`, which is the whole point.
    sco_handle: Option<u16>,
    /// "SCO" or "eSCO".
    sco_link_type: Option<&'static str>,
    /// The agreed air mode, named.
    sco_air_mode: Option<&'static str>,
    /// Audio frames taken *off* the link at the head unit and at the phone.
    audio_frames_to_car: u64,
    audio_frames_to_phone: u64,
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
        let window = self.hf_port.lock().ok().and_then(|p| p.window());
        let head_unit = self.scene.classic_device(self.head_unit);
        let status = CarKitStatusJson {
            link: self.phase.name(),
            classic: self.classic_phase().name(),
            phone_address: PHONE_ADDRESS.to_string(),
            head_unit_address: HEAD_UNIT_ADDRESS.to_string(),
            discovered: head_unit
                .map(ClassicDevice::discovered)
                .unwrap_or_default()
                .iter()
                .map(|d| FoundJson {
                    address: d.address.to_string(),
                    class_of_device: format!(
                        "0x{:02X}{:02X}{:02X}",
                        d.class_of_device[2], d.class_of_device[1], d.class_of_device[0]
                    ),
                    name: d.name.clone(),
                })
                .collect(),
            acl_handle: head_unit
                .and_then(|d| d.host().connection())
                .map(|(handle, _)| handle),
            phone_linked: self
                .scene
                .classic_device(self.phone)
                .and_then(|d| d.host().connection())
                .is_some(),
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
            codec: self.hf.active_codec.name(),
            audio: self.ag.audio_state().name(),
            sco_handle: self.audio_connection().map(|sco| sco.handle),
            sco_link_type: self
                .audio_connection()
                .map(|sco| if sco.link_type == 0x02 { "eSCO" } else { "SCO" }),
            sco_air_mode: self.audio_connection().map(|sco| match sco.air_mode {
                0x02 => "CVSD",
                0x03 => "transparent",
                _ => "log-PCM",
            }),
            audio_frames_to_car: self.audio_frames_to_car,
            audio_frames_to_phone: self.audio_frames_to_phone,
            ag_features: self.hf.supported_ag_features,
            hf_features: self.ag.supported_hf_features,
            clip_enabled: self.ag.cli_notification_enabled,
            ciev_enabled: self.ag.indicator_report_enabled,
            credits_out: window.map(|w| w.tx_credits).unwrap_or(0),
            credits_in: window.map(|w| w.rx_credits).unwrap_or(0),
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
            LinkPhase::Inquiring => 1,
            LinkPhase::Paging => 2,
            LinkPhase::Discovering => 3,
            LinkPhase::OpeningDlc => 4,
            LinkPhase::EstablishingSlc => 5,
            LinkPhase::ConfiguringHeadUnit => 6,
            LinkPhase::Ready | LinkPhase::Failed => 7,
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
        let head_unit = self.scene.classic_device(self.head_unit);
        let found =
            head_unit.and_then(|d| d.discovered().iter().find(|f| f.address == PHONE_ADDRESS));
        vec![
            step(
                "inquiry",
                "Inquiry — find the phone on the air, then ask it its name",
                1,
                match found {
                    Some(device) => format!(
                        "{} answered the GIAC with Class of Device 0x{:02X}{:02X}{:02X}; \
                         a Remote Name Request turned that into {:?} — an inquiry result \
                         carries no name at all.",
                        device.address,
                        device.class_of_device[2],
                        device.class_of_device[1],
                        device.class_of_device[0],
                        device.name.as_deref().unwrap_or("(not yet resolved)")
                    ),
                    None => String::new(),
                },
            ),
            step(
                "page",
                "Paging — an ACL connection to the phone",
                2,
                match head_unit.and_then(|d| d.host().connection()) {
                    Some((handle, peer)) => format!(
                        "connection handle {handle:#06x} to {peer}; every layer above \
                         this one is a payload inside its ACL packets."
                    ),
                    None => String::new(),
                },
            ),
            step(
                "sdp",
                "SDP over L2CAP PSM 1 — find the phone's Audio Gateway record",
                3,
                self.sdp_detail.clone().unwrap_or_default(),
            ),
            step(
                "dlc",
                "RFCOMM over L2CAP PSM 3 — multiplexer, then PN / SABM / UA and the credit grant",
                4,
                self.dlc_detail.clone().unwrap_or_default(),
            ),
            step(
                "slc",
                "Service Level Connection — BRSF, BAC, CIND, CMER, CHLD, BIND",
                5,
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
                6,
                if self.command_queue.is_empty() && reached >= 6 {
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
                7,
                match self.call {
                    CallPhase::Idle => "idle".into(),
                    other => other.name().to_string(),
                },
            ),
            // The audio connection is not a *stage* of the bring-up: it
            // comes and goes while everything above stays put, so it is
            // shown as itself rather than folded into the ladder.
            StepJson {
                id: "sco",
                label: "SCO / eSCO — the call audio",
                state: match (self.audio_up, self.ag.audio_state()) {
                    (true, _) => "done",
                    (false, AudioConnectionState::Disconnected) => "pending",
                    (false, _) => "active",
                },
                detail: self
                    .audio_detail
                    .clone()
                    .unwrap_or_else(|| match self.ag.audio_state() {
                        AudioConnectionState::Negotiating => {
                            "Codec Connection procedure running: +BCS out, waiting for AT+BCS."
                                .into()
                        }
                        AudioConnectionState::Connecting => {
                            "codec settled; Setup Synchronous Connection in flight.".into()
                        }
                        _ => String::new(),
                    }),
            },
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

    /// The link the AT bytes ride is a real BR/EDR one, and this is where
    /// that stops being a claim: the head unit was told an *address*, and
    /// everything it knows beyond that — the phone's Class of Device, its
    /// name, its RFCOMM channel — it learned over the air.
    #[test]
    fn test_the_head_unit_finds_the_phone_by_inquiry_before_it_can_call_it() {
        let kit = connected();
        let head_unit = kit.scene.classic_device(kit.head_unit).expect("head unit");

        let found = head_unit
            .discovered()
            .iter()
            .find(|d| d.address == PHONE_ADDRESS)
            .expect("the inquiry found the phone");
        assert_eq!(
            found.class_of_device, PHONE_CLASS_OF_DEVICE,
            "the Class of Device is the phone's, read off an Inquiry Result"
        );
        assert_eq!(
            found.name.as_deref(),
            Some("Simble Phone"),
            "and its name, which only a Remote Name Request can supply — an \
             inquiry result carries none"
        );
    }

    /// Both ends agree there is an ACL connection, which is the difference
    /// between a link and a drawing of one.
    #[test]
    fn test_the_at_bytes_ride_an_acl_connection_both_ends_can_see() {
        let kit = connected();
        let (car_handle, car_peer) = kit
            .scene
            .classic_device(kit.head_unit)
            .and_then(|d| d.host().connection())
            .expect("the head unit has an ACL connection");
        let (phone_handle, phone_peer) = kit
            .scene
            .classic_device(kit.phone)
            .and_then(|d| d.host().connection())
            .expect("the phone has one too");
        assert_eq!(car_peer, PHONE_ADDRESS);
        assert_eq!(phone_peer, HEAD_UNIT_ADDRESS);
        // Handles are allocated per controller, so they need not match — but
        // neither may be the "no connection" sentinel.
        assert_ne!(car_handle, 0);
        assert_ne!(phone_handle, 0);
    }

    /// The phases are walked in the order the sequence actually happens in,
    /// and none is skipped. A link that reached `Ready` without ever being
    /// in `Paging` would be a worse bug than one that never got there.
    #[test]
    fn test_the_link_walks_the_bredr_sequence_in_order() {
        let mut kit = CarKit::new();
        kit.start();
        let events = drive(&mut kit, |k| k.phase() == LinkPhase::Ready);
        let phases: Vec<LinkPhase> = events
            .into_iter()
            .filter_map(|e| match e {
                CarKitEvent::LinkPhase(p) => Some(p),
                _ => None,
            })
            .collect();
        let expected = [
            LinkPhase::Inquiring,
            LinkPhase::Paging,
            LinkPhase::Discovering,
            LinkPhase::OpeningDlc,
            LinkPhase::EstablishingSlc,
            LinkPhase::ConfiguringHeadUnit,
            LinkPhase::Ready,
        ];
        let mut cursor = 0;
        for phase in expected {
            let found = phases[cursor..]
                .iter()
                .position(|p| *p == phase)
                .unwrap_or_else(|| panic!("{phase:?} missing after {cursor}: {phases:?}"));
            cursor += found + 1;
        }
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
        // The profile version came out of the same record. A
        // BluetoothProfileDescriptorList encodes it as `major << 8 | minor`
        // in decimal, so reading the minor as a nibble reports 1.9 as "v1.0"
        // — a wrong number that still looks like a version, which is the
        // only reason this is asserted rather than eyeballed.
        assert!(
            detail.contains("HFP v1.9"),
            "the record advertises HFP 1.9: {detail}"
        );
        assert!(
            detail.contains("bytes out"),
            "and the search was a real transaction with a size: {detail}"
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
        let window = kit
            .hf_port
            .lock()
            .unwrap()
            .window()
            .expect("the data link is open");
        assert!(
            window.tx_credits > 0,
            "the head unit may only write while it holds credits"
        );
        assert!(window.rx_initial_credits > 0, "the phone granted credits");
        assert_eq!(
            window.dlci,
            AG_RFCOMM_CHANNEL << 1,
            "the DLCI is the server channel SDP advertised, doubled"
        );
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

    // --- the audio connection ----------------------------------------------

    /// A Service Level Connection with no call has no audio. This is the
    /// thing HFP separates the two connections *for*: a paired phone does
    /// not hold a headset's microphone open all day.
    #[test]
    fn test_a_ready_link_carries_no_audio_until_there_is_a_call() {
        let kit = connected();
        assert_eq!(kit.audio_state(), AudioConnectionState::Disconnected);
        assert!(kit.audio_connection().is_none());
        assert_eq!(kit.audio_frames_received(), (0, 0));
    }

    /// The whole path, end to end: a call arrives, the codec is negotiated
    /// over AT, the phone opens a synchronous link over HCI, and audio
    /// crosses it in both directions on a handle that is not the ACL's.
    #[test]
    fn test_a_call_brings_up_a_real_sco_link_and_carries_audio_both_ways() {
        let mut kit = connected();
        assert!(kit.incoming_call("+15550142"));
        drive(&mut kit, |k| k.audio_connection().is_some());

        let sco = kit.audio_connection().expect("the audio link exists");
        let (acl_handle, _) = kit
            .scene
            .classic_device(kit.phone)
            .and_then(|d| d.host().connection())
            .expect("the ACL is still there");
        assert_ne!(
            sco.handle, acl_handle,
            "call audio has a handle of its own; addressing it to the ACL \
             handle is delivered to nobody"
        );
        assert_eq!(
            kit.audio_state(),
            AudioConnectionState::Connected,
            "and the profile has been told, not just the transport"
        );

        // Both ends agree, which is what separates a link from one end's
        // belief in one.
        let car_sco = kit
            .scene
            .classic_device(kit.head_unit)
            .and_then(ClassicDevice::sco)
            .expect("the head unit has the audio link too");
        assert_eq!(car_sco.handle, sco.handle);

        // Frames cross in both directions. `audio_frames_received` counts
        // what came *off* the link, so a count that moves is proof of
        // routing rather than of writing.
        drive(&mut kit, |k| {
            let (to_car, to_phone) = k.audio_frames_received();
            to_car > 2 && to_phone > 2
        });
    }

    /// mSBC needs an eSCO link and transparent air coding, and the codec
    /// choice has to reach the *controller* — as a Voice Setting and a
    /// packet-type mask — or the call comes up narrowband with everyone
    /// still calling it wideband.
    #[test]
    fn test_the_negotiated_codec_decides_the_link_type_the_controller_makes() {
        let mut kit = connected();
        assert!(kit.incoming_call("+15550142"));
        drive(&mut kit, |k| k.audio_connection().is_some());

        let sco = kit.audio_connection().expect("the audio link exists");
        let codec = kit.ag.negotiated_codec();
        assert_eq!(
            codec,
            AudioCodec::Msbc,
            "both ends offer mSBC, so the AG must pick it over CVSD"
        );
        assert_eq!(sco.link_type, 0x02, "wideband speech rides eSCO, not SCO");
        assert_eq!(
            sco.air_mode, 0x03,
            "and transparent air coding, because the controller must not \
             touch an mSBC frame"
        );
    }

    /// Hanging up takes the audio and nothing else. A head unit that let the
    /// ACL go here would pay for a whole inquiry-page-SDP-RFCOMM bring-up on
    /// the next call.
    #[test]
    fn test_ending_a_call_drops_the_audio_and_keeps_the_service_level_connection() {
        let mut kit = connected();
        assert!(kit.incoming_call("+15550142"));
        drive(&mut kit, |k| k.audio_connection().is_some());
        assert!(kit.hang_up());
        drive(&mut kit, |k| k.audio_connection().is_none());

        assert_eq!(kit.audio_state(), AudioConnectionState::Disconnected);
        assert_eq!(kit.phase(), LinkPhase::Ready, "the SLC is still up");
        assert!(
            kit.scene
                .classic_device(kit.head_unit)
                .and_then(|d| d.host().connection())
                .is_some(),
            "and so is the ACL under it"
        );
        assert!(
            kit.hf_port.lock().ok().and_then(|p| p.window()).is_some(),
            "and the RFCOMM data link, which is what makes the next call cheap"
        );

        // And the link really comes back for a second call, on a fresh
        // handle — proof that nothing was left half-open.
        assert!(kit.incoming_call("+15550143"));
        drive(&mut kit, |k| k.audio_connection().is_some());
    }

    /// The negative case: a head unit that refuses audio leaves nothing
    /// half-open at either end, and the call's signalling carries on.
    #[test]
    fn test_a_refused_audio_connection_leaves_no_handle_anywhere() {
        let mut kit = connected();
        if let Some(device) = kit.scene.classic_device_mut(kit.head_unit) {
            // 0x0D — Connection Rejected due to Limited Resources.
            device.set_sco_policy(crate::device::ScoPolicy::Reject(0x0D));
        }
        assert!(kit.incoming_call("+15550142"));

        // Give it as long as a successful setup would have taken, twice over.
        for step in 0..60 {
            kit.tick(step * 100);
        }
        // The setup really was attempted and really was refused — without
        // this the rest of the test passes just as well on a build where no
        // audio is ever opened at all.
        assert_eq!(
            kit.scene
                .classic_device(kit.phone)
                .and_then(ClassicDevice::sco_failure),
            Some(0x0D),
            "the phone must be told *why*, in a Synchronous Connection \
             Complete carrying the head unit's reason"
        );
        assert!(
            kit.audio_connection().is_none(),
            "the phone must not hold a handle the head unit refused"
        );
        assert!(
            kit.scene
                .classic_device(kit.head_unit)
                .and_then(ClassicDevice::sco)
                .is_none(),
            "and the head unit must not hold one it rejected"
        );
        assert_eq!(kit.audio_frames_received(), (0, 0), "no audio moved");
        assert_eq!(
            kit.call_phase(),
            CallPhase::Incoming,
            "the call itself is unaffected: AT signalling does not need SCO"
        );
        assert_eq!(kit.phase(), LinkPhase::Ready);
    }

    /// The Car page draws its SCO box solid off `sco_handle` being present,
    /// so the JSON contract is part of the feature rather than a detail of
    /// it: a renamed field leaves the page showing a dashed box beside a
    /// working link and nothing anywhere says why.
    #[test]
    fn test_the_pages_status_carries_the_audio_connection() {
        let mut kit = connected();
        let idle = kit.status_json(0);
        assert!(idle.contains("\"audio\":\"disconnected\""), "{idle}");
        assert!(idle.contains("\"sco_handle\":null"), "{idle}");

        assert!(kit.incoming_call("+15550142"));
        drive(&mut kit, |k| k.audio_connection().is_some());
        drive(&mut kit, |k| k.audio_frames_received().0 > 0);

        let json = kit.status_json(0);
        assert!(json.contains("\"audio\":\"connected\""), "{json}");
        assert!(json.contains("\"sco_link_type\":\"eSCO\""), "{json}");
        assert!(json.contains("\"sco_air_mode\":\"transparent\""), "{json}");
        assert!(json.contains("\"codec\":\"mSBC\""), "{json}");
        assert!(!json.contains("\"sco_handle\":null"), "{json}");
        assert!(!json.contains("\"audio_frames_to_car\":0"), "{json}");
    }
}
