// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Volume Offset Control Service (VOCS, UUID 0x1845).
//!
//! Models one audio output's volume offset/audio-location state (VOCS Section 2) plus the
//! Volume Offset Control Point clients write to adjust it (VOCS Section 3.3). A VOCS
//! instance describes a single audio output the same way an AICS instance
//! (`crate::profiles::aics`) describes a single audio input; real deployments expose it as a
//! secondary service included by a Volume Control Service, which Simble doesn't model (no
//! GATT "Include" declaration support yet), so `register()` just adds VOCS as its own
//! service.
//!
//! The `VolumeOffsetState` is shared (`Arc<Mutex<_>>`, the same bridge shape
//! `crate::android::gatt_server` uses between its server and observer) between
//! `VolumeOffsetControlService` and the `AttributeHandler` `register()` attaches to the
//! Volume Offset Control Point, so a real ATT write arriving through `GattDatabase::write`
//! drives the same validation host-side callers get. As with AICS, a Volume Offset Control
//! Point write targets only this service's single state and either succeeds or fails
//! atomically, so a rejected write surfaces as an ATT application error code (becoming a
//! real ATT Error Response on the wire) rather than a notification payload.
//!
//! The Change_Counter check (VOCS Section 3.3.1) is the same mechanism AICS's Control Point
//! uses: a write is rejected unless its Change_Counter operand matches the current
//! `VolumeOffsetState.change_counter`, which then advances by one on every successful write
//! so a client can detect it raced another write.

use std::sync::{Arc, Mutex};

use crate::att::error_code as att_error_code;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// VOCS Service and characteristic UUIDs.
pub mod vocs_uuid {
    use crate::types::Uuid;

    /// Volume Offset Control Service UUID.
    pub const VOLUME_OFFSET_CONTROL_SERVICE: Uuid = Uuid::Uuid16(0x1845);
    /// Volume Offset State characteristic UUID.
    pub const VOLUME_OFFSET_STATE: Uuid = Uuid::Uuid16(0x2B80);
    /// Audio Location characteristic UUID.
    pub const AUDIO_LOCATION: Uuid = Uuid::Uuid16(0x2B81);
    /// Volume Offset Control Point characteristic UUID.
    pub const VOLUME_OFFSET_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2B82);
    /// Audio Output Description characteristic UUID.
    pub const AUDIO_OUTPUT_DESCRIPTION: Uuid = Uuid::Uuid16(0x2B83);
}

/// Volume Offset Control Point opcodes (VOCS Section 3.3).
pub mod opcode {
    /// Set Volume Offset control-point opcode.
    pub const SET_VOLUME_OFFSET: u8 = 0x01;
}

/// VOCS application error codes (VOCS Section 1.6), returned as the ATT error code when a
/// Control Point write is rejected.
pub mod error_code {
    /// Invalid Change Counter error code.
    pub const INVALID_CHANGE_COUNTER: u8 = 0x80;
    /// Opcode Not Supported error code.
    pub const OPCODE_NOT_SUPPORTED: u8 = 0x81;
    /// Value Out Of Range error code.
    pub const VALUE_OUT_OF_RANGE: u8 = 0x82;
}

const MIN_VOLUME_OFFSET: i16 = -255;
const MAX_VOLUME_OFFSET: i16 = 255;

/// VOCS Section 2.1 - Volume Offset State characteristic value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VolumeOffsetState {
    /// Volume Offset.
    pub volume_offset: i16,
    /// Change Counter.
    pub change_counter: u8,
}

impl VolumeOffsetState {
    /// Serializes to the characteristic wire format.
    pub fn to_bytes(&self) -> [u8; 3] {
        let offset = self.volume_offset.to_le_bytes();
        [offset[0], offset[1], self.change_counter]
    }

    fn increment_change_counter(&mut self) {
        self.change_counter = self.change_counter.wrapping_add(1);
    }

    /// VOCS 3.3.1 - Set Volume Offset Operation: `[Change_Counter(1), Volume_Offset(2,
    /// signed little-endian)]`. Rejects a stale change counter before range-checking the
    /// operand.
    fn apply_control_point(&mut self, data: &[u8]) -> Result<(), u8> {
        let Some((&op, rest)) = data.split_first() else {
            return Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH);
        };
        if op != opcode::SET_VOLUME_OFFSET {
            return Err(error_code::OPCODE_NOT_SUPPORTED);
        }
        let &[change_counter, lo, hi] = rest else {
            return Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH);
        };
        let volume_offset = i16::from_le_bytes([lo, hi]);

        if change_counter != self.change_counter {
            return Err(error_code::INVALID_CHANGE_COUNTER);
        }
        if !(MIN_VOLUME_OFFSET..=MAX_VOLUME_OFFSET).contains(&volume_offset) {
            return Err(error_code::VALUE_OUT_OF_RANGE);
        }

        self.volume_offset = volume_offset;
        self.increment_change_counter();
        Ok(())
    }
}

/// Owns writes to the Volume Offset Control Point value attribute (attached via
/// `GattDatabase::set_handler`), so a raw ATT write drives the state machine instead of
/// overwriting the control point's stored bytes.
#[derive(Debug)]
struct VolumeOffsetControlPointHandler {
    state: Arc<Mutex<VolumeOffsetState>>,
    volume_offset_state_value_handle: u16,
}

impl AttributeHandler for VolumeOffsetControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().unwrap();
        state.apply_control_point(value)?;
        let _ = db.set_value(self.volume_offset_state_value_handle, &state.to_bytes());
        Ok(())
    }
}

/// Volume Offset Control Service GATT container plus the Volume Offset State it owns.
#[derive(Debug, Clone)]
pub struct VolumeOffsetControlService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Value attribute handle of the Volume Offset State characteristic.
    pub volume_offset_state_value_handle: u16,
    /// Value attribute handle of the Audio Location characteristic.
    pub audio_location_value_handle: u16,
    /// Value attribute handle of the Control Point characteristic.
    pub control_point_value_handle: u16,
    /// Value attribute handle of the Audio Output Description characteristic.
    pub audio_output_description_value_handle: u16,
    state: Arc<Mutex<VolumeOffsetState>>,
}

impl VolumeOffsetControlService {
    /// Registers one VOCS instance (one audio output) into a GATT database.
    pub fn register(db: &mut GattDatabase, audio_location: u32, description: &str) -> Self {
        let service_handle = db.add_service(vocs_uuid::VOLUME_OFFSET_CONTROL_SERVICE, false);

        let volume_offset_state = VolumeOffsetState::default();

        let (_, volume_offset_state_value_handle) = db.add_characteristic(
            vocs_uuid::VOLUME_OFFSET_STATE,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            volume_offset_state.to_bytes().to_vec(),
            AttributePermissions::read_only(),
        );

        let (_, audio_location_value_handle) = db.add_characteristic(
            vocs_uuid::AUDIO_LOCATION,
            CharacteristicProperties(
                CharacteristicProperties::READ
                    | CharacteristicProperties::NOTIFY
                    | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
            ),
            audio_location.to_le_bytes().to_vec(),
            AttributePermissions::default(),
        );

        let (_, control_point_value_handle) = db.add_characteristic(
            vocs_uuid::VOLUME_OFFSET_CONTROL_POINT,
            CharacteristicProperties(CharacteristicProperties::WRITE),
            vec![],
            AttributePermissions::write_only(),
        );

        let (_, audio_output_description_value_handle) = db.add_characteristic(
            vocs_uuid::AUDIO_OUTPUT_DESCRIPTION,
            CharacteristicProperties(
                CharacteristicProperties::READ
                    | CharacteristicProperties::NOTIFY
                    | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
            ),
            description.as_bytes().to_vec(),
            AttributePermissions::default(),
        );

        let state = Arc::new(Mutex::new(volume_offset_state));
        db.set_handler(
            control_point_value_handle,
            Box::new(VolumeOffsetControlPointHandler {
                state: state.clone(),
                volume_offset_state_value_handle,
            }),
        )
        .expect("control point handle was just allocated");

        Self {
            service_handle,
            volume_offset_state_value_handle,
            audio_location_value_handle,
            control_point_value_handle,
            audio_output_description_value_handle,
            state,
        }
    }

    /// Snapshot of the current Volume Offset State.
    pub fn volume_offset_state(&self) -> VolumeOffsetState {
        *self.state.lock().unwrap()
    }

    /// Host-side convenience for driving the Volume Offset Control Point (VOCS 3.3):
    /// routes `data` through the same `GattDatabase::write` path a remote client's ATT
    /// write takes, dispatching to the handler `register()` attached.
    pub fn write_control_point(&mut self, db: &mut GattDatabase, data: &[u8]) -> Result<(), u8> {
        db.write(self.control_point_value_handle, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profiles::bap::audio_location;

    fn new_service(db: &mut GattDatabase) -> VolumeOffsetControlService {
        VolumeOffsetControlService::register(db, audio_location::NOT_ALLOWED, "")
    }

    fn set_volume_offset_pdu(change_counter: u8, volume_offset: i16) -> Vec<u8> {
        let mut buf = vec![opcode::SET_VOLUME_OFFSET, change_counter];
        buf.extend_from_slice(&volume_offset.to_le_bytes());
        buf
    }

    #[test]
    fn test_initial_state() {
        let mut db = GattDatabase::new();
        let vocs = new_service(&mut db);
        assert_eq!(vocs.volume_offset_state(), VolumeOffsetState::default());
        assert_eq!(
            db.read(vocs.volume_offset_state_value_handle, 0).unwrap(),
            &[0, 0, 0]
        );
        assert_eq!(
            db.read(vocs.audio_location_value_handle, 0).unwrap(),
            &audio_location::NOT_ALLOWED.to_le_bytes()
        );
        assert_eq!(
            db.read(vocs.audio_output_description_value_handle, 0)
                .unwrap(),
            b""
        );
    }

    #[test]
    fn test_unsupported_opcode_is_rejected() {
        let mut db = GattDatabase::new();
        let mut vocs = new_service(&mut db);
        assert_eq!(
            vocs.write_control_point(&mut db, &[0xFF]),
            Err(error_code::OPCODE_NOT_SUPPORTED)
        );
    }

    #[test]
    fn test_stale_change_counter_is_rejected() {
        let mut db = GattDatabase::new();
        let mut vocs = new_service(&mut db);

        assert_eq!(
            vocs.write_control_point(&mut db, &set_volume_offset_pdu(1, 0)),
            Err(error_code::INVALID_CHANGE_COUNTER)
        );
        assert_eq!(vocs.volume_offset_state(), VolumeOffsetState::default());
    }

    #[test]
    fn test_volume_offset_out_of_range_is_rejected() {
        let mut db = GattDatabase::new();
        let mut vocs = new_service(&mut db);

        assert_eq!(
            vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, MIN_VOLUME_OFFSET - 1)),
            Err(error_code::VALUE_OUT_OF_RANGE)
        );
        assert_eq!(
            vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, MAX_VOLUME_OFFSET + 1)),
            Err(error_code::VALUE_OUT_OF_RANGE)
        );
        assert_eq!(vocs.volume_offset_state().change_counter, 0);
    }

    #[test]
    fn test_set_volume_offset() {
        let mut db = GattDatabase::new();
        let mut vocs = new_service(&mut db);

        vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, -255))
            .unwrap();
        assert_eq!(
            vocs.volume_offset_state(),
            VolumeOffsetState {
                volume_offset: -255,
                change_counter: 1,
            }
        );
        assert_eq!(
            db.read(vocs.volume_offset_state_value_handle, 0).unwrap(),
            &[0x01, 0xFF, 1]
        );
    }

    #[test]
    fn test_set_volume_offset_requires_fresh_change_counter_each_time() {
        let mut db = GattDatabase::new();
        let mut vocs = new_service(&mut db);

        vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, 10))
            .unwrap();
        assert_eq!(
            vocs.write_control_point(&mut db, &set_volume_offset_pdu(0, 20)),
            Err(error_code::INVALID_CHANGE_COUNTER)
        );
        vocs.write_control_point(&mut db, &set_volume_offset_pdu(1, 20))
            .unwrap();
        assert_eq!(vocs.volume_offset_state().volume_offset, 20);
    }

    #[test]
    fn test_set_audio_location() {
        let mut db = GattDatabase::new();
        let vocs = new_service(&mut db);

        db.write(
            vocs.audio_location_value_handle,
            &audio_location::FRONT_LEFT.to_le_bytes(),
        )
        .unwrap();
        assert_eq!(
            db.read(vocs.audio_location_value_handle, 0).unwrap(),
            &audio_location::FRONT_LEFT.to_le_bytes()
        );
    }

    #[test]
    fn test_set_audio_output_description() {
        let mut db = GattDatabase::new();
        let vocs = new_service(&mut db);

        db.write(vocs.audio_output_description_value_handle, b"Left Speaker")
            .unwrap();
        assert_eq!(
            db.read(vocs.audio_output_description_value_handle, 0)
                .unwrap(),
            b"Left Speaker"
        );
    }
}
