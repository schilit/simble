// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Media Control Profile (MCP): Media Control Service (MCS, UUID 0x1848) and its Generic
//! Media Control Service variant (GMCS, UUID 0x1849).
//!
//! Models one media player: media state (MCS Section 3.17), current-track title/
//! duration/position characteristics, and the Media Control Point (MCS Section 3.18)
//! whose opcodes drive a play/pause/seek state machine. LE Audio devices expose the
//! device-wide player as GMCS ([`MediaControlService::register_generic`]); app-specific
//! players use MCS proper ([`MediaControlService::register`]).
//!
//! The Media Control Point is wired through `AttributeHandler`, so an ordinary
//! `GattDatabase::write` to it runs the opcode. Per MCS Section 3.18.2 the write
//! itself always succeeds and the outcome is reported in a `[Requested_Opcode,
//! Result_Code]` notification; Simble exposes that payload via
//! [`MediaControlService::last_control_point_notification`], so `on_write` returns `Ok`
//! even for a rejected opcode.

use crate::att::error_code as att_error_code;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use std::sync::{Arc, Mutex};

/// MCS/GMCS Service and characteristic UUIDs.
pub mod mcp_uuid {
    use crate::types::Uuid;

    /// Media Control Service UUID.
    pub const MEDIA_CONTROL_SERVICE: Uuid = Uuid::Uuid16(0x1848);
    /// Generic Media Control Service UUID.
    pub const GENERIC_MEDIA_CONTROL_SERVICE: Uuid = Uuid::Uuid16(0x1849);
    /// Media Player Name characteristic UUID.
    pub const MEDIA_PLAYER_NAME: Uuid = Uuid::Uuid16(0x2B93);
    /// Track Changed characteristic UUID.
    pub const TRACK_CHANGED: Uuid = Uuid::Uuid16(0x2B96);
    /// Track Title characteristic UUID.
    pub const TRACK_TITLE: Uuid = Uuid::Uuid16(0x2B97);
    /// Track Duration characteristic UUID.
    pub const TRACK_DURATION: Uuid = Uuid::Uuid16(0x2B98);
    /// Track Position characteristic UUID.
    pub const TRACK_POSITION: Uuid = Uuid::Uuid16(0x2B99);
    /// Media State characteristic UUID.
    pub const MEDIA_STATE: Uuid = Uuid::Uuid16(0x2BA3);
    /// Media Control Point characteristic UUID.
    pub const MEDIA_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2BA4);
    /// Media Control Point Opcodes Supported characteristic UUID.
    pub const MEDIA_CONTROL_POINT_OPCODES_SUPPORTED: Uuid = Uuid::Uuid16(0x2BA5);
    /// Content Control Id characteristic UUID.
    pub const CONTENT_CONTROL_ID: Uuid = Uuid::Uuid16(0x2BBA);
}

/// Track Duration / Track Position are signed 32-bit counts of 0.01-second units; an
/// unknown duration or position is reported as -1 (MCS Sections 3.5/3.6).
pub const TRACK_LENGTH_UNKNOWN: i32 = -1;

/// Media State characteristic values (MCS Section 3.17).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
// The SIG can add a value to this field; `#[non_exhaustive]` is what stops
// that being a breaking change for every downstream `match`.
#[non_exhaustive]
pub enum MediaState {
    /// Inactive.
    Inactive = 0x00,
    /// Playing.
    Playing = 0x01,
    /// Paused.
    Paused = 0x02,
    /// Seeking.
    Seeking = 0x03,
}

impl MediaState {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x00 => Self::Inactive,
            0x01 => Self::Playing,
            0x02 => Self::Paused,
            0x03 => Self::Seeking,
            _ => return None,
        })
    }
}

/// Media Control Point opcodes (MCS Section 3.18.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
// The SIG can add a value to this field; `#[non_exhaustive]` is what stops
// that being a breaking change for every downstream `match`.
#[non_exhaustive]
pub enum MediaControlPointOpcode {
    /// Play.
    Play = 0x01,
    /// Pause.
    Pause = 0x02,
    /// Fast rewind.
    FastRewind = 0x03,
    /// Fast forward.
    FastForward = 0x04,
    /// Stop.
    Stop = 0x05,
    /// Move relative.
    MoveRelative = 0x10,
    /// Previous segment.
    PreviousSegment = 0x20,
    /// Next segment.
    NextSegment = 0x21,
    /// First segment.
    FirstSegment = 0x22,
    /// Last segment.
    LastSegment = 0x23,
    /// Goto segment.
    GotoSegment = 0x24,
    /// Previous track.
    PreviousTrack = 0x30,
    /// Next track.
    NextTrack = 0x31,
    /// First track.
    FirstTrack = 0x32,
    /// Last track.
    LastTrack = 0x33,
    /// Goto track.
    GotoTrack = 0x34,
    /// Previous group.
    PreviousGroup = 0x40,
    /// Next group.
    NextGroup = 0x41,
    /// First group.
    FirstGroup = 0x42,
    /// Last group.
    LastGroup = 0x43,
    /// Goto group.
    GotoGroup = 0x44,
}

impl MediaControlPointOpcode {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x01 => Self::Play,
            0x02 => Self::Pause,
            0x03 => Self::FastRewind,
            0x04 => Self::FastForward,
            0x05 => Self::Stop,
            0x10 => Self::MoveRelative,
            0x20 => Self::PreviousSegment,
            0x21 => Self::NextSegment,
            0x22 => Self::FirstSegment,
            0x23 => Self::LastSegment,
            0x24 => Self::GotoSegment,
            0x30 => Self::PreviousTrack,
            0x31 => Self::NextTrack,
            0x32 => Self::FirstTrack,
            0x33 => Self::LastTrack,
            0x34 => Self::GotoTrack,
            0x40 => Self::PreviousGroup,
            0x41 => Self::NextGroup,
            0x42 => Self::FirstGroup,
            0x43 => Self::LastGroup,
            0x44 => Self::GotoGroup,
            _ => return None,
        })
    }

    /// This opcode's bit in the Media Control Point Opcodes Supported bitmask (MCS
    /// Section 3.19 assigns bits in opcode listing order, not by opcode value).
    pub fn supported_bit(self) -> u32 {
        match self {
            Self::Play => opcodes_supported::PLAY,
            Self::Pause => opcodes_supported::PAUSE,
            Self::FastRewind => opcodes_supported::FAST_REWIND,
            Self::FastForward => opcodes_supported::FAST_FORWARD,
            Self::Stop => opcodes_supported::STOP,
            Self::MoveRelative => opcodes_supported::MOVE_RELATIVE,
            Self::PreviousSegment => opcodes_supported::PREVIOUS_SEGMENT,
            Self::NextSegment => opcodes_supported::NEXT_SEGMENT,
            Self::FirstSegment => opcodes_supported::FIRST_SEGMENT,
            Self::LastSegment => opcodes_supported::LAST_SEGMENT,
            Self::GotoSegment => opcodes_supported::GOTO_SEGMENT,
            Self::PreviousTrack => opcodes_supported::PREVIOUS_TRACK,
            Self::NextTrack => opcodes_supported::NEXT_TRACK,
            Self::FirstTrack => opcodes_supported::FIRST_TRACK,
            Self::LastTrack => opcodes_supported::LAST_TRACK,
            Self::GotoTrack => opcodes_supported::GOTO_TRACK,
            Self::PreviousGroup => opcodes_supported::PREVIOUS_GROUP,
            Self::NextGroup => opcodes_supported::NEXT_GROUP,
            Self::FirstGroup => opcodes_supported::FIRST_GROUP,
            Self::LastGroup => opcodes_supported::LAST_GROUP,
            Self::GotoGroup => opcodes_supported::GOTO_GROUP,
        }
    }
}

/// Result codes carried in the Media Control Point notification (MCS Section 3.18.2).
pub mod result_code {
    /// Success.
    pub const SUCCESS: u8 = 0x01;
    /// Opcode Not Supported.
    pub const OPCODE_NOT_SUPPORTED: u8 = 0x02;
    /// Media Player Inactive.
    pub const MEDIA_PLAYER_INACTIVE: u8 = 0x03;
    /// Command Cannot Be Completed.
    pub const COMMAND_CANNOT_BE_COMPLETED: u8 = 0x04;
}

/// Media Control Point Opcodes Supported bitmask (MCS Section 3.19).
pub mod opcodes_supported {
    // fmt: off
    /// Play.
    pub const PLAY: u32 = 1 << 0;
    /// Pause.
    pub const PAUSE: u32 = 1 << 1;
    /// Fast Rewind.
    pub const FAST_REWIND: u32 = 1 << 2;
    /// Fast Forward.
    pub const FAST_FORWARD: u32 = 1 << 3;
    /// Stop.
    pub const STOP: u32 = 1 << 4;
    /// Move Relative.
    pub const MOVE_RELATIVE: u32 = 1 << 5;
    /// Previous Segment.
    pub const PREVIOUS_SEGMENT: u32 = 1 << 6;
    /// Next Segment.
    pub const NEXT_SEGMENT: u32 = 1 << 7;
    /// First Segment.
    pub const FIRST_SEGMENT: u32 = 1 << 8;
    /// Last Segment.
    pub const LAST_SEGMENT: u32 = 1 << 9;
    /// Goto Segment.
    pub const GOTO_SEGMENT: u32 = 1 << 10;
    /// Previous Track.
    pub const PREVIOUS_TRACK: u32 = 1 << 11;
    /// Next Track.
    pub const NEXT_TRACK: u32 = 1 << 12;
    /// First Track.
    pub const FIRST_TRACK: u32 = 1 << 13;
    /// Last Track.
    pub const LAST_TRACK: u32 = 1 << 14;
    /// Goto Track.
    pub const GOTO_TRACK: u32 = 1 << 15;
    /// Previous Group.
    pub const PREVIOUS_GROUP: u32 = 1 << 16;
    /// Next Group.
    pub const NEXT_GROUP: u32 = 1 << 17;
    /// First Group.
    pub const FIRST_GROUP: u32 = 1 << 18;
    /// Last Group.
    pub const LAST_GROUP: u32 = 1 << 19;
    /// Goto Group.
    pub const GOTO_GROUP: u32 = 1 << 20;
    // fmt: on

    /// What Simble's simulated player implements: basic transport control plus track
    /// navigation. Segment and group navigation would need an object-transfer-backed
    /// track model the simulator doesn't have.
    pub const DEFAULT: u32 = PLAY
        | PAUSE
        | FAST_REWIND
        | FAST_FORWARD
        | STOP
        | MOVE_RELATIVE
        | PREVIOUS_TRACK
        | NEXT_TRACK
        | FIRST_TRACK
        | LAST_TRACK;
}

/// Handles this `register()` call allocated, shared with the Control Point handler so
/// state-machine transitions can republish the affected characteristic values.
#[derive(Debug, Clone, Copy)]
struct PlayerHandles {
    track_changed_value_handle: u16,
    track_title_value_handle: u16,
    track_duration_value_handle: u16,
    track_position_value_handle: u16,
    media_state_value_handle: u16,
    control_point_value_handle: u16,
}

/// Mutable media player state shared between the service handle and the Control Point
/// handler (see [`MediaControlService`]). Track position deliberately lives only in the
/// Track Position attribute value, not here: clients seek by writing that attribute
/// directly (MCS Section 3.6), and mirroring it in this struct would let the two drift.
#[derive(Debug)]
struct MediaPlayerState {
    media_state: MediaState,
    track_duration: i32,
    supported_opcodes: u32,
    /// Payload of the most recent `[Requested_Opcode, Result_Code]` Control Point
    /// notification (MCS Section 3.18.2), kept here because the handler mechanism can't
    /// store it as the Control Point's own attribute value (the handled attribute is
    /// detached from the database during dispatch).
    last_control_point_notification: Option<[u8; 2]>,
    handles: PlayerHandles,
}

impl MediaPlayerState {
    fn set_media_state(&mut self, db: &mut GattDatabase, media_state: MediaState) {
        self.media_state = media_state;
        let _ = db.set_value(self.handles.media_state_value_handle, &[media_state as u8]);
    }

    /// Current position per the Track Position attribute, tolerating a client that
    /// stored fewer than 4 bytes (Write Without Response can't be rejected).
    fn track_position(&self, db: &GattDatabase) -> i32 {
        db.read(self.handles.track_position_value_handle, 0)
            .ok()
            .and_then(|value| value.get(..4))
            .map(|bytes| i32::from_le_bytes(bytes.try_into().expect("4-byte slice")))
            .unwrap_or(0)
    }

    fn set_track_position(&self, db: &mut GattDatabase, position: i32) {
        // Clamp into the current track; an unknown duration can't bound the position.
        let position = if self.track_duration == TRACK_LENGTH_UNKNOWN {
            position.max(0)
        } else {
            position.clamp(0, self.track_duration)
        };
        let _ = db.set_value(
            self.handles.track_position_value_handle,
            &position.to_le_bytes(),
        );
    }

    /// Applies one Media Control Point opcode, returning the Result_Code for the
    /// notification (MCS Section 3.18.2).
    fn apply_opcode(
        &mut self,
        db: &mut GattDatabase,
        op: MediaControlPointOpcode,
        param: Option<i32>,
    ) -> u8 {
        if self.supported_opcodes & op.supported_bit() == 0 {
            return result_code::OPCODE_NOT_SUPPORTED;
        }
        if self.media_state == MediaState::Inactive {
            return result_code::MEDIA_PLAYER_INACTIVE;
        }
        match op {
            MediaControlPointOpcode::Play => self.set_media_state(db, MediaState::Playing),
            MediaControlPointOpcode::Pause => self.set_media_state(db, MediaState::Paused),
            MediaControlPointOpcode::FastRewind | MediaControlPointOpcode::FastForward => {
                self.set_media_state(db, MediaState::Seeking)
            }
            MediaControlPointOpcode::Stop => {
                self.set_media_state(db, MediaState::Paused);
                self.set_track_position(db, 0);
            }
            MediaControlPointOpcode::MoveRelative => {
                // Move Relative carries a signed 32-bit offset parameter (MCS 3.18.1.5).
                let Some(offset) = param else {
                    return result_code::COMMAND_CANNOT_BE_COMPLETED;
                };
                let position = self.track_position(db).saturating_add(offset);
                self.set_track_position(db, position);
            }
            MediaControlPointOpcode::PreviousTrack
            | MediaControlPointOpcode::NextTrack
            | MediaControlPointOpcode::FirstTrack
            | MediaControlPointOpcode::LastTrack
            | MediaControlPointOpcode::GotoTrack => {
                // The simulated player has no track list; any track change is modeled as
                // restarting playback at position 0 and signaling Track Changed
                // (MCS Section 3.4: notification-only characteristic, empty value).
                self.set_track_position(db, 0);
                let _ = db.set_value(self.handles.track_changed_value_handle, &[]);
            }
            // Segment/group navigation is excluded from `opcodes_supported::DEFAULT`;
            // reachable only when a caller opts into bits the simulator can't honor.
            _ => return result_code::COMMAND_CANNOT_BE_COMPLETED,
        }
        result_code::SUCCESS
    }
}

/// `AttributeHandler` for the Media Control Point.
#[derive(Debug)]
struct MediaControlPointHandler {
    state: Arc<Mutex<MediaPlayerState>>,
}

impl AttributeHandler for MediaControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().expect("MCP state lock poisoned");
        let control_point_value_handle = state.handles.control_point_value_handle;

        let (&op_byte, params) = value
            .split_first()
            .ok_or(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)?;
        let result = match MediaControlPointOpcode::from_u8(op_byte) {
            None => result_code::OPCODE_NOT_SUPPORTED,
            Some(op) => {
                let param = params
                    .get(..4)
                    .map(|p| i32::from_le_bytes(p.try_into().expect("4-byte slice")));
                state.apply_opcode(db, op, param)
            }
        };

        // Retrievable via `MediaControlService::last_control_point_notification`; also
        // pushed at the stored Control Point value for symmetry with ASCS's convention,
        // though that write is a no-op while the handled attribute is detached from `db`.
        state.last_control_point_notification = Some([op_byte, result]);
        let _ = db.set_value(control_point_value_handle, &[op_byte, result]);
        Ok(())
    }
}

/// Media Control Service GATT container plus the media player state it owns.
#[derive(Debug)]
pub struct MediaControlService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Value attribute handle of the Media Player Name characteristic.
    pub media_player_name_value_handle: u16,
    /// Value attribute handle of the Track Changed characteristic.
    pub track_changed_value_handle: u16,
    /// Value attribute handle of the Track Title characteristic.
    pub track_title_value_handle: u16,
    /// Value attribute handle of the Track Duration characteristic.
    pub track_duration_value_handle: u16,
    /// Value attribute handle of the Track Position characteristic.
    pub track_position_value_handle: u16,
    /// Value attribute handle of the Media State characteristic.
    pub media_state_value_handle: u16,
    /// Value attribute handle of the Control Point characteristic.
    pub control_point_value_handle: u16,
    /// Value attribute handle of the Opcodes Supported characteristic.
    pub opcodes_supported_value_handle: u16,
    /// Value attribute handle of the Content Control Id characteristic.
    pub content_control_id_value_handle: u16,
    state: Arc<Mutex<MediaPlayerState>>,
}

impl MediaControlService {
    /// Registers an app-specific Media Control Service (UUID 0x1848). `ccid` is the
    /// Content Control ID that ties this player to audio-context metadata elsewhere in
    /// the LE Audio stack (MCS Section 3.21).
    pub fn register(db: &mut GattDatabase, player_name: &str, ccid: u8) -> Self {
        Self::register_with_uuid(db, mcp_uuid::MEDIA_CONTROL_SERVICE, player_name, ccid)
    }

    /// Registers the device-wide Generic Media Control Service (UUID 0x1849).
    pub fn register_generic(db: &mut GattDatabase, player_name: &str, ccid: u8) -> Self {
        Self::register_with_uuid(
            db,
            mcp_uuid::GENERIC_MEDIA_CONTROL_SERVICE,
            player_name,
            ccid,
        )
    }

    fn register_with_uuid(
        db: &mut GattDatabase,
        service: crate::types::Uuid,
        player_name: &str,
        ccid: u8,
    ) -> Self {
        let service_handle = db.add_service(service, true);
        let read_notify = CharacteristicProperties(
            CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
        );

        let (_, media_player_name_value_handle) = db.add_characteristic(
            mcp_uuid::MEDIA_PLAYER_NAME,
            read_notify,
            player_name.as_bytes().to_vec(),
            AttributePermissions::read_only(),
        );

        let (_, track_changed_value_handle) = db.add_characteristic(
            mcp_uuid::TRACK_CHANGED,
            CharacteristicProperties(CharacteristicProperties::NOTIFY),
            // Track Changed carries no value; it only signals (MCS Section 3.4).
            vec![],
            AttributePermissions::read_only(),
        );

        let (_, track_title_value_handle) = db.add_characteristic(
            mcp_uuid::TRACK_TITLE,
            read_notify,
            vec![],
            AttributePermissions::read_only(),
        );

        let (_, track_duration_value_handle) = db.add_characteristic(
            mcp_uuid::TRACK_DURATION,
            read_notify,
            TRACK_LENGTH_UNKNOWN.to_le_bytes().to_vec(),
            AttributePermissions::read_only(),
        );

        let (_, track_position_value_handle) = db.add_characteristic(
            mcp_uuid::TRACK_POSITION,
            CharacteristicProperties(
                CharacteristicProperties::READ
                    | CharacteristicProperties::WRITE
                    | CharacteristicProperties::WRITE_WITHOUT_RESPONSE
                    | CharacteristicProperties::NOTIFY,
            ),
            0i32.to_le_bytes().to_vec(),
            AttributePermissions::default(),
        );

        let (_, media_state_value_handle) = db.add_characteristic(
            mcp_uuid::MEDIA_STATE,
            read_notify,
            vec![MediaState::Paused as u8],
            AttributePermissions::read_only(),
        );

        let (_, control_point_value_handle) = db.add_characteristic(
            mcp_uuid::MEDIA_CONTROL_POINT,
            CharacteristicProperties(
                CharacteristicProperties::WRITE
                    | CharacteristicProperties::WRITE_WITHOUT_RESPONSE
                    | CharacteristicProperties::NOTIFY,
            ),
            vec![],
            AttributePermissions::write_only(),
        );

        let (_, opcodes_supported_value_handle) = db.add_characteristic(
            mcp_uuid::MEDIA_CONTROL_POINT_OPCODES_SUPPORTED,
            read_notify,
            opcodes_supported::DEFAULT.to_le_bytes().to_vec(),
            AttributePermissions::read_only(),
        );

        let (_, content_control_id_value_handle) = db.add_characteristic(
            mcp_uuid::CONTENT_CONTROL_ID,
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![ccid],
            AttributePermissions::read_only(),
        );

        let state = Arc::new(Mutex::new(MediaPlayerState {
            media_state: MediaState::Paused,
            track_duration: TRACK_LENGTH_UNKNOWN,
            supported_opcodes: opcodes_supported::DEFAULT,
            last_control_point_notification: None,
            handles: PlayerHandles {
                track_changed_value_handle,
                track_title_value_handle,
                track_duration_value_handle,
                track_position_value_handle,
                media_state_value_handle,
                control_point_value_handle,
            },
        }));
        db.set_handler(
            control_point_value_handle,
            Box::new(MediaControlPointHandler {
                state: Arc::clone(&state),
            }),
        )
        .expect("control point handle just allocated");

        Self {
            service_handle,
            media_player_name_value_handle,
            track_changed_value_handle,
            track_title_value_handle,
            track_duration_value_handle,
            track_position_value_handle,
            media_state_value_handle,
            control_point_value_handle,
            opcodes_supported_value_handle,
            content_control_id_value_handle,
            state,
        }
    }

    /// Returns the media state.
    pub fn media_state(&self) -> MediaState {
        self.state
            .lock()
            .expect("MCP state lock poisoned")
            .media_state
    }

    /// Current track position in 0.01-second units; the Track Position attribute in
    /// `db` is the source of truth because clients seek by writing it directly.
    pub fn track_position(&self, db: &GattDatabase) -> i32 {
        self.state
            .lock()
            .expect("MCP state lock poisoned")
            .track_position(db)
    }

    /// The `[Requested_Opcode, Result_Code]` payload of the most recent Media Control
    /// Point notification (MCS Section 3.18.2), or `None` before the first operation.
    pub fn last_control_point_notification(&self) -> Option<[u8; 2]> {
        self.state
            .lock()
            .expect("MCP state lock poisoned")
            .last_control_point_notification
    }

    /// Host-side (media player app) media-state change, e.g. going Inactive when no
    /// media application is running.
    pub fn set_media_state(&self, db: &mut GattDatabase, media_state: MediaState) {
        let mut state = self.state.lock().expect("MCP state lock poisoned");
        state.set_media_state(db, media_state);
    }

    /// Host-side track change: publishes the new title/duration, rewinds the position,
    /// and signals Track Changed — what a real player does when the current track moves
    /// on (MCS Sections 3.4-3.6). `duration` is in 0.01-second units, or
    /// [`TRACK_LENGTH_UNKNOWN`].
    pub fn set_track(&self, db: &mut GattDatabase, title: &str, duration: i32) {
        let mut state = self.state.lock().expect("MCP state lock poisoned");
        state.track_duration = duration;
        let _ = db.set_value(state.handles.track_title_value_handle, title.as_bytes());
        let _ = db.set_value(
            state.handles.track_duration_value_handle,
            &duration.to_le_bytes(),
        );
        state.set_track_position(db, 0);
        let _ = db.set_value(state.handles.track_changed_value_handle, &[]);
    }
}
