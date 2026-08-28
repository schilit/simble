// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The two halves of Auracast, as Android profile proxies:
//! `android::BluetoothLeBroadcast` (the source) and
//! `android::BluetoothLeBroadcastAssistant` (the phone).
//!
//! A script can build any GATT database by hand, so a heart-rate monitor is
//! already fully expressible. A profile with *behaviour* is not: the protocol
//! lives in Rust, in [`crate::device::BigBroadcaster`] and
//! [`crate::profiles::BroadcastAudioScanService`], and until now a script
//! could not reach either. These bindings compose those; they reimplement
//! nothing.
//!
//! ```rhai
//! let broadcast = android::BluetoothLeBroadcast("SimBLE Auracast");
//! broadcast.start_broadcast(#{
//!     broadcast_id: 0xC0FFEE,
//!     broadcast_code: (),                  // () = unencrypted
//!     subgroups: [ #{
//!         codec: "lc3_48_2",
//!         bis: ["FRONT_LEFT", "FRONT_RIGHT"],
//!     } ],
//! });
//!
//! fn on_broadcast_started(broadcast, reason, broadcast_id) { … }
//! fn on_playback_started(broadcast, reason, broadcast_id) { … }
//! ```
//!
//! ```rhai
//! let assistant = android::BluetoothLeBroadcastAssistant("Phone");
//! assistant.add_source("AA:BB:CC:00:00:01", metadata, false);
//!
//! fn on_receive_state_changed(assistant, sink, source_id, state) {
//!     assert(state.pa_sync_state == "SynchronizedToPa", "the earbud joined");
//! }
//! ```
//!
//! **Callbacks are free functions with the object prepended**, the convention
//! `on_services_discovered(client)` already set — Rhai closures cannot live
//! inside a callback object here, for the reason given in the
//! [`crate::scripting`] module docs.
//!
//! **No HCI type crosses into Rhai.** The source binding owns the whole
//! advertising/BIG ladder and hands the script `broadcast.state` and a
//! `BluetoothLeBroadcastMetadata`-shaped map; the Assistant owns the BASS
//! control-point encoding and hands the script a Broadcast Receive State map.
//! That is what a profile proxy *is*.
//!
//! # Where Android has no equivalent, and where we have no Android
//!
//! * **`server.add_bass(n)`** registers the *Scan Delegator* — the earbud
//!   side. Android is always the Assistant and ships no proxy for being a
//!   Delegator, so this keeps the `add_pacs`/`add_ascs`/`add_ras` registrar
//!   spelling rather than inventing an Android class name for it.
//! * **`server.report_sync_outcome(...)`** is how something with a radio tells
//!   the Delegator what a synchronisation attempt actually achieved. On a real
//!   earbud that is the controller; here the script stands in for it. Android
//!   has nothing like it because Android is never the Delegator.
//! * **`start_searching_for_sources()`** does only the half of Android's method
//!   that is reachable: it writes BASS Remote Scan Started to the connected
//!   sink. Android also scans for Broadcast Audio Announcements and reports
//!   them through `onSourceFound`; [`crate::device::central::LeCentral`] has no
//!   scan surface of its own (it scans only to learn a connect target's address
//!   type), and `parse_scan_reports` decodes only *legacy* advertising reports
//!   while an Auracast source advertises with an extended set. So no
//!   `on_source_found` is delivered, and a script gets its metadata from
//!   `broadcast.get_all_broadcast_metadata()` instead.
//! * **`is_group_op`** is accepted and *not honoured*: applying an operation to
//!   a sink's whole coordinated set needs a CSIP set coordinator, and nothing
//!   wires [`crate::profiles::csip`] to this. The operation reaches the one
//!   connected sink either way.
//! * **`reason`** in Android's callbacks is a `BluetoothStatusCodes` value.
//!   There is no mapping from an HCI status or an ATT error to one, so these
//!   carry the controller's or the peer's own status byte — `0` on success —
//!   rather than an invented constant.
//! * **One Broadcast Receive State slot.** A Delegator may publish several
//!   characteristics with the same UUID, one per source slot, and
//!   [`LeCentral`](crate::device::central::LeCentral) resolves a characteristic
//!   by UUID and takes the first match — so this Assistant reads and subscribes
//!   to slot 0 and never sees the rest. Reaching a second slot needs
//!   handle-addressed operations on the central, which do not exist. BASS
//!   mandates only one slot, so this is a limit rather than a bug, but it is a
//!   real one: `get_all_sources` reports what the Assistant has *seen*.
//! * **One sink per Assistant**, because the central holds one link at a time.
//!   Android's proxy addresses every connected sink.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;

use rhai::{AST, Array, Dynamic, Engine, EvalAltResult, Map, Module};

use crate::device::big_broadcaster::{BigBroadcaster, BroadcastConfig, BroadcastState};
use crate::device::central::CentralEvent;
use crate::profiles::bap::{FrameDuration, SamplingFrequency, audio_location};
use crate::profiles::bass::{
    ANY_BIS, BigEncryption, BroadcastAudioScanService, BroadcastReceiveState,
    ControlPointOperation, PeriodicAdvertisingSyncParams, PeriodicAdvertisingSyncState,
    SubgroupInfo, bass_uuid,
};
use crate::scripting::bindings::{ScriptGattServer, dynamic_to_bytes, runtime_error};
use crate::scripting::client::ScriptGattClient;
use crate::types::Address;

// ---------------------------------------------------------------------------
// Vocabulary shared by both halves
// ---------------------------------------------------------------------------

/// The audio locations a script may name as strings, mapped to the *same*
/// `bap::audio_location` constants `audio::location::*` exposes. Names, not
/// values — nothing here re-declares a bitmask.
const LOCATION_NAMES: &[(&str, u32)] = &[
    ("NOT_ALLOWED", audio_location::NOT_ALLOWED),
    ("FRONT_LEFT", audio_location::FRONT_LEFT),
    ("FRONT_RIGHT", audio_location::FRONT_RIGHT),
    ("FRONT_CENTER", audio_location::FRONT_CENTER),
    (
        "LOW_FREQUENCY_EFFECTS_1",
        audio_location::LOW_FREQUENCY_EFFECTS_1,
    ),
    ("BACK_LEFT", audio_location::BACK_LEFT),
    ("BACK_RIGHT", audio_location::BACK_RIGHT),
    ("SIDE_LEFT", audio_location::SIDE_LEFT),
    ("SIDE_RIGHT", audio_location::SIDE_RIGHT),
    ("TOP_CENTER", audio_location::TOP_CENTER),
    ("STEREO", audio_location::STEREO),
];

/// One audio location, named or numeric.
fn audio_location_of(value: &Dynamic) -> Result<u32, Box<EvalAltResult>> {
    if let Ok(n) = value.as_int() {
        return u32::try_from(n)
            .map_err(|_| runtime_error(format!("bis: {n} is not an audio location bitmask")));
    }
    let name = value.clone().into_string().map_err(|actual| {
        runtime_error(format!(
            "bis: expected an audio::location::* constant or its name, got {actual}"
        ))
    })?;
    LOCATION_NAMES
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, bits)| *bits)
        .ok_or_else(|| {
            runtime_error(format!(
                "bis: {name:?} is not an audio location; known names: {}",
                LOCATION_NAMES
                    .iter()
                    .map(|(n, _)| *n)
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })
}

/// The BAP codec configuration a subgroup is published with.
///
/// BAP Table 4.2 defines sixteen named LC3 settings (`8_1` … `48_6`). Exactly
/// one of them is named here — `lc3_48_2` — because it is the only one whose
/// BASE this project has checked byte-for-byte against a foreign stack
/// (`big_broadcaster::tests::test_base_matches_bumble`, and the two
/// `tests/interop/auracast_*.py` runs). Transcribing the other fifteen from
/// memory is exactly how this project shipped four invented UUIDs, so anything
/// else is spelled out:
///
/// ```rhai
/// codec: #{ sampling_frequency_hz: 24000, frame_duration_us: 10000,
///           octets_per_frame: 60 }
/// ```
struct CodecSettings {
    sampling_frequency: SamplingFrequency,
    frame_duration: FrameDuration,
    octets_per_codec_frame: u16,
}

impl CodecSettings {
    /// The verified default: 48 kHz, 10 ms frames, 100 octets — BAP's `48_2`,
    /// and what [`BroadcastConfig::default`] already carries.
    fn lc3_48_2() -> Self {
        let default = BroadcastConfig::default();
        Self {
            sampling_frequency: default.sampling_frequency,
            frame_duration: default.frame_duration,
            octets_per_codec_frame: default.octets_per_codec_frame,
        }
    }

    fn parse(value: &Dynamic) -> Result<Self, Box<EvalAltResult>> {
        if value.is_unit() {
            return Ok(Self::lc3_48_2());
        }
        if let Some(name) = value.clone().try_cast::<rhai::ImmutableString>() {
            return match name.as_str() {
                "lc3_48_2" => Ok(Self::lc3_48_2()),
                other => Err(runtime_error(format!(
                    "codec: {other:?} is not a named configuration. Only \"lc3_48_2\" is named, \
                     because it is the only one whose BASE has been checked against a foreign \
                     stack; spell any other out as \
                     #{{sampling_frequency_hz, frame_duration_us, octets_per_frame}}"
                ))),
            };
        }
        let map = value.clone().try_cast::<Map>().ok_or_else(|| {
            runtime_error("codec: expected \"lc3_48_2\" or a codec configuration map")
        })?;
        let hz = int_field(&map, "sampling_frequency_hz")?
            .ok_or_else(|| runtime_error("codec: sampling_frequency_hz is required"))?;
        let us = int_field(&map, "frame_duration_us")?
            .ok_or_else(|| runtime_error("codec: frame_duration_us is required"))?;
        let octets = int_field(&map, "octets_per_frame")?
            .ok_or_else(|| runtime_error("codec: octets_per_frame is required"))?;
        Ok(Self {
            sampling_frequency: sampling_frequency_of(hz)?,
            frame_duration: frame_duration_of(us)?,
            octets_per_codec_frame: u16::try_from(octets).map_err(|_| {
                runtime_error(format!("codec: octets_per_frame out of range: {octets}"))
            })?,
        })
    }

    /// SDUs carry one codec frame, so the SDU interval is the frame duration
    /// and the largest SDU is the frame — the single-frame-per-SDU shape
    /// [`BroadcastConfig`] documents.
    fn sdu_interval_us(&self) -> u32 {
        match self.frame_duration {
            FrameDuration::Duration7500Us => 7_500,
            FrameDuration::Duration10000Us => 10_000,
        }
    }
}

fn sampling_frequency_of(hz: i64) -> Result<SamplingFrequency, Box<EvalAltResult>> {
    Ok(match hz {
        8_000 => SamplingFrequency::Freq8000,
        11_025 => SamplingFrequency::Freq11025,
        16_000 => SamplingFrequency::Freq16000,
        22_050 => SamplingFrequency::Freq22050,
        24_000 => SamplingFrequency::Freq24000,
        32_000 => SamplingFrequency::Freq32000,
        44_100 => SamplingFrequency::Freq44100,
        48_000 => SamplingFrequency::Freq48000,
        88_200 => SamplingFrequency::Freq88200,
        96_000 => SamplingFrequency::Freq96000,
        176_400 => SamplingFrequency::Freq176400,
        192_000 => SamplingFrequency::Freq192000,
        384_000 => SamplingFrequency::Freq384000,
        other => {
            return Err(runtime_error(format!(
                "codec: {other} Hz is not an assigned LC3 sampling frequency"
            )));
        }
    })
}

fn frame_duration_of(us: i64) -> Result<FrameDuration, Box<EvalAltResult>> {
    Ok(match us {
        7_500 => FrameDuration::Duration7500Us,
        10_000 => FrameDuration::Duration10000Us,
        other => {
            return Err(runtime_error(format!(
                "codec: {other} us is not an assigned LC3 frame duration (7500 or 10000)"
            )));
        }
    })
}

/// Reads an optional integer field, rejecting a present-but-wrong-typed one
/// rather than silently defaulting.
fn int_field(map: &Map, key: &str) -> Result<Option<i64>, Box<EvalAltResult>> {
    match map.get(key) {
        None => Ok(None),
        Some(value) if value.is_unit() => Ok(None),
        Some(value) => value
            .as_int()
            .map(Some)
            .map_err(|actual| runtime_error(format!("{key}: expected an integer, got {actual}"))),
    }
}

fn string_field(map: &Map, key: &str) -> Result<Option<String>, Box<EvalAltResult>> {
    match map.get(key) {
        None => Ok(None),
        Some(value) if value.is_unit() => Ok(None),
        Some(value) => value
            .clone()
            .into_string()
            .map(Some)
            .map_err(|actual| runtime_error(format!("{key}: expected a string, got {actual}"))),
    }
}

fn array_field(map: &Map, key: &str) -> Result<Array, Box<EvalAltResult>> {
    match map.get(key) {
        None => Ok(Array::new()),
        Some(value) if value.is_unit() => Ok(Array::new()),
        Some(value) => value
            .clone()
            .try_cast::<Array>()
            .ok_or_else(|| runtime_error(format!("{key}: expected an array"))),
    }
}

/// The name of a PA_Sync_State, matching the Rust variant so a script reads
/// `state.pa_sync_state == "SynchronizedToPa"`.
fn pa_sync_state_name(state: PeriodicAdvertisingSyncState) -> &'static str {
    match state {
        PeriodicAdvertisingSyncState::NotSynchronizedToPa => "NotSynchronizedToPa",
        PeriodicAdvertisingSyncState::SyncInfoRequest => "SyncInfoRequest",
        PeriodicAdvertisingSyncState::SynchronizedToPa => "SynchronizedToPa",
        PeriodicAdvertisingSyncState::FailedToSynchronizeToPa => "FailedToSynchronizeToPa",
        PeriodicAdvertisingSyncState::NoPast => "NoPast",
    }
}

/// The inverse of [`pa_sync_state_name`], for `report_sync_outcome`.
fn pa_sync_state_of(name: &str) -> Option<PeriodicAdvertisingSyncState> {
    Some(match name {
        "NotSynchronizedToPa" => PeriodicAdvertisingSyncState::NotSynchronizedToPa,
        "SyncInfoRequest" => PeriodicAdvertisingSyncState::SyncInfoRequest,
        "SynchronizedToPa" => PeriodicAdvertisingSyncState::SynchronizedToPa,
        "FailedToSynchronizeToPa" => PeriodicAdvertisingSyncState::FailedToSynchronizeToPa,
        "NoPast" => PeriodicAdvertisingSyncState::NoPast,
        _ => return None,
    })
}

fn big_encryption_name(encryption: BigEncryption) -> &'static str {
    match encryption {
        BigEncryption::NotEncrypted => "NotEncrypted",
        BigEncryption::BroadcastCodeRequired => "BroadcastCodeRequired",
        BigEncryption::Decrypting => "Decrypting",
        BigEncryption::BadCode => "BadCode",
    }
}

/// A Broadcast Receive State as the script sees it — Android's
/// `BluetoothLeBroadcastReceiveState`, snake-cased.
fn receive_state_map(state: &BroadcastReceiveState) -> Map {
    let mut map = Map::new();
    map.insert("source_id".into(), (state.source_id as i64).into());
    map.insert(
        "source_device".into(),
        state.source_address.to_string().into(),
    );
    map.insert(
        "source_address_type".into(),
        (state.source_address_type as i64).into(),
    );
    map.insert(
        "source_advertising_sid".into(),
        (state.source_adv_sid as i64).into(),
    );
    map.insert("broadcast_id".into(), (state.broadcast_id as i64).into());
    map.insert(
        "pa_sync_state".into(),
        pa_sync_state_name(state.pa_sync_state).into(),
    );
    map.insert(
        "big_encryption".into(),
        big_encryption_name(state.big_encryption).into(),
    );
    let subgroups: Array = state
        .subgroups
        .iter()
        .map(|subgroup| {
            let mut entry = Map::new();
            entry.insert("bis_sync".into(), (subgroup.bis_sync as i64).into());
            entry.insert(
                "metadata".into(),
                Dynamic::from_blob(subgroup.metadata.clone()),
            );
            entry
        })
        .map(Dynamic::from_map)
        .collect();
    map.insert("subgroups".into(), subgroups.into());
    map
}

// ---------------------------------------------------------------------------
// android::BluetoothLeBroadcast — the source
// ---------------------------------------------------------------------------

/// One callback the source owes the script, already resolved to its name and
/// arguments (the object itself is prepended at dispatch).
type Callback = (&'static str, Vec<Dynamic>);

/// Script-side handle to an Auracast broadcast source. `Rc<RefCell>` for the
/// same reason [`ScriptGattClient`] is: Rhai requires `Clone`, a live state
/// machine must not be, so every copy a script holds drives the same source.
#[derive(Clone)]
pub struct ScriptBroadcastSource {
    inner: Rc<RefCell<SourceInner>>,
}

struct SourceInner {
    name: String,
    /// The address the host put this device on the air with. A
    /// [`BigBroadcaster`] is transport-free and has no idea what its own
    /// address is, but a `BluetoothLeBroadcastMetadata` is addressed by it —
    /// so the host stamps it here, exactly as `ScriptedPeripheral` stamps a
    /// server's identity.
    address: Address,
    broadcaster: Option<BigBroadcaster>,
    /// Where the broadcaster's state was when it was last sampled, so a
    /// transition can be turned into a callback exactly once.
    last_state: Option<BroadcastState>,
    /// True once the broadcast has ever been on the air: it separates "never
    /// started" (a failure is `onBroadcastStartFailed`) from "was running"
    /// (a failure is `onBroadcastStopped`).
    ever_created: bool,
    /// H4 packets a script call has queued that the host has not sent yet.
    outbox: Vec<Vec<u8>>,
    /// Callbacks waiting to be dispatched.
    pending: Vec<Callback>,
}

impl ScriptBroadcastSource {
    fn create(name: &str) -> Self {
        Self {
            inner: Rc::new(RefCell::new(SourceInner {
                name: name.to_string(),
                address: Address::ANY,
                broadcaster: None,
                last_state: None,
                ever_created: false,
                outbox: Vec::new(),
                pending: Vec::new(),
            })),
        }
    }

    /// The source's name, which is also the Broadcast Name it publishes.
    pub fn name(&self) -> String {
        self.inner.borrow().name.clone()
    }

    /// Stamps the on-air identity. The script cannot know it — the host
    /// allocates addresses — and a metadata record is addressed by it.
    pub fn set_address(&self, address: Address) {
        self.inner.borrow_mut().address = address;
    }

    /// Drains the packets script calls have queued for the controller.
    pub fn take_outbox(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.inner.borrow_mut().outbox)
    }

    /// Feeds one controller→host packet to the broadcaster and returns what to
    /// send back.
    pub fn on_packet(&self, packet: &[u8]) -> Vec<Vec<u8>> {
        let mut inner = self.inner.borrow_mut();
        let Some(broadcaster) = inner.broadcaster.as_mut() else {
            return Vec::new();
        };
        broadcaster.on_packet(packet)
    }

    /// True once SDUs written to this source actually go out.
    pub fn is_streaming(&self) -> bool {
        self.inner
            .borrow()
            .broadcaster
            .as_ref()
            .is_some_and(BigBroadcaster::is_streaming)
    }

    /// A one-word label for the source's state, for a page or a status line.
    pub fn state_label(&self) -> &'static str {
        match self.inner.borrow().broadcaster.as_ref().map(|b| b.state()) {
            None => "idle",
            Some(BroadcastState::Streaming) => "streaming",
            Some(BroadcastState::Terminated) => "stopped",
            Some(BroadcastState::Failed(_)) => "failed",
            Some(_) => "starting",
        }
    }

    /// Turns everything that has happened since the last call into callbacks,
    /// in the order Android would have delivered them.
    pub fn take_callbacks(&self) -> Vec<Callback> {
        {
            let mut inner = self.inner.borrow_mut();
            inner.observe_transitions();
        }
        std::mem::take(&mut self.inner.borrow_mut().pending)
    }
}

impl SourceInner {
    fn broadcast_id(&self) -> i64 {
        self.broadcaster
            .as_ref()
            .map(|b| b.config().broadcast_id as i64)
            .unwrap_or_default()
    }

    fn push(&mut self, name: &'static str, args: Vec<Dynamic>) {
        self.pending.push((name, args));
    }

    /// Maps [`BroadcastState`] transitions onto Android's callbacks.
    ///
    /// Android splits "the broadcast exists" from "audio is playing", and so
    /// does the ladder underneath: the BIG exists once `LE Create BIG Complete`
    /// lands (`OpeningDataPaths`), and SDUs only flow once every ISO data path
    /// is open (`Streaming`). Both edges are reported even when a single tick
    /// crosses both.
    fn observe_transitions(&mut self) {
        let Some(state) = self.broadcaster.as_ref().map(|b| b.state()) else {
            return;
        };
        if let Some(status) = self
            .broadcaster
            .as_mut()
            .and_then(BigBroadcaster::take_update_status)
        {
            let id = self.broadcast_id();
            if status == 0 {
                self.push("on_broadcast_updated", vec![0i64.into(), id.into()]);
                let metadata = self.metadata_map();
                self.push(
                    "on_broadcast_metadata_changed",
                    vec![id.into(), Dynamic::from_map(metadata)],
                );
            } else {
                self.push(
                    "on_broadcast_update_failed",
                    vec![(status as i64).into(), id.into()],
                );
            }
        }
        let previous = self.last_state;
        if previous == Some(state) {
            return;
        }
        self.last_state = Some(state);
        let id = self.broadcast_id();
        let created = |s: Option<BroadcastState>| {
            matches!(
                s,
                Some(BroadcastState::OpeningDataPaths) | Some(BroadcastState::Streaming)
            )
        };
        if created(Some(state)) && !created(previous) {
            self.ever_created = true;
            self.push("on_broadcast_started", vec![0i64.into(), id.into()]);
        }
        match state {
            BroadcastState::Streaming => {
                self.push("on_playback_started", vec![0i64.into(), id.into()]);
            }
            BroadcastState::Terminated => {
                if previous == Some(BroadcastState::Streaming) {
                    self.push("on_playback_stopped", vec![0i64.into(), id.into()]);
                }
                self.push("on_broadcast_stopped", vec![0i64.into(), id.into()]);
            }
            BroadcastState::Failed(status) => {
                let reason = Dynamic::from(status as i64);
                if self.ever_created {
                    if previous == Some(BroadcastState::Streaming) {
                        self.push("on_playback_stopped", vec![reason.clone(), id.into()]);
                    }
                    self.push("on_broadcast_stopped", vec![reason, id.into()]);
                } else {
                    self.push("on_broadcast_start_failed", vec![reason]);
                }
            }
            _ => {}
        }
    }

    /// The `BluetoothLeBroadcastMetadata` a receiver — or an Assistant — needs
    /// to find and join this broadcast.
    fn metadata_map(&self) -> Map {
        let mut map = Map::new();
        let Some(broadcaster) = self.broadcaster.as_ref() else {
            return map;
        };
        let config = broadcaster.config();
        map.insert("source_device".into(), self.address.to_string().into());
        // The broadcaster advertises with `own_address_type` 0x00, so what a
        // scanner reports — and what an Add Source must carry — is public.
        map.insert("source_address_type".into(), 0i64.into());
        map.insert(
            "source_advertising_sid".into(),
            (config.advertising_sid as i64).into(),
        );
        map.insert("broadcast_id".into(), (config.broadcast_id as i64).into());
        map.insert(
            "broadcast_name".into(),
            config.broadcast_name.clone().into(),
        );
        map.insert(
            "pa_sync_interval".into(),
            (config.periodic_advertising_interval as i64).into(),
        );
        map.insert("encrypted".into(), config.broadcast_code.is_some().into());
        map.insert(
            "presentation_delay_micros".into(),
            (config.presentation_delay_us as i64).into(),
        );
        // One subgroup: `BroadcastConfig::base()` publishes exactly one, so
        // reporting more here would describe a BASE that is not on the air.
        let bis: Array = (1..=config.num_bis)
            .map(|index| {
                let mut entry = Map::new();
                entry.insert("index".into(), (index as i64).into());
                entry.insert(
                    "audio_location".into(),
                    (channel_allocation(index, config.num_bis) as i64).into(),
                );
                Dynamic::from_map(entry)
            })
            .collect();
        let mut subgroup = Map::new();
        subgroup.insert("bis".into(), bis.into());
        // Every BIS the source published, as an Add Source BIS_Sync bitmask:
        // bit 0 is BIS index 1 (BASS Section 3.1.1.4).
        let all_bis = if config.num_bis >= 32 {
            u32::MAX
        } else {
            (1u32 << config.num_bis) - 1
        };
        subgroup.insert("bis_sync".into(), (all_bis as i64).into());
        subgroup.insert(
            "metadata".into(),
            Dynamic::from_blob(config.metadata.clone()),
        );
        map.insert(
            "subgroups".into(),
            Array::from([Dynamic::from_map(subgroup)]).into(),
        );
        map
    }
}

/// The channel a BIS carries, the same rule `BroadcastConfig::base()` applies.
/// Duplicated as a lookup rather than exported from the device layer because
/// it is one line and exporting it would put a BASE-building detail in the
/// device module's public surface.
fn channel_allocation(index: u8, num_bis: u8) -> u32 {
    if num_bis == 1 {
        return audio_location::FRONT_CENTER;
    }
    match index {
        1 => audio_location::FRONT_LEFT,
        2 => audio_location::FRONT_RIGHT,
        n => 1u32 << (u32::from(n) + 1),
    }
}

/// Builds a [`BroadcastConfig`] from a `BluetoothLeBroadcastSettings`-shaped
/// map. Java's builder has no idiomatic Rhai equivalent, so a map literal is
/// the translation; nothing here is a builder in disguise.
fn broadcast_config(name: &str, settings: &Map) -> Result<BroadcastConfig, Box<EvalAltResult>> {
    let mut config = BroadcastConfig {
        broadcast_name: string_field(settings, "broadcast_name")?
            .unwrap_or_else(|| name.to_string()),
        ..Default::default()
    };
    if let Some(id) = int_field(settings, "broadcast_id")? {
        // The Broadcast_ID is 24 bits on the air (BAP Section 3.7.2.1); a
        // wider value would be silently truncated into a different broadcast.
        if !(0..=0x00FF_FFFF).contains(&id) {
            return Err(runtime_error(format!(
                "broadcast_id: {id:#X} does not fit the 24-bit Broadcast_ID"
            )));
        }
        config.broadcast_id = id as u32;
    }
    if let Some(sid) = int_field(settings, "advertising_sid")? {
        config.advertising_sid = u8::try_from(sid)
            .map_err(|_| runtime_error(format!("advertising_sid: out of range: {sid}")))?;
    }
    match settings.get("broadcast_code") {
        None => {}
        Some(value) if value.is_unit() => config.broadcast_code = None,
        Some(value) => {
            let bytes = dynamic_to_bytes(value.clone())?;
            // A Broadcast_Code is 16 octets (Vol 3, Part C, Section 3.2.6.5);
            // a shorter one is zero-extended, as the spec requires.
            if bytes.len() > 16 {
                return Err(runtime_error(format!(
                    "broadcast_code: {} bytes; a Broadcast_Code is at most 16",
                    bytes.len()
                )));
            }
            let mut code = [0u8; 16];
            code[..bytes.len()].copy_from_slice(&bytes);
            config.broadcast_code = Some(code);
        }
    }

    let subgroups = array_field(settings, "subgroups")?;
    if subgroups.len() > 1 {
        // Not a script error so much as a limit of what is underneath:
        // `BroadcastConfig::base()` publishes exactly one subgroup, so a
        // second one here would describe a BASE that never goes on the air.
        return Err(runtime_error(format!(
            "subgroups: {} given, but BroadcastConfig publishes exactly one subgroup in its BASE",
            subgroups.len()
        )));
    }
    if let Some(subgroup) = subgroups.first() {
        let subgroup = subgroup
            .clone()
            .try_cast::<Map>()
            .ok_or_else(|| runtime_error("subgroups: each entry must be a map"))?;
        let codec = CodecSettings::parse(subgroup.get("codec").unwrap_or(&Dynamic::UNIT))?;
        config.sampling_frequency = codec.sampling_frequency;
        config.frame_duration = codec.frame_duration;
        config.octets_per_codec_frame = codec.octets_per_codec_frame;
        config.sdu_interval_us = codec.sdu_interval_us();
        config.max_sdu = codec.octets_per_codec_frame;

        let bis = array_field(&subgroup, "bis")?;
        if !bis.is_empty() {
            config.num_bis = u8::try_from(bis.len())
                .map_err(|_| runtime_error("bis: too many BISes for one BIG"))?;
            // The locations are validated even though `BroadcastConfig::base()`
            // derives the allocation itself: a script that names a location the
            // BASE will not carry should hear about it rather than be ignored.
            let declared: Vec<u32> = bis
                .iter()
                .map(audio_location_of)
                .collect::<Result<_, _>>()?;
            let derived: Vec<u32> = (1..=config.num_bis)
                .map(|index| channel_allocation(index, config.num_bis))
                .collect();
            if declared != derived {
                return Err(runtime_error(format!(
                    "bis: BroadcastConfig assigns channel allocations by BIS index, so {} BIS(es) \
                     are published as [{}] — it cannot publish [{}]",
                    config.num_bis,
                    derived
                        .iter()
                        .map(|l| audio_location::describe(*l))
                        .collect::<Vec<_>>()
                        .join(", "),
                    declared
                        .iter()
                        .map(|l| audio_location::describe(*l))
                        .collect::<Vec<_>>()
                        .join(", "),
                )));
            }
        }
        if let Some(metadata) = subgroup.get("metadata")
            && !metadata.is_unit()
        {
            config.metadata = dynamic_to_bytes(metadata.clone())?;
        }
    }
    Ok(config)
}

// ---------------------------------------------------------------------------
// android::BluetoothLeBroadcastAssistant — the phone
// ---------------------------------------------------------------------------

/// A control-point operation the Assistant has issued or is holding back,
/// with what it needs to report the answer.
#[derive(Debug, Clone)]
enum AssistantOp {
    AddSource { metadata: Map },
    ModifySource { source_id: u8 },
    RemoveSource { source_id: u8 },
    SearchStarted,
    SearchStopped,
}

impl AssistantOp {
    /// The pair of Android callbacks this operation answers with.
    fn callbacks(&self) -> (&'static str, &'static str) {
        match self {
            Self::AddSource { .. } => ("on_source_added", "on_source_add_failed"),
            Self::ModifySource { .. } => ("on_source_modified", "on_source_modify_failed"),
            Self::RemoveSource { .. } => ("on_source_removed", "on_source_remove_failed"),
            Self::SearchStarted => ("on_search_started", "on_search_start_failed"),
            Self::SearchStopped => ("on_search_stopped", "on_search_stop_failed"),
        }
    }
}

/// Script-side handle to a Broadcast Assistant. Pure GATT underneath: it
/// writes BASS Add/Modify/Remove Source to a Scan Delegator's control point
/// and reads the Broadcast Receive State back. Nothing below GATT is involved.
#[derive(Clone)]
pub struct ScriptBroadcastAssistant {
    inner: Rc<RefCell<AssistantInner>>,
}

struct AssistantInner {
    name: String,
    /// The GATT half. The Assistant *is* a central, so it borrows the whole
    /// central-role binding rather than reimplementing a connection.
    client: ScriptGattClient,
    sink: Option<Address>,
    /// Operations raised before discovery finished. They are held here rather
    /// than queued on the central so the Receive State subscription goes out
    /// first — otherwise the very notification an Add Source provokes arrives
    /// before anything is listening for it.
    deferred: VecDeque<(AssistantOp, Vec<u8>)>,
    /// Operations on the wire, oldest first, awaiting their write completion.
    issued: VecDeque<AssistantOp>,
    /// Add Sources whose write was accepted but whose Receive State has not
    /// been published yet — BASS assigns the Source_ID, so the Assistant only
    /// learns it from the state that comes back.
    awaiting_source: usize,
    /// Every Broadcast Receive State this Assistant has seen, by Source_ID.
    sources: BTreeMap<u8, BroadcastReceiveState>,
    pending: Vec<Callback>,
}

impl ScriptBroadcastAssistant {
    fn create(name: &str) -> Self {
        Self {
            inner: Rc::new(RefCell::new(AssistantInner {
                name: name.to_string(),
                client: ScriptGattClient::create_for(name),
                sink: None,
                deferred: VecDeque::new(),
                issued: VecDeque::new(),
                awaiting_source: 0,
                sources: BTreeMap::new(),
                pending: Vec::new(),
            })),
        }
    }

    /// The Assistant's name.
    pub fn name(&self) -> String {
        self.inner.borrow().name.clone()
    }

    /// The GATT client underneath — what hosts the Assistant as a central.
    pub fn client(&self) -> ScriptGattClient {
        self.inner.borrow().client.clone()
    }

    /// Turns one central event into the Assistant's callbacks, and drives the
    /// BASS bookkeeping (subscribe on discovery, Source_ID assignment) that
    /// makes them meaningful.
    pub fn observe(&self, event: &CentralEvent) -> Vec<Callback> {
        {
            let mut inner = self.inner.borrow_mut();
            inner.observe(event);
        }
        std::mem::take(&mut self.inner.borrow_mut().pending)
    }
}

impl AssistantInner {
    fn push(&mut self, name: &'static str, args: Vec<Dynamic>) {
        self.pending.push((name, args));
    }

    fn sink_name(&self) -> Dynamic {
        self.sink
            .map(|address| address.to_string())
            .unwrap_or_default()
            .into()
    }

    /// Queues one control-point write, or holds it back until discovery has
    /// finished and the Receive State subscription is in place.
    fn issue(&mut self, op: AssistantOp, pdu: Vec<u8>) {
        if self.client.with_central(|c| c.is_ready()) {
            self.client.with_central(|c| {
                c.queue_write(
                    bass_uuid::BROADCAST_AUDIO_SCAN_CONTROL_POINT,
                    pdu.clone(),
                    true,
                )
            });
            self.issued.push_back(op);
        } else {
            self.deferred.push_back((op, pdu));
        }
    }

    fn observe(&mut self, event: &CentralEvent) {
        match event {
            CentralEvent::ServicesDiscovered { .. } => {
                // Subscribe first, then read, then let the held-back
                // operations go: the order a real Assistant uses, and the only
                // order in which an Add Source's own notification is seen.
                self.client.with_central(|c| {
                    c.queue_subscribe(bass_uuid::BROADCAST_RECEIVE_STATE, true);
                    c.queue_read(bass_uuid::BROADCAST_RECEIVE_STATE);
                });
                while let Some((op, pdu)) = self.deferred.pop_front() {
                    self.client.with_central(|c| {
                        c.queue_write(bass_uuid::BROADCAST_AUDIO_SCAN_CONTROL_POINT, pdu, true)
                    });
                    self.issued.push_back(op);
                }
            }
            CentralEvent::CharacteristicWrite { uuid, status, .. }
                if *uuid == bass_uuid::BROADCAST_AUDIO_SCAN_CONTROL_POINT =>
            {
                let Some(op) = self.issued.pop_front() else {
                    return;
                };
                let (_, failed) = op.callbacks();
                let sink = self.sink_name();
                if *status != 0 {
                    let reason = Dynamic::from(*status as i64);
                    match op {
                        AssistantOp::AddSource { metadata } => {
                            self.push(failed, vec![sink, Dynamic::from_map(metadata), reason])
                        }
                        AssistantOp::ModifySource { source_id }
                        | AssistantOp::RemoveSource { source_id } => {
                            self.push(failed, vec![sink, (source_id as i64).into(), reason])
                        }
                        AssistantOp::SearchStarted | AssistantOp::SearchStopped => {
                            self.push(failed, vec![reason])
                        }
                    }
                    return;
                }
                match op {
                    // The Source_ID is BASS's to assign, so `onSourceAdded`
                    // waits for the Receive State that carries it.
                    AssistantOp::AddSource { .. } => self.awaiting_source += 1,
                    AssistantOp::ModifySource { source_id } => self.push(
                        "on_source_modified",
                        vec![sink, (source_id as i64).into(), 0i64.into()],
                    ),
                    AssistantOp::RemoveSource { source_id } => {
                        self.sources.remove(&source_id);
                        self.push(
                            "on_source_removed",
                            vec![sink, (source_id as i64).into(), 0i64.into()],
                        )
                    }
                    AssistantOp::SearchStarted => self.push("on_search_started", vec![0i64.into()]),
                    AssistantOp::SearchStopped => self.push("on_search_stopped", vec![0i64.into()]),
                }
            }
            CentralEvent::CharacteristicRead { uuid, value, .. }
            | CentralEvent::CharacteristicChanged { uuid, value, .. }
                if *uuid == bass_uuid::BROADCAST_RECEIVE_STATE =>
            {
                self.on_receive_state(value);
            }
            _ => {}
        }
    }

    fn on_receive_state(&mut self, value: &[u8]) {
        // An empty value is a slot with no source in it (BASS Section 3.2),
        // which is what a Remove Source leaves behind.
        if value.is_empty() {
            return;
        }
        let Some(state) = BroadcastReceiveState::parse(value) else {
            return;
        };
        let sink = self.sink_name();
        let source_id = state.source_id;
        let is_new = !self.sources.contains_key(&source_id);
        if is_new && self.awaiting_source > 0 {
            self.awaiting_source -= 1;
            self.push(
                "on_source_added",
                vec![sink.clone(), (source_id as i64).into(), 0i64.into()],
            );
        }
        let map = receive_state_map(&state);
        self.sources.insert(source_id, state);
        self.push(
            "on_receive_state_changed",
            vec![sink, (source_id as i64).into(), Dynamic::from_map(map)],
        );
    }
}

/// Builds an Add Source control-point PDU out of a
/// `BluetoothLeBroadcastMetadata`-shaped map.
fn add_source_operation(metadata: &Map) -> Result<ControlPointOperation, Box<EvalAltResult>> {
    let address = string_field(metadata, "source_device")?
        .ok_or_else(|| runtime_error("add_source: metadata has no source_device"))?;
    let advertiser_address = address
        .parse::<Address>()
        .map_err(|e| runtime_error(format!("add_source: source_device {address:?}: {e}")))?;
    let advertiser_address_type =
        u8::try_from(int_field(metadata, "source_address_type")?.unwrap_or(0))
            .map_err(|_| runtime_error("add_source: source_address_type out of range"))?;
    let advertising_sid = u8::try_from(int_field(metadata, "source_advertising_sid")?.unwrap_or(0))
        .map_err(|_| runtime_error("add_source: source_advertising_sid out of range"))?;
    let broadcast_id = int_field(metadata, "broadcast_id")?
        .ok_or_else(|| runtime_error("add_source: metadata has no broadcast_id"))?;
    if !(0..=0x00FF_FFFF).contains(&broadcast_id) {
        return Err(runtime_error(format!(
            "add_source: broadcast_id {broadcast_id:#X} does not fit 24 bits"
        )));
    }
    // 0xFFFF is PA_Interval Unknown (BASS Section 3.1.1.4), which is what an
    // Assistant that has not synchronised to the train itself must send.
    let pa_interval = u16::try_from(int_field(metadata, "pa_sync_interval")?.unwrap_or(0xFFFF))
        .map_err(|_| runtime_error("add_source: pa_sync_interval out of range"))?;
    let pa_sync = match string_field(metadata, "pa_sync")?.as_deref() {
        None | Some("SynchronizeToPaPastNotAvailable") => {
            PeriodicAdvertisingSyncParams::SynchronizeToPaPastNotAvailable
        }
        Some("SynchronizeToPaPastAvailable") => {
            PeriodicAdvertisingSyncParams::SynchronizeToPaPastAvailable
        }
        Some("DoNotSynchronizeToPa") => PeriodicAdvertisingSyncParams::DoNotSynchronizeToPa,
        Some(other) => {
            return Err(runtime_error(format!(
                "add_source: pa_sync {other:?} is not a PA_Sync parameter"
            )));
        }
    };
    Ok(ControlPointOperation::AddSource {
        advertiser_address_type,
        advertiser_address,
        advertising_sid,
        broadcast_id: broadcast_id as u32,
        pa_sync,
        pa_interval,
        subgroups: subgroups_of(metadata)?,
    })
}

/// The subgroup list of an Add/Modify Source, from a metadata map. An absent
/// list means "every BIS, no preference" — BASS's own [`ANY_BIS`].
fn subgroups_of(metadata: &Map) -> Result<Vec<SubgroupInfo>, Box<EvalAltResult>> {
    let subgroups = array_field(metadata, "subgroups")?;
    if subgroups.is_empty() {
        return Ok(vec![SubgroupInfo {
            bis_sync: ANY_BIS,
            metadata: Vec::new(),
        }]);
    }
    subgroups
        .iter()
        .map(|entry| {
            let entry = entry
                .clone()
                .try_cast::<Map>()
                .ok_or_else(|| runtime_error("subgroups: each entry must be a map"))?;
            let bis_sync = match int_field(&entry, "bis_sync")? {
                Some(bits) => u32::try_from(bits)
                    .map_err(|_| runtime_error(format!("bis_sync: out of range: {bits}")))?,
                None => ANY_BIS,
            };
            let metadata = match entry.get("metadata") {
                Some(value) if !value.is_unit() => dynamic_to_bytes(value.clone())?,
                _ => Vec::new(),
            };
            Ok(SubgroupInfo { bis_sync, metadata })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Registers both proxies, the Scan Delegator registrar, and their
/// constructors in the `android` module.
///
/// Called from [`crate::scripting::bindings::register`], and so from
/// [`crate::scripting::new_engine`] — every surface (the playground,
/// `run_test`, MCP, the pages) sees the same API. That is deliberately unlike
/// `add_pacs`/`add_ascs`/`add_ras`, which are registered inside
/// `scripting::test_script::register_web_extensions` and are therefore invisible to any
/// engine that module does not build.
pub fn register(engine: &mut Engine, android: &mut Module) {
    register_source(engine, android);
    register_assistant(engine, android);
    register_delegator(engine);
}

fn register_source(engine: &mut Engine, android: &mut Module) {
    engine
        .register_type_with_name::<ScriptBroadcastSource>("BluetoothLeBroadcast")
        .register_get("name", |source: &mut ScriptBroadcastSource| source.name())
        .register_get("state", |source: &mut ScriptBroadcastSource| {
            source.state_label().to_string()
        })
        .register_fn(
            // `startBroadcast(BluetoothLeBroadcastSettings)`. The Java type is
            // a builder; a map literal is the honest translation.
            "start_broadcast",
            |source: &mut ScriptBroadcastSource, settings: Map| -> Result<(), Box<EvalAltResult>> {
                let name = source.name();
                let config = broadcast_config(&name, &settings)?;
                let mut inner = source.inner.borrow_mut();
                if inner.broadcaster.as_ref().is_some_and(|b| {
                    !matches!(
                        b.state(),
                        BroadcastState::Terminated | BroadcastState::Failed(_)
                    )
                }) {
                    return Err(runtime_error(
                        "start_broadcast: this source is already broadcasting; \
                         stop_broadcast first",
                    ));
                }
                let mut broadcaster = BigBroadcaster::new(config);
                inner.outbox.extend(broadcaster.start());
                inner.broadcaster = Some(broadcaster);
                inner.last_state = None;
                inner.ever_created = false;
                Ok(())
            },
        )
        .register_fn(
            "stop_broadcast",
            |source: &mut ScriptBroadcastSource,
             broadcast_id: i64|
             -> Result<(), Box<EvalAltResult>> {
                let mut inner = source.inner.borrow_mut();
                let Some(broadcaster) = inner.broadcaster.as_ref() else {
                    return Err(runtime_error("stop_broadcast: nothing is broadcasting"));
                };
                if broadcaster.config().broadcast_id as i64 != broadcast_id {
                    return Err(runtime_error(format!(
                        "stop_broadcast: {broadcast_id:#X} is not this source's Broadcast_ID"
                    )));
                }
                let packet = broadcaster.terminate();
                inner.outbox.push(packet);
                Ok(())
            },
        )
        .register_fn(
            // `updateBroadcast(int, BluetoothLeBroadcastSettings)`. Only the
            // subgroup metadata can change while the BIG runs; everything else
            // in the BASE has to agree with the LE Create BIG the controller
            // already acted on. `BigBroadcaster::update_metadata` says the same.
            "update_broadcast",
            |source: &mut ScriptBroadcastSource,
             broadcast_id: i64,
             settings: Map|
             -> Result<(), Box<EvalAltResult>> {
                let name = source.name();
                let config = broadcast_config(&name, &settings)?;
                let mut inner = source.inner.borrow_mut();
                let Some(broadcaster) = inner.broadcaster.as_mut() else {
                    return Err(runtime_error("update_broadcast: nothing is broadcasting"));
                };
                if broadcaster.config().broadcast_id as i64 != broadcast_id {
                    return Err(runtime_error(format!(
                        "update_broadcast: {broadcast_id:#X} is not this source's Broadcast_ID"
                    )));
                }
                let Some(packet) = broadcaster.update_metadata(config.metadata) else {
                    return Err(runtime_error(
                        "update_broadcast: the broadcast is not streaming yet, so there is \
                         no periodic train to rewrite",
                    ));
                };
                inner.outbox.push(packet);
                Ok(())
            },
        )
        .register_fn(
            "is_playing",
            |source: &mut ScriptBroadcastSource, broadcast_id: i64| -> bool {
                let inner = source.inner.borrow();
                inner.broadcaster.as_ref().is_some_and(|b| {
                    b.is_streaming() && b.config().broadcast_id as i64 == broadcast_id
                })
            },
        )
        .register_fn(
            "get_all_broadcast_metadata",
            |source: &mut ScriptBroadcastSource| -> Array {
                let inner = source.inner.borrow();
                if inner.broadcaster.is_none() {
                    return Array::new();
                }
                Array::from([Dynamic::from_map(inner.metadata_map())])
            },
        )
        .register_fn(
            // The media plane. Android has no equivalent — an app hands audio
            // to the LE Audio framework, never to the proxy — so this is named
            // for what it does rather than after a Java method that would be
            // an invention. Returns false while the data paths are not open,
            // which is when a real controller would drop the SDU.
            "send_audio",
            |source: &mut ScriptBroadcastSource,
             bis_index: i64,
             sdu: Dynamic|
             -> Result<bool, Box<EvalAltResult>> {
                let bytes = dynamic_to_bytes(sdu)?;
                let index = u8::try_from(bis_index)
                    .map_err(|_| runtime_error(format!("send_audio: bad BIS index {bis_index}")))?;
                let mut inner = source.inner.borrow_mut();
                let Some(broadcaster) = inner.broadcaster.as_mut() else {
                    return Ok(false);
                };
                match broadcaster.send_sdu(index, &bytes) {
                    Some(packet) => {
                        inner.outbox.push(packet);
                        Ok(true)
                    }
                    None => Ok(false),
                }
            },
        );

    android.set_native_fn(
        "BluetoothLeBroadcast",
        |name: &str| -> Result<ScriptBroadcastSource, Box<EvalAltResult>> {
            Ok(ScriptBroadcastSource::create(name))
        },
    );
}

fn register_assistant(engine: &mut Engine, android: &mut Module) {
    engine
        .register_type_with_name::<ScriptBroadcastAssistant>("BluetoothLeBroadcastAssistant")
        .register_get("name", |a: &mut ScriptBroadcastAssistant| a.name())
        .register_get("sink", |a: &mut ScriptBroadcastAssistant| {
            a.inner
                .borrow()
                .sink
                .map(|address| address.to_string())
                .unwrap_or_default()
        })
        .register_get("connected", |a: &mut ScriptBroadcastAssistant| {
            a.client().with_central(|c| c.connection_handle() != 0)
        })
        .register_fn(
            "add_source",
            |assistant: &mut ScriptBroadcastAssistant,
             sink: &str,
             metadata: Map,
             _is_group_op: bool|
             -> Result<(), Box<EvalAltResult>> {
                let operation = add_source_operation(&metadata)?;
                assistant.connect_to(sink)?;
                assistant
                    .inner
                    .borrow_mut()
                    .issue(AssistantOp::AddSource { metadata }, operation.to_bytes());
                Ok(())
            },
        )
        .register_fn(
            "modify_source",
            |assistant: &mut ScriptBroadcastAssistant,
             sink: &str,
             source_id: i64,
             metadata: Map|
             -> Result<(), Box<EvalAltResult>> {
                let source_id = u8::try_from(source_id).map_err(|_| {
                    runtime_error(format!("modify_source: bad Source_ID {source_id}"))
                })?;
                let pa_sync = match string_field(&metadata, "pa_sync")?.as_deref() {
                    Some("DoNotSynchronizeToPa") => {
                        PeriodicAdvertisingSyncParams::DoNotSynchronizeToPa
                    }
                    Some("SynchronizeToPaPastAvailable") => {
                        PeriodicAdvertisingSyncParams::SynchronizeToPaPastAvailable
                    }
                    None | Some("SynchronizeToPaPastNotAvailable") => {
                        PeriodicAdvertisingSyncParams::SynchronizeToPaPastNotAvailable
                    }
                    Some(other) => {
                        return Err(runtime_error(format!(
                            "modify_source: pa_sync {other:?} is not a PA_Sync parameter"
                        )));
                    }
                };
                let pa_interval =
                    u16::try_from(int_field(&metadata, "pa_sync_interval")?.unwrap_or(0xFFFF))
                        .map_err(|_| {
                            runtime_error("modify_source: pa_sync_interval out of range")
                        })?;
                let operation = ControlPointOperation::ModifySource {
                    source_id,
                    pa_sync,
                    pa_interval,
                    subgroups: subgroups_of(&metadata)?,
                };
                assistant.connect_to(sink)?;
                assistant.inner.borrow_mut().issue(
                    AssistantOp::ModifySource { source_id },
                    operation.to_bytes(),
                );
                Ok(())
            },
        )
        .register_fn(
            "remove_source",
            |assistant: &mut ScriptBroadcastAssistant,
             sink: &str,
             source_id: i64|
             -> Result<(), Box<EvalAltResult>> {
                let source_id = u8::try_from(source_id).map_err(|_| {
                    runtime_error(format!("remove_source: bad Source_ID {source_id}"))
                })?;
                let operation = ControlPointOperation::RemoveSource { source_id };
                assistant.connect_to(sink)?;
                assistant.inner.borrow_mut().issue(
                    AssistantOp::RemoveSource { source_id },
                    operation.to_bytes(),
                );
                Ok(())
            },
        )
        .register_fn(
            // Half of Android's `startSearchingForSources(List<ScanFilter>)`:
            // the BASS half, telling the sink a scan is underway on its behalf
            // (Remote Scan Started, BASS Section 3.1.1.2). The scanning half is
            // not implemented — see this module's header — so no
            // `on_source_found` is ever delivered.
            "start_searching_for_sources",
            |assistant: &mut ScriptBroadcastAssistant| -> Result<(), Box<EvalAltResult>> {
                assistant.tell_sink(
                    "start_searching_for_sources",
                    AssistantOp::SearchStarted,
                    ControlPointOperation::RemoteScanStarted,
                )
            },
        )
        .register_fn(
            "stop_searching_for_sources",
            |assistant: &mut ScriptBroadcastAssistant| -> Result<(), Box<EvalAltResult>> {
                assistant.tell_sink(
                    "stop_searching_for_sources",
                    AssistantOp::SearchStopped,
                    ControlPointOperation::RemoteScanStopped,
                )
            },
        )
        .register_fn(
            "get_all_sources",
            |assistant: &mut ScriptBroadcastAssistant, _sink: &str| -> Array {
                assistant
                    .inner
                    .borrow()
                    .sources
                    .values()
                    .map(|state| Dynamic::from_map(receive_state_map(state)))
                    .collect()
            },
        )
        .register_fn(
            // The Assistant's return path to the host, the same `emit` the
            // central and the server already have.
            "emit",
            |assistant: &mut ScriptBroadcastAssistant,
             kind: &str,
             payload: Dynamic|
             -> Result<(), Box<EvalAltResult>> {
                let client = assistant.client();
                client.emit(kind, payload)
            },
        );

    android.set_native_fn(
        "BluetoothLeBroadcastAssistant",
        |name: &str| -> Result<ScriptBroadcastAssistant, Box<EvalAltResult>> {
            Ok(ScriptBroadcastAssistant::create(name))
        },
    );
}

impl ScriptBroadcastAssistant {
    /// Points the Assistant at `sink`, if it is not already talking to it.
    ///
    /// Android's proxy has no `connect`: `addSource` reaches a sink whether or
    /// not a link is up, because the framework owns the connection. This does
    /// the same thing rather than inventing a method Android does not have.
    fn connect_to(&self, sink: &str) -> Result<(), Box<EvalAltResult>> {
        let address = sink
            .parse::<Address>()
            .map_err(|e| runtime_error(format!("sink {sink:?} is not an address: {e}")))?;
        let mut inner = self.inner.borrow_mut();
        if inner.sink == Some(address) {
            return Ok(());
        }
        if let Some(existing) = inner.sink {
            return Err(runtime_error(format!(
                "this Assistant is already talking to {existing}; simble's central holds one \
                 link at a time, so a second sink needs a second Assistant"
            )));
        }
        inner.sink = Some(address);
        inner.client.connect(address);
        Ok(())
    }

    /// Sends a sink-wide control-point operation — one that names no source.
    ///
    /// Android's `startSearchingForSources(List<ScanFilter>)` takes no sink
    /// either: its framework knows every connected one and writes Remote Scan
    /// Started to all of them. simble's Assistant holds a single link and
    /// learns which sink that is from the first operation that names one, so
    /// this needs that to have happened.
    fn tell_sink(
        &self,
        method: &str,
        op: AssistantOp,
        operation: ControlPointOperation,
    ) -> Result<(), Box<EvalAltResult>> {
        let mut inner = self.inner.borrow_mut();
        if inner.sink.is_none() {
            return Err(runtime_error(format!(
                "{method}: this Assistant is not talking to a sink yet. Android's framework \
                 knows every connected sink; simble's Assistant learns its one sink from the \
                 first call that names one (add_source, modify_source, remove_source)."
            )));
        }
        inner.issue(op, operation.to_bytes());
        Ok(())
    }
}

/// The Scan Delegator side: a registrar on a `BluetoothGattServer`, in the
/// `add_pacs`/`add_ascs`/`add_ras` spelling.
///
/// Android has no proxy for *being* a Delegator — a phone is always the
/// Assistant — so there is no Android name to mirror here, and inventing one
/// would be worse than not having one.
fn register_delegator(engine: &mut Engine) {
    engine.register_fn(
        "add_bass",
        |server: &mut ScriptGattServer,
         num_receive_states: i64|
         -> Result<(), Box<EvalAltResult>> {
            let slots = usize::try_from(num_receive_states).unwrap_or(0);
            if slots == 0 {
                return Err(runtime_error(
                    "add_bass: BASS requires at least one Broadcast Receive State (Section 3.2)",
                ));
            }
            let service = server
                .with_server(|s| BroadcastAudioScanService::register(&mut s.device.gatt_db, slots));
            server.set_bass(service);
            Ok(())
        },
    );
    engine.register_fn(
        // The Delegator's view of its own slots, so a script can see which
        // sources an Assistant added and what each is synchronised to. No
        // Android equivalent, for the same reason `add_bass` has none.
        "receive_states",
        |server: &mut ScriptGattServer| -> Array {
            server
                .with_bass(|bass, _db| {
                    let mut states = Array::new();
                    for index in 0.. {
                        match bass.receive_state(index) {
                            Some(state) => {
                                states.push(Dynamic::from_map(receive_state_map(&state)))
                            }
                            // `receive_state` answers `None` both for a slot
                            // that is empty and for one that does not exist,
                            // so walking off the end needs the handle count.
                            None if index >= bass.receive_state_value_handles.len() => break,
                            None => {}
                        }
                    }
                    states
                })
                .unwrap_or_default()
        },
    );
    engine.register_fn(
        // No Android equivalent, deliberately: on a real earbud the controller
        // carries out the synchronisation Add Source *requested* and reports
        // what it achieved. Nothing in a scripted Delegator owns a radio, so
        // the script stands in for one. `BroadcastAudioScanService`'s own doc
        // calls this the composition step it deliberately does not take.
        "report_sync_outcome",
        |server: &mut ScriptGattServer,
         source_id: i64,
         pa_sync_state: &str,
         bis_sync: i64|
         -> Result<(), Box<EvalAltResult>> {
            let source_id = u8::try_from(source_id).map_err(|_| {
                runtime_error(format!("report_sync_outcome: bad Source_ID {source_id}"))
            })?;
            let state = pa_sync_state_of(pa_sync_state).ok_or_else(|| {
                runtime_error(format!(
                    "report_sync_outcome: {pa_sync_state:?} is not a PA_Sync_State"
                ))
            })?;
            let bis_sync = u32::try_from(bis_sync).map_err(|_| {
                runtime_error(format!("report_sync_outcome: bad BIS_Sync {bis_sync}"))
            })?;
            server
                .with_bass(|bass, db| bass.report_sync_outcome(db, source_id, state, bis_sync))
                .ok_or_else(|| {
                    runtime_error(
                        "report_sync_outcome: this server has no BASS; call add_bass first",
                    )
                })?
                .map_err(|status| {
                    runtime_error(format!(
                        "report_sync_outcome: BASS rejected Source_ID {source_id}: {status:#04X}"
                    ))
                })
        },
    );
}

/// Whether the script defines `name` with `arity` parameters.
///
/// The same (name, arity) resolution `Handlers::detect` uses in the central
/// role — a proxy has far more callbacks than a `BluetoothGattCallback`, so
/// they are looked up on demand rather than enumerated in a struct.
pub(crate) fn defines(ast: &AST, name: &str, arity: usize) -> bool {
    ast.iter_functions()
        .any(|f| f.name == name && f.params.len() == arity)
}
