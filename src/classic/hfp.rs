// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Hands-Free Profile (HFP): AT-command signaling and call-state machine
//! layered over an RFCOMM [`crate::classic::rfcomm::Dlc`]'s byte stream.
//!
//! This module is transport-free, like the packet/profile layers elsewhere
//! in Simble: [`HfProtocol`] (Hands-Free role) and [`AgProtocol`] (Audio
//! Gateway role) each expose a synchronous `receive(&mut self, data: &[u8])
//! -> (Vec<Vec<u8>>, Vec<HfpEvent>)` that buffers partial `\r`/`\r\n`-framed
//! AT lines and returns bytes to write back plus events for the caller to
//! observe — the caller is responsible for moving those bytes to/from an
//! open [`crate::classic::rfcomm::Dlc`] (via `Multiplexer::write` and
//! `MultiplexerEvent::DataReceived`), the same way an ASCS operation is
//! driven by a GATT characteristic write rather than owning the database.
//!
//! Covers the Service Level Connection procedure (`AT+BRSF`/`AT+CIND`/
//! `AT+CMER`, optional codec negotiation and HF-indicator exchange), AG
//! indicator updates, call control (answer/reject/hangup/dial/hold/list),
//! volume and voice-recognition signaling, and SDP record
//! construction/lookup.
//!
//! ## The audio connection
//!
//! HFP has two connections, and they are independent: the Service Level
//! Connection above, and the **audio connection** — a SCO or eSCO link that
//! exists only while there is something to hear. [`AudioConnectionState`]
//! tracks the second, and [`AgProtocol::start_audio_connection`] runs the
//! Codec Connection procedure (`+BCS`/`AT+BCS`) that precedes it. Opening
//! the synchronous link is the **AG's** job under HFP v1.9 4.11, whether the
//! trigger was a call or the HF's `AT+BCC`; the HF never opens one.
//!
//! This module still owns no transport. It ends at
//! [`HfpEvent::AudioConnectionRequested`], which says "the codec is settled,
//! open the link" — and something that owns an HCI channel
//! ([`crate::device::ClassicHost`], driven by [`crate::device::CarKit`])
//! answers it with Setup Synchronous Connection and calls
//! [`AgProtocol::on_audio_connected`] when the controller says it is up.
//!
//! What is **not** here: any codec. [`AudioCodec::voice_setting`] and
//! [`AudioCodec::esco_packet_type`] say what a host must ask the controller
//! for, and the payload bytes cross the link untouched. Nothing encodes
//! CVSD, and nothing feeds `crate::audio`'s mSBC or LC3 encoders into the
//! link yet — the two ends agree on a codec and carry each other's bytes.
//! eSCO parameter tables (S1-S4) are still out of scope: the link type and
//! packet-type mask are modelled, the air-interface timings behind them are
//! not (see `tests/hfp_test.rs` for what was ported vs. skipped from
//! Bumble's `hfp_test.py`).

use crate::classic::at::{AtCommand, AtParameter, AtParsingError, AtResponse, AtSubCode};
use crate::classic::sdp::{
    AttributeIdSpec, DataElement, SdpClient, SdpUuid, ServiceAttribute, attribute_id,
};
use crate::types::SimbleError;
use std::collections::{BTreeMap, BTreeSet, HashSet};

// ---------------------------------------------------------------------------
// Normative feature bitmasks (AT+BRSF)
// ---------------------------------------------------------------------------

/// HF supported-features bitmask, exchanged at Service Level Connection via
/// `AT+BRSF` (HFP v1.9 4.34.1).
pub mod hf_feature {
    /// EC/NR function.
    pub(crate) const EC_NR: u32 = 0x001;
    /// Three-way calling.
    pub const THREE_WAY_CALLING: u32 = 0x002;
    /// CLI presentation capability.
    pub const CLI_PRESENTATION_CAPABILITY: u32 = 0x004;
    /// Voice recognition activation.
    pub(crate) const VOICE_RECOGNITION_ACTIVATION: u32 = 0x008;
    /// Remote volume control.
    pub(crate) const REMOTE_VOLUME_CONTROL: u32 = 0x010;
    /// Enhanced call status.
    pub const ENHANCED_CALL_STATUS: u32 = 0x020;
    /// Enhanced call control.
    pub const ENHANCED_CALL_CONTROL: u32 = 0x040;
    /// Codec negotiation.
    pub const CODEC_NEGOTIATION: u32 = 0x080;
    /// HF indicators.
    pub const HF_INDICATORS: u32 = 0x100;
    /// eSCO S4 settings supported.
    pub const ESCO_S4_SETTINGS_SUPPORTED: u32 = 0x200;
    /// Enhanced voice recognition status.
    pub(crate) const ENHANCED_VOICE_RECOGNITION_STATUS: u32 = 0x400;
    /// Voice recognition text.
    pub(crate) const VOICE_RECOGNITION_TEXT: u32 = 0x800;
}

/// AG supported-features bitmask, reported at Service Level Connection via
/// `+BRSF` (HFP v1.9 4.34.2).
pub mod ag_feature {
    /// Three-way calling.
    pub const THREE_WAY_CALLING: u32 = 0x001;
    /// EC/NR function.
    pub(crate) const EC_NR: u32 = 0x002;
    /// Voice recognition function.
    pub(crate) const VOICE_RECOGNITION_FUNCTION: u32 = 0x004;
    /// In-band ring tone capability.
    pub const IN_BAND_RING_TONE_CAPABILITY: u32 = 0x008;
    /// Attach a number to a voice tag.
    pub const VOICE_TAG: u32 = 0x010;
    /// Ability to reject a call.
    pub const REJECT_CALL: u32 = 0x020;
    /// Enhanced call status.
    pub const ENHANCED_CALL_STATUS: u32 = 0x040;
    /// Enhanced call control.
    pub const ENHANCED_CALL_CONTROL: u32 = 0x080;
    /// Extended error result codes.
    pub const EXTENDED_ERROR_RESULT_CODES: u32 = 0x100;
    /// Codec negotiation.
    pub const CODEC_NEGOTIATION: u32 = 0x200;
    /// HF indicators.
    pub const HF_INDICATORS: u32 = 0x400;
    /// eSCO S4 settings supported.
    pub const ESCO_S4_SETTINGS_SUPPORTED: u32 = 0x800;
    /// Enhanced voice recognition status.
    pub(crate) const ENHANCED_VOICE_RECOGNITION_STATUS: u32 = 0x1000;
    /// Voice recognition text.
    pub(crate) const VOICE_RECOGNITION_TEXT: u32 = 0x2000;
}

const STATUS_CODES: [&str; 8] = [
    "+CME ERROR",
    "BLACKLISTED",
    "BUSY",
    "DELAYED",
    "ERROR",
    "NO ANSWER",
    "NO CARRIER",
    "OK",
];

// ---------------------------------------------------------------------------
// Normative value enums
// ---------------------------------------------------------------------------

/// Negotiable HFP audio codec (AT+BAC/AT+BCS).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// CVSD (narrow-band).
    Cvsd,
    /// mSBC (wide-band speech).
    Msbc,
    /// LC3-SWB (super-wide-band).
    Lc3Swb,
}

impl AudioCodec {
    /// Encodes to its codec ID.
    pub(crate) fn to_value(self) -> u8 {
        match self {
            Self::Cvsd => 0x01,
            Self::Msbc => 0x02,
            Self::Lc3Swb => 0x03,
        }
    }

    /// Decodes from its codec ID.
    pub(crate) fn from_value(value: u8) -> Option<Self> {
        match value {
            0x01 => Some(Self::Cvsd),
            0x02 => Some(Self::Msbc),
            0x03 => Some(Self::Lc3Swb),
            _ => None,
        }
    }

    /// Printable name, for a transcript or a status document.
    pub fn name(self) -> &'static str {
        match self {
            Self::Cvsd => "CVSD",
            Self::Msbc => "mSBC",
            Self::Lc3Swb => "LC3-SWB",
        }
    }

    /// The HCI **Voice Setting** (Vol 4, Part E, Section 6.12) a host asks
    /// the controller for when it opens the synchronous link for this codec.
    ///
    /// This is the codec seam, and it is a *seam*, not a codec. CVSD asks
    /// for CVSD air coding, so the controller does the waveform coding and
    /// the host hands it 8 kHz linear PCM. Everything wider than narrowband
    /// asks for **transparent**: mSBC and LC3-SWB frames are encoded by the
    /// host and the controller must not touch them.
    ///
    /// Simble encodes neither. It carries whatever payload bytes a caller
    /// puts on the link, byte for byte, and reports which codec the two ends
    /// agreed on. Nothing here transcodes; `crate::audio` has SBC and LC3
    /// encoders, and wiring one of them in is a separate job from making the
    /// link exist.
    pub fn voice_setting(self) -> u16 {
        match self {
            // Air coding CVSD (bits 1:0 = 00), 16-bit two's-complement
            // linear input — the value Zephyr calls BT_VOICE_CVSD_16BIT.
            Self::Cvsd => 0x0060,
            // Air coding transparent (bits 1:0 = 11).
            Self::Msbc | Self::Lc3Swb => 0x0063,
        }
    }

    /// Whether this codec needs an **extended** synchronous (eSCO) link.
    ///
    /// Narrowband CVSD fits a plain SCO HV3 link. Wideband speech does not:
    /// mSBC over an HV3 link has no retransmission and no error detection,
    /// so HFP requires eSCO for it. That choice reaches the controller as
    /// the packet-type mask below, and nowhere else.
    pub fn requires_esco(self) -> bool {
        !matches!(self, Self::Cvsd)
    }

    /// The HCI `Packet_Type` mask a Setup Synchronous Connection carries for
    /// this codec: HV1|HV2|HV3 for narrowband, EV3 for wideband.
    ///
    /// It is the mask, not a separate field, that tells a controller which
    /// kind of link to make — which is why an mSBC call set up with HV-only
    /// packet types comes back as a plain SCO link and sounds wrong.
    pub fn esco_packet_type(self) -> u16 {
        if self.requires_esco() {
            0x0008 // EV3
        } else {
            0x0007 // HV1 | HV2 | HV3
        }
    }
}

/// Where an HFP **audio connection** — the SCO/eSCO link that carries the
/// call itself — has got to.
///
/// HFP separates the two connections deliberately: the Service Level
/// Connection is AT commands over RFCOMM and exists whenever the phone is
/// paired, while the audio connection exists only while there is something
/// to hear. They come up and go down independently, and a profile that
/// conflated them would put a headset's microphone live for the whole drive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioConnectionState {
    /// No synchronous link, and none being set up.
    Disconnected,
    /// The Codec Connection procedure is running: `+BCS` has gone out and
    /// the far end's `AT+BCS` has not come back.
    Negotiating,
    /// The codec is settled and the synchronous link is being opened. This
    /// is the state the *transport* is expected to act on.
    Connecting,
    /// The synchronous link is up and carrying audio.
    Connected,
}

impl AudioConnectionState {
    /// Stable identifier for a UI or a status document.
    pub fn name(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Negotiating => "negotiating",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
        }
    }
}

/// HF indicator (AT+BIND / +BIND).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HfIndicator {
    /// Enhanced Safety indicator.
    EnhancedSafety,
    /// Battery Level indicator.
    BatteryLevel,
}

impl HfIndicator {
    /// Encodes to its assigned number.
    pub(crate) fn to_value(self) -> u8 {
        match self {
            Self::EnhancedSafety => 1,
            Self::BatteryLevel => 2,
        }
    }

    /// Decodes from its assigned number.
    pub(crate) fn from_value(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::EnhancedSafety),
            2 => Some(Self::BatteryLevel),
            _ => None,
        }
    }
}

/// Three-way / call-hold operation (AT+CHLD).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallHoldOperation {
    /// Release all held calls (CHLD=0).
    ReleaseAllHeldCalls,
    /// Release all active calls, accept the other (CHLD=1).
    ReleaseAllActiveCalls,
    /// Release a specific call (CHLD=1x).
    ReleaseSpecificCall,
    /// Hold all active calls, accept the other (CHLD=2).
    HoldAllActiveCalls,
    /// Hold all calls except a specific one (CHLD=2x).
    HoldAllCallsExcept,
    /// Add a held call to the conversation (CHLD=3).
    AddHeldCall,
    /// Connect the two calls and disconnect (CHLD=4).
    ConnectTwoCalls,
}

impl CallHoldOperation {
    /// Returns the AT+CHLD code string.
    pub fn code(self) -> &'static str {
        match self {
            Self::ReleaseAllHeldCalls => "0",
            Self::ReleaseAllActiveCalls => "1",
            Self::ReleaseSpecificCall => "1x",
            Self::HoldAllActiveCalls => "2",
            Self::HoldAllCallsExcept => "2x",
            Self::AddHeldCall => "3",
            Self::ConnectTwoCalls => "4",
        }
    }

    /// Parses an AT+CHLD code string.
    pub(crate) fn from_code(code: &str) -> Option<Self> {
        match code {
            "0" => Some(Self::ReleaseAllHeldCalls),
            "1" => Some(Self::ReleaseAllActiveCalls),
            "1x" => Some(Self::ReleaseSpecificCall),
            "2" => Some(Self::HoldAllActiveCalls),
            "2x" => Some(Self::HoldAllCallsExcept),
            "3" => Some(Self::AddHeldCall),
            "4" => Some(Self::ConnectTwoCalls),
            _ => None,
        }
    }
}

/// AG indicator reported in +CIND / +CIEV.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgIndicator {
    /// Network service availability.
    Service,
    /// Call active.
    Call,
    /// Call setup state.
    CallSetup,
    /// Call-held state.
    CallHeld,
    /// Signal strength.
    Signal,
    /// Roaming status.
    Roam,
    /// Battery charge level.
    BatteryCharge,
}

impl AgIndicator {
    /// Returns the indicator's +CIND name.
    pub(crate) fn wire_name(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Call => "call",
            Self::CallSetup => "callsetup",
            Self::CallHeld => "callheld",
            Self::Signal => "signal",
            Self::Roam => "roam",
            Self::BatteryCharge => "battchg",
        }
    }

    /// Parses an indicator from its +CIND name.
    pub(crate) fn from_wire_name(name: &str) -> Option<Self> {
        match name {
            "service" => Some(Self::Service),
            "call" => Some(Self::Call),
            "callsetup" => Some(Self::CallSetup),
            "callheld" => Some(Self::CallHeld),
            "signal" => Some(Self::Signal),
            "roam" => Some(Self::Roam),
            "battchg" => Some(Self::BatteryCharge),
            _ => None,
        }
    }
}

/// Call direction in a +CLCC entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallInfoDirection {
    /// Outgoing (mobile-originated).
    MobileOriginated,
    /// Incoming (mobile-terminated).
    MobileTerminated,
}

impl CallInfoDirection {
    /// Encodes to its +CLCC value.
    pub(crate) fn to_value(self) -> u8 {
        match self {
            Self::MobileOriginated => 0,
            Self::MobileTerminated => 1,
        }
    }

    /// Decodes from its +CLCC value.
    pub(crate) fn from_value(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::MobileOriginated),
            1 => Some(Self::MobileTerminated),
            _ => None,
        }
    }
}

/// Call status in a +CLCC entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallInfoStatus {
    /// Active call.
    Active,
    /// Held call.
    Held,
    /// Dialing (outgoing).
    Dialing,
    /// Alerting (outgoing, remote ringing).
    Alerting,
    /// Incoming call.
    Incoming,
    /// Waiting call.
    Waiting,
}

impl CallInfoStatus {
    /// Encodes to its +CLCC value.
    pub(crate) fn to_value(self) -> u8 {
        match self {
            Self::Active => 0,
            Self::Held => 1,
            Self::Dialing => 2,
            Self::Alerting => 3,
            Self::Incoming => 4,
            Self::Waiting => 5,
        }
    }

    /// Decodes from its +CLCC value.
    pub(crate) fn from_value(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Active),
            1 => Some(Self::Held),
            2 => Some(Self::Dialing),
            3 => Some(Self::Alerting),
            4 => Some(Self::Incoming),
            5 => Some(Self::Waiting),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Call bearer/teleservice mode in a +CLCC entry.
pub enum CallInfoMode {
    /// Voice.
    Voice,
    /// Data.
    Data,
    /// Fax.
    Fax,
    /// Unknown.
    Unknown,
}

impl CallInfoMode {
    /// Encodes to its +CLCC value.
    pub(crate) fn to_value(self) -> u8 {
        match self {
            Self::Voice => 0,
            Self::Data => 1,
            Self::Fax => 2,
            Self::Unknown => 9,
        }
    }

    /// Decodes from its +CLCC value.
    pub(crate) fn from_value(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Voice),
            1 => Some(Self::Data),
            2 => Some(Self::Fax),
            9 => Some(Self::Unknown),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Whether a +CLCC call is part of a multiparty (conference) call.
pub enum CallInfoMultiParty {
    /// Not in a conference.
    NotInConference,
    /// In a conference.
    InConference,
}

impl CallInfoMultiParty {
    /// Encodes to its +CLCC value.
    pub(crate) fn to_value(self) -> u8 {
        match self {
            Self::NotInConference => 0,
            Self::InConference => 1,
        }
    }

    /// Decodes from its +CLCC value.
    pub(crate) fn from_value(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::NotInConference),
            1 => Some(Self::InConference),
            _ => None,
        }
    }
}

/// One call entry as reported by AT+CLCC (+CLCC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallInfo {
    /// Call index (+CLCC).
    pub index: u32,
    /// Call direction.
    pub direction: CallInfoDirection,
    /// Call status.
    pub status: CallInfoStatus,
    /// Bearer/teleservice mode.
    pub mode: CallInfoMode,
    /// Multiparty (conference) membership.
    pub multi_party: CallInfoMultiParty,
    /// Remote party number, if present.
    pub number: Option<String>,
    /// Address type byte, if present.
    pub kind: Option<u8>,
}

/// Voice-recognition activation state (AT+BVRA / +BVRA).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VoiceRecognitionState {
    /// Deactivated.
    Disable,
    /// Activated.
    Enable,
    /// Enhanced VR ready to accept audio.
    EnhancedReady,
}

impl VoiceRecognitionState {
    /// Encodes to its +BVRA value.
    pub(crate) fn to_value(self) -> u8 {
        match self {
            Self::Disable => 0,
            Self::Enable => 1,
            Self::EnhancedReady => 2,
        }
    }

    /// Decodes from its +BVRA value.
    pub(crate) fn from_value(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Disable),
            1 => Some(Self::Enable),
            2 => Some(Self::EnhancedReady),
            _ => None,
        }
    }
}

/// Extended audio-gateway error reported in +CME ERROR.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmeError {
    /// Phone failure.
    PhoneFailure,
    /// Operation not allowed.
    OperationNotAllowed,
    /// Operation not supported.
    OperationNotSupported,
    /// Memory full.
    MemoryFull,
    /// Invalid index.
    InvalidIndex,
    /// Not found.
    NotFound,
}

impl CmeError {
    /// Returns the +CME ERROR code.
    pub(crate) fn to_value(self) -> u32 {
        match self {
            Self::PhoneFailure => 0,
            Self::OperationNotAllowed => 3,
            Self::OperationNotSupported => 4,
            Self::MemoryFull => 20,
            Self::InvalidIndex => 21,
            Self::NotFound => 22,
        }
    }
}

/// Caller line identification carried in a +CLIP notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallLineIdentification {
    /// Caller number.
    pub number: String,
    /// Address type byte.
    pub kind: u32,
    /// Subaddress, if present.
    pub subaddr: Option<String>,
    /// Subaddress type, if present.
    pub satype: Option<u32>,
    /// Alphanumeric name, if present.
    pub alpha: Option<String>,
    /// CLI validity code, if present.
    pub cli_validity: Option<u32>,
}

fn nonempty_param_str(param: Option<&AtParameter>) -> Option<&str> {
    match param {
        Some(AtParameter::Value(v)) if !v.is_empty() => std::str::from_utf8(v).ok(),
        _ => None,
    }
}

impl CallLineIdentification {
    /// Parses a +CLIP parameter list.
    pub(crate) fn parse_from(parameters: &[AtParameter]) -> Result<Self, AtParsingError> {
        let number = nonempty_param_str(parameters.first())
            .ok_or_else(|| AtParsingError("CLIP: missing number".into()))?
            .to_string();
        let kind = nonempty_param_str(parameters.get(1))
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| AtParsingError("CLIP: missing type".into()))?;
        let subaddr = if parameters.len() >= 3 {
            parameters
                .get(2)
                .and_then(AtParameter::as_str)
                .map(str::to_string)
        } else {
            None
        };
        let satype = if parameters.len() >= 4 {
            nonempty_param_str(parameters.get(3)).and_then(|s| s.parse().ok())
        } else {
            None
        };
        let alpha = if parameters.len() >= 5 {
            parameters
                .get(4)
                .and_then(AtParameter::as_str)
                .map(str::to_string)
        } else {
            None
        };
        let cli_validity = if parameters.len() >= 6 {
            nonempty_param_str(parameters.get(5)).and_then(|s| s.parse().ok())
        } else {
            None
        };
        Ok(Self {
            number,
            kind,
            subaddr,
            satype,
            alpha,
            cli_validity,
        })
    }

    /// Renders the +CLIP parameter string (3GPP TS 27.007 7.6).
    ///
    /// `<number>`, `<subaddr>` and `<alpha>` are string-typed parameters, so
    /// they go on the wire in double quotes. This is not cosmetic: a real
    /// HF reads them with a quoted-string parser (Zephyr's `clip_handle`
    /// calls `at_get_string`) and gets nothing from a bare value. Absent
    /// trailing optional fields are omitted rather than sent as empty
    /// commas, which is also what a real AG does.
    pub(crate) fn to_clip_string(&self) -> String {
        let quoted = |value: &str| format!("\"{value}\"");
        let mut fields = vec![quoted(&self.number), self.kind.to_string()];
        let optional = [
            self.subaddr.as_deref().map(quoted),
            self.satype.map(|v| v.to_string()),
            self.alpha.as_deref().map(quoted),
            self.cli_validity.map(|v| v.to_string()),
        ];
        if let Some(last) = optional.iter().rposition(Option::is_some) {
            for value in &optional[..=last] {
                fields.push(value.clone().unwrap_or_default());
            }
        }
        fields.join(",")
    }
}

// ---------------------------------------------------------------------------
// Indicator state
// ---------------------------------------------------------------------------

/// An AG indicator plus its supported range and current value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgIndicatorState {
    /// Which AG indicator.
    pub indicator: AgIndicator,
    /// Values this indicator can take.
    pub(crate) supported_values: BTreeSet<u32>,
    /// Current indicator value.
    pub current_status: u32,
    /// Whether +CIEV reporting is enabled for it.
    pub enabled: bool,
}

impl AgIndicatorState {
    fn with_range(indicator: AgIndicator, lo: u32, hi: u32) -> Self {
        Self {
            indicator,
            supported_values: (lo..=hi).collect(),
            current_status: 0,
            enabled: true,
        }
    }

    /// Call indicator state (range 0-1).
    pub fn call() -> Self {
        Self::with_range(AgIndicator::Call, 0, 1)
    }

    /// Call-setup indicator state (range 0-3).
    pub fn callsetup() -> Self {
        Self::with_range(AgIndicator::CallSetup, 0, 3)
    }

    /// Call-held indicator state (range 0-2).
    pub fn callheld() -> Self {
        Self::with_range(AgIndicator::CallHeld, 0, 2)
    }

    /// Service indicator state (range 0-1).
    pub fn service() -> Self {
        Self::with_range(AgIndicator::Service, 0, 1)
    }

    /// Signal indicator state (range 0-5).
    pub fn signal() -> Self {
        Self::with_range(AgIndicator::Signal, 0, 5)
    }

    /// Roaming indicator state (range 0-1).
    pub fn roam() -> Self {
        Self::with_range(AgIndicator::Roam, 0, 1)
    }

    /// Battery-charge indicator state (range 0-5).
    pub fn battchg() -> Self {
        Self::with_range(AgIndicator::BatteryCharge, 0, 5)
    }

    /// Renders the `("name",(min-max))` (or `("name",(v1,v2,...))` for a
    /// non-contiguous set) text used in an `+CIND: ...` test response.
    pub(crate) fn on_test_text(&self) -> String {
        let min = *self.supported_values.iter().next().unwrap_or(&0);
        let max = *self.supported_values.iter().next_back().unwrap_or(&0);
        let contiguous = self.supported_values.len() as u32 == max.saturating_sub(min) + 1;
        let values_text = if contiguous {
            format!("({min}-{max})")
        } else {
            format!(
                "({})",
                self.supported_values
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            )
        };
        format!("(\"{}\",{})", self.indicator.wire_name(), values_text)
    }
}

/// An HF indicator plus its supported/enabled state and value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HfIndicatorState {
    /// Which HF indicator.
    pub indicator: HfIndicator,
    /// Whether the peer supports it.
    pub supported: bool,
    /// Whether it is enabled.
    pub enabled: bool,
    /// Current value.
    pub current_status: u32,
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Configuration for the Hands-Free side of an HFP connection.
#[derive(Debug, Clone)]
pub struct HfConfiguration {
    /// HF supported-features bitmask (`hf_feature::*`).
    pub supported_hf_features: u32,
    /// HF indicators to advertise.
    pub supported_hf_indicators: Vec<HfIndicator>,
    /// Audio codecs the HF supports.
    pub supported_audio_codecs: Vec<AudioCodec>,
}

/// Configuration for the Audio Gateway side of an HFP connection.
#[derive(Debug, Clone)]
pub struct AgConfiguration {
    /// AG supported-features bitmask (`ag_feature::*`).
    pub supported_ag_features: u32,
    /// AG indicators with their ranges.
    pub supported_ag_indicators: Vec<AgIndicatorState>,
    /// HF indicators the AG supports.
    pub supported_hf_indicators: Vec<HfIndicator>,
    /// Supported AT+CHLD operations.
    pub supported_ag_call_hold_operations: Vec<CallHoldOperation>,
    /// Audio codecs the AG supports.
    pub supported_audio_codecs: Vec<AudioCodec>,
}

// ---------------------------------------------------------------------------
// Events
// ---------------------------------------------------------------------------

/// Outcome of feeding data into [`HfProtocol::receive`] or
/// [`AgProtocol::receive`], for the caller to act on.
#[derive(Debug, Clone, PartialEq)]
pub enum HfpEvent {
    /// The Service Level Connection procedure completed.
    SlcComplete,
    /// A caller-initiated ad hoc command (outside the SLC procedure, e.g.
    /// `AT+CLCC`) finished; `responses` holds every non-status line received
    /// before the final `OK`/error status.
    CommandCompleted {
        /// Command.
        command: String,
        /// Ok.
        ok: bool,
        /// Responses.
        responses: Vec<AtResponse>,
    },
    /// HF-observed: the AG updated an indicator (`+CIEV`).
    AgIndicatorUpdated(AgIndicatorState),
    /// AG-observed: the HF updated an HF indicator (`AT+BIEV`).
    HfIndicatorUpdated(HfIndicatorState),
    /// Either side accepted a codec (`AT+BCS`/`+BCS`), unsolicited.
    CodecNegotiated(AudioCodec),
    /// AG-observed: HF reported its available codecs (`AT+BAC`).
    SupportedAudioCodecs(Vec<AudioCodec>),
    /// AG-observed: HF requested the codec connection procedure (`AT+BCC`).
    CodecConnectionRequested,
    /// The codec is settled and the **AG** must now open the synchronous
    /// link. This is the seam between HFP and the transport: HFP decides
    /// *whether* there is audio and *which codec*; the SCO/eSCO connection
    /// itself is HCI, and belongs to whoever owns the controller.
    ///
    /// Only the AG side raises it — HFP v1.9 4.11.3 makes establishing the
    /// audio connection the Audio Gateway's job, however it was triggered.
    AudioConnectionRequested(AudioCodec),
    /// The audio connection changed state, at either end. `Connected` is
    /// reported only once the transport has said the synchronous link is
    /// really up, never on the strength of the AT handshake alone.
    AudioConnectionState(AudioConnectionState),
    /// Either side reported a speaker volume change (`AT+VGS`/`+VGS`).
    SpeakerVolume(u8),
    /// Either side reported a microphone volume change (`AT+VGM`/`+VGM`).
    MicrophoneVolume(u8),
    /// HF-observed: the AG signaled an incoming call (`RING`).
    Ring,
    /// HF-observed: caller-line-identification notification (`+CLIP`).
    CliNotification(CallLineIdentification),
    /// Either side reported voice-recognition state (`AT+BVRA`/`+BVRA`).
    VoiceRecognition(VoiceRecognitionState),
    /// AG-observed: the HF answered an incoming call (`ATA`).
    Answer,
    /// AG-observed: the HF hung up (`AT+CHUP`).
    HangUp,
    /// AG-observed: the HF dialed a number (`ATD...`).
    Dial(String),
    /// AG-observed: the HF requested a call-hold operation (`AT+CHLD`).
    CallHold {
        /// Operation.
        operation: CallHoldOperation,
        /// Call index.
        call_index: Option<u32>,
    },
}

// ---------------------------------------------------------------------------
// Wire framing helpers
// ---------------------------------------------------------------------------

fn command_bytes(cmd: &str) -> Vec<u8> {
    let mut bytes = cmd.as_bytes().to_vec();
    bytes.push(b'\r');
    bytes
}

fn response_bytes(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 4);
    bytes.extend_from_slice(b"\r\n");
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\r\n");
    bytes
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Extracts `\r\n<content>\r\n`-framed response lines (as sent by
/// [`response_bytes`]), draining consumed bytes from `buffer` and leaving any
/// trailing partial line for the next call.
fn extract_response_lines(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    while let Some(header) = find_subslice(buffer, b"\r\n") {
        let Some(trailer_rel) = find_subslice(&buffer[header + 2..], b"\r\n") else {
            break;
        };
        let trailer = header + 2 + trailer_rel;
        lines.push(buffer[header + 2..trailer].to_vec());
        buffer.drain(..trailer + 2);
    }
    lines
}

/// Extracts `\r`-terminated command lines (as sent by [`command_bytes`]).
fn extract_command_lines(buffer: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut lines = Vec::new();
    while let Some(idx) = buffer.iter().position(|&b| b == b'\r') {
        lines.push(buffer[..idx].to_vec());
        buffer.drain(..=idx);
    }
    lines
}

fn parse_one_cind_indicator(param: &AtParameter) -> Option<AgIndicatorState> {
    let AtParameter::List(pair) = param else {
        return None;
    };
    let name = pair.first()?.as_str()?;
    let indicator = AgIndicator::from_wire_name(name)?;
    let AtParameter::List(ranges) = pair.get(1)? else {
        return None;
    };
    let mut supported_values = BTreeSet::new();
    for value in ranges {
        let text = value.as_str()?;
        let mut bounds = text.split('-');
        let lo: u32 = bounds.next()?.parse().ok()?;
        let hi: u32 = match bounds.next() {
            Some(s) => s.parse().ok()?,
            None => lo,
        };
        supported_values.extend(lo..=hi);
    }
    Some(AgIndicatorState {
        indicator,
        supported_values,
        current_status: 0,
        enabled: true,
    })
}

/// Extracts `CallInfo`s from the `+CLCC: ...` lines of a completed
/// `AT+CLCC` [`HfpEvent::CommandCompleted`].
pub fn parse_call_infos(responses: &[AtResponse]) -> Vec<CallInfo> {
    responses.iter().filter_map(parse_one_call_info).collect()
}

/// Extracts the operator name from the `+COPS: <mode>,<format>,"<name>"`
/// line of a completed `AT+COPS?` [`HfpEvent::CommandCompleted`].
pub fn parse_network_operator(responses: &[AtResponse]) -> Option<String> {
    responses
        .iter()
        .find(|r| r.code == "+COPS")
        .and_then(|r| r.parameters.get(2))
        .and_then(AtParameter::as_str)
        .map(str::to_string)
}

fn parse_one_call_info(response: &AtResponse) -> Option<CallInfo> {
    if response.code != "+CLCC" {
        return None;
    }
    let p = &response.parameters;
    Some(CallInfo {
        index: p.first()?.as_u32()?,
        direction: CallInfoDirection::from_value(p.get(1)?.as_u32()?)?,
        status: CallInfoStatus::from_value(p.get(2)?.as_u32()?)?,
        mode: CallInfoMode::from_value(p.get(3)?.as_u32()?)?,
        multi_party: CallInfoMultiParty::from_value(p.get(4)?.as_u32()?)?,
        number: p.get(5).and_then(AtParameter::as_str).map(str::to_string),
        kind: p.get(6).and_then(AtParameter::as_u32).map(|v| v as u8),
    })
}

// ---------------------------------------------------------------------------
// HfProtocol (Hands-Free role)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SlcStep {
    Brsf,
    Bac,
    CindTest,
    CindRead,
    Cmer,
    ChldTest,
    Bind,
    BindTest,
    BindRead,
}

/// Hands-Free (HF) side of HFP: drives the Service Level Connection
/// procedure and call-control commands against an Audio Gateway peer.
/// Transport-free — see the module doc comment for how outgoing/incoming
/// bytes are meant to be wired to an RFCOMM `Dlc`.
#[derive(Debug)]
pub struct HfProtocol {
    /// HF supported-features bitmask.
    pub supported_hf_features: u32,
    /// Audio codecs the HF supports.
    pub supported_audio_codecs: Vec<AudioCodec>,
    /// HF indicator states, keyed by indicator.
    pub(crate) hf_indicators: BTreeMap<HfIndicator, HfIndicatorState>,

    /// AG features learned during SLC.
    pub supported_ag_features: u32,
    /// AG call-hold operations learned during SLC.
    pub supported_ag_call_hold_operations: Vec<CallHoldOperation>,
    /// AG indicator states learned during SLC.
    pub ag_indicators: Vec<AgIndicatorState>,

    /// Currently selected audio codec.
    pub active_codec: AudioCodec,
    /// Where the audio (SCO/eSCO) connection has got to. Independent of the
    /// Service Level Connection, which is what `slc_initialized` tracks.
    audio_state: AudioConnectionState,
    /// Whether the Service Level Connection is up.
    pub(crate) slc_initialized: bool,

    slc_step: Option<SlcStep>,
    pending_command: Option<String>,
    pending_responses: Vec<AtResponse>,
    read_buffer: Vec<u8>,
}

impl HfProtocol {
    /// Creates a Hands-Free protocol from its configuration.
    pub fn new(configuration: HfConfiguration) -> Self {
        Self {
            supported_hf_features: configuration.supported_hf_features,
            supported_audio_codecs: configuration.supported_audio_codecs,
            hf_indicators: configuration
                .supported_hf_indicators
                .into_iter()
                .map(|indicator| {
                    (
                        indicator,
                        HfIndicatorState {
                            indicator,
                            supported: false,
                            enabled: false,
                            current_status: 0,
                        },
                    )
                })
                .collect(),
            supported_ag_features: 0,
            supported_ag_call_hold_operations: Vec::new(),
            ag_indicators: Vec::new(),
            active_codec: AudioCodec::Cvsd,
            audio_state: AudioConnectionState::Disconnected,
            slc_initialized: false,
            slc_step: None,
            pending_command: None,
            pending_responses: Vec::new(),
            read_buffer: Vec::new(),
        }
    }

    /// Whether the HF advertises `feature` (`hf_feature::*`).
    pub(crate) fn supports_hf_feature(&self, feature: u32) -> bool {
        self.supported_hf_features & feature != 0
    }

    /// Whether the AG advertised `feature` (`ag_feature::*`).
    pub(crate) fn supports_ag_feature(&self, feature: u32) -> bool {
        self.supported_ag_features & feature != 0
    }

    /// Starts the Service Level Connection procedure (4.2.1): returns the
    /// initial `AT+BRSF=...` command bytes. Feed the AG's replies (and any
    /// further traffic) through [`Self::receive`]; `HfpEvent::SlcComplete`
    /// marks the end of the procedure.
    pub fn start_slc(&mut self) -> Vec<u8> {
        let cmd = format!("AT+BRSF={}", self.supported_hf_features);
        self.begin_step(cmd, SlcStep::Brsf)
    }

    fn begin_step(&mut self, cmd: String, step: SlcStep) -> Vec<u8> {
        let bytes = command_bytes(&cmd);
        self.pending_command = Some(cmd);
        self.pending_responses.clear();
        self.slc_step = Some(step);
        bytes
    }

    /// Whether a command is still waiting for its final status.
    ///
    /// An AT response carries nothing that identifies which command it
    /// answers — only its position in the stream does — so there is exactly
    /// one outstanding-command slot. A caller that issues a second command
    /// before the first completes will have the responses attributed to the
    /// wrong one.
    pub fn has_pending_command(&self) -> bool {
        self.pending_command.is_some()
    }

    /// Sends an ad hoc command outside the SLC procedure; its outcome
    /// arrives via a later `HfpEvent::CommandCompleted` from [`Self::receive`].
    pub fn send_command(&mut self, cmd: impl Into<String>) -> Vec<u8> {
        let cmd = cmd.into();
        let bytes = command_bytes(&cmd);
        self.pending_command = Some(cmd);
        self.pending_responses.clear();
        self.slc_step = None;
        bytes
    }

    /// Answers an incoming call (ATA).
    pub fn answer_incoming_call(&mut self) -> Vec<u8> {
        self.send_command("ATA")
    }

    /// Rejects or hangs up the current call (AT+CHUP).
    pub fn reject_incoming_call(&mut self) -> Vec<u8> {
        self.send_command("AT+CHUP")
    }

    /// Terminates the active call (AT+CHUP).
    pub fn terminate_call(&mut self) -> Vec<u8> {
        self.send_command("AT+CHUP")
    }

    /// Dials `number` (`ATD<number>;`).
    ///
    /// The trailing semicolon is not decoration: under V.250 it is what
    /// makes the call a voice call rather than a data call, and HFP v1.9
    /// 4.19.1 specifies `ATD<number>;` for exactly that reason. An AG that
    /// checks for it (Zephyr's `hfp_ag.c` does) rejects the bare form.
    pub fn dial(&mut self, number: &str) -> Vec<u8> {
        self.send_command(format!("ATD{number};"))
    }

    /// Selects the operator-name format (`AT+COPS=3,0`), which HFP v1.9 4.7
    /// requires the HF to do before the name may be read.
    pub fn select_operator_format(&mut self) -> Vec<u8> {
        self.send_command("AT+COPS=3,0")
    }

    /// Reads the network operator name (`AT+COPS?`); the answer arrives in
    /// the `HfpEvent::CommandCompleted` responses, for
    /// [`parse_network_operator`].
    pub fn query_network_operator(&mut self) -> Vec<u8> {
        self.send_command("AT+COPS?")
    }

    /// Queries current calls (AT+CLCC).
    pub fn query_current_calls(&mut self) -> Vec<u8> {
        self.send_command("AT+CLCC")
    }

    /// Sends an HF indicator value (AT+BIEV).
    pub fn set_hf_indicator(&mut self, indicator: HfIndicator, value: u32) -> Vec<u8> {
        self.send_command(format!("AT+BIEV={},{value}", indicator.to_value()))
    }

    /// Sets voice-recognition state (AT+BVRA).
    pub fn set_voice_recognition(&mut self, state: VoiceRecognitionState) -> Vec<u8> {
        self.send_command(format!("AT+BVRA={}", state.to_value()))
    }

    /// Requests the codec connection procedure (AT+BCC) — asking the AG to
    /// open the audio connection, since the HF may not open one itself.
    pub fn setup_audio_connection(&mut self) -> Vec<u8> {
        self.send_command("AT+BCC")
    }

    // --- the audio connection ----------------------------------------------

    /// Where the audio (SCO/eSCO) connection has got to, as this end sees it.
    pub fn audio_state(&self) -> AudioConnectionState {
        self.audio_state
    }

    /// The transport says the synchronous link is up.
    pub fn on_audio_connected(&mut self) -> Vec<HfpEvent> {
        let mut events = Vec::new();
        self.set_audio_state(AudioConnectionState::Connected, &mut events);
        events
    }

    /// The transport says the synchronous link is gone.
    pub fn on_audio_disconnected(&mut self) -> Vec<HfpEvent> {
        let mut events = Vec::new();
        self.set_audio_state(AudioConnectionState::Disconnected, &mut events);
        events
    }

    fn set_audio_state(&mut self, state: AudioConnectionState, events: &mut Vec<HfpEvent>) {
        if self.audio_state == state {
            return;
        }
        self.audio_state = state;
        events.push(HfpEvent::AudioConnectionState(state));
    }

    /// Sends a call-hold operation (AT+CHLD).
    pub fn hold_call(&mut self, operation: CallHoldOperation, call_index: Option<u32>) -> Vec<u8> {
        let code = match call_index {
            Some(index) => format!("{}{index}", &operation.code()[..1]),
            None => operation.code().to_string(),
        };
        self.send_command(format!("AT+CHLD={code}"))
    }

    /// Feeds newly-arrived bytes (an RFCOMM `Dlc`'s payload), returning any
    /// command lines to send back plus events for the caller to observe.
    pub fn receive(&mut self, data: &[u8]) -> (Vec<Vec<u8>>, Vec<HfpEvent>) {
        self.read_buffer.extend_from_slice(data);
        let mut outgoing = Vec::new();
        let mut events = Vec::new();
        for line in extract_response_lines(&mut self.read_buffer) {
            if line.is_empty() {
                continue;
            }
            let Ok(response) = AtResponse::parse_from(&line) else {
                continue;
            };
            self.handle_response(response, &mut outgoing, &mut events);
        }
        (outgoing, events)
    }

    fn handle_response(
        &mut self,
        response: AtResponse,
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        let is_command_response = self.pending_command.as_ref().is_some_and(|cmd| {
            STATUS_CODES.contains(&response.code.as_str()) || cmd.contains(response.code.as_str())
        });
        if !is_command_response {
            self.handle_unsolicited(response, outgoing, events);
            return;
        }
        if response.code == "OK" {
            let cmd = self.pending_command.take().expect("checked above");
            let responses = std::mem::take(&mut self.pending_responses);
            if let Some(step) = self.slc_step.take() {
                self.advance_slc(step, responses, outgoing, events);
            } else {
                events.push(HfpEvent::CommandCompleted {
                    command: cmd,
                    ok: true,
                    responses,
                });
            }
        } else if STATUS_CODES.contains(&response.code.as_str()) {
            let cmd = self.pending_command.take().expect("checked above");
            let responses = std::mem::take(&mut self.pending_responses);
            self.slc_step = None;
            events.push(HfpEvent::CommandCompleted {
                command: cmd,
                ok: false,
                responses,
            });
        } else {
            self.pending_responses.push(response);
        }
    }

    fn advance_slc(
        &mut self,
        step: SlcStep,
        responses: Vec<AtResponse>,
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        match step {
            SlcStep::Brsf => {
                self.supported_ag_features = responses
                    .first()
                    .and_then(|r| r.parameters.first())
                    .and_then(AtParameter::as_u32)
                    .unwrap_or(0);
                if self.supports_hf_feature(hf_feature::CODEC_NEGOTIATION)
                    && self.supports_ag_feature(ag_feature::CODEC_NEGOTIATION)
                {
                    let codecs = self
                        .supported_audio_codecs
                        .iter()
                        .map(|c| c.to_value().to_string())
                        .collect::<Vec<_>>()
                        .join(",");
                    outgoing.push(self.begin_step(format!("AT+BAC={codecs}"), SlcStep::Bac));
                } else {
                    outgoing.push(self.begin_step("AT+CIND=?".into(), SlcStep::CindTest));
                }
            }
            SlcStep::Bac => {
                outgoing.push(self.begin_step("AT+CIND=?".into(), SlcStep::CindTest));
            }
            SlcStep::CindTest => {
                self.ag_indicators = responses
                    .first()
                    .map(|r| {
                        r.parameters
                            .iter()
                            .filter_map(parse_one_cind_indicator)
                            .collect()
                    })
                    .unwrap_or_default();
                outgoing.push(self.begin_step("AT+CIND?".into(), SlcStep::CindRead));
            }
            SlcStep::CindRead => {
                if let Some(r) = responses.first() {
                    for (state, param) in self.ag_indicators.iter_mut().zip(r.parameters.iter()) {
                        if let Some(v) = param.as_u32() {
                            state.current_status = v;
                        }
                    }
                }
                outgoing.push(self.begin_step("AT+CMER=3,,,1".into(), SlcStep::Cmer));
            }
            SlcStep::Cmer => {
                if self.supports_hf_feature(hf_feature::THREE_WAY_CALLING)
                    && self.supports_ag_feature(ag_feature::THREE_WAY_CALLING)
                {
                    outgoing.push(self.begin_step("AT+CHLD=?".into(), SlcStep::ChldTest));
                } else {
                    self.start_hf_indicators_or_finish(outgoing, events);
                }
            }
            SlcStep::ChldTest => {
                if let Some(AtParameter::List(ops)) =
                    responses.first().and_then(|r| r.parameters.first())
                {
                    self.supported_ag_call_hold_operations = ops
                        .iter()
                        .filter_map(AtParameter::as_str)
                        .filter_map(CallHoldOperation::from_code)
                        .collect();
                }
                self.start_hf_indicators_or_finish(outgoing, events);
            }
            SlcStep::Bind => {
                outgoing.push(self.begin_step("AT+BIND=?".into(), SlcStep::BindTest));
            }
            SlcStep::BindTest => {
                if let Some(AtParameter::List(items)) =
                    responses.first().and_then(|r| r.parameters.first())
                {
                    let supported: HashSet<u32> =
                        items.iter().filter_map(AtParameter::as_u32).collect();
                    for (indicator, state) in self.hf_indicators.iter_mut() {
                        if supported.contains(&(indicator.to_value() as u32)) {
                            state.supported = true;
                        }
                    }
                }
                outgoing.push(self.begin_step("AT+BIND?".into(), SlcStep::BindRead));
            }
            SlcStep::BindRead => {
                for r in &responses {
                    let Some(indicator) = r
                        .parameters
                        .first()
                        .and_then(AtParameter::as_u32)
                        .and_then(|v| HfIndicator::from_value(v as u8))
                    else {
                        continue;
                    };
                    let enabled = r
                        .parameters
                        .get(1)
                        .and_then(AtParameter::as_u32)
                        .unwrap_or(0)
                        != 0;
                    if let Some(state) = self.hf_indicators.get_mut(&indicator) {
                        state.enabled = enabled;
                    }
                }
                self.slc_initialized = true;
                events.push(HfpEvent::SlcComplete);
            }
        }
    }

    fn start_hf_indicators_or_finish(
        &mut self,
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        if self.supports_hf_feature(hf_feature::HF_INDICATORS)
            && self.supports_ag_feature(ag_feature::HF_INDICATORS)
        {
            let indicators = self
                .hf_indicators
                .keys()
                .map(|i| i.to_value().to_string())
                .collect::<Vec<_>>()
                .join(",");
            outgoing.push(self.begin_step(format!("AT+BIND={indicators}"), SlcStep::Bind));
        } else {
            self.slc_initialized = true;
            events.push(HfpEvent::SlcComplete);
        }
    }

    fn handle_unsolicited(
        &mut self,
        response: AtResponse,
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        match response.code.as_str() {
            "+BCS" => {
                if let Some(codec) = response
                    .parameters
                    .first()
                    .and_then(AtParameter::as_u32)
                    .and_then(|v| AudioCodec::from_value(v as u8))
                {
                    self.active_codec = codec;
                    outgoing.push(command_bytes(&format!("AT+BCS={}", codec.to_value())));
                    events.push(HfpEvent::CodecNegotiated(codec));
                    // The HF now expects the AG to open the synchronous
                    // link. It does not open one itself — HFP gives that
                    // job to the AG alone — so all it does here is stop
                    // reporting itself as having no audio.
                    self.set_audio_state(AudioConnectionState::Connecting, events);
                }
            }
            "+CIEV" => {
                let index = response.parameters.first().and_then(AtParameter::as_u32);
                let value = response.parameters.get(1).and_then(AtParameter::as_u32);
                if let (Some(index), Some(value)) = (index, value)
                    && let Some(state) = (index as usize)
                        .checked_sub(1)
                        .and_then(|i| self.ag_indicators.get_mut(i))
                {
                    state.current_status = value;
                    events.push(HfpEvent::AgIndicatorUpdated(state.clone()));
                }
            }
            "+VGS" => {
                if let Some(v) = response.parameters.first().and_then(AtParameter::as_u32) {
                    events.push(HfpEvent::SpeakerVolume(v as u8));
                }
            }
            "+VGM" => {
                if let Some(v) = response.parameters.first().and_then(AtParameter::as_u32) {
                    events.push(HfpEvent::MicrophoneVolume(v as u8));
                }
            }
            "RING" => events.push(HfpEvent::Ring),
            "+CLIP" => {
                if let Ok(cli) = CallLineIdentification::parse_from(&response.parameters) {
                    events.push(HfpEvent::CliNotification(cli));
                }
            }
            "+BVRA" => {
                if let Some(state) = response
                    .parameters
                    .first()
                    .and_then(AtParameter::as_u32)
                    .and_then(VoiceRecognitionState::from_value)
                {
                    events.push(HfpEvent::VoiceRecognition(state));
                }
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// AgProtocol (Audio Gateway role)
// ---------------------------------------------------------------------------

/// Audio Gateway (AG) side of HFP: answers AT commands from an HF peer and
/// exposes methods to send AG-initiated unsolicited responses (ring,
/// indicator updates, volume reports, codec negotiation). Transport-free,
/// like [`HfProtocol`].
#[derive(Debug)]
pub struct AgProtocol {
    /// AG supported-features bitmask.
    pub supported_ag_features: u32,
    /// Supported AT+CHLD operations.
    pub supported_ag_call_hold_operations: Vec<CallHoldOperation>,
    /// AG indicator states.
    pub ag_indicators: Vec<AgIndicatorState>,
    supported_hf_indicators: BTreeSet<HfIndicator>,
    /// HF indicator states, in advertised order.
    pub(crate) hf_indicators: Vec<(HfIndicator, HfIndicatorState)>,

    /// HF features learned during SLC.
    pub supported_hf_features: u32,
    /// Audio codecs the AG supports.
    pub supported_audio_codecs: Vec<AudioCodec>,
    /// Audio codecs the **HF** reported with `AT+BAC`, empty until it does.
    /// Kept apart from the AG's own list because choosing a codec means
    /// intersecting the two.
    pub hf_audio_codecs: Vec<AudioCodec>,
    /// Currently selected audio codec.
    pub active_codec: AudioCodec,
    /// Where the audio (SCO/eSCO) connection has got to.
    audio_state: AudioConnectionState,
    /// Current calls tracked by the AG.
    pub calls: Vec<CallInfo>,

    /// Whether +CIEV indicator reporting is enabled (AT+CMER).
    pub(crate) indicator_report_enabled: bool,
    /// Whether extended +CME ERROR codes are enabled (AT+CMEE).
    pub(crate) cme_error_enabled: bool,
    /// Whether +CLIP notifications are enabled (AT+CLIP).
    pub(crate) cli_notification_enabled: bool,
    /// Whether call-waiting notifications are enabled (AT+CCWA).
    pub(crate) call_waiting_enabled: bool,
    /// Whether in-band ring tones are enabled.
    pub inband_ringtone_enabled: bool,
    /// Network operator name reported by `AT+COPS?`.
    pub network_operator: String,
    /// Whether the HF selected the operator-name format with `AT+COPS=3,0`.
    pub(crate) cops_format_selected: bool,

    remaining_slc_setup_features: HashSet<u32>,
    read_buffer: Vec<u8>,
}

impl AgProtocol {
    /// Creates an Audio Gateway protocol from its configuration.
    pub fn new(configuration: AgConfiguration) -> Self {
        Self {
            supported_ag_features: configuration.supported_ag_features,
            supported_ag_call_hold_operations: configuration.supported_ag_call_hold_operations,
            ag_indicators: configuration.supported_ag_indicators,
            supported_hf_indicators: configuration.supported_hf_indicators.into_iter().collect(),
            hf_indicators: Vec::new(),
            supported_hf_features: 0,
            supported_audio_codecs: configuration.supported_audio_codecs,
            hf_audio_codecs: Vec::new(),
            active_codec: AudioCodec::Cvsd,
            audio_state: AudioConnectionState::Disconnected,
            calls: Vec::new(),
            indicator_report_enabled: false,
            cme_error_enabled: false,
            cli_notification_enabled: false,
            call_waiting_enabled: false,
            inband_ringtone_enabled: true,
            network_operator: String::new(),
            cops_format_selected: false,
            remaining_slc_setup_features: HashSet::new(),
            read_buffer: Vec::new(),
        }
    }

    /// Whether the HF advertised `feature` (`hf_feature::*`).
    pub(crate) fn supports_hf_feature(&self, feature: u32) -> bool {
        self.supported_hf_features & feature != 0
    }

    /// Whether the AG advertises `feature` (`ag_feature::*`).
    pub(crate) fn supports_ag_feature(&self, feature: u32) -> bool {
        self.supported_ag_features & feature != 0
    }

    /// Feeds newly-arrived bytes (an RFCOMM `Dlc`'s payload), returning any
    /// response lines to send back plus events for the caller to observe.
    pub fn receive(&mut self, data: &[u8]) -> (Vec<Vec<u8>>, Vec<HfpEvent>) {
        self.read_buffer.extend_from_slice(data);
        let mut outgoing = Vec::new();
        let mut events = Vec::new();
        for line in extract_command_lines(&mut self.read_buffer) {
            if line.is_empty() {
                continue;
            }
            match AtCommand::parse_from(&line) {
                Ok(cmd) => self.dispatch(cmd, &mut outgoing, &mut events),
                Err(_) => outgoing.push(response_bytes("ERROR")),
            }
        }
        (outgoing, events)
    }

    fn dispatch(
        &mut self,
        cmd: AtCommand,
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        match (cmd.code.as_str(), cmd.sub_code) {
            ("BRSF", AtSubCode::Set) => self.on_brsf(&cmd.parameters, outgoing),
            ("BAC", AtSubCode::Set) => self.on_bac(&cmd.parameters, outgoing, events),
            ("BCS", AtSubCode::Set) => self.on_bcs(&cmd.parameters, outgoing, events),
            ("BVRA", AtSubCode::Set) => self.on_bvra(&cmd.parameters, outgoing, events),
            ("CHLD", AtSubCode::Set) => self.on_chld(&cmd.parameters, outgoing, events),
            ("CHLD", AtSubCode::Test) => self.on_chld_test(outgoing, events),
            ("CIND", AtSubCode::Test) => self.on_cind_test(outgoing),
            ("CIND", AtSubCode::Read) => self.on_cind_read(outgoing, events),
            ("CMER", AtSubCode::Set) => self.on_cmer(&cmd.parameters, outgoing),
            ("CMEE", AtSubCode::Set) => self.on_cmee(&cmd.parameters, outgoing),
            ("CCWA", AtSubCode::Set) => self.on_ccwa(&cmd.parameters, outgoing),
            ("BIND", AtSubCode::Set) => self.on_bind(&cmd.parameters, outgoing),
            ("BIND", AtSubCode::Test) => self.on_bind_test(outgoing),
            ("BIND", AtSubCode::Read) => self.on_bind_read(outgoing, events),
            ("BIEV", AtSubCode::Set) => self.on_biev(&cmd.parameters, outgoing, events),
            ("BIA", AtSubCode::Set) => self.on_bia(&cmd.parameters, outgoing),
            ("BCC", AtSubCode::None) => self.on_bcc(outgoing, events),
            ("A", AtSubCode::None) => self.on_answer(outgoing, events),
            ("D", AtSubCode::None) => self.on_dial(&cmd.parameters, outgoing, events),
            ("CHUP", AtSubCode::None) => self.on_chup(outgoing, events),
            ("CLCC", AtSubCode::None) => self.on_clcc(outgoing),
            ("CLIP", AtSubCode::Set) => self.on_clip(&cmd.parameters, outgoing),
            ("COPS", AtSubCode::Set) => self.on_cops_set(&cmd.parameters, outgoing),
            ("COPS", AtSubCode::Read) => self.on_cops_read(outgoing),
            ("VGS", AtSubCode::Set) => self.on_vgs(&cmd.parameters, outgoing, events),
            ("VGM", AtSubCode::Set) => self.on_vgm(&cmd.parameters, outgoing, events),
            _ => outgoing.push(response_bytes("ERROR")),
        }
    }

    fn cme_error_response(&self, error: CmeError) -> Vec<u8> {
        if self.cme_error_enabled {
            response_bytes(&format!("+CME ERROR: {}", error.to_value()))
        } else {
            response_bytes("ERROR")
        }
    }

    fn check_remaining_slc_commands(&mut self, events: &mut Vec<HfpEvent>) {
        if self.remaining_slc_setup_features.is_empty() {
            events.push(HfpEvent::SlcComplete);
        }
    }

    fn on_brsf(&mut self, params: &[AtParameter], outgoing: &mut Vec<Vec<u8>>) {
        self.supported_hf_features = params.first().and_then(AtParameter::as_u32).unwrap_or(0);
        outgoing.push(response_bytes(&format!(
            "+BRSF: {}",
            self.supported_ag_features
        )));
        outgoing.push(response_bytes("OK"));
        if self.supports_hf_feature(hf_feature::HF_INDICATORS)
            && self.supports_ag_feature(ag_feature::HF_INDICATORS)
        {
            self.remaining_slc_setup_features
                .insert(hf_feature::HF_INDICATORS);
        }
        if self.supports_hf_feature(hf_feature::THREE_WAY_CALLING)
            && self.supports_ag_feature(ag_feature::THREE_WAY_CALLING)
        {
            self.remaining_slc_setup_features
                .insert(hf_feature::THREE_WAY_CALLING);
        }
    }

    /// `AT+BAC=<codec>,…` — the HF listing the codecs *it* can do.
    ///
    /// This used to be stored into `supported_audio_codecs`, which is the
    /// **AG's own** list from its configuration, destroying it. Nothing
    /// noticed while codec negotiation stopped at the AT layer: the field
    /// was only ever read back to build an SDP record, from the
    /// configuration rather than from here. It matters the moment a codec
    /// has to be *chosen*, because choosing means intersecting two lists and
    /// there was only one list left.
    fn on_bac(
        &mut self,
        params: &[AtParameter],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        self.hf_audio_codecs = params
            .iter()
            .filter_map(AtParameter::as_u32)
            .filter_map(|v| AudioCodec::from_value(v as u8))
            .collect();
        events.push(HfpEvent::SupportedAudioCodecs(self.hf_audio_codecs.clone()));
        outgoing.push(response_bytes("OK"));
    }

    fn on_bcs(
        &mut self,
        params: &[AtParameter],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        match params
            .first()
            .and_then(AtParameter::as_u32)
            .and_then(|v| AudioCodec::from_value(v as u8))
        {
            Some(codec) => {
                self.active_codec = codec;
                outgoing.push(response_bytes("OK"));
                events.push(HfpEvent::CodecNegotiated(codec));
                // The Codec Connection procedure is over (HFP v1.9 4.11.3);
                // opening the synchronous link is the next step and the AG's
                // to take. Only when a procedure was actually running: an
                // `AT+BCS` arriving out of the blue settles a codec but does
                // not authorise audio nobody asked for.
                if self.audio_state == AudioConnectionState::Negotiating {
                    self.set_audio_state(AudioConnectionState::Connecting, events);
                    events.push(HfpEvent::AudioConnectionRequested(codec));
                }
            }
            None => outgoing.push(response_bytes("ERROR")),
        }
    }

    fn on_bvra(
        &mut self,
        params: &[AtParameter],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        outgoing.push(response_bytes("OK"));
        if let Some(state) = params
            .first()
            .and_then(AtParameter::as_u32)
            .and_then(VoiceRecognitionState::from_value)
        {
            events.push(HfpEvent::VoiceRecognition(state));
        }
    }

    fn on_chld(
        &mut self,
        params: &[AtParameter],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        let Some(raw) = params.first().and_then(AtParameter::as_str) else {
            outgoing.push(self.cme_error_response(CmeError::OperationNotSupported));
            return;
        };
        let (code, call_index): (String, Option<u32>) = if raw.len() > 1 {
            match raw[1..].parse() {
                Ok(index) => (format!("{}x", &raw[..1]), Some(index)),
                Err(_) => {
                    outgoing.push(self.cme_error_response(CmeError::OperationNotSupported));
                    return;
                }
            }
        } else {
            (raw.to_string(), None)
        };
        let Some(operation) = CallHoldOperation::from_code(&code) else {
            outgoing.push(self.cme_error_response(CmeError::OperationNotSupported));
            return;
        };
        if !self.supported_ag_call_hold_operations.contains(&operation) {
            outgoing.push(self.cme_error_response(CmeError::OperationNotSupported));
            return;
        }
        if let Some(index) = call_index
            && !self.calls.iter().any(|c| c.index == index)
        {
            outgoing.push(self.cme_error_response(CmeError::InvalidIndex));
            return;
        }
        outgoing.push(response_bytes("OK"));
        events.push(HfpEvent::CallHold {
            operation,
            call_index,
        });
    }

    fn on_chld_test(&mut self, outgoing: &mut Vec<Vec<u8>>, events: &mut Vec<HfpEvent>) {
        if !self.supports_ag_feature(ag_feature::THREE_WAY_CALLING) {
            outgoing.push(response_bytes("ERROR"));
            return;
        }
        let ops = self
            .supported_ag_call_hold_operations
            .iter()
            .map(|op| op.code())
            .collect::<Vec<_>>()
            .join(",");
        outgoing.push(response_bytes(&format!("+CHLD: ({ops})")));
        outgoing.push(response_bytes("OK"));
        self.remaining_slc_setup_features
            .remove(&hf_feature::THREE_WAY_CALLING);
        self.check_remaining_slc_commands(events);
    }

    fn on_cind_test(&mut self, outgoing: &mut Vec<Vec<u8>>) {
        if self.ag_indicators.is_empty() {
            outgoing.push(self.cme_error_response(CmeError::NotFound));
            return;
        }
        let text = self
            .ag_indicators
            .iter()
            .map(AgIndicatorState::on_test_text)
            .collect::<Vec<_>>()
            .join(",");
        outgoing.push(response_bytes(&format!("+CIND: {text}")));
        outgoing.push(response_bytes("OK"));
    }

    fn on_cind_read(&mut self, outgoing: &mut Vec<Vec<u8>>, events: &mut Vec<HfpEvent>) {
        if self.ag_indicators.is_empty() {
            outgoing.push(self.cme_error_response(CmeError::NotFound));
            return;
        }
        let text = self
            .ag_indicators
            .iter()
            .map(|s| s.current_status.to_string())
            .collect::<Vec<_>>()
            .join(",");
        outgoing.push(response_bytes(&format!("+CIND: {text}")));
        outgoing.push(response_bytes("OK"));
        self.check_remaining_slc_commands(events);
    }

    fn on_cmer(&mut self, params: &[AtParameter], outgoing: &mut Vec<Vec<u8>>) {
        let mode = params.first().and_then(AtParameter::as_u32);
        let keypad = params.get(1).and_then(AtParameter::as_u32).unwrap_or(0);
        let display = params.get(2).and_then(AtParameter::as_u32).unwrap_or(0);
        let indicator = params.get(3).and_then(AtParameter::as_u32).unwrap_or(0);
        if mode != Some(3) || keypad != 0 || display != 0 || (indicator != 0 && indicator != 1) {
            outgoing.push(self.cme_error_response(CmeError::InvalidIndex));
        }
        self.indicator_report_enabled = indicator != 0;
        outgoing.push(response_bytes("OK"));
    }

    fn on_cmee(&mut self, params: &[AtParameter], outgoing: &mut Vec<Vec<u8>>) {
        self.cme_error_enabled = params.first().and_then(AtParameter::as_u32).unwrap_or(0) != 0;
        outgoing.push(response_bytes("OK"));
    }

    fn on_ccwa(&mut self, params: &[AtParameter], outgoing: &mut Vec<Vec<u8>>) {
        self.call_waiting_enabled = params.first().and_then(AtParameter::as_u32).unwrap_or(0) != 0;
        outgoing.push(response_bytes("OK"));
    }

    fn on_bind(&mut self, params: &[AtParameter], outgoing: &mut Vec<Vec<u8>>) {
        if !self.supports_ag_feature(ag_feature::HF_INDICATORS) {
            outgoing.push(response_bytes("ERROR"));
            return;
        }
        let peer_supported: HashSet<HfIndicator> = params
            .iter()
            .filter_map(AtParameter::as_u32)
            .filter_map(|v| HfIndicator::from_value(v as u8))
            .collect();
        self.hf_indicators = self
            .supported_hf_indicators
            .iter()
            .filter(|i| peer_supported.contains(i))
            .map(|&indicator| {
                (
                    indicator,
                    HfIndicatorState {
                        indicator,
                        supported: false,
                        enabled: false,
                        current_status: 0,
                    },
                )
            })
            .collect();
        outgoing.push(response_bytes("OK"));
    }

    fn on_bind_test(&mut self, outgoing: &mut Vec<Vec<u8>>) {
        if !self.supports_ag_feature(ag_feature::HF_INDICATORS) {
            outgoing.push(response_bytes("ERROR"));
            return;
        }
        let text = self
            .supported_hf_indicators
            .iter()
            .map(|i| i.to_value().to_string())
            .collect::<Vec<_>>()
            .join(",");
        outgoing.push(response_bytes(&format!("+BIND: ({text})")));
        outgoing.push(response_bytes("OK"));
    }

    fn on_bind_read(&mut self, outgoing: &mut Vec<Vec<u8>>, events: &mut Vec<HfpEvent>) {
        if !self.supports_ag_feature(ag_feature::HF_INDICATORS) {
            outgoing.push(response_bytes("ERROR"));
            return;
        }
        for (indicator, _) in &self.hf_indicators {
            outgoing.push(response_bytes(&format!(
                "+BIND: {},1",
                indicator.to_value()
            )));
        }
        outgoing.push(response_bytes("OK"));
        self.remaining_slc_setup_features
            .remove(&hf_feature::HF_INDICATORS);
        self.check_remaining_slc_commands(events);
    }

    fn on_biev(
        &mut self,
        params: &[AtParameter],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        if !self.supports_ag_feature(ag_feature::HF_INDICATORS) {
            outgoing.push(response_bytes("ERROR"));
            return;
        }
        let indicator = params
            .first()
            .and_then(AtParameter::as_u32)
            .and_then(|v| HfIndicator::from_value(v as u8));
        let value = params.get(1).and_then(AtParameter::as_u32);
        let (Some(indicator), Some(value)) = (indicator, value) else {
            outgoing.push(response_bytes("ERROR"));
            return;
        };
        let Some(entry) = self.hf_indicators.iter_mut().find(|(i, _)| *i == indicator) else {
            outgoing.push(response_bytes("ERROR"));
            return;
        };
        entry.1.current_status = value;
        events.push(HfpEvent::HfIndicatorUpdated(entry.1.clone()));
        outgoing.push(response_bytes("OK"));
    }

    fn on_bia(&mut self, params: &[AtParameter], outgoing: &mut Vec<Vec<u8>>) {
        for (enabled, state) in params.iter().zip(self.ag_indicators.iter_mut()) {
            if let Some(v) = enabled.as_u32() {
                state.enabled = v != 0;
            }
        }
        outgoing.push(response_bytes("OK"));
    }

    /// `AT+BCC` — the HF asking for audio. The HF cannot open the
    /// synchronous link itself; this is how it asks the AG to.
    fn on_bcc(&mut self, outgoing: &mut Vec<Vec<u8>>, events: &mut Vec<HfpEvent>) {
        events.push(HfpEvent::CodecConnectionRequested);
        outgoing.push(response_bytes("OK"));
        // `OK` first, then whatever the audio connection needs: the HF's
        // command is answered before an unsolicited `+BCS` follows it, which
        // is the order an AT client's one-outstanding-command slot requires.
        let (audio_out, audio_events) = self.start_audio_connection();
        outgoing.extend(audio_out);
        events.extend(audio_events);
    }

    fn on_answer(&mut self, outgoing: &mut Vec<Vec<u8>>, events: &mut Vec<HfpEvent>) {
        events.push(HfpEvent::Answer);
        outgoing.push(response_bytes("OK"));
    }

    fn on_dial(
        &mut self,
        params: &[AtParameter],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        // V.250's trailing `;` selects a voice call; it is part of the
        // command, not part of the number, so it is stripped before the
        // number reaches the caller.
        let number = params
            .first()
            .and_then(AtParameter::as_str)
            .unwrap_or("")
            .trim_end_matches(';')
            .to_string();
        events.push(HfpEvent::Dial(number));
        outgoing.push(response_bytes("OK"));
    }

    fn on_chup(&mut self, outgoing: &mut Vec<Vec<u8>>, events: &mut Vec<HfpEvent>) {
        events.push(HfpEvent::HangUp);
        outgoing.push(response_bytes("OK"));
    }

    fn on_clcc(&mut self, outgoing: &mut Vec<Vec<u8>>) {
        for call in &self.calls {
            let mut resp = format!(
                "+CLCC: {},{},{},{},{}",
                call.index,
                call.direction.to_value(),
                call.status.to_value(),
                call.mode.to_value(),
                call.multi_party.to_value()
            );
            if let Some(number) = &call.number {
                resp.push_str(&format!(",\"{number}\""));
            }
            if let Some(kind) = call.kind {
                resp.push_str(&format!(",{kind}"));
            }
            outgoing.push(response_bytes(&resp));
        }
        outgoing.push(response_bytes("OK"));
    }

    fn on_clip(&mut self, params: &[AtParameter], outgoing: &mut Vec<Vec<u8>>) {
        self.cli_notification_enabled = params.first().and_then(AtParameter::as_str) == Some("1");
        outgoing.push(response_bytes("OK"));
    }

    /// `AT+COPS=<mode>,<format>` (HFP v1.9 4.7): the HF must select mode 3
    /// (set format only, do not attempt network selection) and format 0
    /// (long alphanumeric) before it may read the name, so any other pair is
    /// refused rather than silently accepted.
    fn on_cops_set(&mut self, params: &[AtParameter], outgoing: &mut Vec<Vec<u8>>) {
        let mode = params.first().and_then(AtParameter::as_u32);
        let format = params.get(1).and_then(AtParameter::as_u32);
        if (mode, format) != (Some(3), Some(0)) {
            outgoing.push(self.cme_error_response(CmeError::OperationNotSupported));
            return;
        }
        self.cops_format_selected = true;
        outgoing.push(response_bytes("OK"));
    }

    /// `AT+COPS?` (HFP v1.9 4.7). Answered only once the format has been
    /// selected; a bare read is an ordering error, not a missing name.
    fn on_cops_read(&mut self, outgoing: &mut Vec<Vec<u8>>) {
        if !self.cops_format_selected {
            outgoing.push(self.cme_error_response(CmeError::OperationNotAllowed));
            return;
        }
        outgoing.push(response_bytes(&format!(
            "+COPS: 0,0,\"{}\"",
            self.network_operator
        )));
        outgoing.push(response_bytes("OK"));
    }

    fn on_vgs(
        &mut self,
        params: &[AtParameter],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        if let Some(v) = params.first().and_then(AtParameter::as_u32) {
            events.push(HfpEvent::SpeakerVolume(v as u8));
        }
        outgoing.push(response_bytes("OK"));
    }

    fn on_vgm(
        &mut self,
        params: &[AtParameter],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<HfpEvent>,
    ) {
        if let Some(v) = params.first().and_then(AtParameter::as_u32) {
            events.push(HfpEvent::MicrophoneVolume(v as u8));
        }
        outgoing.push(response_bytes("OK"));
    }

    /// Sends `+BCS: <codec>` unsolicited, starting AG-initiated codec
    /// negotiation; completion (`AT+BCS=<codec>` from the HF) arrives as
    /// `HfpEvent::CodecNegotiated` from a later [`Self::receive`].
    pub fn negotiate_codec(&mut self, codec: AudioCodec) -> Vec<u8> {
        response_bytes(&format!("+BCS: {}", codec.to_value()))
    }

    // --- the audio connection ----------------------------------------------

    /// Where the audio (SCO/eSCO) connection has got to.
    pub fn audio_state(&self) -> AudioConnectionState {
        self.audio_state
    }

    /// The codec the two ends settled on, once one has been chosen.
    pub fn negotiated_codec(&self) -> AudioCodec {
        self.active_codec
    }

    /// Begin establishing the audio connection (HFP v1.9 4.11).
    ///
    /// Establishing it is always the **AG's** job, whatever triggered it —
    /// an incoming call, the HF answering, or the HF asking with `AT+BCC`.
    /// The HF never opens the synchronous link itself, which is why this
    /// method exists only here.
    ///
    /// Where both ends advertised Codec Negotiation, this runs the Codec
    /// Connection procedure first: `+BCS: <codec>` goes out and the link is
    /// opened only when the HF's `AT+BCS=<codec>` confirms it. Where they
    /// did not, there is nothing to negotiate — CVSD is the only codec HFP
    /// guarantees — and the caller is told to open the link at once.
    ///
    /// Returns the bytes to write and the events to act on. The
    /// [`HfpEvent::AudioConnectionRequested`] in there is the seam: HFP has
    /// decided there is audio and which codec, and something that owns an
    /// HCI channel must now open the link and call
    /// [`Self::on_audio_connected`].
    pub fn start_audio_connection(&mut self) -> (Vec<Vec<u8>>, Vec<HfpEvent>) {
        let (mut outgoing, mut events) = (Vec::new(), Vec::new());
        if self.audio_state != AudioConnectionState::Disconnected {
            // Already negotiating, connecting or up. A second trigger — the
            // HF pressing answer while the AG is already opening audio for
            // the ring — must not start a second procedure.
            return (outgoing, events);
        }
        match self.codec_to_negotiate() {
            Some(codec) => {
                self.active_codec = codec;
                self.set_audio_state(AudioConnectionState::Negotiating, &mut events);
                outgoing.push(self.negotiate_codec(codec));
            }
            None => {
                self.active_codec = AudioCodec::Cvsd;
                self.set_audio_state(AudioConnectionState::Connecting, &mut events);
                events.push(HfpEvent::AudioConnectionRequested(AudioCodec::Cvsd));
            }
        }
        (outgoing, events)
    }

    /// The transport says the synchronous link is up.
    pub fn on_audio_connected(&mut self) -> Vec<HfpEvent> {
        let mut events = Vec::new();
        self.set_audio_state(AudioConnectionState::Connected, &mut events);
        events
    }

    /// The transport says the synchronous link is gone — because the audio
    /// was hung up, or because the ACL under it went away.
    pub fn on_audio_disconnected(&mut self) -> Vec<HfpEvent> {
        let mut events = Vec::new();
        self.set_audio_state(AudioConnectionState::Disconnected, &mut events);
        events
    }

    /// The best codec both ends support, or `None` when the Codec
    /// Connection procedure does not apply — either end lacking the Codec
    /// Negotiation feature is enough, and then CVSD is used without any
    /// `+BCS` exchange at all.
    fn codec_to_negotiate(&self) -> Option<AudioCodec> {
        if !self.supports_ag_feature(ag_feature::CODEC_NEGOTIATION)
            || !self.supports_hf_feature(hf_feature::CODEC_NEGOTIATION)
        {
            return None;
        }
        // Widest first: LC3-SWB, then mSBC, then narrowband CVSD. The AG
        // picks, but only from what the HF said it can decode — offering a
        // codec the far end never listed is how a call comes up silent.
        [AudioCodec::Lc3Swb, AudioCodec::Msbc, AudioCodec::Cvsd]
            .into_iter()
            .find(|codec| {
                self.supported_audio_codecs.contains(codec) && self.hf_audio_codecs.contains(codec)
            })
    }

    fn set_audio_state(&mut self, state: AudioConnectionState, events: &mut Vec<HfpEvent>) {
        if self.audio_state == state {
            return;
        }
        self.audio_state = state;
        events.push(HfpEvent::AudioConnectionState(state));
    }

    /// Emits a RING alert for an incoming call.
    pub fn send_ring(&self) -> Vec<u8> {
        response_bytes("RING")
    }

    /// Reports a speaker volume change (+VGS).
    pub fn set_speaker_volume(&mut self, level: u8) -> Vec<u8> {
        response_bytes(&format!("+VGS: {level}"))
    }

    /// Reports a microphone volume change (+VGM).
    pub fn set_microphone_volume(&mut self, level: u8) -> Vec<u8> {
        response_bytes(&format!("+VGM: {level}"))
    }

    /// Enables or disables in-band ring tones.
    pub fn set_inband_ringtone_enabled(&mut self, enabled: bool) -> Vec<u8> {
        self.inband_ringtone_enabled = enabled;
        response_bytes(&format!("+BSIR: {}", enabled as u8))
    }

    /// Updates `indicator`'s status and returns the `+CIEV: ...` line to
    /// send; fails if `indicator` isn't one of this AG's configured indicators.
    pub fn update_ag_indicator(
        &mut self,
        indicator: AgIndicator,
        value: u32,
    ) -> Result<Vec<u8>, SimbleError> {
        let index = self
            .ag_indicators
            .iter()
            .position(|s| s.indicator == indicator)
            .ok_or_else(|| {
                SimbleError::DeviceError(format!("HFP: AG indicator {indicator:?} not supported"))
            })?;
        self.ag_indicators[index].current_status = value;
        Ok(response_bytes(&format!("+CIEV: {},{value}", index + 1)))
    }

    /// Sends a caller-line-identification notification (+CLIP).
    pub fn send_cli_notification(&mut self, cli: &CallLineIdentification) -> Vec<u8> {
        response_bytes(&format!("+CLIP: {}", cli.to_clip_string()))
    }

    /// Sends a raw response line to the HF.
    pub fn send_response(&self, text: &str) -> Vec<u8> {
        response_bytes(text)
    }
}

// ---------------------------------------------------------------------------
// SDP records
// ---------------------------------------------------------------------------

const SDP_SUPPORTED_FEATURES_ATTRIBUTE_ID: u16 = 0x0311;

const BT_HANDSFREE_SERVICE: SdpUuid = SdpUuid::Uuid16(0x111E);
const BT_HANDSFREE_AUDIO_GATEWAY_SERVICE: SdpUuid = SdpUuid::Uuid16(0x111F);
const BT_GENERIC_AUDIO_SERVICE: SdpUuid = SdpUuid::Uuid16(0x1203);

/// HFP profile version advertised in SDP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(non_camel_case_types)]
pub enum ProfileVersion {
    /// HFP v1.5.
    V1_5,
    /// HFP v1.6.
    V1_6,
    /// HFP v1.7.
    V1_7,
    /// HFP v1.8.
    V1_8,
    /// HFP v1.9.
    V1_9,
}

impl ProfileVersion {
    /// Returns the SDP profile-version value.
    pub(crate) fn to_value(self) -> u16 {
        match self {
            Self::V1_5 => 0x0105,
            Self::V1_6 => 0x0106,
            Self::V1_7 => 0x0107,
            Self::V1_8 => 0x0108,
            Self::V1_9 => 0x0109,
        }
    }

    /// Parses an SDP profile-version value.
    pub(crate) fn from_value(value: u16) -> Option<Self> {
        match value {
            0x0105 => Some(Self::V1_5),
            0x0106 => Some(Self::V1_6),
            0x0107 => Some(Self::V1_7),
            0x0108 => Some(Self::V1_8),
            0x0109 => Some(Self::V1_9),
            _ => None,
        }
    }
}

/// HF-side SDP `SupportedFeatures` bitmask (distinct from the AT+BRSF
/// runtime bitmask in [`hf_feature`] — see HFP v1.9 6.3).
pub mod hf_sdp_feature {
    /// EC/NR function.
    pub(crate) const EC_NR: u16 = 0x01;
    /// Three-way calling.
    pub const THREE_WAY_CALLING: u16 = 0x02;
    /// CLI presentation capability.
    pub const CLI_PRESENTATION_CAPABILITY: u16 = 0x04;
    /// Voice recognition activation.
    pub(crate) const VOICE_RECOGNITION_ACTIVATION: u16 = 0x08;
    /// Remote volume control.
    pub(crate) const REMOTE_VOLUME_CONTROL: u16 = 0x10;
    /// Wide-band speech.
    pub const WIDE_BAND_SPEECH: u16 = 0x20;
    /// Enhanced voice recognition status.
    pub(crate) const ENHANCED_VOICE_RECOGNITION_STATUS: u16 = 0x40;
    /// Voice recognition text.
    pub(crate) const VOICE_RECOGNITION_TEXT: u16 = 0x80;
    /// Super-wide-band speech.
    pub const SUPER_WIDE_BAND: u16 = 0x100;
}

/// AG-side SDP `SupportedFeatures` bitmask (distinct from the AT+BRSF
/// runtime bitmask in [`ag_feature`] — see HFP v1.9 6.3).
pub mod ag_sdp_feature {
    /// Three-way calling.
    pub const THREE_WAY_CALLING: u16 = 0x01;
    /// EC/NR function.
    pub(crate) const EC_NR: u16 = 0x02;
    /// Voice recognition function.
    pub(crate) const VOICE_RECOGNITION_FUNCTION: u16 = 0x04;
    /// In-band ring tone capability.
    pub const IN_BAND_RING_TONE_CAPABILITY: u16 = 0x08;
    /// Attach a number to a voice tag.
    pub const VOICE_TAG: u16 = 0x10;
    /// Wide-band speech.
    pub const WIDE_BAND_SPEECH: u16 = 0x20;
    /// Enhanced voice recognition status.
    pub(crate) const ENHANCED_VOICE_RECOGNITION_STATUS: u16 = 0x40;
    /// Voice recognition text.
    pub(crate) const VOICE_RECOGNITION_TEXT: u16 = 0x80;
    /// Super-wide-band speech.
    pub const SUPER_WIDE_BAND_SPEED_SPEECH: u16 = 0x100;
}

/// Builds the SDP service record for HFP Hands-Free support.
pub fn make_hf_sdp_records(
    service_record_handle: u32,
    rfcomm_channel: u8,
    configuration: &HfConfiguration,
    version: ProfileVersion,
) -> Vec<ServiceAttribute> {
    let mut features: u16 = 0;
    let bits = [
        (hf_feature::EC_NR, hf_sdp_feature::EC_NR),
        (
            hf_feature::THREE_WAY_CALLING,
            hf_sdp_feature::THREE_WAY_CALLING,
        ),
        (
            hf_feature::CLI_PRESENTATION_CAPABILITY,
            hf_sdp_feature::CLI_PRESENTATION_CAPABILITY,
        ),
        (
            hf_feature::VOICE_RECOGNITION_ACTIVATION,
            hf_sdp_feature::VOICE_RECOGNITION_ACTIVATION,
        ),
        (
            hf_feature::REMOTE_VOLUME_CONTROL,
            hf_sdp_feature::REMOTE_VOLUME_CONTROL,
        ),
        (
            hf_feature::ENHANCED_VOICE_RECOGNITION_STATUS,
            hf_sdp_feature::ENHANCED_VOICE_RECOGNITION_STATUS,
        ),
        (
            hf_feature::VOICE_RECOGNITION_TEXT,
            hf_sdp_feature::VOICE_RECOGNITION_TEXT,
        ),
    ];
    for (runtime_bit, sdp_bit) in bits {
        if configuration.supported_hf_features & runtime_bit != 0 {
            features |= sdp_bit;
        }
    }
    if configuration
        .supported_audio_codecs
        .contains(&AudioCodec::Msbc)
    {
        features |= hf_sdp_feature::WIDE_BAND_SPEECH;
    }

    vec![
        ServiceAttribute::new(
            attribute_id::SERVICE_RECORD_HANDLE,
            DataElement::unsigned_integer_32(service_record_handle),
        ),
        ServiceAttribute::new(
            attribute_id::SERVICE_CLASS_ID_LIST,
            DataElement::sequence(vec![
                DataElement::uuid(BT_HANDSFREE_SERVICE),
                DataElement::uuid(BT_GENERIC_AUDIO_SERVICE),
            ]),
        ),
        ServiceAttribute::new(
            attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID)]),
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_RFCOMM_PROTOCOL_ID),
                    DataElement::unsigned_integer_8(rfcomm_channel),
                ]),
            ]),
        ),
        ServiceAttribute::new(
            attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
            DataElement::sequence(vec![DataElement::sequence(vec![
                DataElement::uuid(BT_HANDSFREE_SERVICE),
                DataElement::unsigned_integer_16(version.to_value()),
            ])]),
        ),
        ServiceAttribute::new(
            SDP_SUPPORTED_FEATURES_ATTRIBUTE_ID,
            DataElement::unsigned_integer_16(features),
        ),
    ]
}

/// Builds the SDP service record for HFP Audio-Gateway support.
pub fn make_ag_sdp_records(
    service_record_handle: u32,
    rfcomm_channel: u8,
    configuration: &AgConfiguration,
    version: ProfileVersion,
) -> Vec<ServiceAttribute> {
    let mut features: u16 = 0;
    let bits = [
        (ag_feature::EC_NR, ag_sdp_feature::EC_NR),
        (
            ag_feature::THREE_WAY_CALLING,
            ag_sdp_feature::THREE_WAY_CALLING,
        ),
        (
            ag_feature::ENHANCED_VOICE_RECOGNITION_STATUS,
            ag_sdp_feature::ENHANCED_VOICE_RECOGNITION_STATUS,
        ),
        (
            ag_feature::VOICE_RECOGNITION_TEXT,
            ag_sdp_feature::VOICE_RECOGNITION_TEXT,
        ),
        (
            ag_feature::IN_BAND_RING_TONE_CAPABILITY,
            ag_sdp_feature::IN_BAND_RING_TONE_CAPABILITY,
        ),
        (
            ag_feature::VOICE_RECOGNITION_FUNCTION,
            ag_sdp_feature::VOICE_RECOGNITION_FUNCTION,
        ),
    ];
    for (runtime_bit, sdp_bit) in bits {
        if configuration.supported_ag_features & runtime_bit != 0 {
            features |= sdp_bit;
        }
    }
    if configuration
        .supported_audio_codecs
        .contains(&AudioCodec::Msbc)
    {
        features |= ag_sdp_feature::WIDE_BAND_SPEECH;
    }

    vec![
        ServiceAttribute::new(
            attribute_id::SERVICE_RECORD_HANDLE,
            DataElement::unsigned_integer_32(service_record_handle),
        ),
        ServiceAttribute::new(
            attribute_id::SERVICE_CLASS_ID_LIST,
            DataElement::sequence(vec![
                DataElement::uuid(BT_HANDSFREE_AUDIO_GATEWAY_SERVICE),
                DataElement::uuid(BT_GENERIC_AUDIO_SERVICE),
            ]),
        ),
        ServiceAttribute::new(
            attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID)]),
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_RFCOMM_PROTOCOL_ID),
                    DataElement::unsigned_integer_8(rfcomm_channel),
                ]),
            ]),
        ),
        ServiceAttribute::new(
            attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
            DataElement::sequence(vec![DataElement::sequence(vec![
                DataElement::uuid(BT_HANDSFREE_AUDIO_GATEWAY_SERVICE),
                DataElement::unsigned_integer_16(version.to_value()),
            ])]),
        ),
        ServiceAttribute::new(
            SDP_SUPPORTED_FEATURES_ATTRIBUTE_ID,
            DataElement::unsigned_integer_16(features),
        ),
    ]
}

fn protocol_descriptor_channel(value: &DataElement) -> Option<u8> {
    let (channel, _) = value
        .as_sequence()?
        .get(1)?
        .as_sequence()?
        .get(1)?
        .as_unsigned_integer()?;
    Some(channel as u8)
}

fn profile_descriptor_version(value: &DataElement) -> Option<u16> {
    let (version, _) = value
        .as_sequence()?
        .first()?
        .as_sequence()?
        .get(1)?
        .as_unsigned_integer()?;
    Some(version as u16)
}

/// Searches for a Hands-Free SDP record on the peer reachable via
/// `transport`, returning `(rfcomm channel, profile version, SDP features)`.
pub fn find_hf_sdp_record(
    sdp_client: &mut SdpClient,
    mut transport: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<Option<(u8, ProfileVersion, u16)>, SimbleError> {
    let results = sdp_client.search_attributes(
        &[BT_HANDSFREE_SERVICE],
        &[
            AttributeIdSpec::Single(attribute_id::PROTOCOL_DESCRIPTOR_LIST),
            AttributeIdSpec::Single(attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST),
            AttributeIdSpec::Single(SDP_SUPPORTED_FEATURES_ATTRIBUTE_ID),
            AttributeIdSpec::Single(attribute_id::SERVICE_CLASS_ID_LIST),
        ],
        &mut transport,
    )?;

    for attrs in &results {
        let mut channel = None;
        let mut version = None;
        let mut features = None;
        let mut is_ag_record = false;
        for attr in attrs {
            match attr.id {
                attribute_id::PROTOCOL_DESCRIPTOR_LIST => {
                    channel = protocol_descriptor_channel(&attr.value)
                }
                attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST => {
                    version = profile_descriptor_version(&attr.value)
                        .and_then(ProfileVersion::from_value);
                }
                SDP_SUPPORTED_FEATURES_ATTRIBUTE_ID => {
                    if let Some((value, _)) = attr.value.as_unsigned_integer() {
                        features = Some(value as u16);
                    }
                }
                attribute_id::SERVICE_CLASS_ID_LIST
                    if attr
                        .value
                        .as_sequence()
                        .and_then(|l| l.first())
                        .and_then(DataElement::as_uuid)
                        == Some(BT_HANDSFREE_AUDIO_GATEWAY_SERVICE) =>
                {
                    is_ag_record = true;
                }
                _ => {}
            }
        }
        if is_ag_record {
            continue;
        }
        if let (Some(channel), Some(version), Some(features)) = (channel, version, features) {
            return Ok(Some((channel, version, features)));
        }
    }
    Ok(None)
}

/// Searches for an Audio-Gateway SDP record on the peer reachable via
/// `transport`, returning `(rfcomm channel, profile version, SDP features)`.
pub fn find_ag_sdp_record(
    sdp_client: &mut SdpClient,
    mut transport: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<Option<(u8, ProfileVersion, u16)>, SimbleError> {
    let results = sdp_client.search_attributes(
        &[BT_HANDSFREE_AUDIO_GATEWAY_SERVICE],
        &[
            AttributeIdSpec::Single(attribute_id::PROTOCOL_DESCRIPTOR_LIST),
            AttributeIdSpec::Single(attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST),
            AttributeIdSpec::Single(SDP_SUPPORTED_FEATURES_ATTRIBUTE_ID),
        ],
        &mut transport,
    )?;

    for attrs in &results {
        let mut channel = None;
        let mut version = None;
        let mut features = None;
        for attr in attrs {
            match attr.id {
                attribute_id::PROTOCOL_DESCRIPTOR_LIST => {
                    channel = protocol_descriptor_channel(&attr.value)
                }
                attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST => {
                    version = profile_descriptor_version(&attr.value)
                        .and_then(ProfileVersion::from_value);
                }
                SDP_SUPPORTED_FEATURES_ATTRIBUTE_ID => {
                    if let Some((value, _)) = attr.value.as_unsigned_integer() {
                        features = Some(value as u16);
                    }
                }
                _ => {}
            }
        }
        if let (Some(channel), Some(version), Some(features)) = (channel, version, features) {
            return Ok(Some((channel, version, features)));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_response_lines_batched() {
        let mut buffer = b"\r\n+BIND: (1,2)\r\n\r\nOK\r\n".to_vec();
        let lines = extract_response_lines(&mut buffer);
        assert_eq!(lines, vec![b"+BIND: (1,2)".to_vec(), b"OK".to_vec()]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_extract_response_lines_partial_kept() {
        let mut buffer = b"\r\nOK\r\n\r\n+CIEV".to_vec();
        let lines = extract_response_lines(&mut buffer);
        assert_eq!(lines, vec![b"OK".to_vec()]);
        assert_eq!(buffer, b"\r\n+CIEV".to_vec());
    }

    #[test]
    fn test_extract_command_lines_batched() {
        let mut buffer = b"ATA\rAT+CHUP\r".to_vec();
        let lines = extract_command_lines(&mut buffer);
        assert_eq!(lines, vec![b"ATA".to_vec(), b"AT+CHUP".to_vec()]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn test_ag_indicator_state_on_test_text_contiguous_and_sparse() {
        assert_eq!(AgIndicatorState::call().on_test_text(), "(\"call\",(0-1))");
        let sparse = AgIndicatorState {
            indicator: AgIndicator::Signal,
            supported_values: [0u32, 2, 4].into_iter().collect(),
            current_status: 0,
            enabled: true,
        };
        assert_eq!(sparse.on_test_text(), "(\"signal\",(0,2,4))");
    }

    #[test]
    fn test_parse_one_cind_indicator_round_trip() {
        let state = AgIndicatorState::callsetup();
        let text = state.on_test_text();
        let params =
            crate::classic::at::parse_parameters(text.as_bytes()).expect("valid parameters");
        let parsed = parse_one_cind_indicator(&params[0]).expect("parses");
        assert_eq!(parsed.indicator, AgIndicator::CallSetup);
        assert_eq!(parsed.supported_values, state.supported_values);
    }

    #[test]
    fn test_slc_end_to_end_minimal_features() {
        let mut hf = HfProtocol::new(HfConfiguration {
            supported_hf_features: 0,
            supported_hf_indicators: Vec::new(),
            supported_audio_codecs: Vec::new(),
        });
        let mut ag = AgProtocol::new(AgConfiguration {
            supported_ag_features: 0,
            supported_ag_indicators: vec![AgIndicatorState::call()],
            supported_hf_indicators: Vec::new(),
            supported_ag_call_hold_operations: Vec::new(),
            supported_audio_codecs: Vec::new(),
        });

        let mut to_ag = vec![hf.start_slc()];
        let mut slc_complete = false;
        for _ in 0..10 {
            let mut to_hf = Vec::new();
            for line in to_ag.drain(..) {
                let (out, _) = ag.receive(&line);
                to_hf.extend(out);
            }
            let mut next_to_ag = Vec::new();
            for line in to_hf {
                let (out, events) = hf.receive(&line);
                next_to_ag.extend(out);
                if events.contains(&HfpEvent::SlcComplete) {
                    slc_complete = true;
                }
            }
            to_ag = next_to_ag;
            if slc_complete && to_ag.is_empty() {
                break;
            }
        }
        assert!(slc_complete);
        assert!(hf.slc_initialized);
        assert_eq!(hf.ag_indicators.len(), 1);
        assert_eq!(hf.ag_indicators[0].indicator, AgIndicator::Call);
    }
}
