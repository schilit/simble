// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Apple Media Service (AMS).
//!
//! AMS is Apple's proprietary 128-bit-UUID GATT service through which an iOS device
//! (the Media Source, MS) exposes media playback to an accessory (the Media Remote,
//! MR). Simble simulates the iPhone side: [`MediaService`] registers the MS's three
//! characteristics (AMS Specification, "Apple Media Service"):
//!
//! - **Remote Command** (write + notify): the MR writes a RemoteCommandID to control
//!   playback; the MS notifies the list of currently supported commands.
//! - **Entity Update** (write + notify): the MR writes `[EntityID, AttributeID...]`
//!   to subscribe; the MS notifies `[EntityID, AttributeID, EntityUpdateFlags,
//!   Value]` when a subscribed attribute changes (values are UTF-8 strings).
//! - **Entity Attribute** (write + read): the MR writes `[EntityID, AttributeID]`
//!   to select an attribute, then reads back its full (untruncated) value.
//!
//! Media state (player / queue / track) lives in shared state; Remote Command
//! writes drive a small playback state machine through an [`AttributeHandler`], and
//! host-side setters ([`MediaService::set_track`] etc.) produce Entity Update
//! payloads only for attributes the accessory subscribed to, parked for retrieval
//! via [`MediaService::take_entity_updates`] (the `mcp.rs`
//! `last_control_point_notification` convention, queued because one change can fan
//! out into several updates).
//!
//! [`AmsClient`] is the accessory (MR) side: sans-IO command/subscription builders
//! and notification parsers shaped like `crate::client::GattClient`, caching the
//! typed media state the updates carry.

use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

/// AMS service and characteristic UUIDs (Apple-assigned, 128-bit; stored
/// little-endian as `Uuid::Uuid128` expects).
pub mod ams_uuid {
    use crate::types::Uuid;

    /// 89D3502B-0F36-433A-8EF4-C502AD55F8DC
    pub const APPLE_MEDIA_SERVICE: Uuid = Uuid::Uuid128([
        0xDC, 0xF8, 0x55, 0xAD, 0x02, 0xC5, 0xF4, 0x8E, 0x3A, 0x43, 0x36, 0x0F, 0x2B, 0x50, 0xD3,
        0x89,
    ]);
    /// 9B3C81D8-57B1-4A8A-B8DF-0E56F7CA51C2
    pub const REMOTE_COMMAND: Uuid = Uuid::Uuid128([
        0xC2, 0x51, 0xCA, 0xF7, 0x56, 0x0E, 0xDF, 0xB8, 0x8A, 0x4A, 0xB1, 0x57, 0xD8, 0x81, 0x3C,
        0x9B,
    ]);
    /// 2F7CABCE-808D-411F-9A0C-BB92BA96C102
    pub const ENTITY_UPDATE: Uuid = Uuid::Uuid128([
        0x02, 0xC1, 0x96, 0xBA, 0x92, 0xBB, 0x0C, 0x9A, 0x1F, 0x41, 0x8D, 0x80, 0xCE, 0xAB, 0x7C,
        0x2F,
    ]);
    /// C6B2F38C-23AB-46D8-A6AB-A3A870BBD5D7
    pub const ENTITY_ATTRIBUTE: Uuid = Uuid::Uuid128([
        0xD7, 0xD5, 0xBB, 0x70, 0xA8, 0xA3, 0xAB, 0xA6, 0xD8, 0x46, 0xAB, 0x23, 0x8C, 0xF3, 0xB2,
        0xC6,
    ]);
}

/// AMS-specific ATT application error codes (AMS Specification, "Error Codes").
pub mod error_code {
    /// The MR sent a command to an MS in a state where it cannot be processed.
    pub const INVALID_STATE: u8 = 0xA0;
    /// The command is improperly formatted, unknown, or not currently supported.
    pub const INVALID_COMMAND: u8 = 0xA1;
    /// The requested entity attribute is absent on the MS.
    pub const ABSENT_ATTRIBUTE: u8 = 0xA2;
}

/// RemoteCommandID values written to the Remote Command characteristic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum RemoteCommandId {
    Play = 0,
    Pause = 1,
    TogglePlayPause = 2,
    NextTrack = 3,
    PreviousTrack = 4,
    VolumeUp = 5,
    VolumeDown = 6,
    AdvanceRepeatMode = 7,
    AdvanceShuffleMode = 8,
    SkipForward = 9,
    SkipBackward = 10,
    LikeTrack = 11,
    DislikeTrack = 12,
    BookmarkTrack = 13,
}

impl RemoteCommandId {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Play,
            1 => Self::Pause,
            2 => Self::TogglePlayPause,
            3 => Self::NextTrack,
            4 => Self::PreviousTrack,
            5 => Self::VolumeUp,
            6 => Self::VolumeDown,
            7 => Self::AdvanceRepeatMode,
            8 => Self::AdvanceShuffleMode,
            9 => Self::SkipForward,
            10 => Self::SkipBackward,
            11 => Self::LikeTrack,
            12 => Self::DislikeTrack,
            13 => Self::BookmarkTrack,
            _ => return None,
        })
    }
}

/// Every command the simulated player implements; the default supported set.
pub const ALL_REMOTE_COMMANDS: [RemoteCommandId; 14] = [
    RemoteCommandId::Play,
    RemoteCommandId::Pause,
    RemoteCommandId::TogglePlayPause,
    RemoteCommandId::NextTrack,
    RemoteCommandId::PreviousTrack,
    RemoteCommandId::VolumeUp,
    RemoteCommandId::VolumeDown,
    RemoteCommandId::AdvanceRepeatMode,
    RemoteCommandId::AdvanceShuffleMode,
    RemoteCommandId::SkipForward,
    RemoteCommandId::SkipBackward,
    RemoteCommandId::LikeTrack,
    RemoteCommandId::DislikeTrack,
    RemoteCommandId::BookmarkTrack,
];

/// EntityID values in Entity Update / Entity Attribute exchanges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EntityId {
    Player = 0,
    Queue = 1,
    Track = 2,
}

impl EntityId {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Player,
            1 => Self::Queue,
            2 => Self::Track,
            _ => return None,
        })
    }
}

/// EntityUpdateFlags bitmask in Entity Update notifications.
pub mod entity_update_flags {
    /// The value was truncated to fit the notification; the full value is
    /// available through the Entity Attribute characteristic.
    pub const TRUNCATED: u8 = 1 << 0;
}

/// Player entity AttributeID values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PlayerAttributeId {
    Name = 0,
    PlaybackInfo = 1,
    Volume = 2,
}

impl PlayerAttributeId {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Name,
            1 => Self::PlaybackInfo,
            2 => Self::Volume,
            _ => return None,
        })
    }
}

/// Queue entity AttributeID values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QueueAttributeId {
    Index = 0,
    Count = 1,
    ShuffleMode = 2,
    RepeatMode = 3,
}

impl QueueAttributeId {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Index,
            1 => Self::Count,
            2 => Self::ShuffleMode,
            3 => Self::RepeatMode,
            _ => return None,
        })
    }
}

/// Track entity AttributeID values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrackAttributeId {
    Artist = 0,
    Album = 1,
    Title = 2,
    Duration = 3,
}

impl TrackAttributeId {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Artist,
            1 => Self::Album,
            2 => Self::Title,
            3 => Self::Duration,
            _ => return None,
        })
    }
}

/// Shuffle mode values carried in the Queue ShuffleMode attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ShuffleMode {
    Off = 0,
    One = 1,
    All = 2,
}

impl ShuffleMode {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Off,
            1 => Self::One,
            2 => Self::All,
            _ => return None,
        })
    }

    fn advance(self) -> Self {
        match self {
            Self::Off => Self::One,
            Self::One => Self::All,
            Self::All => Self::Off,
        }
    }
}

/// Repeat mode values carried in the Queue RepeatMode attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RepeatMode {
    Off = 0,
    One = 1,
    All = 2,
}

impl RepeatMode {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Off,
            1 => Self::One,
            2 => Self::All,
            _ => return None,
        })
    }

    fn advance(self) -> Self {
        match self {
            Self::Off => Self::One,
            Self::One => Self::All,
            Self::All => Self::Off,
        }
    }
}

/// Playback state values, the first field of the PlaybackInfo attribute string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PlaybackState {
    Paused = 0,
    Playing = 1,
    Rewinding = 2,
    FastForwarding = 3,
}

impl PlaybackState {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Paused,
            1 => Self::Playing,
            2 => Self::Rewinding,
            3 => Self::FastForwarding,
            _ => return None,
        })
    }
}

/// The PlaybackInfo attribute value: a comma-separated
/// `PlaybackState,PlaybackRate,ElapsedTime` string on the wire.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaybackInfo {
    pub playback_state: PlaybackState,
    pub playback_rate: f32,
    pub elapsed_time: f32,
}

impl PlaybackInfo {
    fn to_wire(self) -> String {
        format!(
            "{},{},{}",
            self.playback_state as u8, self.playback_rate, self.elapsed_time
        )
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        let mut fields = value.split(',');
        let playback_state = PlaybackState::from_u8(fields.next()?.parse().ok()?)?;
        let playback_rate = fields.next()?.parse().ok()?;
        let elapsed_time = fields.next()?.parse().ok()?;
        Some(Self {
            playback_state,
            playback_rate,
            elapsed_time,
        })
    }
}

/// Longest attribute value an Entity Update notification carries before setting
/// TRUNCATED: a notification under the default ATT MTU (23) has 20 payload bytes,
/// and the `[EntityID, AttributeID, EntityUpdateFlags]` header uses 3 of them.
pub const ENTITY_UPDATE_MAX_VALUE_LEN: usize = 17;

/// Volume step for VolumeUp/VolumeDown, matching the 16-step ringer/volume
/// granularity iOS applies to hardware volume presses.
const VOLUME_STEP: f32 = 1.0 / 16.0;

/// Skip distance in seconds for SkipForward/SkipBackward, iOS's default skip
/// interval for podcast-style transports.
const SKIP_INTERVAL: f32 = 15.0;

/// Handles shared with the characteristic handlers so state changes can publish
/// notify payloads.
#[derive(Debug, Clone, Copy)]
struct AmsHandles {
    remote_command_value_handle: u16,
    entity_update_value_handle: u16,
    entity_attribute_value_handle: u16,
}

/// Shared MS state: media player model, per-entity subscriptions, and the parked
/// Entity Update payloads.
#[derive(Debug)]
struct AmsState {
    player_name: String,
    playback_state: PlaybackState,
    playback_rate: f32,
    elapsed_time: f32,
    volume: f32,
    queue_index: u32,
    queue_count: u32,
    shuffle_mode: ShuffleMode,
    repeat_mode: RepeatMode,
    track_artist: String,
    track_album: String,
    track_title: String,
    track_duration: f32,
    supported_commands: Vec<RemoteCommandId>,
    /// Subscribed AttributeIDs, indexed by `EntityId as usize`.
    subscriptions: [BTreeSet<u8>; 3],
    /// Entity Update notification payloads not yet drained by the host; a queue
    /// (not a single slot) because one change fans out into one payload per
    /// subscribed attribute.
    pending_entity_updates: Vec<Vec<u8>>,
    handles: AmsHandles,
}

impl AmsState {
    /// Current wire value (UTF-8 string) of one entity attribute; `None` when the
    /// AttributeID doesn't exist for the entity.
    fn attribute_value(&self, entity: EntityId, attribute_id: u8) -> Option<String> {
        match entity {
            EntityId::Player => Some(match PlayerAttributeId::from_u8(attribute_id)? {
                PlayerAttributeId::Name => self.player_name.clone(),
                PlayerAttributeId::PlaybackInfo => self.playback_info().to_wire(),
                PlayerAttributeId::Volume => self.volume.to_string(),
            }),
            EntityId::Queue => Some(match QueueAttributeId::from_u8(attribute_id)? {
                QueueAttributeId::Index => self.queue_index.to_string(),
                QueueAttributeId::Count => self.queue_count.to_string(),
                QueueAttributeId::ShuffleMode => (self.shuffle_mode as u8).to_string(),
                QueueAttributeId::RepeatMode => (self.repeat_mode as u8).to_string(),
            }),
            EntityId::Track => Some(match TrackAttributeId::from_u8(attribute_id)? {
                TrackAttributeId::Artist => self.track_artist.clone(),
                TrackAttributeId::Album => self.track_album.clone(),
                TrackAttributeId::Title => self.track_title.clone(),
                TrackAttributeId::Duration => self.track_duration.to_string(),
            }),
        }
    }

    fn playback_info(&self) -> PlaybackInfo {
        PlaybackInfo {
            playback_state: self.playback_state,
            playback_rate: self.playback_rate,
            elapsed_time: self.elapsed_time,
        }
    }

    /// Queues an Entity Update payload for `attribute_id` if the accessory
    /// subscribed to it, truncating the value to what a default-MTU notification
    /// can carry.
    fn emit_update(&mut self, db: &mut GattDatabase, entity: EntityId, attribute_id: u8) {
        if !self.subscriptions[entity as usize].contains(&attribute_id) {
            return;
        }
        let value = self
            .attribute_value(entity, attribute_id)
            .expect("only existing attributes are subscribable");
        let value = value.as_bytes();
        let truncated = value.len() > ENTITY_UPDATE_MAX_VALUE_LEN;
        let mut payload = Vec::with_capacity(3 + value.len().min(ENTITY_UPDATE_MAX_VALUE_LEN));
        payload.push(entity as u8);
        payload.push(attribute_id);
        payload.push(if truncated {
            entity_update_flags::TRUNCATED
        } else {
            0
        });
        payload.extend_from_slice(&value[..value.len().min(ENTITY_UPDATE_MAX_VALUE_LEN)]);
        let _ = db.set_value(self.handles.entity_update_value_handle, &payload);
        self.pending_entity_updates.push(payload);
    }

    fn set_playback_state(&mut self, db: &mut GattDatabase, playback_state: PlaybackState) {
        self.playback_state = playback_state;
        self.emit_update(db, EntityId::Player, PlayerAttributeId::PlaybackInfo as u8);
    }

    /// Applies one Remote Command; errors are ATT application errors on the write
    /// (AMS has no result-notification channel, unlike MCS-style control points).
    fn apply_command(&mut self, db: &mut GattDatabase, command: RemoteCommandId) -> Result<(), u8> {
        if !self.supported_commands.contains(&command) {
            return Err(error_code::INVALID_COMMAND);
        }
        match command {
            RemoteCommandId::Play => self.set_playback_state(db, PlaybackState::Playing),
            RemoteCommandId::Pause => self.set_playback_state(db, PlaybackState::Paused),
            RemoteCommandId::TogglePlayPause => {
                let next = if self.playback_state == PlaybackState::Playing {
                    PlaybackState::Paused
                } else {
                    PlaybackState::Playing
                };
                self.set_playback_state(db, next);
            }
            RemoteCommandId::NextTrack | RemoteCommandId::PreviousTrack => {
                // The simulator has no real track list; a track change is modeled as
                // moving the queue index (wrapping at the ends, as iOS does when
                // repeat-all is on) and restarting playback time.
                if self.queue_count > 0 {
                    self.queue_index = if command == RemoteCommandId::NextTrack {
                        (self.queue_index + 1) % self.queue_count
                    } else {
                        (self.queue_index + self.queue_count - 1) % self.queue_count
                    };
                }
                self.elapsed_time = 0.0;
                self.emit_update(db, EntityId::Queue, QueueAttributeId::Index as u8);
                self.emit_update(db, EntityId::Player, PlayerAttributeId::PlaybackInfo as u8);
            }
            RemoteCommandId::VolumeUp | RemoteCommandId::VolumeDown => {
                let step = if command == RemoteCommandId::VolumeUp {
                    VOLUME_STEP
                } else {
                    -VOLUME_STEP
                };
                self.volume = (self.volume + step).clamp(0.0, 1.0);
                self.emit_update(db, EntityId::Player, PlayerAttributeId::Volume as u8);
            }
            RemoteCommandId::AdvanceRepeatMode => {
                self.repeat_mode = self.repeat_mode.advance();
                self.emit_update(db, EntityId::Queue, QueueAttributeId::RepeatMode as u8);
            }
            RemoteCommandId::AdvanceShuffleMode => {
                self.shuffle_mode = self.shuffle_mode.advance();
                self.emit_update(db, EntityId::Queue, QueueAttributeId::ShuffleMode as u8);
            }
            RemoteCommandId::SkipForward | RemoteCommandId::SkipBackward => {
                let step = if command == RemoteCommandId::SkipForward {
                    SKIP_INTERVAL
                } else {
                    -SKIP_INTERVAL
                };
                let limit = if self.track_duration > 0.0 {
                    self.track_duration
                } else {
                    f32::MAX
                };
                self.elapsed_time = (self.elapsed_time + step).clamp(0.0, limit);
                self.emit_update(db, EntityId::Player, PlayerAttributeId::PlaybackInfo as u8);
            }
            // Ratings/bookmarks have no observable attribute in the AMS entity
            // model; accepting them silently matches an MS with no rating UI.
            RemoteCommandId::LikeTrack
            | RemoteCommandId::DislikeTrack
            | RemoteCommandId::BookmarkTrack => {}
        }
        Ok(())
    }

    fn supported_commands_payload(&self) -> Vec<u8> {
        self.supported_commands.iter().map(|&c| c as u8).collect()
    }
}

/// [`AttributeHandler`] for the Remote Command characteristic.
#[derive(Debug)]
struct RemoteCommandHandler {
    state: Arc<Mutex<AmsState>>,
}

impl AttributeHandler for RemoteCommandHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().expect("AMS state lock poisoned");
        // The value is a single RemoteCommandID byte; anything else is an
        // improperly formatted or unknown command (AMS Specification, "Remote
        // Command": error 0xA1).
        let [command_byte] = value else {
            return Err(error_code::INVALID_COMMAND);
        };
        let command = RemoteCommandId::from_u8(*command_byte).ok_or(error_code::INVALID_COMMAND)?;
        state.apply_command(db, command)
    }
}

/// [`AttributeHandler`] for the Entity Update characteristic (subscription writes).
#[derive(Debug)]
struct EntityUpdateHandler {
    state: Arc<Mutex<AmsState>>,
}

impl AttributeHandler for EntityUpdateHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().expect("AMS state lock poisoned");
        let (&entity_byte, attribute_ids) =
            value.split_first().ok_or(error_code::INVALID_COMMAND)?;
        let entity = EntityId::from_u8(entity_byte).ok_or(error_code::INVALID_COMMAND)?;
        // Validate the whole list before mutating so a bad AttributeID doesn't
        // leave a half-applied subscription.
        for &attribute_id in attribute_ids {
            if state.attribute_value(entity, attribute_id).is_none() {
                return Err(error_code::INVALID_COMMAND);
            }
        }
        for &attribute_id in attribute_ids {
            state.subscriptions[entity as usize].insert(attribute_id);
            // iOS sends the current value immediately upon subscription, so the MR
            // starts from a known state rather than waiting for the next change.
            state.emit_update(db, entity, attribute_id);
        }
        Ok(())
    }
}

/// [`AttributeHandler`] for the Entity Attribute characteristic: a write selects
/// `[EntityID, AttributeID]` and stores the full value for a follow-up read.
#[derive(Debug)]
struct EntityAttributeHandler {
    state: Arc<Mutex<AmsState>>,
}

impl AttributeHandler for EntityAttributeHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let state = self.state.lock().expect("AMS state lock poisoned");
        let [entity_byte, attribute_id] = value else {
            return Err(error_code::INVALID_COMMAND);
        };
        let entity = EntityId::from_u8(*entity_byte).ok_or(error_code::INVALID_COMMAND)?;
        let value = state
            .attribute_value(entity, *attribute_id)
            .ok_or(error_code::ABSENT_ATTRIBUTE)?;
        // The handled attribute stays in the database during dispatch, so this
        // stores the full value where the accessory's read will find it.
        let _ = db.set_value(
            state.handles.entity_attribute_value_handle,
            value.as_bytes(),
        );
        Ok(())
    }
}

/// AMS Media Source (simulated iPhone) GATT container plus the media player state
/// it owns.
#[derive(Debug)]
pub struct MediaService {
    pub service_handle: u16,
    pub remote_command_value_handle: u16,
    pub entity_update_value_handle: u16,
    pub entity_attribute_value_handle: u16,
    state: Arc<Mutex<AmsState>>,
}

impl MediaService {
    pub fn register(db: &mut GattDatabase, player_name: &str) -> Self {
        let service_handle = db.add_service(ams_uuid::APPLE_MEDIA_SERVICE, true);

        let supported_commands = ALL_REMOTE_COMMANDS.to_vec();
        let supported_payload: Vec<u8> = supported_commands.iter().map(|&c| c as u8).collect();

        // The Remote Command value doubles as the parked supported-commands notify
        // payload, so it keeps read permission for host-side inspection even though
        // a real MS is write/notify only.
        let (_, remote_command_value_handle) = db.add_characteristic(
            ams_uuid::REMOTE_COMMAND,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::NOTIFY,
            ),
            supported_payload,
            AttributePermissions::default(),
        );

        let (_, entity_update_value_handle) = db.add_characteristic(
            ams_uuid::ENTITY_UPDATE,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::NOTIFY,
            ),
            vec![],
            AttributePermissions::default(),
        );

        let (_, entity_attribute_value_handle) = db.add_characteristic(
            ams_uuid::ENTITY_ATTRIBUTE,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::WRITE,
            ),
            vec![],
            AttributePermissions::default(),
        );

        let state = Arc::new(Mutex::new(AmsState {
            player_name: player_name.to_string(),
            playback_state: PlaybackState::Paused,
            playback_rate: 1.0,
            elapsed_time: 0.0,
            volume: 0.5,
            queue_index: 0,
            queue_count: 0,
            shuffle_mode: ShuffleMode::Off,
            repeat_mode: RepeatMode::Off,
            track_artist: String::new(),
            track_album: String::new(),
            track_title: String::new(),
            track_duration: 0.0,
            supported_commands,
            subscriptions: Default::default(),
            pending_entity_updates: Vec::new(),
            handles: AmsHandles {
                remote_command_value_handle,
                entity_update_value_handle,
                entity_attribute_value_handle,
            },
        }));
        for (handle, handler) in [
            (
                remote_command_value_handle,
                Box::new(RemoteCommandHandler {
                    state: Arc::clone(&state),
                }) as Box<dyn AttributeHandler>,
            ),
            (
                entity_update_value_handle,
                Box::new(EntityUpdateHandler {
                    state: Arc::clone(&state),
                }),
            ),
            (
                entity_attribute_value_handle,
                Box::new(EntityAttributeHandler {
                    state: Arc::clone(&state),
                }),
            ),
        ] {
            db.set_handler(handle, handler)
                .expect("handle just allocated");
        }

        Self {
            service_handle,
            remote_command_value_handle,
            entity_update_value_handle,
            entity_attribute_value_handle,
            state,
        }
    }

    fn with_state<R>(&self, f: impl FnOnce(&mut AmsState) -> R) -> R {
        let mut state = self.state.lock().expect("AMS state lock poisoned");
        f(&mut state)
    }

    /// Host-side supported-command set change (e.g. a different app taking over
    /// playback); republished as the Remote Command notify payload.
    pub fn set_supported_commands(&self, db: &mut GattDatabase, commands: &[RemoteCommandId]) {
        self.with_state(|state| {
            state.supported_commands = commands.to_vec();
            let payload = state.supported_commands_payload();
            let _ = db.set_value(state.handles.remote_command_value_handle, &payload);
        });
    }

    pub fn set_player_name(&self, db: &mut GattDatabase, name: &str) {
        self.with_state(|state| {
            state.player_name = name.to_string();
            state.emit_update(db, EntityId::Player, PlayerAttributeId::Name as u8);
        });
    }

    pub fn set_playback_info(&self, db: &mut GattDatabase, info: PlaybackInfo) {
        self.with_state(|state| {
            state.playback_state = info.playback_state;
            state.playback_rate = info.playback_rate;
            state.elapsed_time = info.elapsed_time;
            state.emit_update(db, EntityId::Player, PlayerAttributeId::PlaybackInfo as u8);
        });
    }

    /// `volume` is 0.0..=1.0 (the wire value is its decimal string).
    pub fn set_volume(&self, db: &mut GattDatabase, volume: f32) {
        self.with_state(|state| {
            state.volume = volume.clamp(0.0, 1.0);
            state.emit_update(db, EntityId::Player, PlayerAttributeId::Volume as u8);
        });
    }

    pub fn set_queue(&self, db: &mut GattDatabase, index: u32, count: u32) {
        self.with_state(|state| {
            state.queue_index = index;
            state.queue_count = count;
            state.emit_update(db, EntityId::Queue, QueueAttributeId::Index as u8);
            state.emit_update(db, EntityId::Queue, QueueAttributeId::Count as u8);
        });
    }

    pub fn set_shuffle_mode(&self, db: &mut GattDatabase, mode: ShuffleMode) {
        self.with_state(|state| {
            state.shuffle_mode = mode;
            state.emit_update(db, EntityId::Queue, QueueAttributeId::ShuffleMode as u8);
        });
    }

    pub fn set_repeat_mode(&self, db: &mut GattDatabase, mode: RepeatMode) {
        self.with_state(|state| {
            state.repeat_mode = mode;
            state.emit_update(db, EntityId::Queue, QueueAttributeId::RepeatMode as u8);
        });
    }

    /// Host-side track change: publishes new metadata and restarts playback time.
    /// `duration` is in seconds.
    pub fn set_track(
        &self,
        db: &mut GattDatabase,
        artist: &str,
        album: &str,
        title: &str,
        duration: f32,
    ) {
        self.with_state(|state| {
            state.track_artist = artist.to_string();
            state.track_album = album.to_string();
            state.track_title = title.to_string();
            state.track_duration = duration;
            state.elapsed_time = 0.0;
            state.emit_update(db, EntityId::Track, TrackAttributeId::Artist as u8);
            state.emit_update(db, EntityId::Track, TrackAttributeId::Album as u8);
            state.emit_update(db, EntityId::Track, TrackAttributeId::Title as u8);
            state.emit_update(db, EntityId::Track, TrackAttributeId::Duration as u8);
            state.emit_update(db, EntityId::Player, PlayerAttributeId::PlaybackInfo as u8);
        });
    }

    pub fn playback_state(&self) -> PlaybackState {
        self.with_state(|state| state.playback_state)
    }

    pub fn playback_info(&self) -> PlaybackInfo {
        self.with_state(|state| state.playback_info())
    }

    pub fn volume(&self) -> f32 {
        self.with_state(|state| state.volume)
    }

    pub fn queue_index(&self) -> u32 {
        self.with_state(|state| state.queue_index)
    }

    pub fn shuffle_mode(&self) -> ShuffleMode {
        self.with_state(|state| state.shuffle_mode)
    }

    pub fn repeat_mode(&self) -> RepeatMode {
        self.with_state(|state| state.repeat_mode)
    }

    pub fn supported_commands(&self) -> Vec<RemoteCommandId> {
        self.with_state(|state| state.supported_commands.clone())
    }

    /// Drains the Entity Update notification payloads produced since the last
    /// call, in emission order.
    pub fn take_entity_updates(&self) -> Vec<Vec<u8>> {
        self.with_state(|state| std::mem::take(&mut state.pending_entity_updates))
    }
}

// ---------------------------------------------------------------------------
// Accessory (Media Remote) side
// ---------------------------------------------------------------------------

/// One parsed Entity Update notification. `attribute_id` stays raw because its
/// meaning depends on `entity`; `value` stays bytes because a TRUNCATED value can
/// end mid-UTF-8-sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityUpdate {
    pub entity: EntityId,
    pub attribute_id: u8,
    pub flags: u8,
    pub value: Vec<u8>,
}

impl EntityUpdate {
    pub fn truncated(&self) -> bool {
        self.flags & entity_update_flags::TRUNCATED != 0
    }
}

/// Sans-IO Media Remote (accessory) engine: builds Remote Command / Entity Update
/// / Entity Attribute write payloads and parses notifications, caching the typed
/// media state they carry.
#[derive(Debug)]
pub struct AmsClient {
    pub supported_commands: Vec<RemoteCommandId>,
    pub player_name: String,
    pub playback_info: PlaybackInfo,
    pub volume: f32,
    pub queue_index: u32,
    pub queue_count: u32,
    pub shuffle_mode: ShuffleMode,
    pub repeat_mode: RepeatMode,
    pub track_artist: String,
    pub track_album: String,
    pub track_title: String,
    pub track_duration: f32,
}

impl Default for AmsClient {
    fn default() -> Self {
        Self {
            supported_commands: Vec::new(),
            player_name: String::new(),
            playback_info: PlaybackInfo {
                playback_state: PlaybackState::Paused,
                playback_rate: 0.0,
                elapsed_time: 0.0,
            },
            volume: 1.0,
            queue_index: 0,
            queue_count: 0,
            shuffle_mode: ShuffleMode::Off,
            repeat_mode: RepeatMode::Off,
            track_artist: String::new(),
            track_album: String::new(),
            track_title: String::new(),
            track_duration: 0.0,
        }
    }
}

impl AmsClient {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builds the Remote Command write payload for `command`.
    pub fn create_remote_command(&self, command: RemoteCommandId) -> Vec<u8> {
        vec![command as u8]
    }

    /// Builds the Entity Update subscription write payload for `attribute_ids` of
    /// `entity`.
    pub fn create_entity_update_subscription(
        &self,
        entity: EntityId,
        attribute_ids: &[u8],
    ) -> Vec<u8> {
        let mut payload = Vec::with_capacity(1 + attribute_ids.len());
        payload.push(entity as u8);
        payload.extend_from_slice(attribute_ids);
        payload
    }

    /// Builds the Entity Attribute selection write payload; a follow-up read of
    /// the characteristic returns the full value.
    pub fn create_entity_attribute_selector(&self, entity: EntityId, attribute_id: u8) -> Vec<u8> {
        vec![entity as u8, attribute_id]
    }

    /// Processes a Remote Command notification: the list of RemoteCommandIDs the
    /// MS currently supports (unknown bytes are skipped, as a newer iOS may send
    /// commands this accessory doesn't know).
    pub fn on_remote_command_notification(&mut self, data: &[u8]) {
        self.supported_commands = data
            .iter()
            .filter_map(|&b| RemoteCommandId::from_u8(b))
            .collect();
    }

    /// Processes an Entity Update notification, caching the value it carries
    /// unless it is truncated (fetch a truncated attribute's full value through
    /// the Entity Attribute characteristic and [`Self::cache_full_value`]).
    pub fn on_entity_update_notification(&mut self, data: &[u8]) -> Option<EntityUpdate> {
        if data.len() < 3 {
            return None;
        }
        let update = EntityUpdate {
            entity: EntityId::from_u8(data[0])?,
            attribute_id: data[1],
            flags: data[2],
            value: data[3..].to_vec(),
        };
        if !update.truncated() {
            self.cache_full_value(update.entity, update.attribute_id, &update.value);
        }
        Some(update)
    }

    /// Caches an attribute value read back through the Entity Attribute
    /// characteristic (or delivered untruncated in an update).
    pub fn cache_full_value(&mut self, entity: EntityId, attribute_id: u8, value: &[u8]) {
        let Ok(text) = std::str::from_utf8(value) else {
            return;
        };
        match entity {
            EntityId::Player => match PlayerAttributeId::from_u8(attribute_id) {
                Some(PlayerAttributeId::Name) => self.player_name = text.to_string(),
                Some(PlayerAttributeId::PlaybackInfo) => {
                    if let Some(info) = PlaybackInfo::from_wire(text) {
                        self.playback_info = info;
                    }
                }
                Some(PlayerAttributeId::Volume) => {
                    if let Ok(volume) = text.parse() {
                        self.volume = volume;
                    }
                }
                None => {}
            },
            EntityId::Queue => match QueueAttributeId::from_u8(attribute_id) {
                Some(QueueAttributeId::Index) => {
                    if let Ok(index) = text.parse() {
                        self.queue_index = index;
                    }
                }
                Some(QueueAttributeId::Count) => {
                    if let Ok(count) = text.parse() {
                        self.queue_count = count;
                    }
                }
                Some(QueueAttributeId::ShuffleMode) => {
                    if let Some(mode) = text.parse().ok().and_then(ShuffleMode::from_u8) {
                        self.shuffle_mode = mode;
                    }
                }
                Some(QueueAttributeId::RepeatMode) => {
                    if let Some(mode) = text.parse().ok().and_then(RepeatMode::from_u8) {
                        self.repeat_mode = mode;
                    }
                }
                None => {}
            },
            EntityId::Track => match TrackAttributeId::from_u8(attribute_id) {
                Some(TrackAttributeId::Artist) => self.track_artist = text.to_string(),
                Some(TrackAttributeId::Album) => self.track_album = text.to_string(),
                Some(TrackAttributeId::Title) => self.track_title = text.to_string(),
                Some(TrackAttributeId::Duration) => {
                    if let Ok(duration) = text.parse() {
                        self.track_duration = duration;
                    }
                }
                None => {}
            },
        }
    }
}
