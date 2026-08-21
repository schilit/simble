// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio Input Control Service (AICS, UUID 0x1843).
//!
//! Models one audio input's gain/mute/mode state (AICS Section 2.2.1) plus the Audio Input
//! Control Point clients write to drive it (AICS Section 3.5). An AICS instance always
//! describes a single audio input (e.g. one microphone, one line-in) - a device exposing
//! several inputs registers one `AudioInputControlService` per input, each getting its own
//! GATT service instance. Real deployments expose AICS as a secondary service included by a
//! Volume Control Service (VCS); Simble has no GATT "Include" declaration support yet, so
//! `register()` just adds AICS as its own service and leaves that relationship unmodeled.
//!
//! Like ASCS, AICS has real internal state beyond a static GATT value. The
//! `AudioInputState` is shared (`Arc<Mutex<_>>`, the same bridge shape
//! `crate::android::gatt_server` uses between its server and observer) between
//! `AudioInputControlService` and the `AttributeHandler` `register()` attaches to the Audio
//! Input Control Point, so a real ATT write arriving through `GattDatabase::write` drives
//! the same validation host-side callers get. Unlike ASCS's per-item notification vector,
//! an Audio Input Control Point write only ever targets the single Audio Input State this
//! service owns, so a write either succeeds or fails atomically - a rejection surfaces as
//! an ATT application error code (becoming a real ATT Error Response on the wire), matching
//! how a real AICS server rejects a write at the ATT layer rather than notifying a
//! per-operation response back on the control point itself.
//!
//! Every operation's wire format reserves a Change_Counter byte (AICS Section 3.5.1: "the
//! server shall not perform the requested operation" when it doesn't match), so this
//! implementation validates it on every opcode. The reference this was ported from checks it
//! only for Mute, leaving Unmute/Set Gain Setting/Set Manual|Automatic Gain Mode unchecked -
//! that's inconsistent with the field the wire format actually reserves for every opcode, so
//! validation here is uniform across all five instead.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::att::error_code as att_error_code;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// AICS Service and characteristic UUIDs.
pub mod aics_uuid {
    use crate::types::Uuid;

    pub const AUDIO_INPUT_CONTROL_SERVICE: Uuid = Uuid::Uuid16(0x1843);
    pub const AUDIO_INPUT_STATE: Uuid = Uuid::Uuid16(0x2B77);
    pub const GAIN_SETTINGS_ATTRIBUTE: Uuid = Uuid::Uuid16(0x2B78);
    pub const AUDIO_INPUT_TYPE: Uuid = Uuid::Uuid16(0x2B79);
    pub const AUDIO_INPUT_STATUS: Uuid = Uuid::Uuid16(0x2B7A);
    pub const AUDIO_INPUT_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2B7B);
    pub const AUDIO_INPUT_DESCRIPTION: Uuid = Uuid::Uuid16(0x2B7C);
}

/// Audio Input Control Point opcodes (AICS Section 3.5.1).
pub mod opcode {
    pub const SET_GAIN_SETTING: u8 = 0x01;
    pub const UNMUTE: u8 = 0x02;
    pub const MUTE: u8 = 0x03;
    pub const SET_MANUAL_GAIN_MODE: u8 = 0x04;
    pub const SET_AUTOMATIC_GAIN_MODE: u8 = 0x05;
}

/// AICS application error codes (AICS Section 1.6), returned as the ATT error code when a
/// Control Point write is rejected.
pub mod error_code {
    pub const INVALID_CHANGE_COUNTER: u8 = 0x80;
    pub const OPCODE_NOT_SUPPORTED: u8 = 0x81;
    pub const MUTE_DISABLED: u8 = 0x82;
    pub const VALUE_OUT_OF_RANGE: u8 = 0x83;
    pub const GAIN_MODE_CHANGE_NOT_ALLOWED: u8 = 0x84;
}

const GAIN_SETTINGS_MIN_VALUE: u8 = 0;
const GAIN_SETTINGS_MAX_VALUE: u8 = 255;

/// AICS Section 2.2.1.2 - Mute field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum Mute {
    #[default]
    NotMuted = 0x00,
    Muted = 0x01,
    Disabled = 0x02,
}

/// AICS Section 2.2.1.3 - Gain Mode field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum GainMode {
    ManualOnly = 0x00,
    AutomaticOnly = 0x01,
    #[default]
    Manual = 0x02,
    Automatic = 0x03,
}

/// AICS Section 3.4 - Audio Input Status field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioInputStatus {
    Inactive = 0x00,
    Active = 0x01,
}

/// Bluetooth Assigned Numbers - Audio Input Type field (AICS Section 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AudioInputType {
    Unspecified = 0x00,
    Bluetooth = 0x01,
    Microphone = 0x02,
    Analog = 0x03,
    Digital = 0x04,
    Radio = 0x05,
    Streaming = 0x06,
    Ambient = 0x07,
}

/// AICS Section 2.2.1 - Audio Input State characteristic value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AudioInputState {
    pub gain_setting: u8,
    pub mute: Mute,
    pub gain_mode: GainMode,
    pub change_counter: u8,
}

impl AudioInputState {
    pub fn to_bytes(&self) -> [u8; 4] {
        [
            self.gain_setting,
            self.mute as u8,
            self.gain_mode as u8,
            self.change_counter,
        ]
    }

    fn increment_change_counter(&mut self) {
        self.change_counter = self.change_counter.wrapping_add(1);
    }

    /// AICS 3.5.1 - every Control Point operand carries the client's view of the change
    /// counter; a stale value is rejected with Invalid Change Counter.
    fn check_change_counter(&self, change_counter: u8) -> Result<(), u8> {
        if change_counter != self.change_counter {
            return Err(error_code::INVALID_CHANGE_COUNTER);
        }
        Ok(())
    }

    /// AICS 3.5.2.1 - Set Gain Setting. Silently ignored (not an error) while Gain Mode is
    /// Automatic or Automatic Only, matching AICS 3.5.2.1's "shall not perform the
    /// operation" without a Control Point rejection code, and doesn't advance the change
    /// counter even on success (only Mute/Gain Mode transitions do, per 3.5).
    fn apply_set_gain_setting(
        &mut self,
        properties: &GainSettingsProperties,
        change_counter: u8,
        gain_setting: u8,
    ) -> Result<(), u8> {
        self.check_change_counter(change_counter)?;
        if !matches!(self.gain_mode, GainMode::Manual | GainMode::ManualOnly) {
            return Ok(());
        }
        if !(properties.gain_settings_minimum..=properties.gain_settings_maximum)
            .contains(&gain_setting)
        {
            return Err(error_code::VALUE_OUT_OF_RANGE);
        }
        self.gain_setting = gain_setting;
        Ok(())
    }

    /// AICS 3.5.2.2 - Unmute.
    fn apply_unmute(&mut self, change_counter: u8) -> Result<(), u8> {
        if self.mute == Mute::Disabled {
            return Err(error_code::MUTE_DISABLED);
        }
        self.check_change_counter(change_counter)?;
        if self.mute == Mute::NotMuted {
            return Ok(());
        }
        self.mute = Mute::NotMuted;
        self.increment_change_counter();
        Ok(())
    }

    /// AICS 3.5.2.3 - Mute.
    fn apply_mute(&mut self, change_counter: u8) -> Result<(), u8> {
        if self.mute == Mute::Disabled {
            return Err(error_code::MUTE_DISABLED);
        }
        self.check_change_counter(change_counter)?;
        if self.mute == Mute::Muted {
            return Ok(());
        }
        self.mute = Mute::Muted;
        self.increment_change_counter();
        Ok(())
    }

    /// AICS 3.5.2.4 - Set Manual Gain Mode. Rejected when Gain Mode is Manual Only or
    /// Automatic Only (those modes forbid client-driven mode changes).
    fn apply_set_manual_gain_mode(&mut self, change_counter: u8) -> Result<(), u8> {
        self.check_change_counter(change_counter)?;
        match self.gain_mode {
            GainMode::AutomaticOnly | GainMode::ManualOnly => {
                Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
            }
            GainMode::Manual => Ok(()),
            GainMode::Automatic => {
                self.gain_mode = GainMode::Manual;
                self.increment_change_counter();
                Ok(())
            }
        }
    }

    /// AICS 3.5.2.5 - Set Automatic Gain Mode. Rejected under the same Manual/Automatic-Only
    /// restriction as Set Manual Gain Mode.
    fn apply_set_automatic_gain_mode(&mut self, change_counter: u8) -> Result<(), u8> {
        self.check_change_counter(change_counter)?;
        match self.gain_mode {
            GainMode::AutomaticOnly | GainMode::ManualOnly => {
                Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
            }
            GainMode::Automatic => Ok(()),
            GainMode::Manual => {
                self.gain_mode = GainMode::Automatic;
                self.increment_change_counter();
                Ok(())
            }
        }
    }

    /// Applies an Audio Input Control Point write (AICS 3.5). Returns the ATT application
    /// error code to respond with on failure (Section 1.6), rather than a notification
    /// payload - unlike ASCS's batched per-ASE opcodes, every AICS opcode targets this
    /// service's single Audio Input State and either fully applies or is fully rejected.
    fn apply_control_point(
        &mut self,
        properties: &GainSettingsProperties,
        data: &[u8],
    ) -> Result<(), u8> {
        let Some((&op, rest)) = data.split_first() else {
            return Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH);
        };

        match op {
            opcode::SET_GAIN_SETTING => match rest {
                &[change_counter, gain_setting] => {
                    self.apply_set_gain_setting(properties, change_counter, gain_setting)
                }
                _ => Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH),
            },
            opcode::UNMUTE => match rest {
                &[change_counter] => self.apply_unmute(change_counter),
                _ => Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH),
            },
            opcode::MUTE => match rest {
                &[change_counter] => self.apply_mute(change_counter),
                _ => Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH),
            },
            opcode::SET_MANUAL_GAIN_MODE => match rest {
                &[change_counter] => self.apply_set_manual_gain_mode(change_counter),
                _ => Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH),
            },
            opcode::SET_AUTOMATIC_GAIN_MODE => match rest {
                &[change_counter] => self.apply_set_automatic_gain_mode(change_counter),
                _ => Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH),
            },
            _ => Err(error_code::OPCODE_NOT_SUPPORTED),
        }
    }
}

/// AICS Section 3.2 - Gain Settings Properties characteristic value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GainSettingsProperties {
    pub gain_settings_units: u8,
    pub gain_settings_minimum: u8,
    pub gain_settings_maximum: u8,
}

impl Default for GainSettingsProperties {
    fn default() -> Self {
        Self {
            gain_settings_units: 1,
            gain_settings_minimum: GAIN_SETTINGS_MIN_VALUE,
            gain_settings_maximum: GAIN_SETTINGS_MAX_VALUE,
        }
    }
}

impl GainSettingsProperties {
    pub fn to_bytes(&self) -> [u8; 3] {
        [
            self.gain_settings_units,
            self.gain_settings_minimum,
            self.gain_settings_maximum,
        ]
    }
}

/// Owns writes to the Audio Input Control Point value attribute (attached via
/// `GattDatabase::set_handler`), so a raw ATT write drives the state machine instead of
/// overwriting the control point's stored bytes.
#[derive(Debug)]
struct AudioInputControlPointHandler {
    state: Arc<Mutex<AudioInputState>>,
    // Registration-time constants, copied here so the handler needs no back-reference to
    // the service that registered it.
    gain_settings_properties: GainSettingsProperties,
    audio_input_state_value_handle: u16,
}

impl AttributeHandler for AudioInputControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().unwrap();
        state.apply_control_point(&self.gain_settings_properties, value)?;
        let _ = db.set_value(self.audio_input_state_value_handle, &state.to_bytes());
        Ok(())
    }
}

/// Audio Input Control Service GATT container plus the Audio Input State it owns.
#[derive(Debug, Clone)]
pub struct AudioInputControlService {
    pub service_handle: u16,
    pub audio_input_state_value_handle: u16,
    pub gain_settings_properties_value_handle: u16,
    pub audio_input_type_value_handle: u16,
    pub audio_input_status_value_handle: u16,
    pub control_point_value_handle: u16,
    pub audio_input_description_value_handle: u16,
    pub gain_settings_properties: GainSettingsProperties,
    state: Arc<Mutex<AudioInputState>>,
}

impl AudioInputControlService {
    /// Registers one AICS instance (one audio input) into a GATT database.
    pub fn register(
        db: &mut GattDatabase,
        gain_settings_properties: GainSettingsProperties,
        audio_input_type: AudioInputType,
        audio_input_status: AudioInputStatus,
        description: &str,
    ) -> Self {
        let service_handle = db.add_service(aics_uuid::AUDIO_INPUT_CONTROL_SERVICE, false);

        let audio_input_state = AudioInputState::default();

        let (_, audio_input_state_value_handle) = db.add_characteristic(
            aics_uuid::AUDIO_INPUT_STATE,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            audio_input_state.to_bytes().to_vec(),
            AttributePermissions::read_only(),
        );

        let (_, gain_settings_properties_value_handle) = db.add_characteristic(
            aics_uuid::GAIN_SETTINGS_ATTRIBUTE,
            CharacteristicProperties(CharacteristicProperties::READ),
            gain_settings_properties.to_bytes().to_vec(),
            AttributePermissions::read_only(),
        );

        let (_, audio_input_type_value_handle) = db.add_characteristic(
            aics_uuid::AUDIO_INPUT_TYPE,
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![audio_input_type as u8],
            AttributePermissions::read_only(),
        );

        let (_, audio_input_status_value_handle) = db.add_characteristic(
            aics_uuid::AUDIO_INPUT_STATUS,
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![audio_input_status as u8],
            AttributePermissions::read_only(),
        );

        let (_, control_point_value_handle) = db.add_characteristic(
            aics_uuid::AUDIO_INPUT_CONTROL_POINT,
            CharacteristicProperties(CharacteristicProperties::WRITE),
            vec![],
            AttributePermissions::write_only(),
        );

        let (_, audio_input_description_value_handle) = db.add_characteristic(
            aics_uuid::AUDIO_INPUT_DESCRIPTION,
            CharacteristicProperties(
                CharacteristicProperties::READ
                    | CharacteristicProperties::NOTIFY
                    | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
            ),
            description.as_bytes().to_vec(),
            AttributePermissions::default(),
        );

        let state = Arc::new(Mutex::new(audio_input_state));
        db.set_handler(
            control_point_value_handle,
            Box::new(AudioInputControlPointHandler {
                state: state.clone(),
                gain_settings_properties,
                audio_input_state_value_handle,
            }),
        )
        .expect("control point handle was just allocated");

        Self {
            service_handle,
            audio_input_state_value_handle,
            gain_settings_properties_value_handle,
            audio_input_type_value_handle,
            audio_input_status_value_handle,
            control_point_value_handle,
            audio_input_description_value_handle,
            gain_settings_properties,
            state,
        }
    }

    /// Snapshot of the current Audio Input State.
    pub fn audio_input_state(&self) -> AudioInputState {
        *self.state.lock().unwrap()
    }

    /// Host-side mutable access to the Audio Input State, for simulation scenarios the
    /// Control Point can't reach (e.g. forcing `Mute::Disabled` or a non-default Gain
    /// Mode). Bypasses Control Point validation and doesn't republish the GATT value.
    pub fn audio_input_state_mut(&self) -> MutexGuard<'_, AudioInputState> {
        self.state.lock().unwrap()
    }

    /// Host-side convenience for driving the Audio Input Control Point (AICS 3.5): routes
    /// `data` through the same `GattDatabase::write` path a remote client's ATT write
    /// takes, dispatching to the handler `register()` attached. Returns the ATT
    /// application error code a real server would put in its Error Response (Section 1.6).
    pub fn write_control_point(&mut self, db: &mut GattDatabase, data: &[u8]) -> Result<(), u8> {
        db.write(self.control_point_value_handle, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_service(db: &mut GattDatabase) -> AudioInputControlService {
        AudioInputControlService::register(
            db,
            GainSettingsProperties::default(),
            AudioInputType::Microphone,
            AudioInputStatus::Active,
            "Bluetooth",
        )
    }

    #[test]
    fn test_initial_state() {
        let mut db = GattDatabase::new();
        let aics = new_service(&mut db);
        assert_eq!(aics.audio_input_state(), AudioInputState::default());
        assert_eq!(
            db.read(aics.audio_input_state_value_handle, 0).unwrap(),
            &[0, Mute::NotMuted as u8, GainMode::Manual as u8, 0]
        );
        assert_eq!(
            db.read(aics.audio_input_status_value_handle, 0).unwrap(),
            &[AudioInputStatus::Active as u8]
        );
    }

    #[test]
    fn test_unsupported_opcode_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        assert_eq!(
            aics.write_control_point(&mut db, &[0xFF]),
            Err(error_code::OPCODE_NOT_SUPPORTED)
        );
    }

    #[test]
    fn test_set_gain_setting_ignored_when_automatic_only() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().gain_mode = GainMode::AutomaticOnly;

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 120]),
            Ok(())
        );
        assert_eq!(aics.audio_input_state().gain_setting, 0);
        assert_eq!(aics.audio_input_state().change_counter, 0);
    }

    #[test]
    fn test_set_gain_setting_ignored_when_automatic() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().gain_mode = GainMode::Automatic;

        aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 120])
            .unwrap();
        assert_eq!(aics.audio_input_state().gain_setting, 0);
    }

    #[test]
    fn test_set_gain_setting_when_manual() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);

        aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 120])
            .unwrap();
        assert_eq!(aics.audio_input_state().gain_setting, 120);
        // Set Gain Setting never advances the change counter.
        assert_eq!(aics.audio_input_state().change_counter, 0);
        assert_eq!(
            db.read(aics.audio_input_state_value_handle, 0).unwrap()[0],
            120
        );
    }

    #[test]
    fn test_set_gain_setting_when_manual_only() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().gain_mode = GainMode::ManualOnly;

        aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 120])
            .unwrap();
        assert_eq!(aics.audio_input_state().gain_setting, 120);
    }

    #[test]
    fn test_set_gain_setting_out_of_range_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = AudioInputControlService::register(
            &mut db,
            GainSettingsProperties {
                gain_settings_units: 1,
                gain_settings_minimum: 10,
                gain_settings_maximum: 20,
            },
            AudioInputType::Microphone,
            AudioInputStatus::Active,
            "",
        );

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::SET_GAIN_SETTING, 0, 5]),
            Err(error_code::VALUE_OUT_OF_RANGE)
        );
        assert_eq!(aics.audio_input_state().gain_setting, 0);
    }

    #[test]
    fn test_unmute_when_muted() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().mute = Mute::Muted;

        aics.write_control_point(&mut db, &[opcode::UNMUTE, 0])
            .unwrap();
        assert_eq!(aics.audio_input_state().mute, Mute::NotMuted);
        assert_eq!(aics.audio_input_state().change_counter, 1);
    }

    #[test]
    fn test_unmute_when_mute_disabled_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().mute = Mute::Disabled;

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::UNMUTE, 0]),
            Err(error_code::MUTE_DISABLED)
        );
        assert_eq!(aics.audio_input_state().mute, Mute::Disabled);
        assert_eq!(aics.audio_input_state().change_counter, 0);
    }

    #[test]
    fn test_mute_when_not_muted() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);

        aics.write_control_point(&mut db, &[opcode::MUTE, 0])
            .unwrap();
        assert_eq!(aics.audio_input_state().mute, Mute::Muted);
        assert_eq!(aics.audio_input_state().change_counter, 1);
    }

    #[test]
    fn test_mute_when_mute_disabled_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().mute = Mute::Disabled;

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::MUTE, 0]),
            Err(error_code::MUTE_DISABLED)
        );
    }

    #[test]
    fn test_stale_change_counter_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::MUTE, 1]),
            Err(error_code::INVALID_CHANGE_COUNTER)
        );
        assert_eq!(aics.audio_input_state().mute, Mute::NotMuted);
        assert_eq!(aics.audio_input_state().change_counter, 0);
    }

    #[test]
    fn test_set_manual_gain_mode_when_automatic() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().gain_mode = GainMode::Automatic;

        aics.write_control_point(&mut db, &[opcode::SET_MANUAL_GAIN_MODE, 0])
            .unwrap();
        assert_eq!(aics.audio_input_state().gain_mode, GainMode::Manual);
        assert_eq!(aics.audio_input_state().change_counter, 1);
    }

    #[test]
    fn test_set_manual_gain_mode_when_already_manual_is_noop() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);

        aics.write_control_point(&mut db, &[opcode::SET_MANUAL_GAIN_MODE, 0])
            .unwrap();
        assert_eq!(aics.audio_input_state().gain_mode, GainMode::Manual);
        assert_eq!(aics.audio_input_state().change_counter, 0);
    }

    #[test]
    fn test_set_manual_gain_mode_when_manual_only_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().gain_mode = GainMode::ManualOnly;

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::SET_MANUAL_GAIN_MODE, 0]),
            Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
        );
    }

    #[test]
    fn test_set_manual_gain_mode_when_automatic_only_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().gain_mode = GainMode::AutomaticOnly;

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::SET_MANUAL_GAIN_MODE, 0]),
            Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
        );
    }

    #[test]
    fn test_set_automatic_gain_mode_when_manual() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);

        aics.write_control_point(&mut db, &[opcode::SET_AUTOMATIC_GAIN_MODE, 0])
            .unwrap();
        assert_eq!(aics.audio_input_state().gain_mode, GainMode::Automatic);
        assert_eq!(aics.audio_input_state().change_counter, 1);
    }

    #[test]
    fn test_set_automatic_gain_mode_when_manual_only_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().gain_mode = GainMode::ManualOnly;

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::SET_AUTOMATIC_GAIN_MODE, 0]),
            Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
        );
    }

    #[test]
    fn test_set_automatic_gain_mode_when_automatic_only_is_rejected() {
        let mut db = GattDatabase::new();
        let mut aics = new_service(&mut db);
        aics.audio_input_state_mut().gain_mode = GainMode::AutomaticOnly;

        assert_eq!(
            aics.write_control_point(&mut db, &[opcode::SET_AUTOMATIC_GAIN_MODE, 0]),
            Err(error_code::GAIN_MODE_CHANGE_NOT_ALLOWED)
        );
    }

    #[test]
    fn test_audio_input_description_write_and_read() {
        let mut db = GattDatabase::new();
        let aics = new_service(&mut db);
        assert_eq!(
            db.read(aics.audio_input_description_value_handle, 0)
                .unwrap(),
            b"Bluetooth"
        );

        db.write(aics.audio_input_description_value_handle, b"Line Input")
            .unwrap();
        assert_eq!(
            db.read(aics.audio_input_description_value_handle, 0)
                .unwrap(),
            b"Line Input"
        );
    }
}
