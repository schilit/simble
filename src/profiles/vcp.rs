// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Volume Control Service (VCS, UUID 0x1844).
//!
//! Provides volume and mute controls for LE Audio streaming sinks and headsets.
//!
//! Models the Volume State (VCS Section 3.1) plus the Volume Control Point clients write to
//! adjust it (VCS Section 3.3), in the same shape as the two services VCS includes: AICS
//! (`crate::profiles::aics`) for an audio input and VOCS (`crate::profiles::vocs`) for an
//! audio output. Real deployments expose those two as secondary services included by VCS,
//! which Simble doesn't model (no GATT "Include" declaration support yet), so each
//! `register()` just adds its own service.
//!
//! The `VolumeState` is shared (`Arc<Mutex<_>>`) between `VolumeControlService` and the
//! `AttributeHandler` `register()` attaches to the Volume Control Point, so a real ATT write
//! arriving through `GattDatabase::write` drives the same validation host-side callers get.
//! Before that handler existed, a peer's Volume Control Point write landed on the control
//! point's stored bytes and no volume ever changed — the same defect
//! `test_ascs_control_point_dispatch_through_att_write` was written for on ASCS.
//!
//! The Change_Counter check (VCS Section 3.3.1) is the mechanism AICS and VOCS use too: a
//! write is rejected with Invalid Change Counter unless its Change_Counter operand matches
//! the current `VolumeState.change_counter`, which then advances by one on every write that
//! actually changes the state, so a client can detect it raced another write.

use std::sync::{Arc, Mutex};

use crate::att::error_code as att_error_code;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};

/// VCS Service UUIDs.
pub mod vcp_uuid {
    use crate::types::Uuid;

    /// Volume Control Service UUID.
    pub const VOLUME_CONTROL_SERVICE: Uuid = Uuid::Uuid16(0x1844);
    /// Volume State characteristic UUID.
    pub const VOLUME_STATE: Uuid = Uuid::Uuid16(0x2B7D);
    /// Volume Control Point characteristic UUID.
    pub const VOLUME_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2B7E);
    /// Volume Flags characteristic UUID.
    pub const VOLUME_FLAGS: Uuid = Uuid::Uuid16(0x2B7F);
}

/// Volume Control Point opcodes (VCS Section 3.3, Table 3.3).
pub mod opcode {
    /// Relative Volume Down control-point opcode.
    pub const RELATIVE_VOLUME_DOWN: u8 = 0x00;
    /// Relative Volume Up control-point opcode.
    pub const RELATIVE_VOLUME_UP: u8 = 0x01;
    /// Unmute/Relative Volume Down control-point opcode.
    pub const UNMUTE_RELATIVE_VOLUME_DOWN: u8 = 0x02;
    /// Unmute/Relative Volume Up control-point opcode.
    pub const UNMUTE_RELATIVE_VOLUME_UP: u8 = 0x03;
    /// Set Absolute Volume control-point opcode.
    pub const SET_ABSOLUTE_VOLUME: u8 = 0x04;
    /// Unmute control-point opcode.
    pub const UNMUTE: u8 = 0x05;
    /// Mute control-point opcode.
    pub const MUTE: u8 = 0x06;
}

/// VCS application error codes (VCS Section 1.6), returned as the ATT error code when a
/// Control Point write is rejected.
pub mod error_code {
    /// Invalid Change Counter error code.
    pub const INVALID_CHANGE_COUNTER: u8 = 0x80;
    /// Opcode Not Supported error code.
    pub const OPCODE_NOT_SUPPORTED: u8 = 0x81;
}

/// VCS Section 3.1.1.2 - Mute field: not muted.
pub const MUTE_NOT_MUTED: u8 = 0x00;
/// VCS Section 3.1.1.2 - Mute field: muted.
pub const MUTE_MUTED: u8 = 0x01;

/// VCS Section 3.2.1.1 - Volume Flags bit 0, Volume_Setting_Persisted: set once a client has
/// changed the Volume Setting, so a fresh client can tell a user-chosen volume from a
/// power-on default.
pub const VOLUME_SETTING_PERSISTED: u8 = 0x01;

/// The Volume Step Size a relative change moves by. VCS Section 3.3.1 leaves this
/// implementation-defined — it is not a characteristic and a client cannot read it — so this
/// is Simble's choice, not a spec value.
pub const DEFAULT_VOLUME_STEP_SIZE: u8 = 16;

/// VCS Section 3.1 - Volume State characteristic value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VolumeState {
    /// Volume Setting (0-255).
    pub volume_setting: u8,
    /// Mute: [`MUTE_NOT_MUTED`] or [`MUTE_MUTED`].
    pub mute: u8,
    /// Change Counter.
    pub change_counter: u8,
}

impl VolumeState {
    /// Serializes to the characteristic wire format.
    pub fn to_bytes(&self) -> [u8; 3] {
        [self.volume_setting, self.mute, self.change_counter]
    }

    fn increment_change_counter(&mut self) {
        self.change_counter = self.change_counter.wrapping_add(1);
    }

    /// VCS 3.3.1 - every Control Point operand carries the client's view of the change
    /// counter; a stale value is rejected with Invalid Change Counter.
    fn check_change_counter(&self, change_counter: u8) -> Result<(), u8> {
        if change_counter != self.change_counter {
            return Err(error_code::INVALID_CHANGE_COUNTER);
        }
        Ok(())
    }

    /// VCS 3.3.1 - Volume Control Point operations. Every operation is
    /// `[Opcode(1), Change_Counter(1)]`, except Set Absolute Volume which appends a
    /// `Volume_Setting(1)`.
    ///
    /// Returns `Ok(true)` when the Volume Setting or Mute value actually changed (the caller
    /// republishes and notifies), `Ok(false)` for an operation that was valid but left the
    /// state alone — VCS 3.3.1 does not advance the change counter for those, so a client
    /// that muted an already-muted device does not invalidate anyone else's counter.
    fn apply_control_point(&mut self, step_size: u8, data: &[u8]) -> Result<bool, u8> {
        let Some((&op, rest)) = data.split_first() else {
            return Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH);
        };
        // An unsupported opcode is rejected before the operand is parsed: its length is
        // only known per-opcode, and VCS 1.6 gives Opcode Not Supported for this case.
        let change_counter = match op {
            opcode::RELATIVE_VOLUME_DOWN
            | opcode::RELATIVE_VOLUME_UP
            | opcode::UNMUTE_RELATIVE_VOLUME_DOWN
            | opcode::UNMUTE_RELATIVE_VOLUME_UP
            | opcode::UNMUTE
            | opcode::MUTE => match rest {
                &[change_counter] => change_counter,
                _ => return Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH),
            },
            opcode::SET_ABSOLUTE_VOLUME => match rest {
                &[change_counter, _] => change_counter,
                _ => return Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH),
            },
            _ => return Err(error_code::OPCODE_NOT_SUPPORTED),
        };
        self.check_change_counter(change_counter)?;

        let before = *self;
        match op {
            opcode::RELATIVE_VOLUME_DOWN => {
                self.volume_setting = self.volume_setting.saturating_sub(step_size);
            }
            opcode::RELATIVE_VOLUME_UP => {
                self.volume_setting = self.volume_setting.saturating_add(step_size);
            }
            opcode::UNMUTE_RELATIVE_VOLUME_DOWN => {
                self.volume_setting = self.volume_setting.saturating_sub(step_size);
                self.mute = MUTE_NOT_MUTED;
            }
            opcode::UNMUTE_RELATIVE_VOLUME_UP => {
                self.volume_setting = self.volume_setting.saturating_add(step_size);
                self.mute = MUTE_NOT_MUTED;
            }
            opcode::SET_ABSOLUTE_VOLUME => self.volume_setting = rest[1],
            opcode::UNMUTE => self.mute = MUTE_NOT_MUTED,
            opcode::MUTE => self.mute = MUTE_MUTED,
            _ => unreachable!("opcode was validated above"),
        }

        if self.volume_setting == before.volume_setting && self.mute == before.mute {
            return Ok(false);
        }
        self.increment_change_counter();
        Ok(true)
    }
}

/// Owns writes to the Volume Control Point value attribute (attached via
/// `GattDatabase::set_handler`), so a raw ATT write drives the state machine instead of
/// overwriting the control point's stored bytes.
#[derive(Debug)]
struct VolumeControlPointHandler {
    state: Arc<Mutex<VolumeState>>,
    // Registration-time constants, copied here so the handler needs no back-reference to
    // the service that registered it.
    step_size: u8,
    volume_state_value_handle: u16,
    volume_flags_value_handle: u16,
}

impl AttributeHandler for VolumeControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().unwrap();
        if !state.apply_control_point(self.step_size, value)? {
            return Ok(());
        }
        let _ = db.set_value(self.volume_state_value_handle, &state.to_bytes());
        // VCS 3.2.1.1 - the first client-driven change makes the Volume Setting a user
        // setting rather than the power-on default.
        let _ = db.set_value(self.volume_flags_value_handle, &[VOLUME_SETTING_PERSISTED]);
        Ok(())
    }
}

/// Volume Control Service GATT container plus the Volume State it owns.
#[derive(Debug, Clone)]
pub struct VolumeControlService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Value attribute handle of the Volume State characteristic.
    pub volume_state_value_handle: u16,
    /// Value attribute handle of the Control Point characteristic.
    pub control_point_value_handle: u16,
    /// Value attribute handle of the Volume Flags characteristic.
    pub volume_flags_value_handle: u16,
    state: Arc<Mutex<VolumeState>>,
}

impl VolumeControlService {
    /// Registers VCS into a GATT database, with [`DEFAULT_VOLUME_STEP_SIZE`] for relative
    /// volume changes.
    pub fn register(db: &mut GattDatabase, initial_volume: u8, initial_mute: u8) -> Self {
        Self::register_with_step_size(db, initial_volume, initial_mute, DEFAULT_VOLUME_STEP_SIZE)
    }

    /// Registers VCS with an explicit Volume Step Size (VCS 3.3.1 leaves it to the server).
    pub fn register_with_step_size(
        db: &mut GattDatabase,
        initial_volume: u8,
        initial_mute: u8,
        step_size: u8,
    ) -> Self {
        let service_handle = db.add_service(vcp_uuid::VOLUME_CONTROL_SERVICE, true);

        // 1. Volume State (0x2B7D) - Read | Notify
        // Payload: Volume Setting (1B), Mute (1B: 0=Not Muted, 1=Muted), Change Counter (1B)
        let volume_state = VolumeState {
            volume_setting: initial_volume,
            mute: initial_mute,
            change_counter: 0,
        };
        let (_, volume_state_value_handle) = db.add_characteristic(
            vcp_uuid::VOLUME_STATE,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            volume_state.to_bytes().to_vec(),
            AttributePermissions::default(),
        );

        // 2. Volume Control Point (0x2B7E) - Write
        let (_, control_point_value_handle) = db.add_characteristic(
            vcp_uuid::VOLUME_CONTROL_POINT,
            CharacteristicProperties(CharacteristicProperties::WRITE),
            vec![],
            AttributePermissions::write_only(),
        );

        // 3. Volume Flags (0x2B7F) - Read | Notify
        let (_, volume_flags_value_handle) = db.add_characteristic(
            vcp_uuid::VOLUME_FLAGS,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![0x00], // Volume Setting Persisted = 0
            AttributePermissions::default(),
        );

        let state = Arc::new(Mutex::new(volume_state));
        db.set_handler(
            control_point_value_handle,
            Box::new(VolumeControlPointHandler {
                state: state.clone(),
                step_size,
                volume_state_value_handle,
                volume_flags_value_handle,
            }),
        )
        .expect("control point handle was just allocated");

        Self {
            service_handle,
            volume_state_value_handle,
            control_point_value_handle,
            volume_flags_value_handle,
            state,
        }
    }

    /// Snapshot of the current Volume State.
    pub fn volume_state(&self) -> VolumeState {
        *self.state.lock().unwrap()
    }

    /// Host-side convenience for driving the Volume Control Point (VCS 3.3): routes `data`
    /// through the same `GattDatabase::write` path a remote client's ATT write takes,
    /// dispatching to the handler `register()` attached. Returns the ATT application error
    /// code a real server would put in its Error Response (Section 1.6).
    pub fn write_control_point(&mut self, db: &mut GattDatabase, data: &[u8]) -> Result<(), u8> {
        db.write(self.control_point_value_handle, data)
    }
}
