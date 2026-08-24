// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! AVRCP (Audio/Video Remote Control Profile) v1.6.2: media remote control
//! over AV/C frames (`crate::classic::avc`) carried by AVCTP
//! (`crate::classic::avctp`) with PID [`AVRCP_PID`].
//!
//! The AVRCP-specific PDU layer lives in [`Command`], [`Response`], and
//! [`Event`]: vendor-dependent AV/C frames whose data is a small header
//! (PDU ID, packet type, parameter length — AVRCP spec 6.3) followed by
//! per-PDU parameters, reassembled across AV/C frames by [`PduAssembler`].
//! Browsing-channel PDUs (GetFolderItems and friends, AVRCP spec 6.10) are
//! modeled at the same parameter level with [`BrowseableItem`] payloads.
//!
//! [`Protocol`] is the per-connection state machine covering both roles in
//! the house synchronous shape (`receive(pdu) -> (outgoing PDUs, events)`,
//! as in `avdtp::Protocol`): as a controller (CT) it builds command frames
//! and turns responses into [`AvrcpEvent`]s; as a target (TG) it answers
//! commands from its public media-player state fields and emits CHANGED
//! notifications through the `notify_*` methods (AVRCP spec 6.7.2 uses the
//! interim/changed response pair per registration).
//!
//! Scope: signaling only, matching A2DP — cover art (BIP/OBEX) and the
//! continuation PDUs used to stream oversized responses are not modeled.

use crate::classic::avc::{
    self, CommandFrame, CommandType, Frame, PassThrough, ResponseCode, ResponseFrame,
};
use crate::classic::avctp::{self, AvctpEvent};
use crate::classic::sdp::{
    AttributeIdSpec, DataElement, SdpClient, SdpUuid, ServiceAttribute, attribute_id,
};
use crate::types::SimbleError;
use std::collections::HashMap;

/// AVCTP profile ID for AVRCP: the A/V Remote Control service class UUID
/// value (AVRCP spec 4.2.2).
pub const AVRCP_PID: u16 = 0x110E;
/// Company ID carried in every AVRCP vendor-dependent frame.
pub const BLUETOOTH_SIG_COMPANY_ID: u32 = avc::BLUETOOTH_SIG_COMPANY_ID;

/// A/V Remote Control service class UUID (0x110E).
pub(crate) const AV_REMOTE_CONTROL_SERVICE_UUID: SdpUuid = SdpUuid::Uuid16(0x110E);
/// A/V Remote Control Target service class UUID (0x110C).
pub(crate) const AV_REMOTE_CONTROL_TARGET_SERVICE_UUID: SdpUuid = SdpUuid::Uuid16(0x110C);
/// A/V Remote Control Controller service class UUID (0x110F).
pub(crate) const AV_REMOTE_CONTROL_CONTROLLER_SERVICE_UUID: SdpUuid = SdpUuid::Uuid16(0x110F);
/// AVCTP protocol UUID (0x0017) for SDP protocol descriptor lists.
pub(crate) const AVCTP_PROTOCOL_UUID: SdpUuid = SdpUuid::Uuid16(0x0017);
/// SupportedFeatures SDP attribute ID (AVRCP spec 8, service records).
const SUPPORTED_FEATURES_ATTRIBUTE_ID: u16 = 0x0311;

/// Default AVCTP version advertised: 1.4.
pub(crate) const DEFAULT_AVCTP_VERSION: (u8, u8) = (1, 4);
/// Default AVRCP version advertised: 1.6.
pub(crate) const DEFAULT_AVRCP_VERSION: (u8, u8) = (1, 6);

/// Highest absolute volume value: the field is 7 bits, 0x7F = 100%
/// (AVRCP spec 6.13.1).
pub const MAXIMUM_VOLUME: u8 = 0x7F;
/// TrackChanged UID meaning "no track currently selected" (AVRCP spec 6.7.2).
pub const NO_TRACK_UID: u64 = 0xFFFF_FFFF_FFFF_FFFF;
/// GetPlayStatus song length/position when the target does not support them
/// (AVRCP spec 6.7.1).
pub const PLAYBACK_POSITION_UNAVAILABLE: u32 = 0xFFFF_FFFF;

/// AVRCP PDU IDs (AVRCP spec 4.5, Table 4.5).
pub mod pdu_id {
    /// `GET_CAPABILITIES` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_CAPABILITIES: u8 = 0x10;
    /// `LIST_PLAYER_APPLICATION_SETTING_ATTRIBUTES` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const LIST_PLAYER_APPLICATION_SETTING_ATTRIBUTES: u8 = 0x11;
    /// `LIST_PLAYER_APPLICATION_SETTING_VALUES` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const LIST_PLAYER_APPLICATION_SETTING_VALUES: u8 = 0x12;
    /// `GET_CURRENT_PLAYER_APPLICATION_SETTING_VALUE` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_CURRENT_PLAYER_APPLICATION_SETTING_VALUE: u8 = 0x13;
    /// `SET_PLAYER_APPLICATION_SETTING_VALUE` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const SET_PLAYER_APPLICATION_SETTING_VALUE: u8 = 0x14;
    /// `GET_PLAYER_APPLICATION_SETTING_ATTRIBUTE_TEXT` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_PLAYER_APPLICATION_SETTING_ATTRIBUTE_TEXT: u8 = 0x15;
    /// `GET_PLAYER_APPLICATION_SETTING_VALUE_TEXT` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_PLAYER_APPLICATION_SETTING_VALUE_TEXT: u8 = 0x16;
    /// `INFORM_DISPLAYABLE_CHARACTER_SET` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const INFORM_DISPLAYABLE_CHARACTER_SET: u8 = 0x17;
    /// `INFORM_BATTERY_STATUS_OF_CT` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const INFORM_BATTERY_STATUS_OF_CT: u8 = 0x18;
    /// `GET_ELEMENT_ATTRIBUTES` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_ELEMENT_ATTRIBUTES: u8 = 0x20;
    /// `GET_PLAY_STATUS` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_PLAY_STATUS: u8 = 0x30;
    /// `REGISTER_NOTIFICATION` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const REGISTER_NOTIFICATION: u8 = 0x31;
    /// `REQUEST_CONTINUING_RESPONSE` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const REQUEST_CONTINUING_RESPONSE: u8 = 0x40;
    /// `ABORT_CONTINUING_RESPONSE` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const ABORT_CONTINUING_RESPONSE: u8 = 0x41;
    /// `SET_ABSOLUTE_VOLUME` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const SET_ABSOLUTE_VOLUME: u8 = 0x50;
    /// `SET_ADDRESSED_PLAYER` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const SET_ADDRESSED_PLAYER: u8 = 0x60;
    /// `SET_BROWSED_PLAYER` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const SET_BROWSED_PLAYER: u8 = 0x70;
    /// `GET_FOLDER_ITEMS` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_FOLDER_ITEMS: u8 = 0x71;
    /// `CHANGE_PATH` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const CHANGE_PATH: u8 = 0x72;
    /// `GET_ITEM_ATTRIBUTES` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_ITEM_ATTRIBUTES: u8 = 0x73;
    /// `PLAY_ITEM` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const PLAY_ITEM: u8 = 0x74;
    /// `GET_TOTAL_NUMBER_OF_ITEMS` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const GET_TOTAL_NUMBER_OF_ITEMS: u8 = 0x75;
    /// `SEARCH` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const SEARCH: u8 = 0x80;
    /// `ADD_TO_NOW_PLAYING` PDU ID (AVRCP spec 4.5, Table 4.5).
    pub const ADD_TO_NOW_PLAYING: u8 = 0x90;
}

/// GetCapabilities capability IDs (AVRCP spec 6.4.1).
pub mod capability_id {
    /// `COMPANY_ID` GetCapabilities capability ID (AVRCP spec 6.4.1).
    pub const COMPANY_ID: u8 = 0x02;
    /// `EVENTS_SUPPORTED` GetCapabilities capability ID (AVRCP spec 6.4.1).
    pub(crate) const EVENTS_SUPPORTED: u8 = 0x03;
}

/// Notification event IDs (AVRCP spec 6.7.2, Table 6.35).
pub mod event_id {
    /// `PLAYBACK_STATUS_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const PLAYBACK_STATUS_CHANGED: u8 = 0x01;
    /// `TRACK_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const TRACK_CHANGED: u8 = 0x02;
    /// `TRACK_REACHED_END` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const TRACK_REACHED_END: u8 = 0x03;
    /// `TRACK_REACHED_START` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const TRACK_REACHED_START: u8 = 0x04;
    /// `PLAYBACK_POS_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub(crate) const PLAYBACK_POS_CHANGED: u8 = 0x05;
    /// `BATT_STATUS_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const BATT_STATUS_CHANGED: u8 = 0x06;
    /// `SYSTEM_STATUS_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const SYSTEM_STATUS_CHANGED: u8 = 0x07;
    /// `PLAYER_APPLICATION_SETTING_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const PLAYER_APPLICATION_SETTING_CHANGED: u8 = 0x08;
    /// `NOW_PLAYING_CONTENT_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const NOW_PLAYING_CONTENT_CHANGED: u8 = 0x09;
    /// `AVAILABLE_PLAYERS_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub(crate) const AVAILABLE_PLAYERS_CHANGED: u8 = 0x0A;
    /// `ADDRESSED_PLAYER_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const ADDRESSED_PLAYER_CHANGED: u8 = 0x0B;
    /// `UIDS_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const UIDS_CHANGED: u8 = 0x0C;
    /// `VOLUME_CHANGED` notification event ID (AVRCP spec 6.7.2, Table 6.35).
    pub const VOLUME_CHANGED: u8 = 0x0D;
}

/// Status codes for REJECTED responses (AVRCP spec 6.15.3, Table 6.49).
pub mod status_code {
    /// `INVALID_COMMAND` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const INVALID_COMMAND: u8 = 0x00;
    /// `INVALID_PARAMETER` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const INVALID_PARAMETER: u8 = 0x01;
    /// `PARAMETER_CONTENT_ERROR` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const PARAMETER_CONTENT_ERROR: u8 = 0x02;
    /// `INTERNAL_ERROR` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const INTERNAL_ERROR: u8 = 0x03;
    /// `OPERATION_COMPLETED` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const OPERATION_COMPLETED: u8 = 0x04;
    /// `UID_CHANGED` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const UID_CHANGED: u8 = 0x05;
    /// `INVALID_DIRECTION` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const INVALID_DIRECTION: u8 = 0x07;
    /// `NOT_A_DIRECTORY` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const NOT_A_DIRECTORY: u8 = 0x08;
    /// `DOES_NOT_EXIST` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const DOES_NOT_EXIST: u8 = 0x09;
    /// `INVALID_SCOPE` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const INVALID_SCOPE: u8 = 0x0A;
    /// `RANGE_OUT_OF_BOUNDS` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const RANGE_OUT_OF_BOUNDS: u8 = 0x0B;
    /// `FOLDER_ITEM_IS_NOT_PLAYABLE` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const FOLDER_ITEM_IS_NOT_PLAYABLE: u8 = 0x0C;
    /// `MEDIA_IN_USE` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const MEDIA_IN_USE: u8 = 0x0D;
    /// `NOW_PLAYING_LIST_FULL` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const NOW_PLAYING_LIST_FULL: u8 = 0x0E;
    /// `SEARCH_NOT_SUPPORTED` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const SEARCH_NOT_SUPPORTED: u8 = 0x0F;
    /// `SEARCH_IN_PROGRESS` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const SEARCH_IN_PROGRESS: u8 = 0x10;
    /// `INVALID_PLAYER_ID` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const INVALID_PLAYER_ID: u8 = 0x11;
    /// `PLAYER_NOT_BROWSABLE` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const PLAYER_NOT_BROWSABLE: u8 = 0x12;
    /// `PLAYER_NOT_ADDRESSED` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const PLAYER_NOT_ADDRESSED: u8 = 0x13;
    /// `NO_VALID_SEARCH_RESULTS` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const NO_VALID_SEARCH_RESULTS: u8 = 0x14;
    /// `NO_AVAILABLE_PLAYERS` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const NO_AVAILABLE_PLAYERS: u8 = 0x15;
    /// `ADDRESSED_PLAYER_CHANGED` REJECTED status code (AVRCP spec 6.15.3, Table 6.49).
    pub const ADDRESSED_PLAYER_CHANGED: u8 = 0x16;
}

/// Play status values (AVRCP spec 6.7.1).
pub mod play_status {
    /// `STOPPED` play status (AVRCP spec 6.7.1).
    pub const STOPPED: u8 = 0x00;
    /// `PLAYING` play status (AVRCP spec 6.7.1).
    pub const PLAYING: u8 = 0x01;
    /// `PAUSED` play status (AVRCP spec 6.7.1).
    pub const PAUSED: u8 = 0x02;
    /// `FWD_SEEK` play status (AVRCP spec 6.7.1).
    pub const FWD_SEEK: u8 = 0x03;
    /// `REV_SEEK` play status (AVRCP spec 6.7.1).
    pub const REV_SEEK: u8 = 0x04;
    /// `ERROR` play status (AVRCP spec 6.7.1).
    pub const ERROR: u8 = 0xFF;
}

/// Media attribute IDs for element/item attributes (AVRCP spec 26, Appendix E).
pub mod media_attribute_id {
    /// `TITLE` media attribute ID (AVRCP spec Appendix E).
    pub const TITLE: u32 = 0x01;
    /// `ARTIST_NAME` media attribute ID (AVRCP spec Appendix E).
    pub const ARTIST_NAME: u32 = 0x02;
    /// `ALBUM_NAME` media attribute ID (AVRCP spec Appendix E).
    pub const ALBUM_NAME: u32 = 0x03;
    /// `TRACK_NUMBER` media attribute ID (AVRCP spec Appendix E).
    pub const TRACK_NUMBER: u32 = 0x04;
    /// `TOTAL_NUMBER_OF_TRACKS` media attribute ID (AVRCP spec Appendix E).
    pub const TOTAL_NUMBER_OF_TRACKS: u32 = 0x05;
    /// `GENRE` media attribute ID (AVRCP spec Appendix E).
    pub const GENRE: u32 = 0x06;
    /// `PLAYING_TIME` media attribute ID (AVRCP spec Appendix E).
    pub const PLAYING_TIME: u32 = 0x07;
    /// `DEFAULT_COVER_ART` media attribute ID (AVRCP spec Appendix E).
    pub const DEFAULT_COVER_ART: u32 = 0x08;
}

/// Browsing scopes (AVRCP spec 6.10.1, Table 6.29).
pub mod scope {
    /// `MEDIA_PLAYER_LIST` browsing scope (AVRCP spec 6.10.1, Table 6.29).
    pub const MEDIA_PLAYER_LIST: u8 = 0x00;
    /// `MEDIA_PLAYER_VIRTUAL_FILESYSTEM` browsing scope (AVRCP spec 6.10.1, Table 6.29).
    pub const MEDIA_PLAYER_VIRTUAL_FILESYSTEM: u8 = 0x01;
    /// `SEARCH` browsing scope (AVRCP spec 6.10.1, Table 6.29).
    pub const SEARCH: u8 = 0x02;
    /// `NOW_PLAYING` browsing scope (AVRCP spec 6.10.1, Table 6.29).
    pub const NOW_PLAYING: u8 = 0x03;
}

/// Character set IDs (IANA MIBenum values; AVRCP mandates UTF-8).
pub mod character_set_id {
    /// `UTF_8` character set (IANA MIBenum value).
    pub const UTF_8: u16 = 0x006A;
}

/// Battery status values for InformBatteryStatusOfCT (AVRCP spec 6.5.8).
pub mod battery_status {
    /// `NORMAL` battery status for InformBatteryStatusOfCT (AVRCP spec 6.5.8).
    pub const NORMAL: u8 = 0x00;
    /// `WARNING` battery status for InformBatteryStatusOfCT (AVRCP spec 6.5.8).
    pub const WARNING: u8 = 0x01;
    /// `CRITICAL` battery status for InformBatteryStatusOfCT (AVRCP spec 6.5.8).
    pub const CRITICAL: u8 = 0x02;
    /// `EXTERNAL` battery status for InformBatteryStatusOfCT (AVRCP spec 6.5.8).
    pub const EXTERNAL: u8 = 0x03;
    /// `FULL_CHARGE` battery status for InformBatteryStatusOfCT (AVRCP spec 6.5.8).
    pub const FULL_CHARGE: u8 = 0x04;
}

/// Player application setting attribute IDs (AVRCP spec 6.5.1, Appendix F).
pub mod application_setting_attribute {
    /// `EQUALIZER_ON_OFF` player application setting attribute (AVRCP spec Appendix F).
    pub const EQUALIZER_ON_OFF: u8 = 0x01;
    /// `REPEAT_MODE` player application setting attribute (AVRCP spec Appendix F).
    pub const REPEAT_MODE: u8 = 0x02;
    /// `SHUFFLE_ON_OFF` player application setting attribute (AVRCP spec Appendix F).
    pub const SHUFFLE_ON_OFF: u8 = 0x03;
    /// `SCAN_ON_OFF` player application setting attribute (AVRCP spec Appendix F).
    pub const SCAN_ON_OFF: u8 = 0x04;
}

/// Equalizer setting values (AVRCP spec Appendix F).
pub mod equalizer_status {
    /// `OFF` equalizer setting value (AVRCP spec Appendix F).
    pub const OFF: u8 = 0x01;
    /// `ON` equalizer setting value (AVRCP spec Appendix F).
    pub const ON: u8 = 0x02;
}

/// Repeat mode setting values (AVRCP spec Appendix F).
pub mod repeat_mode_status {
    /// `OFF` repeat mode setting value (AVRCP spec Appendix F).
    pub const OFF: u8 = 0x01;
    /// `SINGLE_TRACK_REPEAT` repeat mode setting value (AVRCP spec Appendix F).
    pub const SINGLE_TRACK_REPEAT: u8 = 0x02;
    /// `ALL_TRACK_REPEAT` repeat mode setting value (AVRCP spec Appendix F).
    pub const ALL_TRACK_REPEAT: u8 = 0x03;
    /// `GROUP_REPEAT` repeat mode setting value (AVRCP spec Appendix F).
    pub const GROUP_REPEAT: u8 = 0x04;
}

/// Shuffle setting values (AVRCP spec Appendix F).
pub mod shuffle_status {
    /// `OFF` shuffle setting value (AVRCP spec Appendix F).
    pub const OFF: u8 = 0x01;
    /// `ALL_TRACKS_SHUFFLE` shuffle setting value (AVRCP spec Appendix F).
    pub const ALL_TRACKS_SHUFFLE: u8 = 0x02;
    /// `GROUP_SHUFFLE` shuffle setting value (AVRCP spec Appendix F).
    pub const GROUP_SHUFFLE: u8 = 0x03;
}

/// Scan setting values (AVRCP spec Appendix F).
pub mod scan_status {
    /// `OFF` scan setting value (AVRCP spec Appendix F).
    pub const OFF: u8 = 0x01;
    /// `ALL_TRACKS_SCAN` scan setting value (AVRCP spec Appendix F).
    pub const ALL_TRACKS_SCAN: u8 = 0x02;
    /// `GROUP_SCAN` scan setting value (AVRCP spec Appendix F).
    pub const GROUP_SCAN: u8 = 0x03;
}

/// Controller SupportedFeatures bits (AVRCP spec 8, Table 8.2).
pub mod controller_features {
    /// `CATEGORY_1` controller SupportedFeatures bit (AVRCP spec 8, Table 8.2).
    pub const CATEGORY_1: u16 = 1 << 0;
    /// `CATEGORY_2` controller SupportedFeatures bit (AVRCP spec 8, Table 8.2).
    pub const CATEGORY_2: u16 = 1 << 1;
    /// `CATEGORY_3` controller SupportedFeatures bit (AVRCP spec 8, Table 8.2).
    pub const CATEGORY_3: u16 = 1 << 2;
    /// `CATEGORY_4` controller SupportedFeatures bit (AVRCP spec 8, Table 8.2).
    pub const CATEGORY_4: u16 = 1 << 3;
    /// `SUPPORTS_BROWSING` controller SupportedFeatures bit (AVRCP spec 8, Table 8.2).
    pub const SUPPORTS_BROWSING: u16 = 1 << 6;
    /// `SUPPORTS_COVER_ART_GET_IMAGE_PROPERTIES` controller SupportedFeatures bit.
    pub const SUPPORTS_COVER_ART_GET_IMAGE_PROPERTIES: u16 = 1 << 7;
    /// `SUPPORTS_COVER_ART_GET_IMAGE` controller SupportedFeatures bit (AVRCP spec 8, Table 8.2).
    pub const SUPPORTS_COVER_ART_GET_IMAGE: u16 = 1 << 8;
    /// `SUPPORTS_COVER_ART_GET_LINKED_THUMBNAIL` controller SupportedFeatures bit.
    pub const SUPPORTS_COVER_ART_GET_LINKED_THUMBNAIL: u16 = 1 << 9;
}

/// Target SupportedFeatures bits (AVRCP spec 8, Table 8.1).
pub mod target_features {
    /// `CATEGORY_1` target SupportedFeatures bit (AVRCP spec 8, Table 8.1).
    pub const CATEGORY_1: u16 = 1 << 0;
    /// `CATEGORY_2` target SupportedFeatures bit (AVRCP spec 8, Table 8.1).
    pub const CATEGORY_2: u16 = 1 << 1;
    /// `CATEGORY_3` target SupportedFeatures bit (AVRCP spec 8, Table 8.1).
    pub const CATEGORY_3: u16 = 1 << 2;
    /// `CATEGORY_4` target SupportedFeatures bit (AVRCP spec 8, Table 8.1).
    pub const CATEGORY_4: u16 = 1 << 3;
    /// `PLAYER_APPLICATION_SETTINGS` target SupportedFeatures bit (AVRCP spec 8, Table 8.1).
    pub const PLAYER_APPLICATION_SETTINGS: u16 = 1 << 4;
    /// `GROUP_NAVIGATION` target SupportedFeatures bit (AVRCP spec 8, Table 8.1).
    pub const GROUP_NAVIGATION: u16 = 1 << 5;
    /// `SUPPORTS_BROWSING` target SupportedFeatures bit (AVRCP spec 8, Table 8.1).
    pub const SUPPORTS_BROWSING: u16 = 1 << 6;
    /// `SUPPORTS_MULTIPLE_MEDIA_PLAYER_APPLICATIONS` target SupportedFeatures bit.
    pub const SUPPORTS_MULTIPLE_MEDIA_PLAYER_APPLICATIONS: u16 = 1 << 7;
    /// `SUPPORTS_COVER_ART` target SupportedFeatures bit (AVRCP spec 8, Table 8.1).
    pub const SUPPORTS_COVER_ART: u16 = 1 << 8;
}

/// Browseable item types (AVRCP spec 6.10.2).
pub(crate) mod item_type {
    /// `MEDIA_PLAYER` browseable item type (AVRCP spec 6.10.2).
    pub(crate) const MEDIA_PLAYER: u8 = 0x01;
    /// `FOLDER` browseable item type (AVRCP spec 6.10.2).
    pub(crate) const FOLDER: u8 = 0x02;
    /// `MEDIA_ELEMENT` browseable item type (AVRCP spec 6.10.2).
    pub(crate) const MEDIA_ELEMENT: u8 = 0x03;
}

/// Folder types for [`FolderItem`] (AVRCP spec 6.10.2.2).
pub mod folder_type {
    /// `MIXED` folder type (AVRCP spec 6.10.2.2).
    pub const MIXED: u8 = 0x00;
    /// `TITLES` folder type (AVRCP spec 6.10.2.2).
    pub const TITLES: u8 = 0x01;
    /// `ALBUMS` folder type (AVRCP spec 6.10.2.2).
    pub const ALBUMS: u8 = 0x02;
    /// `ARTISTS` folder type (AVRCP spec 6.10.2.2).
    pub const ARTISTS: u8 = 0x03;
    /// `GENRES` folder type (AVRCP spec 6.10.2.2).
    pub const GENRES: u8 = 0x04;
    /// `PLAYLISTS` folder type (AVRCP spec 6.10.2.2).
    pub const PLAYLISTS: u8 = 0x05;
    /// `YEARS` folder type (AVRCP spec 6.10.2.2).
    pub const YEARS: u8 = 0x06;
}

/// Media types for [`MediaElementItem`] (AVRCP spec 6.10.2.3).
pub mod media_element_type {
    /// `AUDIO` media type (AVRCP spec 6.10.2.3).
    pub const AUDIO: u8 = 0x00;
    /// `VIDEO` media type (AVRCP spec 6.10.2.3).
    pub const VIDEO: u8 = 0x01;
}

/// Major player types for [`MediaPlayerItem`] (AVRCP spec 6.10.2.1).
pub mod major_player_type {
    /// `AUDIO` major player type (AVRCP spec 6.10.2.1).
    pub const AUDIO: u8 = 0x01;
    /// `VIDEO` major player type (AVRCP spec 6.10.2.1).
    pub const VIDEO: u8 = 0x02;
    /// `BROADCASTING_AUDIO` major player type (AVRCP spec 6.10.2.1).
    pub const BROADCASTING_AUDIO: u8 = 0x04;
    /// `BROADCASTING_VIDEO` major player type (AVRCP spec 6.10.2.1).
    pub const BROADCASTING_VIDEO: u8 = 0x08;
}

/// Player sub-types for [`MediaPlayerItem`] (AVRCP spec 6.10.2.1).
pub mod player_sub_type {
    /// `AUDIO_BOOK` player sub-type (AVRCP spec 6.10.2.1).
    pub const AUDIO_BOOK: u32 = 0x01;
    /// `PODCAST` player sub-type (AVRCP spec 6.10.2.1).
    pub const PODCAST: u32 = 0x02;
}

/// ChangePath directions (AVRCP spec 6.10.4.1).
pub mod direction {
    /// `UP` ChangePath direction (AVRCP spec 6.10.4.1).
    pub const UP: u8 = 0x00;
    /// `DOWN` ChangePath direction (AVRCP spec 6.10.4.1).
    pub const DOWN: u8 = 0x01;
}

/// Media player feature bits for [`MediaPlayerItem::features`]: bit N of
/// the 16-octet feature bitmask, octet 0 carrying bits 0-7 (AVRCP spec
/// 6.10.2.1, Table 6.32).
pub mod player_feature {
    /// `SELECT` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const SELECT: u128 = 1 << 0;
    /// `UP` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const UP: u128 = 1 << 1;
    /// `DOWN` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const DOWN: u128 = 1 << 2;
    /// `LEFT` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const LEFT: u128 = 1 << 3;
    /// `RIGHT` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const RIGHT: u128 = 1 << 4;
    /// `RIGHT_UP` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const RIGHT_UP: u128 = 1 << 5;
    /// `RIGHT_DOWN` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const RIGHT_DOWN: u128 = 1 << 6;
    /// `LEFT_UP` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const LEFT_UP: u128 = 1 << 7;
    /// `LEFT_DOWN` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const LEFT_DOWN: u128 = 1 << 8;
    /// `ROOT_MENU` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const ROOT_MENU: u128 = 1 << 9;
    /// `SETUP_MENU` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const SETUP_MENU: u128 = 1 << 10;
    /// `CONTENTS_MENU` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const CONTENTS_MENU: u128 = 1 << 11;
    /// `FAVORITE_MENU` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const FAVORITE_MENU: u128 = 1 << 12;
    /// `EXIT` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const EXIT: u128 = 1 << 13;
    /// `NUM_0` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_0: u128 = 1 << 14;
    /// `NUM_1` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_1: u128 = 1 << 15;
    /// `NUM_2` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_2: u128 = 1 << 16;
    /// `NUM_3` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_3: u128 = 1 << 17;
    /// `NUM_4` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_4: u128 = 1 << 18;
    /// `NUM_5` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_5: u128 = 1 << 19;
    /// `NUM_6` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_6: u128 = 1 << 20;
    /// `NUM_7` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_7: u128 = 1 << 21;
    /// `NUM_8` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_8: u128 = 1 << 22;
    /// `NUM_9` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUM_9: u128 = 1 << 23;
    /// `DOT` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const DOT: u128 = 1 << 24;
    /// `ENTER` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const ENTER: u128 = 1 << 25;
    /// `CLEAR` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const CLEAR: u128 = 1 << 26;
    /// `CHANNEL_UP` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const CHANNEL_UP: u128 = 1 << 27;
    /// `CHANNEL_DOWN` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const CHANNEL_DOWN: u128 = 1 << 28;
    /// `PREVIOUS_CHANNEL` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const PREVIOUS_CHANNEL: u128 = 1 << 29;
    /// `SOUND_SELECT` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const SOUND_SELECT: u128 = 1 << 30;
    /// `INPUT_SELECT` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const INPUT_SELECT: u128 = 1 << 31;
    /// `DISPLAY_INFORMATION` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const DISPLAY_INFORMATION: u128 = 1 << 32;
    /// `HELP` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const HELP: u128 = 1 << 33;
    /// `PAGE_UP` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const PAGE_UP: u128 = 1 << 34;
    /// `PAGE_DOWN` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const PAGE_DOWN: u128 = 1 << 35;
    /// `POWER` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const POWER: u128 = 1 << 36;
    /// `VOLUME_UP` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const VOLUME_UP: u128 = 1 << 37;
    /// `VOLUME_DOWN` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const VOLUME_DOWN: u128 = 1 << 38;
    /// `MUTE` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const MUTE: u128 = 1 << 39;
    /// `PLAY` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const PLAY: u128 = 1 << 40;
    /// `STOP` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const STOP: u128 = 1 << 41;
    /// `PAUSE` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const PAUSE: u128 = 1 << 42;
    /// `RECORD` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const RECORD: u128 = 1 << 43;
    /// `REWIND` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const REWIND: u128 = 1 << 44;
    /// `FAST_FORWARD` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const FAST_FORWARD: u128 = 1 << 45;
    /// `EJECT` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const EJECT: u128 = 1 << 46;
    /// `FORWARD` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const FORWARD: u128 = 1 << 47;
    /// `BACKWARD` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const BACKWARD: u128 = 1 << 48;
    /// `ANGLE` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const ANGLE: u128 = 1 << 49;
    /// `SUBPICTURE` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const SUBPICTURE: u128 = 1 << 50;
    /// `F1` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const F1: u128 = 1 << 51;
    /// `F2` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const F2: u128 = 1 << 52;
    /// `F3` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const F3: u128 = 1 << 53;
    /// `F4` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const F4: u128 = 1 << 54;
    /// `F5` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const F5: u128 = 1 << 55;
    /// `VENDOR_UNIQUE` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const VENDOR_UNIQUE: u128 = 1 << 56;
    /// `BASIC_GROUP_NAVIGATION` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const BASIC_GROUP_NAVIGATION: u128 = 1 << 57;
    /// `ADVANCED_CONTROL_PLAYER` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const ADVANCED_CONTROL_PLAYER: u128 = 1 << 58;
    /// `BROWSING` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const BROWSING: u128 = 1 << 59;
    /// `SEARCHING` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const SEARCHING: u128 = 1 << 60;
    /// `ADD_TO_NOW_PLAYING` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const ADD_TO_NOW_PLAYING: u128 = 1 << 61;
    /// `UIDS_UNIQUE_IN_PLAYER_BROWSE_TREE` media player feature bit.
    pub const UIDS_UNIQUE_IN_PLAYER_BROWSE_TREE: u128 = 1 << 62;
    /// `ONLY_BROWSABLE_WHEN_ADDRESSED` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const ONLY_BROWSABLE_WHEN_ADDRESSED: u128 = 1 << 63;
    /// `ONLY_SEARCHABLE_WHEN_ADDRESSED` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const ONLY_SEARCHABLE_WHEN_ADDRESSED: u128 = 1 << 64;
    /// `NOW_PLAYING` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NOW_PLAYING: u128 = 1 << 65;
    /// `UID_PERSISTENCY` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const UID_PERSISTENCY: u128 = 1 << 66;
    /// `NUMBER_OF_ITEMS` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const NUMBER_OF_ITEMS: u128 = 1 << 67;
    /// `COVER_ART` media player feature bit (AVRCP spec 6.10.2.1, Table 6.32).
    pub const COVER_ART: u128 = 1 << 68;
}

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// Cursor over big-endian AVRCP parameters; every accessor is
/// `Option`-returning so truncated PDUs fail cleanly.
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let slice = self.data.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(slice)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }

    fn u24(&mut self) -> Option<u32> {
        self.take(3)
            .map(|b| u32::from_be_bytes([0, b[0], b[1], b[2]]))
    }

    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Option<u64> {
        self.take(8)
            .map(|b| u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    // Strings decode lossily: AVRCP text is declared UTF-8 but real targets
    // send stray legacy encodings, and one bad name must not kill the PDU.
    fn string8(&mut self) -> Option<String> {
        let length = usize::from(self.u8()?);
        Some(String::from_utf8_lossy(self.take(length)?).into_owned())
    }

    fn string16(&mut self) -> Option<String> {
        let length = usize::from(self.u16()?);
        Some(String::from_utf8_lossy(self.take(length)?).into_owned())
    }

    fn rest(&mut self) -> &'a [u8] {
        let slice = &self.data[self.pos.min(self.data.len())..];
        self.pos = self.data.len();
        slice
    }
}

fn put_string8(out: &mut Vec<u8>, value: &str) {
    out.push(value.len() as u8);
    out.extend_from_slice(value.as_bytes());
}

fn put_string16(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn read_u8_list(reader: &mut Reader) -> Option<Vec<u8>> {
    let count = reader.u8()?;
    (0..count).map(|_| reader.u8()).collect()
}

fn write_u8_list(out: &mut Vec<u8>, values: &[u8]) {
    out.push(values.len() as u8);
    out.extend_from_slice(values);
}

fn read_u32_list(reader: &mut Reader) -> Option<Vec<u32>> {
    let count = reader.u8()?;
    (0..count).map(|_| reader.u32()).collect()
}

fn write_u32_list(out: &mut Vec<u8>, values: &[u32]) {
    out.push(values.len() as u8);
    for value in values {
        out.extend_from_slice(&value.to_be_bytes());
    }
}

// ---------------------------------------------------------------------------
// Media metadata and settings
// ---------------------------------------------------------------------------

/// One media (track) attribute: ID, character set, and text value
/// (AVRCP spec 6.6.1, attribute value entry).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAttribute {
    /// Media attribute ID (see [`media_attribute_id`]).
    pub attribute_id: u32,
    /// Character set of `value` (IANA MIBenum; AVRCP mandates UTF-8).
    pub character_set_id: u16,
    /// The attribute's text value.
    pub value: String,
}

impl MediaAttribute {
    fn read(reader: &mut Reader) -> Option<Self> {
        Some(Self {
            attribute_id: reader.u32()?,
            character_set_id: reader.u16()?,
            value: reader.string16()?,
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.attribute_id.to_be_bytes());
        out.extend_from_slice(&self.character_set_id.to_be_bytes());
        put_string16(out, &self.value);
    }
}

fn read_media_attribute_list(reader: &mut Reader) -> Option<Vec<MediaAttribute>> {
    let count = reader.u8()?;
    (0..count).map(|_| MediaAttribute::read(reader)).collect()
}

fn write_media_attribute_list(out: &mut Vec<u8>, attributes: &[MediaAttribute]) {
    out.push(attributes.len() as u8);
    for attribute in attributes {
        attribute.write(out);
    }
}

/// One player application setting: attribute ID paired with its value ID
/// (AVRCP spec 6.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApplicationSetting {
    /// Setting attribute ID (see [`application_setting_attribute`]).
    pub attribute_id: u8,
    /// Selected value ID for that attribute.
    pub value_id: u8,
}

fn read_setting_list(reader: &mut Reader) -> Option<Vec<ApplicationSetting>> {
    let count = reader.u8()?;
    (0..count)
        .map(|_| {
            Some(ApplicationSetting {
                attribute_id: reader.u8()?,
                value_id: reader.u8()?,
            })
        })
        .collect()
}

fn write_setting_list(out: &mut Vec<u8>, settings: &[ApplicationSetting]) {
    out.push(settings.len() as u8);
    for setting in settings {
        out.push(setting.attribute_id);
        out.push(setting.value_id);
    }
}

/// Displayable text for one setting attribute or value ID
/// (AVRCP spec 6.5.5/6.5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingText {
    /// Attribute ID or value ID, depending on the PDU.
    pub id: u8,
    /// Character set of `text` (IANA MIBenum).
    pub character_set_id: u16,
    /// The displayable text for this attribute or value.
    pub text: String,
}

fn read_setting_text_list(reader: &mut Reader) -> Option<Vec<SettingText>> {
    let count = reader.u8()?;
    (0..count)
        .map(|_| {
            Some(SettingText {
                id: reader.u8()?,
                character_set_id: reader.u16()?,
                text: reader.string8()?,
            })
        })
        .collect()
}

fn write_setting_text_list(out: &mut Vec<u8>, entries: &[SettingText]) {
    out.push(entries.len() as u8);
    for entry in entries {
        out.push(entry.id);
        out.extend_from_slice(&entry.character_set_id.to_be_bytes());
        put_string8(out, &entry.text);
    }
}

/// GetCapabilities payload: which list is present is determined by the
/// capability ID (AVRCP spec 6.4.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapabilityList {
    /// 24-bit company IDs.
    CompanyIds(Vec<u32>),
    /// Notification event IDs.
    EventIds(Vec<u8>),
}

impl CapabilityList {
    /// The GetCapabilities capability ID this list corresponds to.
    pub(crate) fn capability_id(&self) -> u8 {
        match self {
            CapabilityList::CompanyIds(..) => capability_id::COMPANY_ID,
            CapabilityList::EventIds(..) => capability_id::EVENTS_SUPPORTED,
        }
    }
}

// ---------------------------------------------------------------------------
// Browseable items
// ---------------------------------------------------------------------------

/// A media player entry in the player list scope (AVRCP spec 6.10.2.1).
/// `player_sub_type` and `features` use the spec's bit-0-in-first-octet
/// layout, i.e. little-endian integers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaPlayerItem {
    /// Unique player ID within the target.
    pub player_id: u16,
    /// Major player type bitmask (see [`major_player_type`]).
    pub major_player_type: u8,
    /// Player sub-type bitmask (see [`player_sub_type`]).
    pub player_sub_type: u32,
    /// Current play status of the player (see [`play_status`]).
    pub play_status: u8,
    /// 128-bit feature bitmask (see [`player_feature`]).
    pub features: u128,
    /// Character set of `displayable_name` (IANA MIBenum).
    pub character_set_id: u16,
    /// Human-readable player name.
    pub displayable_name: String,
}

impl MediaPlayerItem {
    fn read(reader: &mut Reader) -> Option<Self> {
        let player_id = reader.u16()?;
        let major_player_type = reader.u8()?;
        let player_sub_type = reader
            .take(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))?;
        let play_status = reader.u8()?;
        let feature_bytes: [u8; 16] = reader.take(16)?.try_into().ok()?;
        let features = u128::from_le_bytes(feature_bytes);
        Some(Self {
            player_id,
            major_player_type,
            player_sub_type,
            play_status,
            features,
            character_set_id: reader.u16()?,
            displayable_name: reader.string16()?,
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.player_id.to_be_bytes());
        out.push(self.major_player_type);
        out.extend_from_slice(&self.player_sub_type.to_le_bytes());
        out.push(self.play_status);
        out.extend_from_slice(&self.features.to_le_bytes());
        out.extend_from_slice(&self.character_set_id.to_be_bytes());
        put_string16(out, &self.displayable_name);
    }
}

/// A folder entry in the virtual filesystem scope (AVRCP spec 6.10.2.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderItem {
    /// Unique folder UID within the browsed player.
    pub folder_uid: u64,
    /// Folder type (see [`folder_type`]).
    pub folder_type: u8,
    /// Whether the folder can be played directly.
    pub is_playable: bool,
    /// Character set of `displayable_name` (IANA MIBenum).
    pub character_set_id: u16,
    /// Human-readable folder name.
    pub displayable_name: String,
}

impl FolderItem {
    fn read(reader: &mut Reader) -> Option<Self> {
        Some(Self {
            folder_uid: reader.u64()?,
            folder_type: reader.u8()?,
            is_playable: reader.u8()? != 0,
            character_set_id: reader.u16()?,
            displayable_name: reader.string16()?,
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.folder_uid.to_be_bytes());
        out.push(self.folder_type);
        out.push(u8::from(self.is_playable));
        out.extend_from_slice(&self.character_set_id.to_be_bytes());
        put_string16(out, &self.displayable_name);
    }
}

/// A media element (track) entry (AVRCP spec 6.10.2.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaElementItem {
    /// Unique media element (track) UID.
    pub media_element_uid: u64,
    /// Media type (see [`media_element_type`]).
    pub media_type: u8,
    /// Character set of `displayable_name` (IANA MIBenum).
    pub character_set_id: u16,
    /// Human-readable element name.
    pub displayable_name: String,
    /// Media attributes attached to this element.
    pub attributes: Vec<MediaAttribute>,
}

impl MediaElementItem {
    fn read(reader: &mut Reader) -> Option<Self> {
        Some(Self {
            media_element_uid: reader.u64()?,
            media_type: reader.u8()?,
            character_set_id: reader.u16()?,
            displayable_name: reader.string16()?,
            attributes: read_media_attribute_list(reader)?,
        })
    }

    fn write(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&self.media_element_uid.to_be_bytes());
        out.push(self.media_type);
        out.extend_from_slice(&self.character_set_id.to_be_bytes());
        put_string16(out, &self.displayable_name);
        write_media_attribute_list(out, &self.attributes);
    }
}

/// One entry of a GetFolderItems response, tagged and length-prefixed on
/// the wire (AVRCP spec 6.10.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowseableItem {
    /// A media player entry (player list scope).
    MediaPlayer(MediaPlayerItem),
    /// A folder entry (virtual filesystem scope).
    Folder(FolderItem),
    /// A media element (track) entry.
    MediaElement(MediaElementItem),
}

impl BrowseableItem {
    fn read(reader: &mut Reader) -> Option<Self> {
        let item_type = reader.u8()?;
        let length = usize::from(reader.u16()?);
        let mut body = Reader::new(reader.take(length)?);
        match item_type {
            item_type::MEDIA_PLAYER => Some(BrowseableItem::MediaPlayer(MediaPlayerItem::read(
                &mut body,
            )?)),
            item_type::FOLDER => Some(BrowseableItem::Folder(FolderItem::read(&mut body)?)),
            item_type::MEDIA_ELEMENT => Some(BrowseableItem::MediaElement(MediaElementItem::read(
                &mut body,
            )?)),
            _ => None,
        }
    }

    fn write(&self, out: &mut Vec<u8>) {
        let mut body = Vec::new();
        let item_type = match self {
            BrowseableItem::MediaPlayer(item) => {
                item.write(&mut body);
                item_type::MEDIA_PLAYER
            }
            BrowseableItem::Folder(item) => {
                item.write(&mut body);
                item_type::FOLDER
            }
            BrowseableItem::MediaElement(item) => {
                item.write(&mut body);
                item_type::MEDIA_ELEMENT
            }
        };
        out.push(item_type);
        out.extend_from_slice(&(body.len() as u16).to_be_bytes());
        out.extend_from_slice(&body);
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// One AVRCP command PDU's parameters (AVRCP spec 6.4-6.13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// `GetCapabilities` command PDU (AVRCP spec 6).
    GetCapabilities {
        /// Capability id.
        capability_id: u8,
    },
    /// `ListPlayerApplicationSettingAttributes` command PDU (AVRCP spec 6).
    ListPlayerApplicationSettingAttributes,
    /// `ListPlayerApplicationSettingValues` command PDU (AVRCP spec 6).
    ListPlayerApplicationSettingValues {
        /// Attribute id.
        attribute_id: u8,
    },
    /// `GetCurrentPlayerApplicationSettingValue` command PDU (AVRCP spec 6).
    GetCurrentPlayerApplicationSettingValue {
        /// Attribute ids.
        attribute_ids: Vec<u8>,
    },
    /// `SetPlayerApplicationSettingValue` command PDU (AVRCP spec 6).
    SetPlayerApplicationSettingValue {
        /// Settings.
        settings: Vec<ApplicationSetting>,
    },
    /// `GetPlayerApplicationSettingAttributeText` command PDU (AVRCP spec 6).
    GetPlayerApplicationSettingAttributeText {
        /// Attribute ids.
        attribute_ids: Vec<u8>,
    },
    /// `GetPlayerApplicationSettingValueText` command PDU (AVRCP spec 6).
    GetPlayerApplicationSettingValueText {
        /// Attribute id.
        attribute_id: u8,
        /// Value ids.
        value_ids: Vec<u8>,
    },
    /// `InformDisplayableCharacterSet` command PDU (AVRCP spec 6).
    InformDisplayableCharacterSet {
        /// Character set ids.
        character_set_ids: Vec<u16>,
    },
    /// `InformBatteryStatusOfCt` command PDU (AVRCP spec 6).
    InformBatteryStatusOfCt {
        /// Battery status.
        battery_status: u8,
    },
    /// `GetElementAttributes` command PDU (AVRCP spec 6).
    GetElementAttributes {
        /// Identifier.
        identifier: u64,
        /// Empty means "all attributes" (AVRCP spec 6.6.1).
        attribute_ids: Vec<u32>,
    },
    /// `GetPlayStatus` command PDU (AVRCP spec 6).
    GetPlayStatus,
    /// `RequestContinuingResponse` command PDU (AVRCP spec 6.8.1): "send me
    /// the next fragment of the response to `continuing_pdu_id`".
    RequestContinuingResponse {
        /// The PDU ID of the *original* command whose response is being
        /// continued — not this command's own 0x40.
        continuing_pdu_id: u8,
    },
    /// `AbortContinuingResponse` command PDU (AVRCP spec 6.8.2): "throw away
    /// the rest of the response to `continuing_pdu_id`, I have stopped
    /// listening".
    AbortContinuingResponse {
        /// The PDU ID of the original command whose response is abandoned.
        continuing_pdu_id: u8,
    },
    /// `RegisterNotification` command PDU (AVRCP spec 6).
    RegisterNotification {
        /// Event id.
        event_id: u8,
        /// Only meaningful for PLAYBACK_POS_CHANGED (seconds).
        playback_interval: u32,
    },
    /// `SetAbsoluteVolume` command PDU (AVRCP spec 6).
    SetAbsoluteVolume {
        /// Volume.
        volume: u8,
    },
    /// `SetAddressedPlayer` command PDU (AVRCP spec 6).
    SetAddressedPlayer {
        /// Player id.
        player_id: u16,
    },
    /// `SetBrowsedPlayer` command PDU (AVRCP spec 6).
    SetBrowsedPlayer {
        /// Player id.
        player_id: u16,
    },
    /// `GetFolderItems` command PDU (AVRCP spec 6).
    GetFolderItems {
        /// Scope.
        scope: u8,
        /// Start item.
        start_item: u32,
        /// End item.
        end_item: u32,
        /// Attribute ids.
        attribute_ids: Vec<u32>,
    },
    /// `ChangePath` command PDU (AVRCP spec 6).
    ChangePath {
        /// Uid counter.
        uid_counter: u16,
        /// Direction.
        direction: u8,
        /// Folder uid.
        folder_uid: u64,
    },
    /// `GetItemAttributes` command PDU (AVRCP spec 6).
    GetItemAttributes {
        /// Scope.
        scope: u8,
        /// Uid.
        uid: u64,
        /// Uid counter.
        uid_counter: u16,
        /// Attribute ids.
        attribute_ids: Vec<u32>,
    },
    /// `GetTotalNumberOfItems` command PDU (AVRCP spec 6).
    GetTotalNumberOfItems {
        /// Scope.
        scope: u8,
    },
    /// `Search` command PDU (AVRCP spec 6).
    Search {
        /// Character set id.
        character_set_id: u16,
        /// Search string.
        search_string: String,
    },
    /// `PlayItem` command PDU (AVRCP spec 6).
    PlayItem {
        /// Scope.
        scope: u8,
        /// Uid.
        uid: u64,
        /// Uid counter.
        uid_counter: u16,
    },
    /// `AddToNowPlaying` command PDU (AVRCP spec 6).
    AddToNowPlaying {
        /// Scope.
        scope: u8,
        /// Uid.
        uid: u64,
        /// Uid counter.
        uid_counter: u16,
    },
}

impl Command {
    /// The AVRCP PDU ID for this command (see [`pdu_id`]).
    pub fn pdu_id(&self) -> u8 {
        use pdu_id as id;
        match self {
            Command::GetCapabilities { .. } => id::GET_CAPABILITIES,
            Command::ListPlayerApplicationSettingAttributes => {
                id::LIST_PLAYER_APPLICATION_SETTING_ATTRIBUTES
            }
            Command::ListPlayerApplicationSettingValues { .. } => {
                id::LIST_PLAYER_APPLICATION_SETTING_VALUES
            }
            Command::GetCurrentPlayerApplicationSettingValue { .. } => {
                id::GET_CURRENT_PLAYER_APPLICATION_SETTING_VALUE
            }
            Command::SetPlayerApplicationSettingValue { .. } => {
                id::SET_PLAYER_APPLICATION_SETTING_VALUE
            }
            Command::GetPlayerApplicationSettingAttributeText { .. } => {
                id::GET_PLAYER_APPLICATION_SETTING_ATTRIBUTE_TEXT
            }
            Command::GetPlayerApplicationSettingValueText { .. } => {
                id::GET_PLAYER_APPLICATION_SETTING_VALUE_TEXT
            }
            Command::InformDisplayableCharacterSet { .. } => id::INFORM_DISPLAYABLE_CHARACTER_SET,
            Command::InformBatteryStatusOfCt { .. } => id::INFORM_BATTERY_STATUS_OF_CT,
            Command::GetElementAttributes { .. } => id::GET_ELEMENT_ATTRIBUTES,
            Command::GetPlayStatus => id::GET_PLAY_STATUS,
            Command::RequestContinuingResponse { .. } => id::REQUEST_CONTINUING_RESPONSE,
            Command::AbortContinuingResponse { .. } => id::ABORT_CONTINUING_RESPONSE,
            Command::RegisterNotification { .. } => id::REGISTER_NOTIFICATION,
            Command::SetAbsoluteVolume { .. } => id::SET_ABSOLUTE_VOLUME,
            Command::SetAddressedPlayer { .. } => id::SET_ADDRESSED_PLAYER,
            Command::SetBrowsedPlayer { .. } => id::SET_BROWSED_PLAYER,
            Command::GetFolderItems { .. } => id::GET_FOLDER_ITEMS,
            Command::ChangePath { .. } => id::CHANGE_PATH,
            Command::GetItemAttributes { .. } => id::GET_ITEM_ATTRIBUTES,
            Command::GetTotalNumberOfItems { .. } => id::GET_TOTAL_NUMBER_OF_ITEMS,
            Command::Search { .. } => id::SEARCH,
            Command::PlayItem { .. } => id::PLAY_ITEM,
            Command::AddToNowPlaying { .. } => id::ADD_TO_NOW_PLAYING,
        }
    }

    /// Serializes the PDU parameters (excluding the PDU header).
    pub fn to_parameters(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Command::ListPlayerApplicationSettingAttributes | Command::GetPlayStatus => {}
            Command::GetCapabilities { capability_id } => out.push(*capability_id),
            Command::ListPlayerApplicationSettingValues { attribute_id } => {
                out.push(*attribute_id);
            }
            Command::GetCurrentPlayerApplicationSettingValue { attribute_ids }
            | Command::GetPlayerApplicationSettingAttributeText { attribute_ids } => {
                write_u8_list(&mut out, attribute_ids);
            }
            Command::SetPlayerApplicationSettingValue { settings } => {
                write_setting_list(&mut out, settings);
            }
            Command::GetPlayerApplicationSettingValueText {
                attribute_id,
                value_ids,
            } => {
                out.push(*attribute_id);
                write_u8_list(&mut out, value_ids);
            }
            Command::InformDisplayableCharacterSet { character_set_ids } => {
                out.push(character_set_ids.len() as u8);
                for id in character_set_ids {
                    out.extend_from_slice(&id.to_be_bytes());
                }
            }
            Command::InformBatteryStatusOfCt { battery_status } => out.push(*battery_status),
            Command::RequestContinuingResponse { continuing_pdu_id }
            | Command::AbortContinuingResponse { continuing_pdu_id } => {
                out.push(*continuing_pdu_id);
            }
            Command::GetElementAttributes {
                identifier,
                attribute_ids,
            } => {
                out.extend_from_slice(&identifier.to_be_bytes());
                write_u32_list(&mut out, attribute_ids);
            }
            Command::RegisterNotification {
                event_id,
                playback_interval,
            } => {
                out.push(*event_id);
                out.extend_from_slice(&playback_interval.to_be_bytes());
            }
            Command::SetAbsoluteVolume { volume } => out.push(*volume),
            Command::SetAddressedPlayer { player_id } | Command::SetBrowsedPlayer { player_id } => {
                out.extend_from_slice(&player_id.to_be_bytes());
            }
            Command::GetFolderItems {
                scope,
                start_item,
                end_item,
                attribute_ids,
            } => {
                out.push(*scope);
                out.extend_from_slice(&start_item.to_be_bytes());
                out.extend_from_slice(&end_item.to_be_bytes());
                write_u32_list(&mut out, attribute_ids);
            }
            Command::ChangePath {
                uid_counter,
                direction,
                folder_uid,
            } => {
                out.extend_from_slice(&uid_counter.to_be_bytes());
                out.push(*direction);
                out.extend_from_slice(&folder_uid.to_be_bytes());
            }
            Command::GetItemAttributes {
                scope,
                uid,
                uid_counter,
                attribute_ids,
            } => {
                out.push(*scope);
                out.extend_from_slice(&uid.to_be_bytes());
                out.extend_from_slice(&uid_counter.to_be_bytes());
                write_u32_list(&mut out, attribute_ids);
            }
            Command::GetTotalNumberOfItems { scope } => out.push(*scope),
            Command::Search {
                character_set_id,
                search_string,
            } => {
                out.extend_from_slice(&character_set_id.to_be_bytes());
                put_string16(&mut out, search_string);
            }
            Command::PlayItem {
                scope,
                uid,
                uid_counter,
            }
            | Command::AddToNowPlaying {
                scope,
                uid,
                uid_counter,
            } => {
                out.push(*scope);
                out.extend_from_slice(&uid.to_be_bytes());
                out.extend_from_slice(&uid_counter.to_be_bytes());
            }
        }
        out
    }

    /// Parses the parameters of the PDU identified by `pdu_id`.
    pub fn parse(pdu_id: u8, parameters: &[u8]) -> Option<Command> {
        use self::pdu_id as id;
        let mut r = Reader::new(parameters);
        match pdu_id {
            id::GET_CAPABILITIES => Some(Command::GetCapabilities {
                capability_id: r.u8()?,
            }),
            id::LIST_PLAYER_APPLICATION_SETTING_ATTRIBUTES => {
                Some(Command::ListPlayerApplicationSettingAttributes)
            }
            id::LIST_PLAYER_APPLICATION_SETTING_VALUES => {
                Some(Command::ListPlayerApplicationSettingValues {
                    attribute_id: r.u8()?,
                })
            }
            id::GET_CURRENT_PLAYER_APPLICATION_SETTING_VALUE => {
                Some(Command::GetCurrentPlayerApplicationSettingValue {
                    attribute_ids: read_u8_list(&mut r)?,
                })
            }
            id::SET_PLAYER_APPLICATION_SETTING_VALUE => {
                Some(Command::SetPlayerApplicationSettingValue {
                    settings: read_setting_list(&mut r)?,
                })
            }
            id::GET_PLAYER_APPLICATION_SETTING_ATTRIBUTE_TEXT => {
                Some(Command::GetPlayerApplicationSettingAttributeText {
                    attribute_ids: read_u8_list(&mut r)?,
                })
            }
            id::GET_PLAYER_APPLICATION_SETTING_VALUE_TEXT => {
                Some(Command::GetPlayerApplicationSettingValueText {
                    attribute_id: r.u8()?,
                    value_ids: read_u8_list(&mut r)?,
                })
            }
            id::INFORM_DISPLAYABLE_CHARACTER_SET => {
                let count = r.u8()?;
                Some(Command::InformDisplayableCharacterSet {
                    character_set_ids: (0..count).map(|_| r.u16()).collect::<Option<Vec<_>>>()?,
                })
            }
            id::INFORM_BATTERY_STATUS_OF_CT => Some(Command::InformBatteryStatusOfCt {
                battery_status: r.u8()?,
            }),
            id::GET_ELEMENT_ATTRIBUTES => Some(Command::GetElementAttributes {
                identifier: r.u64()?,
                attribute_ids: read_u32_list(&mut r)?,
            }),
            id::GET_PLAY_STATUS => Some(Command::GetPlayStatus),
            id::REQUEST_CONTINUING_RESPONSE => Some(Command::RequestContinuingResponse {
                continuing_pdu_id: r.u8()?,
            }),
            id::ABORT_CONTINUING_RESPONSE => Some(Command::AbortContinuingResponse {
                continuing_pdu_id: r.u8()?,
            }),
            id::REGISTER_NOTIFICATION => Some(Command::RegisterNotification {
                event_id: r.u8()?,
                playback_interval: r.u32()?,
            }),
            id::SET_ABSOLUTE_VOLUME => Some(Command::SetAbsoluteVolume { volume: r.u8()? }),
            id::SET_ADDRESSED_PLAYER => Some(Command::SetAddressedPlayer {
                player_id: r.u16()?,
            }),
            id::SET_BROWSED_PLAYER => Some(Command::SetBrowsedPlayer {
                player_id: r.u16()?,
            }),
            id::GET_FOLDER_ITEMS => Some(Command::GetFolderItems {
                scope: r.u8()?,
                start_item: r.u32()?,
                end_item: r.u32()?,
                attribute_ids: read_u32_list(&mut r)?,
            }),
            id::CHANGE_PATH => Some(Command::ChangePath {
                uid_counter: r.u16()?,
                direction: r.u8()?,
                folder_uid: r.u64()?,
            }),
            id::GET_ITEM_ATTRIBUTES => Some(Command::GetItemAttributes {
                scope: r.u8()?,
                uid: r.u64()?,
                uid_counter: r.u16()?,
                attribute_ids: read_u32_list(&mut r)?,
            }),
            id::GET_TOTAL_NUMBER_OF_ITEMS => {
                Some(Command::GetTotalNumberOfItems { scope: r.u8()? })
            }
            id::SEARCH => Some(Command::Search {
                character_set_id: r.u16()?,
                search_string: r.string16()?,
            }),
            id::PLAY_ITEM => Some(Command::PlayItem {
                scope: r.u8()?,
                uid: r.u64()?,
                uid_counter: r.u16()?,
            }),
            id::ADD_TO_NOW_PLAYING => Some(Command::AddToNowPlaying {
                scope: r.u8()?,
                uid: r.u64()?,
                uid_counter: r.u16()?,
            }),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Responses
// ---------------------------------------------------------------------------

/// One AVRCP response PDU's parameters, for the accepted/stable response
/// shapes; REJECTED and NOT IMPLEMENTED responses are conveyed by the AV/C
/// response code and handled by [`Protocol`] directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// `GetCapabilities` response PDU (AVRCP spec 6).
    GetCapabilities {
        /// Capabilities.
        capabilities: CapabilityList,
    },
    /// `ListPlayerApplicationSettingAttributes` response PDU (AVRCP spec 6).
    ListPlayerApplicationSettingAttributes {
        /// Attribute ids.
        attribute_ids: Vec<u8>,
    },
    /// `ListPlayerApplicationSettingValues` response PDU (AVRCP spec 6).
    ListPlayerApplicationSettingValues {
        /// Value ids.
        value_ids: Vec<u8>,
    },
    /// `GetCurrentPlayerApplicationSettingValue` response PDU (AVRCP spec 6).
    GetCurrentPlayerApplicationSettingValue {
        /// Settings.
        settings: Vec<ApplicationSetting>,
    },
    /// `SetPlayerApplicationSettingValue` response PDU (AVRCP spec 6).
    SetPlayerApplicationSettingValue,
    /// `GetPlayerApplicationSettingAttributeText` response PDU (AVRCP spec 6).
    GetPlayerApplicationSettingAttributeText {
        /// Entries.
        entries: Vec<SettingText>,
    },
    /// `GetPlayerApplicationSettingValueText` response PDU (AVRCP spec 6).
    GetPlayerApplicationSettingValueText {
        /// Entries.
        entries: Vec<SettingText>,
    },
    /// `InformDisplayableCharacterSet` response PDU (AVRCP spec 6).
    InformDisplayableCharacterSet,
    /// `InformBatteryStatusOfCt` response PDU (AVRCP spec 6).
    InformBatteryStatusOfCt,
    /// `GetElementAttributes` response PDU (AVRCP spec 6).
    GetElementAttributes {
        /// Attributes.
        attributes: Vec<MediaAttribute>,
    },
    /// `GetPlayStatus` response PDU (AVRCP spec 6).
    GetPlayStatus {
        /// Song length.
        song_length: u32,
        /// Song position.
        song_position: u32,
        /// Play status.
        play_status: u8,
    },
    /// `AbortContinuingResponse` response PDU (AVRCP spec 6.8.2): the target
    /// confirms it has dropped the rest of a fragmented response. Carries no
    /// parameters — the ACCEPTED response code is the whole answer.
    AbortContinuingResponse,
    /// `RegisterNotification` response PDU (AVRCP spec 6).
    RegisterNotification {
        /// Event.
        event: Event,
    },
    /// `SetAbsoluteVolume` response PDU (AVRCP spec 6).
    SetAbsoluteVolume {
        /// Volume.
        volume: u8,
    },
    /// `SetAddressedPlayer` response PDU (AVRCP spec 6).
    SetAddressedPlayer {
        /// Status.
        status: u8,
    },
    /// `SetBrowsedPlayer` response PDU (AVRCP spec 6).
    SetBrowsedPlayer {
        /// Status.
        status: u8,
        /// Uid counter.
        uid_counter: u16,
        /// Number of items.
        number_of_items: u32,
        /// Character set id.
        character_set_id: u16,
        /// Folder names.
        folder_names: Vec<String>,
    },
    /// `GetFolderItems` response PDU (AVRCP spec 6).
    GetFolderItems {
        /// Status.
        status: u8,
        /// Uid counter.
        uid_counter: u16,
        /// Items.
        items: Vec<BrowseableItem>,
    },
    /// `ChangePath` response PDU (AVRCP spec 6).
    ChangePath {
        /// Status.
        status: u8,
        /// Number of items.
        number_of_items: u32,
    },
    /// `GetItemAttributes` response PDU (AVRCP spec 6).
    GetItemAttributes {
        /// Status.
        status: u8,
        /// Attributes.
        attributes: Vec<MediaAttribute>,
    },
    /// `GetTotalNumberOfItems` response PDU (AVRCP spec 6).
    GetTotalNumberOfItems {
        /// Status.
        status: u8,
        /// Uid counter.
        uid_counter: u16,
        /// Number of items.
        number_of_items: u32,
    },
    /// `Search` response PDU (AVRCP spec 6).
    Search {
        /// Status.
        status: u8,
        /// Uid counter.
        uid_counter: u16,
        /// Number of items.
        number_of_items: u32,
    },
    /// `PlayItem` response PDU (AVRCP spec 6).
    PlayItem {
        /// Status.
        status: u8,
    },
    /// `AddToNowPlaying` response PDU (AVRCP spec 6).
    AddToNowPlaying {
        /// Status.
        status: u8,
    },
}

impl Response {
    /// The AVRCP PDU ID for this response (see [`pdu_id`]).
    pub fn pdu_id(&self) -> u8 {
        use pdu_id as id;
        match self {
            Response::GetCapabilities { .. } => id::GET_CAPABILITIES,
            Response::ListPlayerApplicationSettingAttributes { .. } => {
                id::LIST_PLAYER_APPLICATION_SETTING_ATTRIBUTES
            }
            Response::ListPlayerApplicationSettingValues { .. } => {
                id::LIST_PLAYER_APPLICATION_SETTING_VALUES
            }
            Response::GetCurrentPlayerApplicationSettingValue { .. } => {
                id::GET_CURRENT_PLAYER_APPLICATION_SETTING_VALUE
            }
            Response::SetPlayerApplicationSettingValue => id::SET_PLAYER_APPLICATION_SETTING_VALUE,
            Response::GetPlayerApplicationSettingAttributeText { .. } => {
                id::GET_PLAYER_APPLICATION_SETTING_ATTRIBUTE_TEXT
            }
            Response::GetPlayerApplicationSettingValueText { .. } => {
                id::GET_PLAYER_APPLICATION_SETTING_VALUE_TEXT
            }
            Response::InformDisplayableCharacterSet => id::INFORM_DISPLAYABLE_CHARACTER_SET,
            Response::InformBatteryStatusOfCt => id::INFORM_BATTERY_STATUS_OF_CT,
            Response::GetElementAttributes { .. } => id::GET_ELEMENT_ATTRIBUTES,
            Response::GetPlayStatus { .. } => id::GET_PLAY_STATUS,
            Response::AbortContinuingResponse => id::ABORT_CONTINUING_RESPONSE,
            Response::RegisterNotification { .. } => id::REGISTER_NOTIFICATION,
            Response::SetAbsoluteVolume { .. } => id::SET_ABSOLUTE_VOLUME,
            Response::SetAddressedPlayer { .. } => id::SET_ADDRESSED_PLAYER,
            Response::SetBrowsedPlayer { .. } => id::SET_BROWSED_PLAYER,
            Response::GetFolderItems { .. } => id::GET_FOLDER_ITEMS,
            Response::ChangePath { .. } => id::CHANGE_PATH,
            Response::GetItemAttributes { .. } => id::GET_ITEM_ATTRIBUTES,
            Response::GetTotalNumberOfItems { .. } => id::GET_TOTAL_NUMBER_OF_ITEMS,
            Response::Search { .. } => id::SEARCH,
            Response::PlayItem { .. } => id::PLAY_ITEM,
            Response::AddToNowPlaying { .. } => id::ADD_TO_NOW_PLAYING,
        }
    }

    /// Serializes the PDU parameters (excluding the PDU header).
    pub fn to_parameters(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Response::SetPlayerApplicationSettingValue
            | Response::InformDisplayableCharacterSet
            | Response::InformBatteryStatusOfCt
            | Response::AbortContinuingResponse => {}
            Response::GetCapabilities { capabilities } => {
                out.push(capabilities.capability_id());
                match capabilities {
                    CapabilityList::CompanyIds(ids) => {
                        out.push(ids.len() as u8);
                        for id in ids {
                            out.extend_from_slice(&id.to_be_bytes()[1..]);
                        }
                    }
                    CapabilityList::EventIds(ids) => write_u8_list(&mut out, ids),
                }
            }
            Response::ListPlayerApplicationSettingAttributes { attribute_ids } => {
                write_u8_list(&mut out, attribute_ids);
            }
            Response::ListPlayerApplicationSettingValues { value_ids } => {
                write_u8_list(&mut out, value_ids);
            }
            Response::GetCurrentPlayerApplicationSettingValue { settings } => {
                write_setting_list(&mut out, settings);
            }
            Response::GetPlayerApplicationSettingAttributeText { entries }
            | Response::GetPlayerApplicationSettingValueText { entries } => {
                write_setting_text_list(&mut out, entries);
            }
            Response::GetElementAttributes { attributes } => {
                write_media_attribute_list(&mut out, attributes);
            }
            Response::GetPlayStatus {
                song_length,
                song_position,
                play_status,
            } => {
                out.extend_from_slice(&song_length.to_be_bytes());
                out.extend_from_slice(&song_position.to_be_bytes());
                out.push(*play_status);
            }
            Response::RegisterNotification { event } => out.extend(event.to_bytes()),
            Response::SetAbsoluteVolume { volume } => out.push(*volume),
            Response::SetAddressedPlayer { status }
            | Response::PlayItem { status }
            | Response::AddToNowPlaying { status } => out.push(*status),
            Response::SetBrowsedPlayer {
                status,
                uid_counter,
                number_of_items,
                character_set_id,
                folder_names,
            } => {
                out.push(*status);
                out.extend_from_slice(&uid_counter.to_be_bytes());
                out.extend_from_slice(&number_of_items.to_be_bytes());
                out.extend_from_slice(&character_set_id.to_be_bytes());
                out.push(folder_names.len() as u8);
                for name in folder_names {
                    put_string16(&mut out, name);
                }
            }
            Response::GetFolderItems {
                status,
                uid_counter,
                items,
            } => {
                out.push(*status);
                out.extend_from_slice(&uid_counter.to_be_bytes());
                out.extend_from_slice(&(items.len() as u16).to_be_bytes());
                for item in items {
                    item.write(&mut out);
                }
            }
            Response::ChangePath {
                status,
                number_of_items,
            } => {
                out.push(*status);
                out.extend_from_slice(&number_of_items.to_be_bytes());
            }
            Response::GetItemAttributes { status, attributes } => {
                out.push(*status);
                write_media_attribute_list(&mut out, attributes);
            }
            Response::GetTotalNumberOfItems {
                status,
                uid_counter,
                number_of_items,
            }
            | Response::Search {
                status,
                uid_counter,
                number_of_items,
            } => {
                out.push(*status);
                out.extend_from_slice(&uid_counter.to_be_bytes());
                out.extend_from_slice(&number_of_items.to_be_bytes());
            }
        }
        out
    }

    /// Parses the parameters of the response PDU identified by `pdu_id`.
    pub fn parse(pdu_id: u8, parameters: &[u8]) -> Option<Response> {
        use self::pdu_id as id;
        let mut r = Reader::new(parameters);
        match pdu_id {
            id::GET_CAPABILITIES => {
                let capability_id = r.u8()?;
                let count = r.u8()?;
                let capabilities = match capability_id {
                    capability_id::COMPANY_ID => CapabilityList::CompanyIds(
                        (0..count).map(|_| r.u24()).collect::<Option<Vec<_>>>()?,
                    ),
                    capability_id::EVENTS_SUPPORTED => CapabilityList::EventIds(
                        (0..count).map(|_| r.u8()).collect::<Option<Vec<_>>>()?,
                    ),
                    _ => return None,
                };
                Some(Response::GetCapabilities { capabilities })
            }
            id::LIST_PLAYER_APPLICATION_SETTING_ATTRIBUTES => {
                Some(Response::ListPlayerApplicationSettingAttributes {
                    attribute_ids: read_u8_list(&mut r)?,
                })
            }
            id::LIST_PLAYER_APPLICATION_SETTING_VALUES => {
                Some(Response::ListPlayerApplicationSettingValues {
                    value_ids: read_u8_list(&mut r)?,
                })
            }
            id::GET_CURRENT_PLAYER_APPLICATION_SETTING_VALUE => {
                Some(Response::GetCurrentPlayerApplicationSettingValue {
                    settings: read_setting_list(&mut r)?,
                })
            }
            id::SET_PLAYER_APPLICATION_SETTING_VALUE => {
                Some(Response::SetPlayerApplicationSettingValue)
            }
            id::GET_PLAYER_APPLICATION_SETTING_ATTRIBUTE_TEXT => {
                Some(Response::GetPlayerApplicationSettingAttributeText {
                    entries: read_setting_text_list(&mut r)?,
                })
            }
            id::GET_PLAYER_APPLICATION_SETTING_VALUE_TEXT => {
                Some(Response::GetPlayerApplicationSettingValueText {
                    entries: read_setting_text_list(&mut r)?,
                })
            }
            id::INFORM_DISPLAYABLE_CHARACTER_SET => Some(Response::InformDisplayableCharacterSet),
            id::INFORM_BATTERY_STATUS_OF_CT => Some(Response::InformBatteryStatusOfCt),
            id::ABORT_CONTINUING_RESPONSE => Some(Response::AbortContinuingResponse),
            id::GET_ELEMENT_ATTRIBUTES => Some(Response::GetElementAttributes {
                attributes: read_media_attribute_list(&mut r)?,
            }),
            id::GET_PLAY_STATUS => Some(Response::GetPlayStatus {
                song_length: r.u32()?,
                song_position: r.u32()?,
                play_status: r.u8()?,
            }),
            id::REGISTER_NOTIFICATION => Some(Response::RegisterNotification {
                event: Event::parse(r.rest())?,
            }),
            id::SET_ABSOLUTE_VOLUME => Some(Response::SetAbsoluteVolume { volume: r.u8()? }),
            id::SET_ADDRESSED_PLAYER => Some(Response::SetAddressedPlayer { status: r.u8()? }),
            id::SET_BROWSED_PLAYER => {
                let status = r.u8()?;
                let uid_counter = r.u16()?;
                let number_of_items = r.u32()?;
                let character_set_id = r.u16()?;
                let folder_depth = r.u8()?;
                Some(Response::SetBrowsedPlayer {
                    status,
                    uid_counter,
                    number_of_items,
                    character_set_id,
                    folder_names: (0..folder_depth)
                        .map(|_| r.string16())
                        .collect::<Option<Vec<_>>>()?,
                })
            }
            id::GET_FOLDER_ITEMS => {
                let status = r.u8()?;
                let uid_counter = r.u16()?;
                let count = r.u16()?;
                Some(Response::GetFolderItems {
                    status,
                    uid_counter,
                    items: (0..count)
                        .map(|_| BrowseableItem::read(&mut r))
                        .collect::<Option<Vec<_>>>()?,
                })
            }
            id::CHANGE_PATH => Some(Response::ChangePath {
                status: r.u8()?,
                number_of_items: r.u32()?,
            }),
            id::GET_ITEM_ATTRIBUTES => Some(Response::GetItemAttributes {
                status: r.u8()?,
                attributes: read_media_attribute_list(&mut r)?,
            }),
            id::GET_TOTAL_NUMBER_OF_ITEMS => Some(Response::GetTotalNumberOfItems {
                status: r.u8()?,
                uid_counter: r.u16()?,
                number_of_items: r.u32()?,
            }),
            id::SEARCH => Some(Response::Search {
                status: r.u8()?,
                uid_counter: r.u16()?,
                number_of_items: r.u32()?,
            }),
            id::PLAY_ITEM => Some(Response::PlayItem { status: r.u8()? }),
            id::ADD_TO_NOW_PLAYING => Some(Response::AddToNowPlaying { status: r.u8()? }),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Notification events
// ---------------------------------------------------------------------------

/// A notification event as carried in RegisterNotification responses
/// (AVRCP spec 6.7.2): the event ID byte followed by event-specific data.
/// Event IDs without structured parameters here (battery/system status and
/// the track boundary events) round-trip through [`Event::Generic`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// `PlaybackStatusChanged` notification event (AVRCP spec 6.7.2).
    PlaybackStatusChanged {
        /// Play status.
        play_status: u8,
    },
    /// `TrackChanged` notification event (AVRCP spec 6.7.2).
    TrackChanged {
        /// Uid.
        uid: u64,
    },
    /// `PlaybackPosChanged` notification event (AVRCP spec 6.7.2).
    PlaybackPosChanged {
        /// Playback position.
        playback_position: u32,
    },
    /// `PlayerApplicationSettingChanged` notification event (AVRCP spec 6.7.2).
    PlayerApplicationSettingChanged {
        /// Settings.
        settings: Vec<ApplicationSetting>,
    },
    /// `NowPlayingContentChanged` notification event (AVRCP spec 6.7.2).
    NowPlayingContentChanged,
    /// `AvailablePlayersChanged` notification event (AVRCP spec 6.7.2).
    AvailablePlayersChanged,
    /// `AddressedPlayerChanged` notification event (AVRCP spec 6.7.2).
    AddressedPlayerChanged {
        /// Player id.
        player_id: u16,
        /// Uid counter.
        uid_counter: u16,
    },
    /// `UidsChanged` notification event (AVRCP spec 6.7.2).
    UidsChanged {
        /// Uid counter.
        uid_counter: u16,
    },
    /// `VolumeChanged` notification event (AVRCP spec 6.7.2).
    VolumeChanged {
        /// Volume.
        volume: u8,
    },
    /// `Generic` notification event (AVRCP spec 6.7.2).
    Generic {
        /// Event id.
        event_id: u8,
        /// Payload data.
        data: Vec<u8>,
    },
}

impl Event {
    /// The notification event ID (see [`event_id`]) for this event.
    pub fn event_id(&self) -> u8 {
        match self {
            Event::PlaybackStatusChanged { .. } => event_id::PLAYBACK_STATUS_CHANGED,
            Event::TrackChanged { .. } => event_id::TRACK_CHANGED,
            Event::PlaybackPosChanged { .. } => event_id::PLAYBACK_POS_CHANGED,
            Event::PlayerApplicationSettingChanged { .. } => {
                event_id::PLAYER_APPLICATION_SETTING_CHANGED
            }
            Event::NowPlayingContentChanged => event_id::NOW_PLAYING_CONTENT_CHANGED,
            Event::AvailablePlayersChanged => event_id::AVAILABLE_PLAYERS_CHANGED,
            Event::AddressedPlayerChanged { .. } => event_id::ADDRESSED_PLAYER_CHANGED,
            Event::UidsChanged { .. } => event_id::UIDS_CHANGED,
            Event::VolumeChanged { .. } => event_id::VOLUME_CHANGED,
            Event::Generic { event_id, .. } => *event_id,
        }
    }

    /// Serializes this event to its notification parameter bytes (event ID
    /// followed by event-specific data).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = vec![self.event_id()];
        match self {
            Event::NowPlayingContentChanged | Event::AvailablePlayersChanged => {}
            Event::PlaybackStatusChanged { play_status } => out.push(*play_status),
            Event::TrackChanged { uid } => out.extend_from_slice(&uid.to_be_bytes()),
            Event::PlaybackPosChanged { playback_position } => {
                out.extend_from_slice(&playback_position.to_be_bytes());
            }
            Event::PlayerApplicationSettingChanged { settings } => {
                write_setting_list(&mut out, settings);
            }
            Event::AddressedPlayerChanged {
                player_id,
                uid_counter,
            } => {
                out.extend_from_slice(&player_id.to_be_bytes());
                out.extend_from_slice(&uid_counter.to_be_bytes());
            }
            Event::UidsChanged { uid_counter } => {
                out.extend_from_slice(&uid_counter.to_be_bytes());
            }
            Event::VolumeChanged { volume } => out.push(*volume),
            Event::Generic { data, .. } => out.extend_from_slice(data),
        }
        out
    }

    /// Parses a notification event from its parameter bytes; unknown event
    /// IDs round-trip through [`Event::Generic`].
    pub fn parse(data: &[u8]) -> Option<Event> {
        let mut r = Reader::new(data);
        let event_id = r.u8()?;
        match event_id {
            event_id::PLAYBACK_STATUS_CHANGED => Some(Event::PlaybackStatusChanged {
                play_status: r.u8()?,
            }),
            event_id::TRACK_CHANGED => Some(Event::TrackChanged { uid: r.u64()? }),
            event_id::PLAYBACK_POS_CHANGED => Some(Event::PlaybackPosChanged {
                playback_position: r.u32()?,
            }),
            event_id::PLAYER_APPLICATION_SETTING_CHANGED => {
                Some(Event::PlayerApplicationSettingChanged {
                    settings: read_setting_list(&mut r)?,
                })
            }
            event_id::NOW_PLAYING_CONTENT_CHANGED => Some(Event::NowPlayingContentChanged),
            event_id::AVAILABLE_PLAYERS_CHANGED => Some(Event::AvailablePlayersChanged),
            event_id::ADDRESSED_PLAYER_CHANGED => Some(Event::AddressedPlayerChanged {
                player_id: r.u16()?,
                uid_counter: r.u16()?,
            }),
            event_id::UIDS_CHANGED => Some(Event::UidsChanged {
                uid_counter: r.u16()?,
            }),
            event_id::VOLUME_CHANGED => Some(Event::VolumeChanged { volume: r.u8()? }),
            _ => Some(Event::Generic {
                event_id,
                data: r.rest().to_vec(),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// PDU assembler
// ---------------------------------------------------------------------------

// AVRCP-level packet types, in the low 2 bits of the PDU header's second
// byte (AVRCP spec 6.3.1).
const PDU_SINGLE: u8 = 0b00;
const PDU_START: u8 = 0b01;
const PDU_CONTINUE: u8 = 0b10;
const PDU_END: u8 = 0b11;

/// Serializes one AVRCP PDU (header + parameters), splitting the parameters
/// into start/continue/end packets when they exceed `max_parameter_size`
/// (AVRCP spec 6.3.1).
///
/// Only the **first** packet is sent when the response is built; the peer
/// pulls each of the rest with a `RequestContinuingResponse` command, which
/// [`Protocol`] answers from the fragments it held back. Fragmenting without
/// that machinery would be worse than not fragmenting at all: the controller
/// would sit on a START packet waiting for a CONTINUE that never comes.
pub fn write_pdu(pdu_id: u8, parameters: &[u8], max_parameter_size: usize) -> Vec<Vec<u8>> {
    let max_parameter_size = max_parameter_size.max(1);
    let chunks: Vec<&[u8]> = if parameters.is_empty() {
        vec![&[]]
    } else {
        parameters.chunks(max_parameter_size).collect()
    };
    let last = chunks.len() - 1;
    chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| {
            let packet_type = match (index, index == last) {
                (_, true) if index == 0 => PDU_SINGLE,
                (0, false) => PDU_START,
                (_, false) => PDU_CONTINUE,
                (_, true) => PDU_END,
            };
            let mut pdu = vec![pdu_id, packet_type];
            pdu.extend_from_slice(&(chunk.len() as u16).to_be_bytes());
            pdu.extend_from_slice(chunk);
            pdu
        })
        .collect()
}

/// Reassembles AVRCP PDUs fragmented across vendor-dependent AV/C frames
/// (AVRCP spec 6.3.1). Malformed or out-of-sequence packets are dropped.
#[derive(Debug, Default)]
pub struct PduAssembler {
    pdu_id: Option<u8>,
    parameters: Vec<u8>,
}

impl PduAssembler {
    /// Creates an empty assembler with no PDU in progress.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a fragmented PDU is part-assembled and waiting for more.
    ///
    /// [`Self::on_pdu`] returns `None` both for "not finished" and for "that
    /// packet was rubbish and I threw everything away"; a controller deciding
    /// whether to ask for a continuation must tell those apart.
    pub fn in_progress(&self) -> bool {
        self.pdu_id.is_some()
    }

    /// Discards any partially reassembled PDU.
    pub(crate) fn reset(&mut self) {
        self.pdu_id = None;
        self.parameters.clear();
    }

    /// Feeds one PDU packet; returns `(pdu_id, parameters)` once complete.
    pub fn on_pdu(&mut self, pdu: &[u8]) -> Option<(u8, Vec<u8>)> {
        let pdu_id = *pdu.first()?;
        let packet_type = pdu.get(1)? & 3;
        let length = usize::from(u16::from_be_bytes([*pdu.get(2)?, *pdu.get(3)?]));
        let Some(parameter) = pdu.get(4..4 + length) else {
            // The parameter length field exceeds the packet.
            self.reset();
            return None;
        };

        if matches!(packet_type, PDU_SINGLE | PDU_START) && self.pdu_id.is_some() {
            // The previous fragmented PDU was never terminated.
            self.reset();
        }
        if matches!(packet_type, PDU_CONTINUE | PDU_END) {
            if self.pdu_id != Some(pdu_id) {
                self.reset();
                return None;
            }
        } else {
            self.pdu_id = Some(pdu_id);
        }

        self.parameters.extend_from_slice(parameter);
        if matches!(packet_type, PDU_SINGLE | PDU_END) {
            let complete = (pdu_id, std::mem::take(&mut self.parameters));
            self.reset();
            return Some(complete);
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// Result of feeding one incoming PDU into [`Protocol::receive`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvrcpEvent {
    // -- Target role --------------------------------------------------------
    /// The controller pressed or released a key (PASS THROUGH).
    KeyEvent {
        /// Operation id.
        operation_id: u8,
        /// Pressed.
        pressed: bool,
        /// Data.
        data: Vec<u8>,
    },
    /// The controller set our absolute volume (already stored in
    /// [`Protocol::volume`]).
    VolumeSet {
        /// Volume.
        volume: u8,
    },
    /// The controller changed player application settings (already stored).
    PlayerAppSettingsSet {
        /// Settings.
        settings: Vec<ApplicationSetting>,
    },
    /// The controller asked us to play an item.
    PlayItemRequested {
        /// Scope.
        scope: u8,
        /// Uid.
        uid: u64,
        /// Uid counter.
        uid_counter: u16,
    },
    /// The controller registered for a notification; a `notify_*` call for
    /// this event will now produce a CHANGED response.
    NotificationRegistered {
        /// Event id.
        event_id: u8,
    },

    // -- Controller role ----------------------------------------------------
    /// The target answered a PASS THROUGH key event.
    PassThroughResponse {
        /// Response.
        response: ResponseCode,
        /// Operation id.
        operation_id: u8,
        /// Pressed.
        pressed: bool,
    },
    /// GetCapabilities(EVENTS_SUPPORTED) response.
    SupportedEventsReceived(Vec<u8>),
    /// GetCapabilities(COMPANY_ID) response.
    SupportedCompanyIdsReceived(Vec<u32>),
    /// Play Status Received event.
    PlayStatusReceived {
        /// Song length.
        song_length: u32,
        /// Song position.
        song_position: u32,
        /// Play status.
        play_status: u8,
    },
    /// Element Attributes Received event.
    ElementAttributesReceived(Vec<MediaAttribute>),
    /// ListPlayerApplicationSettingAttributes response.
    AppSettingAttributesReceived(Vec<u8>),
    /// ListPlayerApplicationSettingValues response for `attribute_id`.
    AppSettingValuesReceived {
        /// Attribute id.
        attribute_id: u8,
        /// Value ids.
        value_ids: Vec<u8>,
    },
    /// GetCurrentPlayerApplicationSettingValue response.
    CurrentAppSettingsReceived(Vec<ApplicationSetting>),
    /// SetPlayerApplicationSettingValue was accepted.
    AppSettingsAccepted,
    /// SetAbsoluteVolume response: the effective volume the target applied.
    VolumeAccepted {
        /// Volume.
        volume: u8,
    },
    /// PlayItem response.
    PlayItemCompleted {
        /// Status.
        status: u8,
    },
    /// A notification arrived: the INTERIM snapshot at registration, or the
    /// CHANGED response that consumed the registration.
    NotificationReceived {
        /// Event.
        event: Event,
        /// Interim.
        interim: bool,
    },
    /// A response PDU without a dedicated event above.
    ResponseReceived {
        /// Response.
        response: Response,
    },
    /// The target rejected our command PDU.
    CommandRejected {
        /// Pdu id.
        pdu_id: u8,
        /// Status code.
        status_code: u8,
    },
    /// The target does not implement our command PDU.
    CommandNotImplemented {
        /// Pdu id.
        pdu_id: u8,
    },
    /// The peer does not support the profile PID at the AVCTP level.
    InvalidPid {
        /// Protocol identifier (PID).
        pid: u16,
    },
}

#[derive(Debug, Clone)]
enum Pending {
    PassThrough {
        operation_id: u8,
        pressed: bool,
    },
    Vendor {
        pdu_id: u8,
        attribute_id: Option<u8>,
    },
}

/// The tail of a response too big for one AV/C frame, held until the
/// controller asks for it (AVRCP spec 6.8.1).
///
/// A target that sent every fragment unprompted would overrun a controller
/// that is only permitted one outstanding transaction per label; a target
/// that sent only the first and kept nothing would leave the controller
/// waiting forever. Both are failures nothing on the wire distinguishes from
/// a slow peer, which is why the fragments live here rather than being
/// regenerated on demand from state that may have moved on.
#[derive(Debug, Clone)]
struct Continuation {
    /// The PDU ID of the response being fragmented — the *original* command's
    /// (0x20 for GetElementAttributes), never 0x40.
    pdu_id: u8,
    /// The AV/C response code every fragment carries, fixed by the first.
    response_code: ResponseCode,
    /// Fragments not yet sent, in order, each a complete AVRCP PDU.
    remaining: std::collections::VecDeque<Vec<u8>>,
}

/// L2CAP/AVCTP/AV/C overhead in front of an AVRCP PDU's parameters: 3 bytes
/// of AVCTP single-packet header (header byte plus the 16-bit PID), 3 bytes
/// of AV/C frame header (ctype-or-response, subunit, opcode), 3 bytes of
/// vendor company ID, and the 4-byte AVRCP PDU header.
const AVRCP_PDU_OVERHEAD: usize = 3 + 3 + 3 + 4;

/// The largest AV/C frame AVRCP permits on the control channel (AVRCP spec
/// 4.4.1), which is also the L2CAP MTU an AVRCP control channel must accept.
/// A target that fragmented at a larger size would produce frames a
/// conforming controller is entitled to drop.
const AVRCP_MAXIMUM_FRAME_SIZE: usize = 512;

/// The AVRCP state machine for one connection, covering both roles. Target
/// behavior is driven by the public media-player state fields; controller
/// behavior by the command builder methods. Synchronous: [`Protocol::receive`]
/// takes one incoming AVCTP packet (an L2CAP SDU on the control channel) and
/// returns the packets to send back plus events for the caller.
#[derive(Debug)]
pub struct Protocol {
    // -- Target (TG) media-player state, served to the peer -----------------
    /// Event IDs answered in GetCapabilities and accepted for notification
    /// registration.
    pub supported_events: Vec<u8>,
    /// Company IDs answered in GetCapabilities(COMPANY_ID).
    pub(crate) supported_company_ids: Vec<u32>,
    /// Supported player application settings: attribute ID -> value IDs.
    pub supported_player_app_settings: Vec<(u8, Vec<u8>)>,
    /// Current player application settings.
    pub player_app_settings: Vec<ApplicationSetting>,
    /// Current absolute volume (0 to [`MAXIMUM_VOLUME`]).
    pub volume: u8,
    /// Current playback status (see [`play_status`]).
    pub playback_status: u8,
    /// Current track length in milliseconds, served in GetPlayStatus.
    pub(crate) song_length: u32,
    /// Current playback position in milliseconds, served in GetPlayStatus.
    pub(crate) song_position: u32,
    /// UID counter reported to the browsing channel.
    pub(crate) uid_counter: u16,
    /// Currently addressed player ID.
    pub(crate) addressed_player_id: u16,
    /// UID of the current track, served in TrackChanged notifications.
    pub(crate) current_track_uid: u64,
    /// Current track metadata, served in GetElementAttributes responses.
    pub element_attributes: Vec<MediaAttribute>,
    /// The AV/C response code sent for incoming key events (a real player
    /// that cannot honor a key answers NOT IMPLEMENTED or REJECTED).
    pub key_event_response: ResponseCode,

    avctp: avctp::Protocol,
    /// The negotiated L2CAP MTU, kept here as well as in `avctp` because the
    /// AVRCP layer — not the transport — decides where a response is cut.
    peer_mtu: u16,
    /// Part-assembled inbound commands, **keyed by transaction label**, with
    /// the AV/C command type the first fragment carried.
    ///
    /// One assembler for the whole connection was wrong: AVRCP transactions
    /// interleave. A CHANGED notification is unsolicited by definition and
    /// arrives on its own label, so a single assembler would discard a
    /// half-read metadata response the moment the track changed — the exact
    /// pair of things that happen together. The label is 4 bits, so this map
    /// holds at most sixteen entries however badly a peer behaves.
    command_assemblers: HashMap<u8, (CommandType, PduAssembler)>,
    /// Part-assembled inbound responses; see [`Self::command_assemblers`].
    response_assemblers: HashMap<u8, (ResponseCode, PduAssembler)>,
    pending: HashMap<u8, Pending>,
    next_transaction: u8,
    /// Registered notification listeners: event ID -> transaction label.
    notification_listeners: HashMap<u8, u8>,
    /// The unsent tail of a fragmented response, as the target role.
    /// At most one is outstanding: AVRCP 6.8.1 gives the continuation
    /// command only a PDU ID to name it by, so a second fragmented response
    /// in flight would be unaddressable.
    continuation: Option<Continuation>,
    /// Fragmented responses this end continued automatically as the
    /// controller role, counted so a test can prove the round-trip happened
    /// rather than inferring it from a payload that arrived whole.
    continuations_requested: u32,
}

impl Protocol {
    /// Creates a connection state machine (both CT and TG roles) that
    /// fragments outgoing messages to `peer_mtu`.
    pub fn new(peer_mtu: u16) -> Self {
        let mut avctp = avctp::Protocol::new(peer_mtu);
        avctp.register_pid(AVRCP_PID);
        Self {
            supported_events: Vec::new(),
            supported_company_ids: vec![BLUETOOTH_SIG_COMPANY_ID],
            supported_player_app_settings: Vec::new(),
            player_app_settings: Vec::new(),
            volume: 0,
            playback_status: play_status::STOPPED,
            song_length: PLAYBACK_POSITION_UNAVAILABLE,
            song_position: PLAYBACK_POSITION_UNAVAILABLE,
            uid_counter: 0,
            addressed_player_id: 0,
            current_track_uid: NO_TRACK_UID,
            element_attributes: Vec::new(),
            key_event_response: ResponseCode::Accepted,
            avctp,
            peer_mtu,
            command_assemblers: HashMap::new(),
            response_assemblers: HashMap::new(),
            pending: HashMap::new(),
            next_transaction: 0,
            notification_listeners: HashMap::new(),
            continuation: None,
            continuations_requested: 0,
        }
    }

    /// Sets the current track's length in milliseconds, served in
    /// `GetPlayStatus` and in the PLAYING_TIME media attribute.
    pub fn set_song_length(&mut self, milliseconds: u32) {
        self.song_length = milliseconds;
    }

    /// The current track's length in milliseconds.
    pub fn song_length(&self) -> u32 {
        self.song_length
    }

    /// Sets how far into the track playback has got, in milliseconds.
    pub fn set_song_position(&mut self, milliseconds: u32) {
        self.song_position = milliseconds;
    }

    /// How far into the track playback has got, in milliseconds.
    pub fn song_position(&self) -> u32 {
        self.song_position
    }

    /// Sets the UID served in TrackChanged notifications;
    /// [`NO_TRACK_UID`] means nothing is selected.
    pub fn set_current_track_uid(&mut self, uid: u64) {
        self.current_track_uid = uid;
    }

    /// The UID of the track being played.
    pub fn current_track_uid(&self) -> u64 {
        self.current_track_uid
    }

    /// Adopts the MTU the L2CAP control channel negotiated; see
    /// [`avctp::Protocol::set_peer_mtu`].
    pub fn set_peer_mtu(&mut self, peer_mtu: u16) {
        self.peer_mtu = peer_mtu;
        self.avctp.set_peer_mtu(peer_mtu);
    }

    /// The largest AVRCP parameter block that fits in one AV/C frame on this
    /// connection. Anything longer is fragmented and pulled across with
    /// `RequestContinuingResponse`.
    pub fn maximum_parameter_size(&self) -> usize {
        usize::from(self.peer_mtu)
            .min(AVRCP_MAXIMUM_FRAME_SIZE)
            .saturating_sub(AVRCP_PDU_OVERHEAD)
            .max(1)
    }

    /// How many `RequestContinuingResponse` commands this end has sent to
    /// pull the tail of a fragmented response.
    ///
    /// Zero after a `GetElementAttributes` that returned real metadata means
    /// the answer arrived in one packet — which is a *fact about the peer's
    /// track*, not proof that continuation works. A test that wants to prove
    /// it must make the answer too big first.
    pub fn continuations_requested(&self) -> u32 {
        self.continuations_requested
    }

    /// Whether the target role is holding back the tail of a response,
    /// waiting for the controller to ask for it.
    pub fn has_pending_continuation(&self) -> bool {
        self.continuation.is_some()
    }

    // -- Controller (CT) command builders -----------------------------------

    fn allocate_label(&mut self, pending: Pending) -> Result<u8, SimbleError> {
        for offset in 0..16u8 {
            let label = (self.next_transaction + offset) % 16;
            if let std::collections::hash_map::Entry::Vacant(entry) = self.pending.entry(label) {
                entry.insert(pending);
                self.next_transaction = (label + 1) % 16;
                return Ok(label);
            }
        }
        Err(SimbleError::DeviceError(
            "AVRCP: all 16 transaction labels are in flight".into(),
        ))
    }

    /// Sends a PASS THROUGH key press or release to the target.
    pub fn send_key_event(
        &mut self,
        operation_id: u8,
        pressed: bool,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        let label = self.allocate_label(Pending::PassThrough {
            operation_id,
            pressed,
        })?;
        let frame = CommandFrame::pass_through(
            CommandType::Control,
            avc::subunit_type::PANEL,
            0,
            &PassThrough {
                pressed,
                operation_id,
                operation_data: Vec::new(),
            },
        );
        Ok(self.avctp.send_command(label, AVRCP_PID, &frame.to_bytes()))
    }

    /// Sends any AVRCP command PDU with the given AV/C command type.
    ///
    /// The named builders below cover the commands a controller sends in
    /// normal operation; this is the way to send one they do not, including
    /// the continuation commands of AVRCP 6.8, which [`Self::receive`] issues
    /// on its own but a test may want to send by hand.
    pub fn send_avrcp_command(
        &mut self,
        ctype: CommandType,
        command: &Command,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        let attribute_id = match command {
            Command::ListPlayerApplicationSettingValues { attribute_id } => Some(*attribute_id),
            _ => None,
        };
        let label = self.allocate_label(Pending::Vendor {
            pdu_id: command.pdu_id(),
            attribute_id,
        })?;
        let pdus = write_pdu(command.pdu_id(), &command.to_parameters(), usize::MAX);
        let frame = CommandFrame::vendor_dependent(
            ctype,
            avc::subunit_type::PANEL,
            0,
            BLUETOOTH_SIG_COMPANY_ID,
            pdus.into_iter().next().unwrap_or_default(),
        );
        Ok(self.avctp.send_command(label, AVRCP_PID, &frame.to_bytes()))
    }

    /// Asks the target which notification events it supports.
    pub fn get_supported_events(&mut self) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Status,
            &Command::GetCapabilities {
                capability_id: capability_id::EVENTS_SUPPORTED,
            },
        )
    }

    /// Asks the target which company IDs it supports.
    pub fn get_supported_company_ids(&mut self) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Status,
            &Command::GetCapabilities {
                capability_id: capability_id::COMPANY_ID,
            },
        )
    }

    /// Asks the target for its play status.
    pub fn get_play_status(&mut self) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(CommandType::Status, &Command::GetPlayStatus)
    }

    /// Asks the target for track metadata; an empty `attribute_ids` requests
    /// all attributes.
    pub fn get_element_attributes(
        &mut self,
        identifier: u64,
        attribute_ids: &[u32],
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Status,
            &Command::GetElementAttributes {
                identifier,
                attribute_ids: attribute_ids.to_vec(),
            },
        )
    }

    /// Asks the target which player application setting attributes it has.
    pub fn list_player_app_setting_attributes(&mut self) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Status,
            &Command::ListPlayerApplicationSettingAttributes,
        )
    }

    /// Asks the target which values one setting attribute supports.
    pub fn list_player_app_setting_values(
        &mut self,
        attribute_id: u8,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Status,
            &Command::ListPlayerApplicationSettingValues { attribute_id },
        )
    }

    /// Asks the target for the current values of the given setting attributes.
    pub fn get_current_player_app_settings(
        &mut self,
        attribute_ids: &[u8],
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Status,
            &Command::GetCurrentPlayerApplicationSettingValue {
                attribute_ids: attribute_ids.to_vec(),
            },
        )
    }

    /// Changes player application settings on the target.
    pub fn set_player_app_settings(
        &mut self,
        settings: &[ApplicationSetting],
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Control,
            &Command::SetPlayerApplicationSettingValue {
                settings: settings.to_vec(),
            },
        )
    }

    /// Sets the target's absolute volume (0 to [`MAXIMUM_VOLUME`]).
    pub fn set_absolute_volume(&mut self, volume: u8) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(CommandType::Control, &Command::SetAbsoluteVolume { volume })
    }

    /// Asks the target to play an item.
    pub fn play_item(
        &mut self,
        scope: u8,
        uid: u64,
        uid_counter: u16,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Control,
            &Command::PlayItem {
                scope,
                uid,
                uid_counter,
            },
        )
    }

    /// Registers for one notification event. The target answers with an
    /// INTERIM snapshot, then a single CHANGED response when the event
    /// fires; monitoring continuously means re-registering after each
    /// CHANGED (AVRCP spec 6.7.2).
    pub fn register_notification(
        &mut self,
        event_id: u8,
        playback_interval: u32,
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        self.send_avrcp_command(
            CommandType::Notify,
            &Command::RegisterNotification {
                event_id,
                playback_interval,
            },
        )
    }

    // -- Target (TG) notification emitters ----------------------------------

    /// Sends a CHANGED response for `event` to its registered listener, if
    /// any, consuming the registration.
    pub(crate) fn notify_event(&mut self, event: &Event) -> Vec<Vec<u8>> {
        let Some(label) = self.notification_listeners.remove(&event.event_id()) else {
            return Vec::new();
        };
        let parameters = Response::RegisterNotification {
            event: event.clone(),
        }
        .to_parameters();
        self.avrcp_response(
            label,
            ResponseCode::Changed,
            pdu_id::REGISTER_NOTIFICATION,
            &parameters,
        )
    }

    /// Emits a PLAYBACK_STATUS_CHANGED CHANGED notification.
    pub fn notify_playback_status_changed(&mut self, play_status: u8) -> Vec<Vec<u8>> {
        self.notify_event(&Event::PlaybackStatusChanged { play_status })
    }

    /// Emits a TRACK_CHANGED CHANGED notification.
    pub fn notify_track_changed(&mut self, uid: u64) -> Vec<Vec<u8>> {
        self.notify_event(&Event::TrackChanged { uid })
    }

    /// Emits a PLAYBACK_POS_CHANGED CHANGED notification.
    pub fn notify_playback_position_changed(&mut self, playback_position: u32) -> Vec<Vec<u8>> {
        self.notify_event(&Event::PlaybackPosChanged { playback_position })
    }

    /// Emits a PLAYER_APPLICATION_SETTING_CHANGED CHANGED notification.
    pub fn notify_player_app_settings_changed(
        &mut self,
        settings: &[ApplicationSetting],
    ) -> Vec<Vec<u8>> {
        self.notify_event(&Event::PlayerApplicationSettingChanged {
            settings: settings.to_vec(),
        })
    }

    /// Emits a NOW_PLAYING_CONTENT_CHANGED CHANGED notification.
    pub fn notify_now_playing_content_changed(&mut self) -> Vec<Vec<u8>> {
        self.notify_event(&Event::NowPlayingContentChanged)
    }

    /// Emits an AVAILABLE_PLAYERS_CHANGED CHANGED notification.
    pub fn notify_available_players_changed(&mut self) -> Vec<Vec<u8>> {
        self.notify_event(&Event::AvailablePlayersChanged)
    }

    /// Emits an ADDRESSED_PLAYER_CHANGED CHANGED notification.
    pub fn notify_addressed_player_changed(
        &mut self,
        player_id: u16,
        uid_counter: u16,
    ) -> Vec<Vec<u8>> {
        self.notify_event(&Event::AddressedPlayerChanged {
            player_id,
            uid_counter,
        })
    }

    /// Emits a UIDS_CHANGED CHANGED notification.
    pub fn notify_uids_changed(&mut self, uid_counter: u16) -> Vec<Vec<u8>> {
        self.notify_event(&Event::UidsChanged { uid_counter })
    }

    /// Emits a VOLUME_CHANGED CHANGED notification.
    pub fn notify_volume_changed(&mut self, volume: u8) -> Vec<Vec<u8>> {
        self.notify_event(&Event::VolumeChanged { volume })
    }

    // -- Receive path -------------------------------------------------------

    /// Processes one incoming AVCTP packet, returning outgoing packets plus
    /// any events. Malformed packets are dropped without a response.
    pub fn receive(&mut self, pdu: &[u8]) -> (Vec<Vec<u8>>, Vec<AvrcpEvent>) {
        let (mut outgoing, transport_events) = self.avctp.receive(pdu);
        let mut events = Vec::new();
        for transport_event in transport_events {
            match transport_event {
                AvctpEvent::Command {
                    transaction_label,
                    pid: _,
                    payload,
                } => self.on_command(transaction_label, &payload, &mut outgoing, &mut events),
                AvctpEvent::Response {
                    transaction_label,
                    pid: _,
                    payload,
                } => self.on_response(transaction_label, &payload, &mut outgoing, &mut events),
                AvctpEvent::InvalidPid {
                    transaction_label,
                    pid,
                } => {
                    if self.pending.remove(&transaction_label).is_some() {
                        events.push(AvrcpEvent::InvalidPid { pid });
                    }
                }
            }
        }
        (outgoing, events)
    }

    /// Sends one AVRCP response, fragmenting it if the parameters do not fit
    /// in an AV/C frame and holding the tail for `RequestContinuingResponse`.
    fn avrcp_response(
        &mut self,
        transaction_label: u8,
        response_code: ResponseCode,
        pdu_id: u8,
        parameters: &[u8],
    ) -> Vec<Vec<u8>> {
        let mut fragments = write_pdu(pdu_id, parameters, self.maximum_parameter_size());
        let first = if fragments.len() > 1 {
            let rest = fragments.split_off(1);
            self.continuation = Some(Continuation {
                pdu_id,
                response_code,
                remaining: rest.into(),
            });
            fragments.remove(0)
        } else {
            // A *new* answer to the same PDU supersedes any tail the peer
            // never collected, and only that: a CHANGED notification landing
            // between a START fragment and the controller's continuation
            // request must not throw the rest of the metadata away.
            if self
                .continuation
                .as_ref()
                .is_some_and(|c| c.pdu_id == pdu_id)
            {
                self.continuation = None;
            }
            fragments
                .pop()
                .unwrap_or_else(|| vec![pdu_id, PDU_SINGLE, 0, 0])
        };
        self.wrap_response_pdu(transaction_label, response_code, first)
    }

    /// Wraps one already-framed AVRCP PDU in an AV/C vendor-dependent
    /// response and hands it to AVCTP.
    fn wrap_response_pdu(
        &mut self,
        transaction_label: u8,
        response_code: ResponseCode,
        pdu: Vec<u8>,
    ) -> Vec<Vec<u8>> {
        let frame = ResponseFrame::vendor_dependent(
            response_code,
            avc::subunit_type::PANEL,
            0,
            BLUETOOTH_SIG_COMPANY_ID,
            pdu,
        );
        self.avctp
            .send_response(transaction_label, AVRCP_PID, &frame.to_bytes())
    }

    fn rejected_response(&mut self, transaction_label: u8, pdu_id: u8, status: u8) -> Vec<Vec<u8>> {
        self.avrcp_response(transaction_label, ResponseCode::Rejected, pdu_id, &[status])
    }

    fn not_implemented_response(&mut self, transaction_label: u8, pdu_id: u8) -> Vec<Vec<u8>> {
        self.avrcp_response(transaction_label, ResponseCode::NotImplemented, pdu_id, &[])
    }

    /// Echoes an unsupported AV/C command back with a NOT IMPLEMENTED
    /// response code (AV/C General Specification 4.1, section 8.4).
    fn not_implemented_echo(
        &mut self,
        transaction_label: u8,
        command: &CommandFrame,
    ) -> Vec<Vec<u8>> {
        let response = ResponseFrame {
            response: ResponseCode::NotImplemented,
            subunit_type: command.subunit_type,
            subunit_id: command.subunit_id,
            opcode: command.opcode,
            operands: command.operands.clone(),
        };
        self.avctp
            .send_response(transaction_label, AVRCP_PID, &response.to_bytes())
    }

    fn on_command(
        &mut self,
        transaction_label: u8,
        payload: &[u8],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<AvrcpEvent>,
    ) {
        let Some(Frame::Command(command)) = Frame::parse(payload) else {
            return;
        };
        // Only the unit and the PANEL subunit with ID 0 are addressable in
        // AVRCP (AVRCP spec 4.1).
        let addressed = (command.subunit_type == avc::subunit_type::UNIT
            && command.subunit_id == 7)
            || (command.subunit_type == avc::subunit_type::PANEL && command.subunit_id == 0);
        if !addressed {
            let response = self.not_implemented_echo(transaction_label, &command);
            outgoing.extend(response);
            return;
        }

        if command.opcode == avc::opcode::VENDOR_DEPENDENT {
            let Some(vendor) = command.as_vendor_dependent() else {
                return;
            };
            if vendor.company_id != BLUETOOTH_SIG_COMPANY_ID
                || command.subunit_type != avc::subunit_type::PANEL
            {
                return;
            }
            let entry = self
                .command_assemblers
                .entry(transaction_label)
                .or_insert_with(|| (command.ctype, PduAssembler::new()));
            if entry.0 != command.ctype {
                // The *same* transaction changing command type mid-message is
                // a peer that has lost track of its own conversation; drop
                // what was assembled rather than splicing two commands.
                self.command_assemblers.remove(&transaction_label);
                return;
            }
            let complete = entry.1.on_pdu(&vendor.data);
            let ctype = entry.0;
            if !entry.1.in_progress() {
                self.command_assemblers.remove(&transaction_label);
            }
            if let Some((pdu_id, parameters)) = complete {
                self.dispatch_command(
                    transaction_label,
                    ctype,
                    pdu_id,
                    &parameters,
                    outgoing,
                    events,
                );
            }
            return;
        }

        if command.opcode == avc::opcode::PASS_THROUGH {
            if let Some(pass_through) = command.as_pass_through() {
                events.push(AvrcpEvent::KeyEvent {
                    operation_id: pass_through.operation_id,
                    pressed: pass_through.pressed,
                    data: pass_through.operation_data.clone(),
                });
                let frame = ResponseFrame::pass_through(
                    self.key_event_response,
                    avc::subunit_type::PANEL,
                    0,
                    &pass_through,
                );
                outgoing.extend(self.avctp.send_response(
                    transaction_label,
                    AVRCP_PID,
                    &frame.to_bytes(),
                ));
            }
            return;
        }

        let response = self.not_implemented_echo(transaction_label, &command);
        outgoing.extend(response);
    }

    fn dispatch_command(
        &mut self,
        transaction_label: u8,
        ctype: CommandType,
        pdu_id: u8,
        parameters: &[u8],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<AvrcpEvent>,
    ) {
        if !matches!(
            ctype,
            CommandType::Control | CommandType::Status | CommandType::Notify
        ) {
            outgoing.extend(self.rejected_response(
                transaction_label,
                pdu_id,
                status_code::INVALID_COMMAND,
            ));
            return;
        }
        let Some(command) = Command::parse(pdu_id, parameters) else {
            // Unknown or malformed PDUs are dropped; the spec's recovery is
            // the controller's command timeout.
            return;
        };
        match command {
            Command::RequestContinuingResponse { continuing_pdu_id } => {
                outgoing.extend(self.continue_response(transaction_label, continuing_pdu_id));
            }
            Command::AbortContinuingResponse { continuing_pdu_id } => {
                let held = self
                    .continuation
                    .as_ref()
                    .is_some_and(|c| c.pdu_id == continuing_pdu_id);
                if held {
                    self.continuation = None;
                    outgoing.extend(self.avrcp_response(
                        transaction_label,
                        ResponseCode::Accepted,
                        pdu_id::ABORT_CONTINUING_RESPONSE,
                        &[],
                    ));
                } else {
                    // Nothing to abort. AVRCP 6.8.2 makes this an error
                    // rather than a no-op, because a controller aborting a
                    // response the target never held has lost track of the
                    // conversation and needs to know.
                    outgoing.extend(self.rejected_response(
                        transaction_label,
                        pdu_id::ABORT_CONTINUING_RESPONSE,
                        status_code::INVALID_PARAMETER,
                    ));
                }
            }
            Command::GetCapabilities { capability_id } => {
                let response = match capability_id {
                    capability_id::EVENTS_SUPPORTED => Some(Response::GetCapabilities {
                        capabilities: CapabilityList::EventIds(self.supported_events.clone()),
                    }),
                    capability_id::COMPANY_ID => Some(Response::GetCapabilities {
                        capabilities: CapabilityList::CompanyIds(
                            self.supported_company_ids.clone(),
                        ),
                    }),
                    _ => None,
                };
                match response {
                    Some(response) => {
                        outgoing.extend(self.stable_response(transaction_label, &response))
                    }
                    None => outgoing.extend(self.rejected_response(
                        transaction_label,
                        pdu_id,
                        status_code::INVALID_PARAMETER,
                    )),
                }
            }
            Command::SetAbsoluteVolume { volume } => {
                self.volume = volume;
                let response = Response::SetAbsoluteVolume {
                    volume: self.volume,
                };
                outgoing.extend(self.avrcp_response(
                    transaction_label,
                    ResponseCode::Accepted,
                    pdu_id,
                    &response.to_parameters(),
                ));
                events.push(AvrcpEvent::VolumeSet { volume });
            }
            Command::RegisterNotification { event_id, .. } => {
                self.on_register_notification(transaction_label, event_id, outgoing, events);
            }
            Command::GetPlayStatus => {
                let response = Response::GetPlayStatus {
                    song_length: self.song_length,
                    song_position: self.song_position,
                    play_status: self.playback_status,
                };
                outgoing.extend(self.stable_response(transaction_label, &response));
            }
            Command::GetElementAttributes { attribute_ids, .. } => {
                let attributes = if attribute_ids.is_empty() {
                    self.element_attributes.clone()
                } else {
                    self.element_attributes
                        .iter()
                        .filter(|a| attribute_ids.contains(&a.attribute_id))
                        .cloned()
                        .collect()
                };
                let response = Response::GetElementAttributes { attributes };
                outgoing.extend(self.stable_response(transaction_label, &response));
            }
            Command::ListPlayerApplicationSettingAttributes => {
                let response = Response::ListPlayerApplicationSettingAttributes {
                    attribute_ids: self
                        .supported_player_app_settings
                        .iter()
                        .map(|(id, _)| *id)
                        .collect(),
                };
                outgoing.extend(self.stable_response(transaction_label, &response));
            }
            Command::ListPlayerApplicationSettingValues { attribute_id } => {
                let response = Response::ListPlayerApplicationSettingValues {
                    value_ids: self
                        .supported_player_app_settings
                        .iter()
                        .find(|(id, _)| *id == attribute_id)
                        .map(|(_, values)| values.clone())
                        .unwrap_or_default(),
                };
                outgoing.extend(self.stable_response(transaction_label, &response));
            }
            Command::GetCurrentPlayerApplicationSettingValue { attribute_ids } => {
                let settings: Option<Vec<ApplicationSetting>> = attribute_ids
                    .iter()
                    .map(|id| {
                        self.player_app_settings
                            .iter()
                            .find(|s| s.attribute_id == *id)
                            .copied()
                    })
                    .collect();
                match settings {
                    Some(settings) => {
                        let response =
                            Response::GetCurrentPlayerApplicationSettingValue { settings };
                        outgoing.extend(self.stable_response(transaction_label, &response));
                    }
                    None => {
                        outgoing.extend(self.not_implemented_response(transaction_label, pdu_id))
                    }
                }
            }
            Command::SetPlayerApplicationSettingValue { settings } => {
                for setting in &settings {
                    match self
                        .player_app_settings
                        .iter_mut()
                        .find(|s| s.attribute_id == setting.attribute_id)
                    {
                        Some(existing) => existing.value_id = setting.value_id,
                        None => self.player_app_settings.push(*setting),
                    }
                }
                outgoing.extend(self.stable_response(
                    transaction_label,
                    &Response::SetPlayerApplicationSettingValue,
                ));
                events.push(AvrcpEvent::PlayerAppSettingsSet { settings });
            }
            Command::PlayItem {
                scope,
                uid,
                uid_counter,
            } => {
                outgoing.extend(self.stable_response(
                    transaction_label,
                    &Response::PlayItem {
                        status: status_code::OPERATION_COMPLETED,
                    },
                ));
                events.push(AvrcpEvent::PlayItemRequested {
                    scope,
                    uid,
                    uid_counter,
                });
            }
            _ => {
                // Parsed but unsupported by this target.
                outgoing.extend(self.rejected_response(
                    transaction_label,
                    pdu_id,
                    status_code::INVALID_PARAMETER,
                ));
            }
        }
    }

    /// Answers a `RequestContinuingResponse` with the next held fragment.
    ///
    /// The reply carries the **original** PDU ID and the original response
    /// code, not this command's 0x40 and not a fresh code: a controller
    /// reassembles by PDU ID, and a fragment labelled 0x40 would be a
    /// different PDU as far as its assembler is concerned.
    fn continue_response(&mut self, transaction_label: u8, continuing_pdu_id: u8) -> Vec<Vec<u8>> {
        let Some(continuation) = self.continuation.as_mut() else {
            return self.rejected_response(
                transaction_label,
                pdu_id::REQUEST_CONTINUING_RESPONSE,
                status_code::INVALID_PARAMETER,
            );
        };
        if continuation.pdu_id != continuing_pdu_id {
            return self.rejected_response(
                transaction_label,
                pdu_id::REQUEST_CONTINUING_RESPONSE,
                status_code::INVALID_PARAMETER,
            );
        }
        let Some(fragment) = continuation.remaining.pop_front() else {
            self.continuation = None;
            return self.rejected_response(
                transaction_label,
                pdu_id::REQUEST_CONTINUING_RESPONSE,
                status_code::INVALID_PARAMETER,
            );
        };
        let response_code = continuation.response_code;
        if continuation.remaining.is_empty() {
            self.continuation = None;
        }
        self.wrap_response_pdu(transaction_label, response_code, fragment)
    }

    fn stable_response(&mut self, transaction_label: u8, response: &Response) -> Vec<Vec<u8>> {
        self.avrcp_response(
            transaction_label,
            ResponseCode::ImplementedOrStable,
            response.pdu_id(),
            &response.to_parameters(),
        )
    }

    fn on_register_notification(
        &mut self,
        transaction_label: u8,
        event_id: u8,
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<AvrcpEvent>,
    ) {
        if !self.supported_events.contains(&event_id) {
            outgoing.extend(
                self.not_implemented_response(transaction_label, pdu_id::REGISTER_NOTIFICATION),
            );
            return;
        }
        // The INTERIM response snapshots the current state (AVRCP spec 6.7.2).
        let event = match event_id {
            event_id::VOLUME_CHANGED => Some(Event::VolumeChanged {
                volume: self.volume,
            }),
            event_id::PLAYBACK_STATUS_CHANGED => Some(Event::PlaybackStatusChanged {
                play_status: self.playback_status,
            }),
            event_id::NOW_PLAYING_CONTENT_CHANGED => Some(Event::NowPlayingContentChanged),
            event_id::PLAYER_APPLICATION_SETTING_CHANGED => {
                Some(Event::PlayerApplicationSettingChanged {
                    settings: self.player_app_settings.clone(),
                })
            }
            event_id::AVAILABLE_PLAYERS_CHANGED => Some(Event::AvailablePlayersChanged),
            event_id::ADDRESSED_PLAYER_CHANGED => Some(Event::AddressedPlayerChanged {
                player_id: self.addressed_player_id,
                uid_counter: self.uid_counter,
            }),
            event_id::UIDS_CHANGED => Some(Event::UidsChanged {
                uid_counter: self.uid_counter,
            }),
            event_id::TRACK_CHANGED => Some(Event::TrackChanged {
                uid: self.current_track_uid,
            }),
            _ => None,
        };
        let Some(event) = event else {
            // Declared supported but not modeled; no response is sent, the
            // controller's command timeout recovers.
            return;
        };
        let parameters = Response::RegisterNotification { event }.to_parameters();
        outgoing.extend(self.avrcp_response(
            transaction_label,
            ResponseCode::Interim,
            pdu_id::REGISTER_NOTIFICATION,
            &parameters,
        ));
        self.notification_listeners
            .insert(event_id, transaction_label);
        events.push(AvrcpEvent::NotificationRegistered { event_id });
    }

    fn on_response(
        &mut self,
        transaction_label: u8,
        payload: &[u8],
        outgoing: &mut Vec<Vec<u8>>,
        events: &mut Vec<AvrcpEvent>,
    ) {
        let Some(Frame::Response(response)) = Frame::parse(payload) else {
            return;
        };
        if !self.pending.contains_key(&transaction_label) {
            return;
        }

        if response.opcode == avc::opcode::PASS_THROUGH {
            if let Some(Pending::PassThrough {
                operation_id,
                pressed,
            }) = self.pending.remove(&transaction_label)
            {
                events.push(AvrcpEvent::PassThroughResponse {
                    response: response.response,
                    operation_id,
                    pressed,
                });
            }
            return;
        }

        if response.opcode == avc::opcode::VENDOR_DEPENDENT {
            let Some(vendor) = response.as_vendor_dependent() else {
                return;
            };
            if vendor.company_id != BLUETOOTH_SIG_COMPANY_ID
                || response.subunit_type != avc::subunit_type::PANEL
                || response.subunit_id != 0
            {
                return;
            }
            let entry = self
                .response_assemblers
                .entry(transaction_label)
                .or_insert_with(|| (response.response, PduAssembler::new()));
            if entry.0 != response.response {
                self.response_assemblers.remove(&transaction_label);
                return;
            }
            // Read the fragment header *before* feeding the assembler: it is
            // the only thing that says whether more is coming, and the
            // assembler consumes it.
            let fragment = vendor
                .data
                .first()
                .copied()
                .zip(vendor.data.get(1).map(|byte| byte & 3));
            let complete = entry.1.on_pdu(&vendor.data);
            let code = entry.0;
            let still_assembling = entry.1.in_progress();
            if !still_assembling {
                self.response_assemblers.remove(&transaction_label);
            }
            if let Some((pdu_id, parameters)) = complete {
                self.dispatch_response(transaction_label, code, pdu_id, &parameters, events);
                return;
            }
            // The assembler kept the fragment and wants more. AVRCP 6.8.1
            // makes that the *controller's* job to ask for: a target that
            // pushed the rest unprompted would be the one violating the
            // spec, so waiting here means waiting forever. This is the whole
            // of the send-path gap `docs/gaps.md` §2 recorded — and it fails
            // silently, as a metadata query that never answers.
            if let Some((fragment_pdu_id, packet_type)) = fragment
                && matches!(packet_type, PDU_START | PDU_CONTINUE)
                && still_assembling
                && self.pending.contains_key(&transaction_label)
            {
                self.continuations_requested += 1;
                let request = Command::RequestContinuingResponse {
                    continuing_pdu_id: fragment_pdu_id,
                };
                outgoing.extend(self.send_command_on_label(
                    transaction_label,
                    CommandType::Control,
                    &request,
                ));
            }
        }
    }

    /// Builds a command on an *existing* transaction label rather than
    /// allocating a new one.
    ///
    /// `RequestContinuingResponse` is the only caller and needs it: the
    /// fragments it pulls are part of the transaction already in flight, and
    /// this end's own response assembler drops any fragment whose label does
    /// not match the one it started with.
    fn send_command_on_label(
        &mut self,
        transaction_label: u8,
        ctype: CommandType,
        command: &Command,
    ) -> Vec<Vec<u8>> {
        let pdus = write_pdu(command.pdu_id(), &command.to_parameters(), usize::MAX);
        let frame = CommandFrame::vendor_dependent(
            ctype,
            avc::subunit_type::PANEL,
            0,
            BLUETOOTH_SIG_COMPANY_ID,
            pdus.into_iter().next().unwrap_or_default(),
        );
        self.avctp
            .send_command(transaction_label, AVRCP_PID, &frame.to_bytes())
    }

    fn dispatch_response(
        &mut self,
        transaction_label: u8,
        response_code: ResponseCode,
        pdu_id: u8,
        parameters: &[u8],
        events: &mut Vec<AvrcpEvent>,
    ) {
        let Some(Pending::Vendor {
            pdu_id: expected_pdu_id,
            attribute_id,
        }) = self.pending.get(&transaction_label).cloned()
        else {
            self.pending.remove(&transaction_label);
            return;
        };
        if pdu_id != expected_pdu_id {
            self.pending.remove(&transaction_label);
            return;
        }
        match response_code {
            ResponseCode::Rejected => {
                self.pending.remove(&transaction_label);
                events.push(AvrcpEvent::CommandRejected {
                    pdu_id,
                    status_code: parameters.first().copied().unwrap_or(0),
                });
            }
            ResponseCode::NotImplemented => {
                self.pending.remove(&transaction_label);
                events.push(AvrcpEvent::CommandNotImplemented { pdu_id });
            }
            ResponseCode::Interim => {
                // The transaction label stays in flight until the CHANGED
                // response consumes the registration (AVRCP spec 6.7.2).
                if pdu_id == pdu_id::REGISTER_NOTIFICATION
                    && let Some(Response::RegisterNotification { event }) =
                        Response::parse(pdu_id, parameters)
                {
                    events.push(AvrcpEvent::NotificationReceived {
                        event,
                        interim: true,
                    });
                }
            }
            ResponseCode::Changed => {
                self.pending.remove(&transaction_label);
                if pdu_id == pdu_id::REGISTER_NOTIFICATION
                    && let Some(Response::RegisterNotification { event }) =
                        Response::parse(pdu_id, parameters)
                {
                    events.push(AvrcpEvent::NotificationReceived {
                        event,
                        interim: false,
                    });
                }
            }
            ResponseCode::ImplementedOrStable | ResponseCode::Accepted => {
                self.pending.remove(&transaction_label);
                let Some(response) = Response::parse(pdu_id, parameters) else {
                    return;
                };
                events.push(match response {
                    Response::GetCapabilities {
                        capabilities: CapabilityList::EventIds(ids),
                    } => AvrcpEvent::SupportedEventsReceived(ids),
                    Response::GetCapabilities {
                        capabilities: CapabilityList::CompanyIds(ids),
                    } => AvrcpEvent::SupportedCompanyIdsReceived(ids),
                    Response::GetPlayStatus {
                        song_length,
                        song_position,
                        play_status,
                    } => AvrcpEvent::PlayStatusReceived {
                        song_length,
                        song_position,
                        play_status,
                    },
                    Response::GetElementAttributes { attributes } => {
                        AvrcpEvent::ElementAttributesReceived(attributes)
                    }
                    Response::ListPlayerApplicationSettingAttributes { attribute_ids } => {
                        AvrcpEvent::AppSettingAttributesReceived(attribute_ids)
                    }
                    Response::ListPlayerApplicationSettingValues { value_ids } => {
                        AvrcpEvent::AppSettingValuesReceived {
                            attribute_id: attribute_id.unwrap_or(0),
                            value_ids,
                        }
                    }
                    Response::GetCurrentPlayerApplicationSettingValue { settings } => {
                        AvrcpEvent::CurrentAppSettingsReceived(settings)
                    }
                    Response::SetPlayerApplicationSettingValue => AvrcpEvent::AppSettingsAccepted,
                    Response::SetAbsoluteVolume { volume } => AvrcpEvent::VolumeAccepted { volume },
                    Response::PlayItem { status } => AvrcpEvent::PlayItemCompleted { status },
                    other => AvrcpEvent::ResponseReceived { response: other },
                });
            }
            _ => {
                self.pending.remove(&transaction_label);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// SDP integration
// ---------------------------------------------------------------------------

fn version_to_u16(version: (u8, u8)) -> u16 {
    (u16::from(version.0) << 8) | u16::from(version.1)
}

fn version_from_u16(value: u16) -> (u8, u8) {
    ((value >> 8) as u8, value as u8)
}

/// The AVRCP Controller (CT) SDP service record (AVRCP spec 8, Table 8.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerServiceRecord {
    /// SDP service record handle.
    pub service_record_handle: u32,
    /// Advertised AVCTP version as `(major, minor)`.
    pub avctp_version: (u8, u8),
    /// Advertised AVRCP version as `(major, minor)`.
    pub avrcp_version: (u8, u8),
    /// SupportedFeatures bitmask (see [`controller_features`]).
    pub supported_features: u16,
}

impl ControllerServiceRecord {
    /// Builds a controller record with the default AVCTP/AVRCP versions.
    pub fn new(service_record_handle: u32, supported_features: u16) -> Self {
        Self {
            service_record_handle,
            avctp_version: DEFAULT_AVCTP_VERSION,
            avrcp_version: DEFAULT_AVRCP_VERSION,
            supported_features,
        }
    }

    /// Renders this record as an SDP service attribute list.
    pub fn to_service_attributes(&self) -> Vec<ServiceAttribute> {
        make_service_attributes(
            self.service_record_handle,
            &[
                AV_REMOTE_CONTROL_SERVICE_UUID,
                AV_REMOTE_CONTROL_CONTROLLER_SERVICE_UUID,
            ],
            self.avctp_version,
            self.avrcp_version,
            self.supported_features,
            self.supported_features & controller_features::SUPPORTS_BROWSING != 0,
        )
    }
}

/// The AVRCP Target (TG) SDP service record (AVRCP spec 8, Table 8.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetServiceRecord {
    /// SDP service record handle.
    pub service_record_handle: u32,
    /// Advertised AVCTP version as `(major, minor)`.
    pub avctp_version: (u8, u8),
    /// Advertised AVRCP version as `(major, minor)`.
    pub avrcp_version: (u8, u8),
    /// SupportedFeatures bitmask (see [`target_features`]).
    pub supported_features: u16,
}

impl TargetServiceRecord {
    /// Builds a target record with the default AVCTP/AVRCP versions.
    pub fn new(service_record_handle: u32, supported_features: u16) -> Self {
        Self {
            service_record_handle,
            avctp_version: DEFAULT_AVCTP_VERSION,
            avrcp_version: DEFAULT_AVRCP_VERSION,
            supported_features,
        }
    }

    /// Renders this record as an SDP service attribute list.
    pub fn to_service_attributes(&self) -> Vec<ServiceAttribute> {
        make_service_attributes(
            self.service_record_handle,
            &[AV_REMOTE_CONTROL_TARGET_SERVICE_UUID],
            self.avctp_version,
            self.avrcp_version,
            self.supported_features,
            self.supported_features & target_features::SUPPORTS_BROWSING != 0,
        )
    }
}

fn make_service_attributes(
    service_record_handle: u32,
    service_class_uuids: &[SdpUuid],
    avctp_version: (u8, u8),
    avrcp_version: (u8, u8),
    supported_features: u16,
    browsing: bool,
) -> Vec<ServiceAttribute> {
    let mut attributes = vec![
        ServiceAttribute::new(
            attribute_id::SERVICE_RECORD_HANDLE,
            DataElement::unsigned_integer_32(service_record_handle),
        ),
        ServiceAttribute::new(
            attribute_id::BROWSE_GROUP_LIST,
            DataElement::sequence(vec![DataElement::uuid(SdpUuid::SDP_PUBLIC_BROWSE_ROOT)]),
        ),
        ServiceAttribute::new(
            attribute_id::SERVICE_CLASS_ID_LIST,
            DataElement::sequence(
                service_class_uuids
                    .iter()
                    .map(|uuid| DataElement::uuid(*uuid))
                    .collect::<Vec<_>>(),
            ),
        ),
        ServiceAttribute::new(
            attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID),
                    DataElement::unsigned_integer_16(avctp::AVCTP_PSM),
                ]),
                DataElement::sequence(vec![
                    DataElement::uuid(AVCTP_PROTOCOL_UUID),
                    DataElement::unsigned_integer_16(version_to_u16(avctp_version)),
                ]),
            ]),
        ),
        ServiceAttribute::new(
            attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
            DataElement::sequence(vec![DataElement::sequence(vec![
                DataElement::uuid(AV_REMOTE_CONTROL_SERVICE_UUID),
                DataElement::unsigned_integer_16(version_to_u16(avrcp_version)),
            ])]),
        ),
        ServiceAttribute::new(
            SUPPORTED_FEATURES_ATTRIBUTE_ID,
            DataElement::unsigned_integer_16(supported_features),
        ),
    ];
    if browsing {
        attributes.push(ServiceAttribute::new(
            attribute_id::ADDITIONAL_PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID),
                    DataElement::unsigned_integer_16(avctp::AVCTP_BROWSING_PSM),
                ]),
                DataElement::sequence(vec![
                    DataElement::uuid(AVCTP_PROTOCOL_UUID),
                    DataElement::unsigned_integer_16(version_to_u16(avctp_version)),
                ]),
            ]),
        ));
    }
    attributes
}

/// Extracts `(handle, avctp version, avrcp version, features)` from one
/// service's attribute list, with defaults for anything absent.
fn parse_service_record(attributes: &[ServiceAttribute]) -> (u32, (u8, u8), (u8, u8), u16) {
    let mut handle = 0u32;
    let mut avctp_version = DEFAULT_AVCTP_VERSION;
    let mut avrcp_version = DEFAULT_AVRCP_VERSION;
    let mut features = 0u16;
    for attribute in attributes {
        match attribute.id {
            attribute_id::SERVICE_RECORD_HANDLE => {
                if let Some((value, _)) = attribute.value.as_unsigned_integer() {
                    handle = value as u32;
                }
            }
            attribute_id::PROTOCOL_DESCRIPTOR_LIST => {
                // [[L2CAP, PSM], [AVCTP, version]]
                if let Some(descriptors) = attribute.value.as_sequence()
                    && let Some(avctp_descriptor) =
                        descriptors.get(1).and_then(DataElement::as_sequence)
                    && let Some((value, _)) = avctp_descriptor
                        .get(1)
                        .and_then(DataElement::as_unsigned_integer)
                {
                    avctp_version = version_from_u16(value as u16);
                }
            }
            attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST => {
                // [[AV_REMOTE_CONTROL, version]]
                if let Some(descriptors) = attribute.value.as_sequence()
                    && let Some(profile_descriptor) =
                        descriptors.first().and_then(DataElement::as_sequence)
                    && let Some((value, _)) = profile_descriptor
                        .get(1)
                        .and_then(DataElement::as_unsigned_integer)
                {
                    avrcp_version = version_from_u16(value as u16);
                }
            }
            SUPPORTED_FEATURES_ATTRIBUTE_ID => {
                if let Some((value, _)) = attribute.value.as_unsigned_integer() {
                    features = value as u16;
                }
            }
            _ => {}
        }
    }
    (handle, avctp_version, avrcp_version, features)
}

fn find_service_records(
    sdp_client: &mut SdpClient,
    service_class: SdpUuid,
    transport: &mut impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<Vec<(u32, (u8, u8), (u8, u8), u16)>, SimbleError> {
    let results = sdp_client.search_attributes(
        &[service_class],
        &[
            AttributeIdSpec::Single(attribute_id::SERVICE_RECORD_HANDLE),
            AttributeIdSpec::Single(attribute_id::PROTOCOL_DESCRIPTOR_LIST),
            AttributeIdSpec::Single(attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST),
            AttributeIdSpec::Single(SUPPORTED_FEATURES_ATTRIBUTE_ID),
        ],
        transport,
    )?;
    Ok(results
        .iter()
        .map(|attributes| parse_service_record(attributes))
        .collect())
}

/// Searches for AVRCP Controller service records via `sdp_client`/`transport`.
pub fn find_controller_services(
    sdp_client: &mut SdpClient,
    mut transport: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<Vec<ControllerServiceRecord>, SimbleError> {
    Ok(find_service_records(
        sdp_client,
        AV_REMOTE_CONTROL_CONTROLLER_SERVICE_UUID,
        &mut transport,
    )?
    .into_iter()
    .map(
        |(service_record_handle, avctp_version, avrcp_version, supported_features)| {
            ControllerServiceRecord {
                service_record_handle,
                avctp_version,
                avrcp_version,
                supported_features,
            }
        },
    )
    .collect())
}

/// Searches for AVRCP Target service records via `sdp_client`/`transport`.
pub fn find_target_services(
    sdp_client: &mut SdpClient,
    mut transport: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<Vec<TargetServiceRecord>, SimbleError> {
    Ok(find_service_records(
        sdp_client,
        AV_REMOTE_CONTROL_TARGET_SERVICE_UUID,
        &mut transport,
    )?
    .into_iter()
    .map(
        |(service_record_handle, avctp_version, avrcp_version, supported_features)| {
            TargetServiceRecord {
                service_record_handle,
                avctp_version,
                avrcp_version,
                supported_features,
            }
        },
    )
    .collect())
}
