// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Hearing Access Profile (HAP): Hearing Access Service (HAS, UUID 0x1854).
//!
//! Lets a client (e.g. a phone) enumerate and switch the sound presets of a hearing aid.
//! The server publishes a Hearing Aid Features bitfield (HAS Section 3.1), a preset list
//! driven through the Hearing Aid Preset Control Point (HAS Section 3.2), and the Active
//! Preset Index characteristic (HAS Section 3.4).
//!
//! The Preset Control Point is wired through [`AttributeHandler`], so an ordinary
//! [`GattDatabase::write`] to it runs the operation. Read Presets Request responses and
//! Preset Changed operations are indications of the Control Point itself (HAS Section
//! 3.2.2); Simble has no ATT bearer to carry them, so the service queues each indication
//! payload and tests/hosts drain them via
//! [`HearingAccessService::take_control_point_indications`] — the same boundary every
//! other profile draws around notification transport. Two HAS behaviors tied to real
//! connections are likewise out of scope: the MTU >= 49 requirement (HAS Section 2.5)
//! and cross-device preset synchronization in a binaural set (the synchronized-locally
//! opcodes are accepted or rejected per the Preset Synchronization Support feature bit,
//! but there is no second device to forward them to).

use crate::att::error_code as att_error_code;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

/// HAS Service and characteristic UUIDs.
pub mod hap_uuid {
    use crate::types::Uuid;

    pub const HEARING_ACCESS_SERVICE: Uuid = Uuid::Uuid16(0x1854);
    pub const HEARING_AID_FEATURES: Uuid = Uuid::Uuid16(0x2BDA);
    pub const HEARING_AID_PRESET_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2BDB);
    pub const ACTIVE_PRESET_INDEX: Uuid = Uuid::Uuid16(0x2BDC);
}

/// Hearing Aid Preset Control Point opcodes (HAS Section 3.2.1). Opcodes 0x02/0x03 are
/// server-to-client only: they head the indication payloads the operations produce.
pub mod opcode {
    pub const READ_PRESETS_REQUEST: u8 = 0x01;
    pub const READ_PRESET_RESPONSE: u8 = 0x02;
    pub const PRESET_CHANGED: u8 = 0x03;
    pub const WRITE_PRESET_NAME: u8 = 0x04;
    pub const SET_ACTIVE_PRESET: u8 = 0x05;
    pub const SET_NEXT_PRESET: u8 = 0x06;
    pub const SET_PREVIOUS_PRESET: u8 = 0x07;
    pub const SET_ACTIVE_PRESET_SYNCHRONIZED_LOCALLY: u8 = 0x08;
    pub const SET_NEXT_PRESET_SYNCHRONIZED_LOCALLY: u8 = 0x09;
    pub const SET_PREVIOUS_PRESET_SYNCHRONIZED_LOCALLY: u8 = 0x0A;
}

/// HAS application error codes (HAS Section 2.4), plus the Common Profile and Service
/// Error Code HAS borrows for Read Presets Request parameters outside the preset list
/// (Core Spec Supplement Part B, Out of Range), returned as the ATT error code when a
/// Control Point write is rejected.
pub mod error_code {
    pub const INVALID_OPCODE: u8 = 0x80;
    pub const WRITE_NAME_NOT_ALLOWED: u8 = 0x81;
    pub const PRESET_SYNCHRONIZATION_NOT_SUPPORTED: u8 = 0x82;
    pub const PRESET_OPERATION_NOT_POSSIBLE: u8 = 0x83;
    pub const INVALID_PARAMETERS_LENGTH: u8 = 0x84;
    pub const OUT_OF_RANGE: u8 = 0xFF;
}

/// ChangeId values of a Preset Changed operation (HAS Section 3.2.2.2).
pub mod change_id {
    pub const GENERIC_UPDATE: u8 = 0x00;
    pub const PRESET_RECORD_DELETED: u8 = 0x01;
    pub const PRESET_RECORD_AVAILABLE: u8 = 0x02;
    pub const PRESET_RECORD_UNAVAILABLE: u8 = 0x03;
}

/// Preset record Properties bitfield (HAS Section 2.8).
pub mod preset_properties {
    pub const WRITABLE: u8 = 1 << 0;
    pub const IS_AVAILABLE: u8 = 1 << 1;
}

/// Preset names are 1..=40 bytes of UTF-8 (HAS Section 2.8).
pub const MAX_PRESET_NAME_LENGTH: usize = 40;

/// Hearing Aid Type field of the Hearing Aid Features bitfield (HAS Section 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum HearingAidType {
    Binaural = 0b00,
    Monaural = 0b01,
    Banded = 0b10,
}

impl HearingAidType {
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0b00 => Self::Binaural,
            0b01 => Self::Monaural,
            0b10 => Self::Banded,
            _ => return None,
        })
    }
}

/// Hearing Aid Features characteristic value (HAS Section 3.1): a hearing-aid type in
/// bits 0-1 and four single-bit capability flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HearingAidFeatures {
    pub hearing_aid_type: HearingAidType,
    pub preset_synchronization_supported: bool,
    /// Set when the two devices of a binaural set may expose different preset lists.
    pub independent_presets: bool,
    /// Set when the preset list may change at runtime (records added/deleted).
    pub dynamic_presets: bool,
    pub writable_presets_supported: bool,
}

impl HearingAidFeatures {
    pub fn to_byte(self) -> u8 {
        (self.hearing_aid_type as u8)
            | (u8::from(self.preset_synchronization_supported) << 2)
            | (u8::from(self.independent_presets) << 3)
            | (u8::from(self.dynamic_presets) << 4)
            | (u8::from(self.writable_presets_supported) << 5)
    }

    pub fn from_byte(value: u8) -> Option<Self> {
        Some(Self {
            hearing_aid_type: HearingAidType::from_u8(value & 0b11)?,
            preset_synchronization_supported: value & (1 << 2) != 0,
            independent_presets: value & (1 << 3) != 0,
            dynamic_presets: value & (1 << 4) != 0,
            writable_presets_supported: value & (1 << 5) != 0,
        })
    }
}

/// One preset record (HAS Section 2.8): a list index, Properties bits, and a UTF-8 name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PresetRecord {
    pub index: u8,
    pub writable: bool,
    pub available: bool,
    pub name: String,
}

impl PresetRecord {
    /// A writable, available preset — the common case for factory presets.
    pub fn new(index: u8, name: &str) -> Self {
        Self {
            index,
            writable: true,
            available: true,
            name: name.to_string(),
        }
    }

    /// Serializes as `[Index, Properties, Name]`, the record layout embedded in Read
    /// Preset Response and Preset Changed indications (HAS Sections 3.2.2.1/3.2.2.2).
    pub fn to_bytes(&self) -> Vec<u8> {
        let properties = (u8::from(self.writable) * preset_properties::WRITABLE)
            | (u8::from(self.available) * preset_properties::IS_AVAILABLE);
        let mut buf = Vec::with_capacity(2 + self.name.len());
        buf.push(self.index);
        buf.push(properties);
        buf.extend_from_slice(self.name.as_bytes());
        buf
    }

    pub fn parse(data: &[u8]) -> Option<Self> {
        let index = *data.first()?;
        let properties = *data.get(1)?;
        let name = std::str::from_utf8(data.get(2..)?).ok()?.to_string();
        Some(Self {
            index,
            writable: properties & preset_properties::WRITABLE != 0,
            available: properties & preset_properties::IS_AVAILABLE != 0,
            name,
        })
    }
}

/// Mutable server state shared between the service handle and the Preset Control Point
/// handler (see [`HearingAccessService`]).
#[derive(Debug)]
struct HearingAccessState {
    features: HearingAidFeatures,
    /// Keyed by preset index; BTreeMap keeps the spec-mandated increasing-index order
    /// for Read Presets Request responses (HAS Section 3.2.2.1) for free.
    presets: BTreeMap<u8, PresetRecord>,
    active_preset_index: u8,
    /// Pending Control Point indication payloads, drained via
    /// [`HearingAccessService::take_control_point_indications`] (see module docs for why
    /// indications are queued rather than sent).
    control_point_indications: Vec<Vec<u8>>,
    active_preset_index_value_handle: u16,
}

impl HearingAccessState {
    /// Queues one Preset Changed indication: `[0x03, ChangeId, IsLast, params...]` (HAS
    /// Section 3.2.2.2). Each server-initiated change is queued individually, so IsLast
    /// is always 1.
    fn queue_preset_changed(&mut self, change: u8, additional_parameters: &[u8]) {
        let mut payload = vec![opcode::PRESET_CHANGED, change, 1];
        payload.extend_from_slice(additional_parameters);
        self.control_point_indications.push(payload);
    }

    fn set_active_index(&mut self, db: &mut GattDatabase, index: u8) {
        if self.active_preset_index != index {
            self.active_preset_index = index;
            // The Active Preset Index characteristic notifies its new value to clients
            // (HAS Section 3.4); republishing the attribute is Simble's equivalent.
            let _ = db.set_value(self.active_preset_index_value_handle, &[index]);
        }
    }

    fn read_presets_request(&mut self, params: &[u8]) -> Result<(), u8> {
        // The operation carries exactly StartIndex + NumPresets (HAS Section 3.2.2.1).
        let [start_index, num_presets] = *params else {
            return Err(error_code::INVALID_PARAMETERS_LENGTH);
        };
        // Index 0x00 is reserved as "no active preset" and never names a record, and
        // zero requested presets can't be satisfied (HAS Section 3.2.2.1).
        if start_index == 0 || num_presets == 0 {
            return Err(error_code::OUT_OF_RANGE);
        }
        let selected: Vec<Vec<u8>> = self
            .presets
            .range(start_index..)
            .take(num_presets as usize)
            .map(|(_, preset)| preset.to_bytes())
            .collect();
        if selected.is_empty() {
            return Err(error_code::OUT_OF_RANGE);
        }
        let last = selected.len() - 1;
        for (i, record) in selected.into_iter().enumerate() {
            let mut payload = vec![opcode::READ_PRESET_RESPONSE, u8::from(i == last)];
            payload.extend_from_slice(&record);
            self.control_point_indications.push(payload);
        }
        Ok(())
    }

    fn write_preset_name(&mut self, params: &[u8]) -> Result<(), u8> {
        let (&index, name_bytes) = params
            .split_first()
            .ok_or(error_code::INVALID_PARAMETERS_LENGTH)?;
        // A missing record and a non-writable record get the same rejection: the name
        // cannot be written either way (HAS Section 3.2.2.4).
        if !self.presets.get(&index).is_some_and(|p| p.writable) {
            return Err(error_code::WRITE_NAME_NOT_ALLOWED);
        }
        if name_bytes.is_empty() || name_bytes.len() > MAX_PRESET_NAME_LENGTH {
            return Err(error_code::INVALID_PARAMETERS_LENGTH);
        }
        // Names are UTF-8 by definition (HAS Section 2.8); a byte sequence that isn't
        // UTF-8 is a value problem, not a length problem.
        let name =
            std::str::from_utf8(name_bytes).map_err(|_| att_error_code::VALUE_NOT_ALLOWED)?;

        let preset = self.presets.get_mut(&index).expect("writability checked");
        preset.name = name.to_string();
        let record = preset.to_bytes();
        // A renamed record is reported as a Generic Update whose PrevIndex is the
        // record's own index, since its list position did not change (HAS 3.2.2.2).
        let mut additional_parameters = vec![index];
        additional_parameters.extend_from_slice(&record);
        self.queue_preset_changed(change_id::GENERIC_UPDATE, &additional_parameters);
        Ok(())
    }

    fn set_active_preset(&mut self, db: &mut GattDatabase, params: &[u8]) -> Result<(), u8> {
        let &index = params
            .first()
            .ok_or(error_code::INVALID_PARAMETERS_LENGTH)?;
        // Only an existing, available preset can become active (HAS Section 3.2.2.5).
        if !self.presets.get(&index).is_some_and(|p| p.available) {
            return Err(error_code::PRESET_OPERATION_NOT_POSSIBLE);
        }
        self.set_active_index(db, index);
        Ok(())
    }

    /// Cycles the active preset through the available records in index order, wrapping
    /// at either end (HAS Sections 3.2.2.6/3.2.2.7).
    fn set_next_or_previous_preset(
        &mut self,
        db: &mut GattDatabase,
        is_previous: bool,
    ) -> Result<(), u8> {
        let available: Vec<u8> = self
            .presets
            .values()
            .filter(|p| p.available)
            .map(|p| p.index)
            .collect();
        let position = available
            .iter()
            .position(|&index| index == self.active_preset_index)
            .ok_or(error_code::PRESET_OPERATION_NOT_POSSIBLE)?;
        let new_index = if is_previous {
            available[(position + available.len() - 1) % available.len()]
        } else {
            available[(position + 1) % available.len()]
        };
        // Wrapping back onto the sole available preset means there is nothing to
        // switch to (HAS Section 3.2.2.6).
        if new_index == self.active_preset_index {
            return Err(error_code::PRESET_OPERATION_NOT_POSSIBLE);
        }
        self.set_active_index(db, new_index);
        Ok(())
    }

    /// Gate for the synchronized-locally opcode variants (HAS Sections 3.2.2.8-3.2.2.10).
    /// See module docs for why no forwarding to a binaural peer follows.
    fn check_synchronization_supported(&self) -> Result<(), u8> {
        if self.features.preset_synchronization_supported {
            Ok(())
        } else {
            Err(error_code::PRESET_SYNCHRONIZATION_NOT_SUPPORTED)
        }
    }
}

/// [`AttributeHandler`] for the Hearing Aid Preset Control Point.
#[derive(Debug)]
struct PresetControlPointHandler {
    state: Arc<Mutex<HearingAccessState>>,
}

impl AttributeHandler for PresetControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        let mut state = self.state.lock().expect("HAP state lock poisoned");
        let (&op, params) = value
            .split_first()
            .ok_or(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)?;
        match op {
            opcode::READ_PRESETS_REQUEST => state.read_presets_request(params),
            opcode::WRITE_PRESET_NAME => state.write_preset_name(params),
            opcode::SET_ACTIVE_PRESET => state.set_active_preset(db, params),
            opcode::SET_NEXT_PRESET => state.set_next_or_previous_preset(db, false),
            opcode::SET_PREVIOUS_PRESET => state.set_next_or_previous_preset(db, true),
            opcode::SET_ACTIVE_PRESET_SYNCHRONIZED_LOCALLY => {
                state.check_synchronization_supported()?;
                state.set_active_preset(db, params)
            }
            opcode::SET_NEXT_PRESET_SYNCHRONIZED_LOCALLY => {
                state.check_synchronization_supported()?;
                state.set_next_or_previous_preset(db, false)
            }
            opcode::SET_PREVIOUS_PRESET_SYNCHRONIZED_LOCALLY => {
                state.check_synchronization_supported()?;
                state.set_next_or_previous_preset(db, true)
            }
            _ => Err(error_code::INVALID_OPCODE),
        }
    }
}

/// Hearing Access Service GATT container plus the preset-list state it owns.
#[derive(Debug)]
pub struct HearingAccessService {
    pub service_handle: u16,
    pub features_value_handle: u16,
    pub preset_control_point_value_handle: u16,
    pub active_preset_index_value_handle: u16,
    state: Arc<Mutex<HearingAccessState>>,
}

impl HearingAccessService {
    /// Registers HAS with the given features and initial preset list. The lowest preset
    /// index starts active, matching a hearing aid booting into its first preset.
    ///
    /// # Panics
    /// If `presets` is empty (HAS requires at least one preset record to expose the
    /// Control Point's operations meaningfully) or any name is not 1..=40 bytes.
    pub fn register(
        db: &mut GattDatabase,
        features: HearingAidFeatures,
        presets: &[PresetRecord],
    ) -> Self {
        assert!(!presets.is_empty(), "HAS needs at least one preset record");
        for preset in presets {
            assert!(
                !preset.name.is_empty() && preset.name.len() <= MAX_PRESET_NAME_LENGTH,
                "preset names are 1..=40 bytes (HAS Section 2.8)"
            );
        }

        let service_handle = db.add_service(hap_uuid::HEARING_ACCESS_SERVICE, true);

        let (_, features_value_handle) = db.add_characteristic(
            hap_uuid::HEARING_AID_FEATURES,
            CharacteristicProperties(CharacteristicProperties::READ),
            vec![features.to_byte()],
            AttributePermissions::read_only(),
        );

        let (_, preset_control_point_value_handle) = db.add_characteristic(
            hap_uuid::HEARING_AID_PRESET_CONTROL_POINT,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::INDICATE,
            ),
            vec![],
            AttributePermissions::write_only(),
        );

        let preset_map: BTreeMap<u8, PresetRecord> = presets
            .iter()
            .map(|preset| (preset.index, preset.clone()))
            .collect();
        let active_preset_index = *preset_map.keys().next().expect("non-empty checked");

        let (_, active_preset_index_value_handle) = db.add_characteristic(
            hap_uuid::ACTIVE_PRESET_INDEX,
            CharacteristicProperties(
                CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
            ),
            vec![active_preset_index],
            AttributePermissions::read_only(),
        );

        let state = Arc::new(Mutex::new(HearingAccessState {
            features,
            presets: preset_map,
            active_preset_index,
            control_point_indications: Vec::new(),
            active_preset_index_value_handle,
        }));
        db.set_handler(
            preset_control_point_value_handle,
            Box::new(PresetControlPointHandler {
                state: Arc::clone(&state),
            }),
        )
        .expect("control point handle just allocated");

        Self {
            service_handle,
            features_value_handle,
            preset_control_point_value_handle,
            active_preset_index_value_handle,
            state,
        }
    }

    pub fn features(&self) -> HearingAidFeatures {
        self.state.lock().expect("HAP state lock poisoned").features
    }

    pub fn active_preset_index(&self) -> u8 {
        self.state
            .lock()
            .expect("HAP state lock poisoned")
            .active_preset_index
    }

    /// The preset record at `index`, if one exists.
    pub fn preset(&self, index: u8) -> Option<PresetRecord> {
        self.state
            .lock()
            .expect("HAP state lock poisoned")
            .presets
            .get(&index)
            .cloned()
    }

    /// Drains the pending Control Point indication payloads (Read Preset Response and
    /// Preset Changed operations) in the order they were produced.
    pub fn take_control_point_indications(&self) -> Vec<Vec<u8>> {
        std::mem::take(
            &mut self
                .state
                .lock()
                .expect("HAP state lock poisoned")
                .control_point_indications,
        )
    }

    /// Host-side: replaces (or adds) a preset record and reports it as a Generic Update
    /// whose PrevIndex is `prev_index` (HAS Section 3.2.2.2) — how a hearing aid
    /// publishes a preset it changed on its own.
    pub fn generic_update(&self, prev_index: u8, record: PresetRecord) {
        let mut state = self.state.lock().expect("HAP state lock poisoned");
        let mut additional_parameters = vec![prev_index];
        additional_parameters.extend_from_slice(&record.to_bytes());
        state.presets.insert(record.index, record);
        state.queue_preset_changed(change_id::GENERIC_UPDATE, &additional_parameters);
    }

    /// Host-side: deletes a preset record. The active preset cannot be deleted — the
    /// Active Preset Index would dangle (HAS Section 3.4).
    pub fn delete_preset(&self, index: u8) -> Result<(), u8> {
        let mut state = self.state.lock().expect("HAP state lock poisoned");
        if index == state.active_preset_index {
            return Err(error_code::PRESET_OPERATION_NOT_POSSIBLE);
        }
        state
            .presets
            .remove(&index)
            .ok_or(error_code::PRESET_OPERATION_NOT_POSSIBLE)?;
        state.queue_preset_changed(change_id::PRESET_RECORD_DELETED, &[index]);
        Ok(())
    }

    /// Host-side: marks a preset available and reports the change.
    pub fn set_preset_available(&self, index: u8) -> Result<(), u8> {
        let mut state = self.state.lock().expect("HAP state lock poisoned");
        let preset = state
            .presets
            .get_mut(&index)
            .ok_or(error_code::PRESET_OPERATION_NOT_POSSIBLE)?;
        preset.available = true;
        state.queue_preset_changed(change_id::PRESET_RECORD_AVAILABLE, &[index]);
        Ok(())
    }

    /// Host-side: marks a preset unavailable and reports the change. The active preset
    /// cannot become unavailable (HAS Section 3.2.2.5 requires the active preset to be
    /// an available one).
    pub fn set_preset_unavailable(&self, index: u8) -> Result<(), u8> {
        let mut state = self.state.lock().expect("HAP state lock poisoned");
        if index == state.active_preset_index {
            return Err(error_code::PRESET_OPERATION_NOT_POSSIBLE);
        }
        let preset = state
            .presets
            .get_mut(&index)
            .ok_or(error_code::PRESET_OPERATION_NOT_POSSIBLE)?;
        preset.available = false;
        state.queue_preset_changed(change_id::PRESET_RECORD_UNAVAILABLE, &[index]);
        Ok(())
    }
}
