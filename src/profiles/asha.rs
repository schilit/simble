// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio Streaming for Hearing Aids (ASHA, service UUID 0xFDF0) — Google's LE
//! hearing-aid audio protocol, predating LE Audio.
//!
//! A hearing aid exposes ReadOnlyProperties (device capabilities, HiSyncId pairing the
//! two aids of a set, feature map, render delay, supported codecs), an AudioControlPoint
//! (Start/Stop/Status), an AudioStatus result characteristic, a write-only Volume
//! characteristic, and LE_PSM_OUT naming the PSM of the L2CAP CoC channel that carries
//! the actual audio frames.
//!
//! The control point and Volume are wired through `AttributeHandler`, so ordinary
//! `GattDatabase::write` calls drive them. Per the ASHA protocol the control point
//! write itself succeeds and the outcome lands in the AudioStatus characteristic, which
//! the peripheral notifies after Start/Stop; Simble republishes the AudioStatus
//! attribute value instead of sending a notification. Audio-data transport is out of
//! scope, matching every other audio profile's boundary in this codebase: the service
//! advertises the CoC PSM (Simble models CoC channels in `crate::l2cap::coc`), but no
//! G.722 frames flow.

use crate::att::error_code as att_error_code;
use crate::gap::AdvertisingData;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use std::sync::{Arc, Mutex};

/// ASHA service UUID (16-bit, Google-registered) and characteristic UUIDs (128-bit
/// vendor UUIDs from the ASHA specification), stored little-endian per
/// [`crate::types::Uuid::Uuid128`]'s convention.
pub mod asha_uuid {
    use crate::types::Uuid;

    /// Asha Service UUID.
    pub const ASHA_SERVICE: Uuid = Uuid::Uuid16(0xFDF0);
    /// 6333651e-c481-4a3e-9169-7c902aad37bb
    pub const READ_ONLY_PROPERTIES: Uuid = Uuid::Uuid128([
        0xbb, 0x37, 0xad, 0x2a, 0x90, 0x7c, 0x69, 0x91, 0x3e, 0x4a, 0x81, 0xc4, 0x1e, 0x65, 0x33,
        0x63,
    ]);
    /// f0d4de7e-4a88-476c-9d9f-1937b0996cc0
    pub const AUDIO_CONTROL_POINT: Uuid = Uuid::Uuid128([
        0xc0, 0x6c, 0x99, 0xb0, 0x37, 0x19, 0x9f, 0x9d, 0x6c, 0x47, 0x88, 0x4a, 0x7e, 0xde, 0xd4,
        0xf0,
    ]);
    /// 38663f1a-e711-4cac-b641-326b56404837
    pub const AUDIO_STATUS: Uuid = Uuid::Uuid128([
        0x37, 0x48, 0x40, 0x56, 0x6b, 0x32, 0x41, 0xb6, 0xac, 0x4c, 0x11, 0xe7, 0x1a, 0x3f, 0x66,
        0x38,
    ]);
    /// 00e4ca9e-ab14-41e4-8823-f9e70c7e91df
    pub const VOLUME: Uuid = Uuid::Uuid128([
        0xdf, 0x91, 0x7e, 0x0c, 0xe7, 0xf9, 0x23, 0x88, 0xe4, 0x41, 0x14, 0xab, 0x9e, 0xca, 0xe4,
        0x00,
    ]);
    /// 2d410339-82b6-42aa-b34e-e2e01df8cc1a
    pub const LE_PSM_OUT: Uuid = Uuid::Uuid128([
        0x1a, 0xcc, 0xf8, 0x1d, 0xe0, 0xe2, 0x4e, 0xb3, 0xaa, 0x42, 0xb6, 0x82, 0x39, 0x03, 0x41,
        0x2d,
    ]);
}

/// DeviceCapabilities bitmask in ReadOnlyProperties and the service-data advertisement.
pub mod device_capabilities {
    /// Is Right.
    pub const IS_RIGHT: u8 = 0x01;
    /// Is Dual.
    pub const IS_DUAL: u8 = 0x02;
    /// Csis Supported.
    pub const CSIS_SUPPORTED: u8 = 0x04;
}

/// FeatureMap bitmask in ReadOnlyProperties.
pub mod feature_map {
    /// Le Coc Audio Output Streaming Supported.
    pub const LE_COC_AUDIO_OUTPUT_STREAMING_SUPPORTED: u8 = 0x01;
}

/// AudioControlPoint opcodes.
pub mod opcode {
    /// Start control-point opcode.
    pub const START: u8 = 1;
    /// Stop control-point opcode.
    pub const STOP: u8 = 2;
    /// Status control-point opcode.
    pub const STATUS: u8 = 3;
}

/// Codec IDs carried in the Start command's codec field.
pub mod codec {
    /// G722 16khz.
    pub const G722_16KHZ: u8 = 1;
}

/// SupportedCodecs bitmask in ReadOnlyProperties: bit N set means codec ID N is
/// supported, so G.722 at 16 kHz (codec ID 1) is bit 1.
pub mod supported_codecs {
    /// G722 16khz.
    pub const G722_16KHZ: u16 = 1 << 1;
}

/// AudioType field of the Start command.
pub mod audio_type {
    /// Unknown.
    pub const UNKNOWN: u8 = 0x00;
    /// Ringtone.
    pub const RINGTONE: u8 = 0x01;
    /// Phone Call.
    pub const PHONE_CALL: u8 = 0x02;
    /// Media.
    pub const MEDIA: u8 = 0x03;
}

/// Status field of the Status command: what happened to the other peripheral of the
/// binaural set.
pub mod peripheral_status {
    /// Other Peripheral Disconnected.
    pub const OTHER_PERIPHERAL_DISCONNECTED: u8 = 1;
    /// Other Peripheral Connected.
    pub const OTHER_PERIPHERAL_CONNECTED: u8 = 2;
    /// Connection Parameter Updated.
    pub const CONNECTION_PARAMETER_UPDATED: u8 = 3;
}

/// AudioStatus characteristic values. The protocol defines the error values as signed
/// (-1/-2); on the wire they are the two's-complement bytes below.
pub mod audio_status {
    /// Ok.
    pub const OK: u8 = 0x00;
    /// Unknown Command.
    pub const UNKNOWN_COMMAND: u8 = 0xFF;
    /// Illegal Parameters.
    pub const ILLEGAL_PARAMETERS: u8 = 0xFE;
}

/// ReadOnlyProperties characteristic value: a fixed 17-byte layout of
/// `[Version, DeviceCapabilities, HiSyncId(8), FeatureMap, RenderDelay(2 LE),
/// Reserved(2), SupportedCodecs(2 LE)]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadOnlyProperties {
    /// Protocol Version.
    pub protocol_version: u8,
    /// [`device_capabilities`] bitmask.
    pub capabilities: u8,
    /// Identifies the binaural set: both hearing aids of one pair share this ID.
    pub hi_sync_id: [u8; 8],
    /// [`feature_map`] bitmask.
    pub feature_map: u8,
    /// Time in milliseconds from receiving an audio frame to rendering it, so the
    /// central can delay video to lip-sync.
    pub render_delay_milliseconds: u16,
    /// [`supported_codecs`] bitmask.
    pub supported_codecs: u16,
}

impl ReadOnlyProperties {
    /// Serializes to the characteristic wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(17);
        buf.push(self.protocol_version);
        buf.push(self.capabilities);
        buf.extend_from_slice(&self.hi_sync_id);
        buf.push(self.feature_map);
        buf.extend_from_slice(&self.render_delay_milliseconds.to_le_bytes());
        // Reserved for future use; transmitted as zero.
        buf.extend_from_slice(&[0, 0]);
        buf.extend_from_slice(&self.supported_codecs.to_le_bytes());
        buf
    }

    /// Parses a value from its wire bytes.
    pub fn parse(data: &[u8]) -> Option<Self> {
        Some(Self {
            protocol_version: *data.first()?,
            capabilities: *data.get(1)?,
            hi_sync_id: data.get(2..10)?.try_into().ok()?,
            feature_map: *data.get(10)?,
            render_delay_milliseconds: u16::from_le_bytes(data.get(11..13)?.try_into().ok()?),
            // Bytes 13..15 are reserved and ignored on parse.
            supported_codecs: u16::from_le_bytes(data.get(15..17)?.try_into().ok()?),
        })
    }
}

/// Mutable streaming state shared between the service handle and the AudioControlPoint /
/// Volume handlers (see [`AshaService`]). The four `Option`s are populated by a Start
/// command and cleared by Stop — `None` means not streaming.
#[derive(Debug, Default)]
struct AshaState {
    active_codec: Option<u8>,
    audio_type: Option<u8>,
    /// Volume is a signed byte (-128..=0, full mute to unattenuated) per the protocol;
    /// updated by both the Start command and Volume characteristic writes.
    volume: Option<i8>,
    /// Start's "other state" flag: whether the other side of the binaural set is
    /// already connected, so this peripheral can synchronize its audio start.
    other_state: Option<u8>,
    /// Most recent Status command payload ([`peripheral_status`]).
    last_peripheral_status: Option<u8>,
    audio_status_value_handle: u16,
}

/// `AttributeHandler` for the AudioControlPoint.
#[derive(Debug)]
struct AudioControlPointHandler {
    state: Arc<Mutex<AshaState>>,
}

impl AttributeHandler for AudioControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().expect("ASHA state lock poisoned");
        let (&op, params) = value
            .split_first()
            .ok_or(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)?;
        let status = match op {
            opcode::START => {
                // Start carries [Codec, AudioType, Volume, OtherState].
                if let [codec, audiotype, volume, other_state] = *params {
                    state.active_codec = Some(codec);
                    state.audio_type = Some(audiotype);
                    state.volume = Some(volume as i8);
                    state.other_state = Some(other_state);
                    audio_status::OK
                } else {
                    audio_status::ILLEGAL_PARAMETERS
                }
            }
            opcode::STOP => {
                state.active_codec = None;
                state.audio_type = None;
                state.volume = None;
                state.other_state = None;
                audio_status::OK
            }
            opcode::STATUS => {
                // Status only informs about the other peripheral; it produces no
                // AudioStatus update.
                state.last_peripheral_status = Some(
                    *params
                        .first()
                        .ok_or(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)?,
                );
                return Ok(());
            }
            _ => audio_status::UNKNOWN_COMMAND,
        };
        // The outcome is reported through the AudioStatus characteristic, not as an ATT
        // error — the control point also allows Write Without Response, which has no
        // error channel at all.
        let _ = db.set_value(state.audio_status_value_handle, &[status]);
        Ok(())
    }
}

/// `AttributeHandler` for the Volume characteristic (write-without-response only, so
/// the shared state — not the unreadable attribute value — is where the volume lives).
#[derive(Debug)]
struct VolumeHandler {
    state: Arc<Mutex<AshaState>>,
}

impl AttributeHandler for VolumeHandler {
    fn on_write(&mut self, _db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let &volume = value
            .first()
            .ok_or(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)?;
        self.state.lock().expect("ASHA state lock poisoned").volume = Some(volume as i8);
        Ok(())
    }
}

/// ASHA service GATT container plus the streaming state it owns.
#[derive(Debug)]
pub struct AshaService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Value attribute handle of the Read Only Properties characteristic.
    pub read_only_properties_value_handle: u16,
    /// Value attribute handle of the Audio Control Point characteristic.
    pub audio_control_point_value_handle: u16,
    /// Value attribute handle of the Audio Status characteristic.
    pub audio_status_value_handle: u16,
    /// Value attribute handle of the Volume characteristic.
    pub volume_value_handle: u16,
    /// Value attribute handle of the Le Psm Out characteristic.
    pub le_psm_out_value_handle: u16,
    /// The CoC PSM advertised in LE_PSM_OUT (see module docs for the transport
    /// boundary).
    pub psm: u16,
    /// Properties.
    pub properties: ReadOnlyProperties,
    state: Arc<Mutex<AshaState>>,
}

impl AshaService {
    /// Registers the ASHA service. `psm` is the L2CAP CoC PSM the central would connect
    /// to for audio; Simble has no L2CAP server registry, so the caller picks it
    /// (LE dynamic PSMs are 0x0080..=0x00FF).
    pub fn register(db: &mut GattDatabase, properties: ReadOnlyProperties, psm: u16) -> Self {
        let service_handle = db.add_service(asha_uuid::ASHA_SERVICE, true);

        let (_, read_only_properties_value_handle) = db.add_characteristic(
            asha_uuid::READ_ONLY_PROPERTIES,
            CharacteristicProperties(CharacteristicProperties::READ),
            properties.to_bytes(),
            AttributePermissions::read_only(),
        );

        let (_, audio_control_point_value_handle) = db.add_characteristic(
            asha_uuid::AUDIO_CONTROL_POINT,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
            ),
            vec![],
            AttributePermissions::write_only(),
        );

        let (_, audio_status_value_handle) = db.add_characteristic(
            asha_uuid::AUDIO_STATUS,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![audio_status::OK],
            AttributePermissions::read_only(),
        );

        let (_, volume_value_handle) = db.add_characteristic(
            asha_uuid::VOLUME,
            CharacteristicProperties(CharacteristicProperties::WRITE_WITHOUT_RESPONSE),
            vec![],
            AttributePermissions::write_only(),
        );

        let (_, le_psm_out_value_handle) = db.add_characteristic(
            asha_uuid::LE_PSM_OUT,
            CharacteristicProperties(CharacteristicProperties::READ),
            psm.to_le_bytes().to_vec(),
            AttributePermissions::read_only(),
        );

        let state = Arc::new(Mutex::new(AshaState {
            audio_status_value_handle,
            ..Default::default()
        }));
        db.set_handler(
            audio_control_point_value_handle,
            Box::new(AudioControlPointHandler {
                state: Arc::clone(&state),
            }),
        )
        .expect("control point handle just allocated");
        db.set_handler(
            volume_value_handle,
            Box::new(VolumeHandler {
                state: Arc::clone(&state),
            }),
        )
        .expect("volume handle just allocated");

        Self {
            service_handle,
            read_only_properties_value_handle,
            audio_control_point_value_handle,
            audio_status_value_handle,
            volume_value_handle,
            le_psm_out_value_handle,
            psm,
            properties,
            state,
        }
    }

    /// The ASHA Service Data payload for advertising: `[Version, DeviceCapabilities,
    /// HiSyncId truncated to its 4 least significant bytes]` — enough for a central to
    /// match the two aids of a set without connecting.
    pub fn advertising_service_data(&self) -> Vec<u8> {
        let mut data = vec![
            self.properties.protocol_version,
            self.properties.capabilities,
        ];
        data.extend_from_slice(&self.properties.hi_sync_id[..4]);
        data
    }

    /// An advertising payload carrying the ASHA Service Data (AD type 0x16 for service
    /// UUID 0xFDF0).
    pub fn advertising_data(&self) -> AdvertisingData {
        AdvertisingData::new().with_service_data_16(0xFDF0, &self.advertising_service_data())
    }

    /// Codec of the active audio stream, or `None` when not streaming.
    pub fn active_codec(&self) -> Option<u8> {
        self.state
            .lock()
            .expect("ASHA state lock poisoned")
            .active_codec
    }

    /// Returns the audio type.
    pub fn audio_type(&self) -> Option<u8> {
        self.state
            .lock()
            .expect("ASHA state lock poisoned")
            .audio_type
    }

    /// Returns the volume.
    pub fn volume(&self) -> Option<i8> {
        self.state.lock().expect("ASHA state lock poisoned").volume
    }

    /// Returns the OtherState field byte, if present.
    pub fn other_state(&self) -> Option<u8> {
        self.state
            .lock()
            .expect("ASHA state lock poisoned")
            .other_state
    }

    /// The most recent Status command's [`peripheral_status`] value, if any arrived.
    pub fn last_peripheral_status(&self) -> Option<u8> {
        self.state
            .lock()
            .expect("ASHA state lock poisoned")
            .last_peripheral_status
    }
}
