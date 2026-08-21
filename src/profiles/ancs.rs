// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Apple Notification Center Service (ANCS).
//!
//! ANCS is Apple's proprietary 128-bit-UUID GATT service through which an iOS device
//! (the Notification Provider, NP) publishes its notification center to an accessory
//! (the Notification Consumer, NC). Simble simulates the iPhone side:
//! [`NotificationCenterService`] registers the NP's three characteristics
//! (ANCS Specification, "The Apple Notification Center Service"):
//!
//! - **Notification Source** (notify-only): compact 8-byte `[EventID, EventFlags,
//!   CategoryID, CategoryCount, NotificationUID]` records announcing added / modified /
//!   removed notifications.
//! - **Control Point** (write): Get Notification Attributes, Get App Attributes and
//!   Perform Notification Action commands, dispatched through an `AttributeHandler`.
//! - **Data Source** (notify-only): responses to Control Point commands, carrying the
//!   requested attributes as `[AttributeID, Length (u16), Value]` tuples.
//!
//! Host-side calls ([`NotificationCenterService::post_notification`] etc.) mutate a
//! shared notification table and park the resulting Notification Source packet;
//! Control Point writes park their Data Source response the same way (see
//! [`NotificationCenterService::last_data_source_notification`], the convention
//! `mcp::MediaControlService::last_control_point_notification` established).
//!
//! [`AncsClient`] is the accessory (NC) side: sans-IO command builders and
//! notification parsers shaped like `crate::client::GattClient`, including the
//! fragment reassembly the Data Source needs when responses span several
//! MTU-sized notifications.

use crate::att::error_code as att_error_code;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// ANCS service and characteristic UUIDs (Apple-assigned, 128-bit; stored
/// little-endian as `Uuid::Uuid128` expects).
pub mod ancs_uuid {
    use crate::types::Uuid;

    /// 7905F431-B5CE-4E99-A40F-4B1E122D00D0
    pub const APPLE_NOTIFICATION_CENTER_SERVICE: Uuid = Uuid::Uuid128([
        0xD0, 0x00, 0x2D, 0x12, 0x1E, 0x4B, 0x0F, 0xA4, 0x99, 0x4E, 0xCE, 0xB5, 0x31, 0xF4, 0x05,
        0x79,
    ]);
    /// 9FBF120D-6301-42D9-8C58-25E699A21DBD
    pub const NOTIFICATION_SOURCE: Uuid = Uuid::Uuid128([
        0xBD, 0x1D, 0xA2, 0x99, 0xE6, 0x25, 0x58, 0x8C, 0xD9, 0x42, 0x01, 0x63, 0x0D, 0x12, 0xBF,
        0x9F,
    ]);
    /// 69D1D8F3-45E1-49A8-9821-9BBDFDAAD9D9
    pub const CONTROL_POINT: Uuid = Uuid::Uuid128([
        0xD9, 0xD9, 0xAA, 0xFD, 0xBD, 0x9B, 0x21, 0x98, 0xA8, 0x49, 0xE1, 0x45, 0xF3, 0xD8, 0xD1,
        0x69,
    ]);
    /// 22EAC6E9-24D6-4BB5-BE44-B36ACE7C7BFB
    pub const DATA_SOURCE: Uuid = Uuid::Uuid128([
        0xFB, 0x7B, 0x7C, 0xCE, 0x6A, 0xB3, 0x44, 0xBE, 0xB5, 0x4B, 0xD6, 0x24, 0xE9, 0xC6, 0xEA,
        0x22,
    ]);
}

/// ANCS-specific ATT application error codes returned for rejected Control Point
/// writes (ANCS Specification, "Error Codes").
pub mod error_code {
    /// The commandID is not recognized by the NP.
    pub const UNKNOWN_COMMAND: u8 = 0xA0;
    /// The command is improperly formatted.
    pub const INVALID_COMMAND: u8 = 0xA1;
    /// A parameter does not refer to an existing object on the NP.
    pub const INVALID_PARAMETER: u8 = 0xA2;
    /// The action was not performed.
    pub const ACTION_FAILED: u8 = 0xA3;
}

/// EventID values in Notification Source packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum EventId {
    /// Notification added.
    NotificationAdded = 0,
    /// Notification modified.
    NotificationModified = 1,
    /// Notification removed.
    NotificationRemoved = 2,
}

impl EventId {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::NotificationAdded,
            1 => Self::NotificationModified,
            2 => Self::NotificationRemoved,
            _ => return None,
        })
    }
}

/// EventFlags bitmask in Notification Source packets.
pub mod event_flags {
    /// Silent flag bit.
    pub const SILENT: u8 = 1 << 0;
    /// Important flag bit.
    pub const IMPORTANT: u8 = 1 << 1;
    /// Pre Existing flag bit.
    pub const PRE_EXISTING: u8 = 1 << 2;
    /// Positive Action flag bit.
    pub const POSITIVE_ACTION: u8 = 1 << 3;
    /// Negative Action flag bit.
    pub const NEGATIVE_ACTION: u8 = 1 << 4;
}

/// CategoryID values in Notification Source packets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum CategoryId {
    #[default]
    /// Other.
    Other = 0,
    /// Incoming call.
    IncomingCall = 1,
    /// Missed call.
    MissedCall = 2,
    /// Voicemail.
    Voicemail = 3,
    /// Social.
    Social = 4,
    /// Schedule.
    Schedule = 5,
    /// Email.
    Email = 6,
    /// News.
    News = 7,
    /// Health and fitness.
    HealthAndFitness = 8,
    /// Business and finance.
    BusinessAndFinance = 9,
    /// Location.
    Location = 10,
    /// Entertainment.
    Entertainment = 11,
}

impl CategoryId {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Other,
            1 => Self::IncomingCall,
            2 => Self::MissedCall,
            3 => Self::Voicemail,
            4 => Self::Social,
            5 => Self::Schedule,
            6 => Self::Email,
            7 => Self::News,
            8 => Self::HealthAndFitness,
            9 => Self::BusinessAndFinance,
            10 => Self::Location,
            11 => Self::Entertainment,
            _ => return None,
        })
    }
}

/// CommandID values written to the Control Point.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CommandId {
    /// Get notification attributes.
    GetNotificationAttributes = 0,
    /// Get app attributes.
    GetAppAttributes = 1,
    /// Perform notification action.
    PerformNotificationAction = 2,
}

impl CommandId {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::GetNotificationAttributes,
            1 => Self::GetAppAttributes,
            2 => Self::PerformNotificationAction,
            _ => return None,
        })
    }
}

/// Notification AttributeID values for Get Notification Attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotificationAttributeId {
    /// App identifier.
    AppIdentifier = 0,
    /// Title.
    Title = 1,
    /// Subtitle.
    Subtitle = 2,
    /// Message.
    Message = 3,
    /// Message size.
    MessageSize = 4,
    /// Date.
    Date = 5,
    /// Positive action label.
    PositiveActionLabel = 6,
    /// Negative action label.
    NegativeActionLabel = 7,
}

impl NotificationAttributeId {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::AppIdentifier,
            1 => Self::Title,
            2 => Self::Subtitle,
            3 => Self::Message,
            4 => Self::MessageSize,
            5 => Self::Date,
            6 => Self::PositiveActionLabel,
            7 => Self::NegativeActionLabel,
            _ => return None,
        })
    }

    /// Title, Subtitle and Message are the only attributes whose request carries a
    /// 2-byte max-length parameter (ANCS Specification, "Get Notification Attributes").
    pub fn has_max_length(self) -> bool {
        matches!(self, Self::Title | Self::Subtitle | Self::Message)
    }
}

/// App AttributeID values for Get App Attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AppAttributeId {
    /// Display name.
    DisplayName = 0,
}

impl AppAttributeId {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::DisplayName),
            _ => None,
        }
    }
}

/// ActionID values for Perform Notification Action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ActionId {
    /// Positive.
    Positive = 0,
    /// Negative.
    Negative = 1,
}

impl ActionId {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0 => Self::Positive,
            1 => Self::Negative,
            _ => return None,
        })
    }
}

/// One 8-byte Notification Source packet: `[EventID, EventFlags, CategoryID,
/// CategoryCount, NotificationUID (u32 LE)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationEvent {
    /// Event Id.
    pub event_id: EventId,
    /// Event Flags.
    pub event_flags: u8,
    /// Category Id.
    pub category_id: CategoryId,
    /// Category Count.
    pub category_count: u8,
    /// Notification Uid.
    pub notification_uid: u32,
}

impl NotificationEvent {
    /// Packet Size.
    pub const PACKET_SIZE: usize = 8;

    /// Serializes to the characteristic wire format.
    pub fn to_bytes(self) -> [u8; Self::PACKET_SIZE] {
        let uid = self.notification_uid.to_le_bytes();
        [
            self.event_id as u8,
            self.event_flags,
            self.category_id as u8,
            self.category_count,
            uid[0],
            uid[1],
            uid[2],
            uid[3],
        ]
    }

    /// Parses a value from its wire bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        let data: &[u8; Self::PACKET_SIZE] = data.get(..Self::PACKET_SIZE)?.try_into().ok()?;
        Some(Self {
            event_id: EventId::from_u8(data[0])?,
            event_flags: data[1],
            category_id: CategoryId::from_u8(data[2])?,
            category_count: data[3],
            notification_uid: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
        })
    }
}

/// Host-side description of one notification in the simulated iPhone's notification
/// center. Empty strings model attributes the notification simply doesn't have
/// (their Data Source tuples come back with length 0). `date` is free-form but iOS
/// uses `yyyyMMdd'T'HHmmSS` (ANCS Specification, "Get Notification Attributes").
#[derive(Debug, Clone, Default)]
pub struct NotificationContent {
    /// Category Id.
    pub category_id: CategoryId,
    /// `event_flags` bits other than the action bits; POSITIVE_ACTION /
    /// NEGATIVE_ACTION are derived from the action labels below so the flags can
    /// never advertise an action the notification doesn't carry.
    pub event_flags: u8,
    /// App Identifier.
    pub app_identifier: String,
    /// Title.
    pub title: String,
    /// Subtitle.
    pub subtitle: String,
    /// Message.
    pub message: String,
    /// Date.
    pub date: String,
    /// Positive Action Label.
    pub positive_action_label: String,
    /// Negative Action Label.
    pub negative_action_label: String,
}

impl NotificationContent {
    fn effective_flags(&self) -> u8 {
        let mut flags = self.event_flags;
        if !self.positive_action_label.is_empty() {
            flags |= event_flags::POSITIVE_ACTION;
        }
        if !self.negative_action_label.is_empty() {
            flags |= event_flags::NEGATIVE_ACTION;
        }
        flags
    }

    fn attribute_value(&self, id: NotificationAttributeId) -> String {
        match id {
            NotificationAttributeId::AppIdentifier => self.app_identifier.clone(),
            NotificationAttributeId::Title => self.title.clone(),
            NotificationAttributeId::Subtitle => self.subtitle.clone(),
            NotificationAttributeId::Message => self.message.clone(),
            // Message Size is the byte length rendered as an ASCII string, like the
            // other numeric ANCS attributes (ANCS Specification, Table 3-3).
            NotificationAttributeId::MessageSize => self.message.len().to_string(),
            NotificationAttributeId::Date => self.date.clone(),
            NotificationAttributeId::PositiveActionLabel => self.positive_action_label.clone(),
            NotificationAttributeId::NegativeActionLabel => self.negative_action_label.clone(),
        }
    }
}

/// Handles shared with the Control Point handler so command processing can publish
/// Notification Source / Data Source payloads.
#[derive(Debug, Clone, Copy)]
struct AncsHandles {
    notification_source_value_handle: u16,
    data_source_value_handle: u16,
}

/// Shared NP state: the notification table, the installed-app table for Get App
/// Attributes, and the most recent notify payloads parked for host retrieval.
#[derive(Debug)]
struct AncsState {
    notifications: BTreeMap<u32, NotificationContent>,
    /// App identifier -> display name, consulted by Get App Attributes.
    apps: BTreeMap<String, String>,
    next_uid: u32,
    last_notification_source: Option<[u8; NotificationEvent::PACKET_SIZE]>,
    last_data_source: Option<Vec<u8>>,
    last_performed_action: Option<(u32, ActionId)>,
    handles: AncsHandles,
}

impl AncsState {
    fn category_count(&self, category_id: CategoryId) -> u8 {
        let count = self
            .notifications
            .values()
            .filter(|n| n.category_id == category_id)
            .count();
        // CategoryCount is a single byte; iOS saturates rather than wraps.
        count.min(u8::MAX as usize) as u8
    }

    fn emit_event(
        &mut self,
        db: &mut GattDatabase,
        event_id: EventId,
        uid: u32,
        event_flags: u8,
        category_id: CategoryId,
    ) {
        let packet = NotificationEvent {
            event_id,
            event_flags,
            category_id,
            category_count: self.category_count(category_id),
            notification_uid: uid,
        }
        .to_bytes();
        self.last_notification_source = Some(packet);
        let _ = db.set_value(self.handles.notification_source_value_handle, &packet);
    }

    fn get_notification_attributes(&self, request: &[u8]) -> Result<Vec<u8>, u8> {
        // Request layout after the CommandID: NotificationUID (u32 LE), then a list
        // of AttributeIDs where Title/Subtitle/Message carry a u16 max length.
        let uid_bytes = request.get(..4).ok_or(error_code::INVALID_COMMAND)?;
        let uid = u32::from_le_bytes(uid_bytes.try_into().expect("4-byte slice"));
        let content = self
            .notifications
            .get(&uid)
            .ok_or(error_code::INVALID_PARAMETER)?;

        let mut response = vec![CommandId::GetNotificationAttributes as u8];
        response.extend_from_slice(&uid.to_le_bytes());

        let mut rest = &request[4..];
        while let Some((&id_byte, tail)) = rest.split_first() {
            let id =
                NotificationAttributeId::from_u8(id_byte).ok_or(error_code::INVALID_PARAMETER)?;
            let (max_length, tail) = if id.has_max_length() {
                let length_bytes = tail.get(..2).ok_or(error_code::INVALID_COMMAND)?;
                let max = u16::from_le_bytes(length_bytes.try_into().expect("2-byte slice"));
                (max as usize, &tail[2..])
            } else {
                (usize::MAX, tail)
            };

            let value = content.attribute_value(id);
            // Truncation is byte-oriented on the wire; an accessory asking for a
            // short max length may receive a partial final UTF-8 sequence.
            let bytes = &value.as_bytes()[..value.len().min(max_length)];
            response.push(id as u8);
            response.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
            response.extend_from_slice(bytes);
            rest = tail;
        }
        Ok(response)
    }

    fn get_app_attributes(&self, request: &[u8]) -> Result<Vec<u8>, u8> {
        // Request layout after the CommandID: NUL-terminated app identifier, then a
        // list of bare AttributeIDs.
        let nul = request
            .iter()
            .position(|&b| b == 0)
            .ok_or(error_code::INVALID_COMMAND)?;
        let app_identifier =
            std::str::from_utf8(&request[..nul]).map_err(|_| error_code::INVALID_COMMAND)?;
        let display_name = self
            .apps
            .get(app_identifier)
            .ok_or(error_code::INVALID_PARAMETER)?;

        let mut response = vec![CommandId::GetAppAttributes as u8];
        response.extend_from_slice(app_identifier.as_bytes());
        response.push(0);
        for &id_byte in &request[nul + 1..] {
            AppAttributeId::from_u8(id_byte).ok_or(error_code::INVALID_PARAMETER)?;
            response.push(id_byte);
            response.extend_from_slice(&(display_name.len() as u16).to_le_bytes());
            response.extend_from_slice(display_name.as_bytes());
        }
        Ok(response)
    }

    fn perform_notification_action(
        &mut self,
        db: &mut GattDatabase,
        request: &[u8],
    ) -> Result<(), u8> {
        // Request layout after the CommandID: NotificationUID (u32 LE), ActionID.
        let request: &[u8; 5] = request
            .try_into()
            .map_err(|_| error_code::INVALID_COMMAND)?;
        let uid = u32::from_le_bytes([request[0], request[1], request[2], request[3]]);
        let action = ActionId::from_u8(request[4]).ok_or(error_code::INVALID_PARAMETER)?;
        let content = self
            .notifications
            .get(&uid)
            .ok_or(error_code::INVALID_PARAMETER)?;

        // A notification only carries the actions its flags advertise; asking for a
        // missing one is the "action was not performed" case.
        let required_flag = match action {
            ActionId::Positive => event_flags::POSITIVE_ACTION,
            ActionId::Negative => event_flags::NEGATIVE_ACTION,
        };
        if content.effective_flags() & required_flag == 0 {
            return Err(error_code::ACTION_FAILED);
        }

        // Performing either action dismisses the notification on iOS, so the NP
        // follows up with a Removed event.
        let content = self.notifications.remove(&uid).expect("presence checked");
        self.last_performed_action = Some((uid, action));
        self.emit_event(
            db,
            EventId::NotificationRemoved,
            uid,
            content.effective_flags(),
            content.category_id,
        );
        Ok(())
    }
}

/// `AttributeHandler` for the ANCS Control Point.
#[derive(Debug)]
struct ControlPointHandler {
    state: Arc<Mutex<AncsState>>,
}

impl AttributeHandler for ControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().expect("ANCS state lock poisoned");
        let (&command_byte, request) = value
            .split_first()
            .ok_or(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)?;
        // Unlike MCS-style control points, ANCS reports failures as ATT application
        // errors on the write itself, not in a result notification.
        let command = CommandId::from_u8(command_byte).ok_or(error_code::UNKNOWN_COMMAND)?;
        let response = match command {
            CommandId::GetNotificationAttributes => state.get_notification_attributes(request)?,
            CommandId::GetAppAttributes => state.get_app_attributes(request)?,
            CommandId::PerformNotificationAction => {
                // Perform Notification Action has no Data Source response.
                return state.perform_notification_action(db, request);
            }
        };
        let data_source_value_handle = state.handles.data_source_value_handle;
        state.last_data_source = Some(response.clone());
        let _ = db.set_value(data_source_value_handle, &response);
        Ok(())
    }
}

/// ANCS Notification Provider (simulated iPhone) GATT container plus the
/// notification-center state it owns.
#[derive(Debug)]
pub struct NotificationCenterService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Value attribute handle of the Notification Source characteristic.
    pub notification_source_value_handle: u16,
    /// Value attribute handle of the Control Point characteristic.
    pub control_point_value_handle: u16,
    /// Value attribute handle of the Data Source characteristic.
    pub data_source_value_handle: u16,
    state: Arc<Mutex<AncsState>>,
}

impl NotificationCenterService {
    /// Registers the service and its characteristics into a GATT database.
    pub fn register(db: &mut GattDatabase) -> Self {
        let service_handle = db.add_service(ancs_uuid::APPLE_NOTIFICATION_CENTER_SERVICE, true);

        // Notification Source and Data Source are notify-only toward the accessory;
        // read permission is host-side only, letting tests inspect the parked
        // notify payloads (same convention as MCS's Track Changed).
        let (_, notification_source_value_handle) = db.add_characteristic(
            ancs_uuid::NOTIFICATION_SOURCE,
            CharacteristicProperties(CharacteristicProperties::NOTIFY),
            vec![],
            AttributePermissions::read_only(),
        );

        let (_, control_point_value_handle) = db.add_characteristic(
            ancs_uuid::CONTROL_POINT,
            CharacteristicProperties(CharacteristicProperties::WRITE),
            vec![],
            AttributePermissions::write_only(),
        );

        let (_, data_source_value_handle) = db.add_characteristic(
            ancs_uuid::DATA_SOURCE,
            CharacteristicProperties(CharacteristicProperties::NOTIFY),
            vec![],
            AttributePermissions::read_only(),
        );

        let state = Arc::new(Mutex::new(AncsState {
            notifications: BTreeMap::new(),
            apps: BTreeMap::new(),
            next_uid: 0,
            last_notification_source: None,
            last_data_source: None,
            last_performed_action: None,
            handles: AncsHandles {
                notification_source_value_handle,
                data_source_value_handle,
            },
        }));
        db.set_handler(
            control_point_value_handle,
            Box::new(ControlPointHandler {
                state: Arc::clone(&state),
            }),
        )
        .expect("control point handle just allocated");

        Self {
            service_handle,
            notification_source_value_handle,
            control_point_value_handle,
            data_source_value_handle,
            state,
        }
    }

    /// Registers an "installed app" so Get App Attributes can resolve
    /// `app_identifier` to a display name.
    pub fn register_app(&self, app_identifier: &str, display_name: &str) {
        self.state
            .lock()
            .expect("ANCS state lock poisoned")
            .apps
            .insert(app_identifier.to_string(), display_name.to_string());
    }

    /// Host-side notification post. Returns the assigned NotificationUID and emits a
    /// Notification Added event on the Notification Source.
    pub fn post_notification(&self, db: &mut GattDatabase, content: NotificationContent) -> u32 {
        let mut state = self.state.lock().expect("ANCS state lock poisoned");
        let uid = state.next_uid;
        state.next_uid = state.next_uid.wrapping_add(1);
        let flags = content.effective_flags();
        let category_id = content.category_id;
        state.notifications.insert(uid, content);
        state.emit_event(db, EventId::NotificationAdded, uid, flags, category_id);
        uid
    }

    /// Host-side notification update: replaces `uid`'s content and emits a
    /// Notification Modified event. Returns `false` if `uid` isn't in the table.
    pub fn modify_notification(
        &self,
        db: &mut GattDatabase,
        uid: u32,
        content: NotificationContent,
    ) -> bool {
        let mut state = self.state.lock().expect("ANCS state lock poisoned");
        if !state.notifications.contains_key(&uid) {
            return false;
        }
        let flags = content.effective_flags();
        let category_id = content.category_id;
        state.notifications.insert(uid, content);
        state.emit_event(db, EventId::NotificationModified, uid, flags, category_id);
        true
    }

    /// Host-side notification dismissal: drops `uid` from the table and emits a
    /// Notification Removed event. Returns `false` if `uid` isn't in the table.
    pub fn remove_notification(&self, db: &mut GattDatabase, uid: u32) -> bool {
        let mut state = self.state.lock().expect("ANCS state lock poisoned");
        let Some(content) = state.notifications.remove(&uid) else {
            return false;
        };
        state.emit_event(
            db,
            EventId::NotificationRemoved,
            uid,
            content.effective_flags(),
            content.category_id,
        );
        true
    }

    /// Number of notifications currently in the table.
    pub fn notification_count(&self) -> usize {
        self.state
            .lock()
            .expect("ANCS state lock poisoned")
            .notifications
            .len()
    }

    /// The most recent 8-byte Notification Source packet, or `None` before the first
    /// event.
    pub fn last_notification_source_packet(&self) -> Option<[u8; NotificationEvent::PACKET_SIZE]> {
        self.state
            .lock()
            .expect("ANCS state lock poisoned")
            .last_notification_source
    }

    /// The most recent Data Source response payload, or `None` before the first
    /// successful Get Notification Attributes / Get App Attributes command.
    pub fn last_data_source_notification(&self) -> Option<Vec<u8>> {
        self.state
            .lock()
            .expect("ANCS state lock poisoned")
            .last_data_source
            .clone()
    }

    /// The `(NotificationUID, ActionID)` of the most recent successful Perform
    /// Notification Action command.
    pub fn last_performed_action(&self) -> Option<(u32, ActionId)> {
        self.state
            .lock()
            .expect("ANCS state lock poisoned")
            .last_performed_action
    }
}

// ---------------------------------------------------------------------------
// Accessory (Notification Consumer) side
// ---------------------------------------------------------------------------

/// One attribute request for [`AncsClient::create_get_notification_attributes_command`].
#[derive(Debug, Clone, Copy)]
pub struct NotificationAttributeRequest {
    /// Attribute Id.
    pub attribute_id: NotificationAttributeId,
    /// Only meaningful for Title/Subtitle/Message; `None` requests the full value.
    pub max_length: Option<u16>,
}

impl NotificationAttributeRequest {
    /// Creates a new instance.
    pub fn new(attribute_id: NotificationAttributeId) -> Self {
        Self {
            attribute_id,
            max_length: None,
        }
    }

    /// Sets the max length (builder style).
    pub fn with_max_length(attribute_id: NotificationAttributeId, max_length: u16) -> Self {
        Self {
            attribute_id,
            max_length: Some(max_length),
        }
    }
}

/// A fully reassembled Data Source response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataSourceResponse {
    /// Notification attributes.
    NotificationAttributes {
        /// Notification uid.
        notification_uid: u32,
        /// Attributes.
        attributes: Vec<(NotificationAttributeId, String)>,
    },
    /// App attributes.
    AppAttributes {
        /// App identifier.
        app_identifier: String,
        /// Attributes.
        attributes: Vec<(AppAttributeId, String)>,
    },
}

/// What response the client is currently waiting for; Data Source fragments are
/// only meaningful relative to the last command sent.
#[derive(Debug, Clone)]
enum ExpectedResponse {
    NotificationAttributes {
        notification_uid: u32,
        attribute_count: usize,
    },
    AppAttributes {
        app_identifier: String,
        attribute_count: usize,
    },
}

/// Sans-IO Notification Consumer (accessory) engine: builds Control Point command
/// payloads and reassembles Data Source responses that arrive fragmented across
/// MTU-sized notifications.
#[derive(Debug, Default)]
pub struct AncsClient {
    expected: Option<ExpectedResponse>,
    buffer: Vec<u8>,
}

impl AncsClient {
    /// Creates a new instance.
    pub fn new() -> Self {
        Self::default()
    }

    /// Parses one Notification Source notification payload.
    pub fn parse_notification_source(data: &[u8]) -> Option<NotificationEvent> {
        NotificationEvent::from_bytes(data)
    }

    /// Builds a Get Notification Attributes command payload and arms response
    /// reassembly for it.
    pub fn create_get_notification_attributes_command(
        &mut self,
        notification_uid: u32,
        attributes: &[NotificationAttributeRequest],
    ) -> Vec<u8> {
        let mut command = vec![CommandId::GetNotificationAttributes as u8];
        command.extend_from_slice(&notification_uid.to_le_bytes());
        for request in attributes {
            command.push(request.attribute_id as u8);
            if request.attribute_id.has_max_length() {
                // Text attributes always carry a max length on the wire; "no limit"
                // is expressed as the largest encodable length.
                command.extend_from_slice(&request.max_length.unwrap_or(u16::MAX).to_le_bytes());
            }
        }
        self.expected = Some(ExpectedResponse::NotificationAttributes {
            notification_uid,
            attribute_count: attributes.len(),
        });
        self.buffer.clear();
        command
    }

    /// Builds a Get App Attributes command payload and arms response reassembly
    /// for it.
    pub fn create_get_app_attributes_command(
        &mut self,
        app_identifier: &str,
        attributes: &[AppAttributeId],
    ) -> Vec<u8> {
        let mut command = vec![CommandId::GetAppAttributes as u8];
        command.extend_from_slice(app_identifier.as_bytes());
        command.push(0);
        command.extend(attributes.iter().map(|&id| id as u8));
        self.expected = Some(ExpectedResponse::AppAttributes {
            app_identifier: app_identifier.to_string(),
            attribute_count: attributes.len(),
        });
        self.buffer.clear();
        command
    }

    /// Builds a Perform Notification Action command payload. No Data Source
    /// response follows, so no reassembly is armed.
    pub fn create_perform_notification_action_command(
        &self,
        notification_uid: u32,
        action: ActionId,
    ) -> Vec<u8> {
        let mut command = vec![CommandId::PerformNotificationAction as u8];
        command.extend_from_slice(&notification_uid.to_le_bytes());
        command.push(action as u8);
        command
    }

    /// Feeds one Data Source notification fragment. Returns the reassembled
    /// response once every expected attribute tuple has arrived; `None` while more
    /// fragments are needed, no command is outstanding, or the response doesn't
    /// match the outstanding command (which discards the exchange).
    pub fn on_data_source_notification(&mut self, data: &[u8]) -> Option<DataSourceResponse> {
        self.expected.as_ref()?;
        self.buffer.extend_from_slice(data);
        let response = match self.expected.as_ref().expect("checked above") {
            ExpectedResponse::NotificationAttributes {
                notification_uid,
                attribute_count,
            } => Self::try_parse_notification_attributes(
                &self.buffer,
                *notification_uid,
                *attribute_count,
            ),
            ExpectedResponse::AppAttributes {
                app_identifier,
                attribute_count,
            } => Self::try_parse_app_attributes(&self.buffer, app_identifier, *attribute_count),
        };
        match response {
            ParseOutcome::Incomplete => None,
            ParseOutcome::Mismatch => {
                self.expected = None;
                self.buffer.clear();
                None
            }
            ParseOutcome::Complete(response) => {
                self.expected = None;
                self.buffer.clear();
                Some(response)
            }
        }
    }

    fn try_parse_notification_attributes(
        buffer: &[u8],
        expected_uid: u32,
        attribute_count: usize,
    ) -> ParseOutcome {
        if buffer.len() < 5 {
            return ParseOutcome::Incomplete;
        }
        if buffer[0] != CommandId::GetNotificationAttributes as u8 {
            return ParseOutcome::Mismatch;
        }
        let uid = u32::from_le_bytes(buffer[1..5].try_into().expect("4-byte slice"));
        if uid != expected_uid {
            return ParseOutcome::Mismatch;
        }
        match Self::parse_attribute_tuples(&buffer[5..], attribute_count) {
            Some(raw) => {
                let mut attributes = Vec::with_capacity(raw.len());
                for (id, value) in raw {
                    match NotificationAttributeId::from_u8(id) {
                        Some(id) => attributes.push((id, value)),
                        // A tuple id outside the known set means the exchange is
                        // corrupt; reporting Incomplete would stall forever.
                        None => return ParseOutcome::Mismatch,
                    }
                }
                ParseOutcome::Complete(DataSourceResponse::NotificationAttributes {
                    notification_uid: uid,
                    attributes,
                })
            }
            None => ParseOutcome::Incomplete,
        }
    }

    fn try_parse_app_attributes(
        buffer: &[u8],
        expected_app_identifier: &str,
        attribute_count: usize,
    ) -> ParseOutcome {
        if buffer.len() < 2 {
            return ParseOutcome::Incomplete;
        }
        if buffer[0] != CommandId::GetAppAttributes as u8 {
            return ParseOutcome::Mismatch;
        }
        // The response echoes the NUL-terminated app identifier before the tuples.
        let Some(nul) = buffer[1..].iter().position(|&b| b == 0) else {
            return ParseOutcome::Incomplete;
        };
        let Ok(app_identifier) = std::str::from_utf8(&buffer[1..1 + nul]) else {
            return ParseOutcome::Mismatch;
        };
        if app_identifier != expected_app_identifier {
            return ParseOutcome::Mismatch;
        }
        match Self::parse_attribute_tuples(&buffer[2 + nul..], attribute_count) {
            Some(raw) => {
                let mut attributes = Vec::with_capacity(raw.len());
                for (id, value) in raw {
                    match AppAttributeId::from_u8(id) {
                        Some(id) => attributes.push((id, value)),
                        None => return ParseOutcome::Mismatch,
                    }
                }
                ParseOutcome::Complete(DataSourceResponse::AppAttributes {
                    app_identifier: app_identifier.to_string(),
                    attributes,
                })
            }
            None => ParseOutcome::Incomplete,
        }
    }

    /// Parses `[AttributeID, Length (u16 LE), Value]` tuples; `None` until
    /// `expected_count` complete tuples are available.
    fn parse_attribute_tuples(mut data: &[u8], expected_count: usize) -> Option<Vec<(u8, String)>> {
        let mut tuples = Vec::with_capacity(expected_count);
        while tuples.len() < expected_count {
            let header = data.get(..3)?;
            let length = u16::from_le_bytes([header[1], header[2]]) as usize;
            let value = data.get(3..3 + length)?;
            tuples.push((header[0], String::from_utf8_lossy(value).into_owned()));
            data = &data[3 + length..];
        }
        Some(tuples)
    }
}

/// Tri-state result of trying to parse a partially received Data Source response.
#[derive(Debug)]
enum ParseOutcome {
    Incomplete,
    Mismatch,
    Complete(DataSourceResponse),
}
