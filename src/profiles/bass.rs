// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Broadcast Audio Scan Service (BASS, UUID 0x184F).
//!
//! Lets a Broadcast Assistant (e.g. a phone) scan for broadcast Audio Streams on behalf of
//! a Scan Delegator (e.g. an earbud) and tell it which broadcast sources to synchronize
//! to. The Delegator publishes one Broadcast Receive State characteristic per source slot
//! (BASS Section 3.2) and accepts Add/Modify/Remove Source operations on the Broadcast
//! Audio Scan Control Point (BASS Section 3.1).
//!
//! The Control Point is wired through `AttributeHandler`, so an ordinary
//! `GattDatabase::write` to it runs the operation and republishes the affected
//! Broadcast Receive State value — there is no bespoke `write_control_point` method to
//! call. A requested sync here is treated as immediately achieved: the simulated
//! Delegator reports `SynchronizedToPa` / the requested BIS bits as soon as an
//! Add/Modify Source operation asks for them, whether or not any such broadcast exists.
//!
//! That fake is now avoidable in-process, not only against netsim.
//! [`crate::device::BigReceiver`] does the real thing — periodic advertising sync, BASE
//! and BIGInfo, LE BIG Create Sync, ISO data path — checked against Bumble in both
//! directions (`tests/interop/auracast_*.py`) and now against a real broadcaster over
//! [`crate::controller::sim::Link`], which models enough of periodic advertising and
//! BIGs to establish, stream and tear one down (`tests/broadcast_e2e_test.rs`). Nothing
//! here calls it yet; wiring the Delegator's Add Source operation to an actual sync, and
//! reporting the state it really reaches — including the states where it *fails* —
//! is the work this comment is still standing in for.

use crate::att::error_code as att_error_code;
use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use crate::profiles::bap::{read_u24_le, write_u24_le};
use crate::types::Address;
use std::sync::{Arc, Mutex};

/// BASS Service and characteristic UUIDs.
pub mod bass_uuid {
    use crate::types::Uuid;

    /// Broadcast Audio Scan Service UUID.
    pub const BROADCAST_AUDIO_SCAN_SERVICE: Uuid = Uuid::Uuid16(0x184F);
    /// Broadcast Audio Scan Control Point characteristic UUID.
    pub const BROADCAST_AUDIO_SCAN_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2BC7);
    /// Broadcast Receive State characteristic UUID.
    pub const BROADCAST_RECEIVE_STATE: Uuid = Uuid::Uuid16(0x2BC8);
}

/// Broadcast Audio Scan Control Point opcodes (BASS Section 3.1.1).
pub mod opcode {
    /// Remote Scan Stopped control-point opcode.
    pub const REMOTE_SCAN_STOPPED: u8 = 0x00;
    /// Remote Scan Started control-point opcode.
    pub const REMOTE_SCAN_STARTED: u8 = 0x01;
    /// Add Source control-point opcode.
    pub const ADD_SOURCE: u8 = 0x02;
    /// Modify Source control-point opcode.
    pub const MODIFY_SOURCE: u8 = 0x03;
    /// Set Broadcast Code control-point opcode.
    pub const SET_BROADCAST_CODE: u8 = 0x04;
    /// Remove Source control-point opcode.
    pub const REMOVE_SOURCE: u8 = 0x05;
}

/// BASS application error codes (BASS Section 1.5), returned as the ATT error code when a
/// Control Point write is rejected.
pub mod error_code {
    /// Opcode Not Supported error code.
    pub const OPCODE_NOT_SUPPORTED: u8 = 0x80;
    /// Invalid Source Id error code.
    pub const INVALID_SOURCE_ID: u8 = 0x81;
}

/// BIS_Sync value meaning "no preference" in Add/Modify Source operations
/// (BASS Section 3.1.1.4).
pub const ANY_BIS: u32 = 0xFFFF_FFFF;

/// PA_Sync parameter of Add/Modify Source operations (BASS Section 3.1.1.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PeriodicAdvertisingSyncParams {
    /// Do not synchronize to pa.
    DoNotSynchronizeToPa = 0x00,
    /// Synchronize to pa past available.
    SynchronizeToPaPastAvailable = 0x01,
    /// Synchronize to pa past not available.
    SynchronizeToPaPastNotAvailable = 0x02,
}

impl PeriodicAdvertisingSyncParams {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x00 => Self::DoNotSynchronizeToPa,
            0x01 => Self::SynchronizeToPaPastAvailable,
            0x02 => Self::SynchronizeToPaPastNotAvailable,
            _ => return None,
        })
    }
}

/// PA_Sync_State field of a Broadcast Receive State (BASS Section 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PeriodicAdvertisingSyncState {
    /// Not synchronized to pa.
    NotSynchronizedToPa = 0x00,
    /// Sync info request.
    SyncInfoRequest = 0x01,
    /// Synchronized to pa.
    SynchronizedToPa = 0x02,
    /// Failed to synchronize to pa.
    FailedToSynchronizeToPa = 0x03,
    /// No past.
    NoPast = 0x04,
}

impl PeriodicAdvertisingSyncState {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x00 => Self::NotSynchronizedToPa,
            0x01 => Self::SyncInfoRequest,
            0x02 => Self::SynchronizedToPa,
            0x03 => Self::FailedToSynchronizeToPa,
            0x04 => Self::NoPast,
            _ => return None,
        })
    }
}

/// BIG_Encryption field of a Broadcast Receive State (BASS Section 3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum BigEncryption {
    /// Not encrypted.
    NotEncrypted = 0x00,
    /// Broadcast code required.
    BroadcastCodeRequired = 0x01,
    /// Decrypting.
    Decrypting = 0x02,
    /// Bad code.
    BadCode = 0x03,
}

impl BigEncryption {
    /// Maps a wire byte to the matching variant, or `None` if unrecognized.
    pub fn from_u8(value: u8) -> Option<Self> {
        Some(match value {
            0x00 => Self::NotEncrypted,
            0x01 => Self::BroadcastCodeRequired,
            0x02 => Self::Decrypting,
            0x03 => Self::BadCode,
            _ => return None,
        })
    }
}

/// One subgroup entry of an Add/Modify Source operation or a Broadcast Receive State:
/// a 32-bit BIS index bitmask plus LTV metadata (BASS Sections 3.1.1.4 / 3.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubgroupInfo {
    /// Bis Sync.
    pub bis_sync: u32,
    /// Metadata.
    pub metadata: Vec<u8>,
}

/// Serializes subgroups as `[Num_Subgroups, (BIS_Sync(4), Metadata_Length(1),
/// Metadata)*]`, the layout shared by the Control Point operations and the Broadcast
/// Receive State.
pub(crate) fn encode_subgroups(subgroups: &[SubgroupInfo]) -> Vec<u8> {
    let mut buf = vec![subgroups.len() as u8];
    for subgroup in subgroups {
        buf.extend_from_slice(&subgroup.bis_sync.to_le_bytes());
        buf.push(subgroup.metadata.len() as u8);
        buf.extend_from_slice(&subgroup.metadata);
    }
    buf
}

/// Parses the subgroup layout produced by [`encode_subgroups`], consuming `data` to its
/// end (subgroups are always the final field of the structures that carry them).
pub(crate) fn decode_subgroups(data: &[u8]) -> Option<Vec<SubgroupInfo>> {
    let (&num_subgroups, mut rest) = data.split_first()?;
    let mut subgroups = Vec::with_capacity(num_subgroups as usize);
    for _ in 0..num_subgroups {
        let bis_sync = u32::from_le_bytes(rest.get(..4)?.try_into().ok()?);
        let metadata_length = *rest.get(4)? as usize;
        let metadata = rest.get(5..5 + metadata_length)?.to_vec();
        rest = &rest[5 + metadata_length..];
        subgroups.push(SubgroupInfo { bis_sync, metadata });
    }
    Some(subgroups)
}

/// A Broadcast Audio Scan Control Point operation (BASS Section 3.1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlPointOperation {
    /// Remote scan stopped.
    RemoteScanStopped,
    /// Remote scan started.
    RemoteScanStarted,
    /// Add source.
    AddSource {
        /// Advertiser address type.
        advertiser_address_type: u8,
        /// Advertiser address.
        advertiser_address: Address,
        /// Advertising sid.
        advertising_sid: u8,
        /// Broadcast id.
        broadcast_id: u32,
        /// Pa sync.
        pa_sync: PeriodicAdvertisingSyncParams,
        /// Pa interval.
        pa_interval: u16,
        /// Subgroups.
        subgroups: Vec<SubgroupInfo>,
    },
    /// Modify source.
    ModifySource {
        /// Source id.
        source_id: u8,
        /// Pa sync.
        pa_sync: PeriodicAdvertisingSyncParams,
        /// Pa interval.
        pa_interval: u16,
        /// Subgroups.
        subgroups: Vec<SubgroupInfo>,
    },
    /// Set broadcast code.
    SetBroadcastCode {
        /// Source id.
        source_id: u8,
        /// Broadcast code.
        broadcast_code: [u8; 16],
    },
    /// Remove source.
    RemoveSource {
        /// Source id.
        source_id: u8,
    },
}

impl ControlPointOperation {
    /// Serializes to the characteristic wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::RemoteScanStopped => vec![opcode::REMOTE_SCAN_STOPPED],
            Self::RemoteScanStarted => vec![opcode::REMOTE_SCAN_STARTED],
            Self::AddSource {
                advertiser_address_type,
                advertiser_address,
                advertising_sid,
                broadcast_id,
                pa_sync,
                pa_interval,
                subgroups,
            } => {
                let mut buf = vec![opcode::ADD_SOURCE, *advertiser_address_type];
                buf.extend_from_slice(&advertiser_address.bytes);
                buf.push(*advertising_sid);
                buf.extend_from_slice(&write_u24_le(*broadcast_id));
                buf.push(*pa_sync as u8);
                buf.extend_from_slice(&pa_interval.to_le_bytes());
                buf.extend_from_slice(&encode_subgroups(subgroups));
                buf
            }
            Self::ModifySource {
                source_id,
                pa_sync,
                pa_interval,
                subgroups,
            } => {
                let mut buf = vec![opcode::MODIFY_SOURCE, *source_id, *pa_sync as u8];
                buf.extend_from_slice(&pa_interval.to_le_bytes());
                buf.extend_from_slice(&encode_subgroups(subgroups));
                buf
            }
            Self::SetBroadcastCode {
                source_id,
                broadcast_code,
            } => {
                let mut buf = vec![opcode::SET_BROADCAST_CODE, *source_id];
                buf.extend_from_slice(broadcast_code);
                buf
            }
            Self::RemoveSource { source_id } => vec![opcode::REMOVE_SOURCE, *source_id],
        }
    }

    /// Parses a value from its wire bytes.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let (&op, params) = data.split_first()?;
        Some(match op {
            opcode::REMOTE_SCAN_STOPPED => Self::RemoteScanStopped,
            opcode::REMOTE_SCAN_STARTED => Self::RemoteScanStarted,
            opcode::ADD_SOURCE => {
                let advertiser_address_type = *params.first()?;
                let advertiser_address = Address::new(params.get(1..7)?.try_into().ok()?);
                let advertising_sid = *params.get(7)?;
                let broadcast_id = read_u24_le(params.get(8..11)?);
                let pa_sync = PeriodicAdvertisingSyncParams::from_u8(*params.get(11)?)?;
                let pa_interval = u16::from_le_bytes(params.get(12..14)?.try_into().ok()?);
                let subgroups = decode_subgroups(params.get(14..)?)?;
                Self::AddSource {
                    advertiser_address_type,
                    advertiser_address,
                    advertising_sid,
                    broadcast_id,
                    pa_sync,
                    pa_interval,
                    subgroups,
                }
            }
            opcode::MODIFY_SOURCE => {
                let source_id = *params.first()?;
                let pa_sync = PeriodicAdvertisingSyncParams::from_u8(*params.get(1)?)?;
                let pa_interval = u16::from_le_bytes(params.get(2..4)?.try_into().ok()?);
                let subgroups = decode_subgroups(params.get(4..)?)?;
                Self::ModifySource {
                    source_id,
                    pa_sync,
                    pa_interval,
                    subgroups,
                }
            }
            opcode::SET_BROADCAST_CODE => Self::SetBroadcastCode {
                source_id: *params.first()?,
                broadcast_code: params.get(1..17)?.try_into().ok()?,
            },
            opcode::REMOVE_SOURCE => Self::RemoveSource {
                source_id: *params.first()?,
            },
            _ => return None,
        })
    }
}

/// Broadcast Receive State characteristic value (BASS Section 3.2). `bad_code` is
/// present on the wire only when `big_encryption` is [`BigEncryption::BadCode`], where it
/// echoes the 16-byte Broadcast_Code that failed to decrypt the BIG.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BroadcastReceiveState {
    /// Source Id.
    pub source_id: u8,
    /// Source Address Type.
    pub source_address_type: u8,
    /// Source Address.
    pub source_address: Address,
    /// Source Adv Sid.
    pub source_adv_sid: u8,
    /// Broadcast Id.
    pub broadcast_id: u32,
    /// Pa Sync State.
    pub pa_sync_state: PeriodicAdvertisingSyncState,
    /// Big Encryption.
    pub big_encryption: BigEncryption,
    /// Bad Code.
    pub bad_code: Vec<u8>,
    /// Subgroups.
    pub subgroups: Vec<SubgroupInfo>,
}

impl BroadcastReceiveState {
    /// Serializes to the characteristic wire format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![self.source_id, self.source_address_type];
        buf.extend_from_slice(&self.source_address.bytes);
        buf.push(self.source_adv_sid);
        buf.extend_from_slice(&write_u24_le(self.broadcast_id));
        buf.push(self.pa_sync_state as u8);
        buf.push(self.big_encryption as u8);
        buf.extend_from_slice(&self.bad_code);
        buf.extend_from_slice(&encode_subgroups(&self.subgroups));
        buf
    }

    /// Parses a value from its wire bytes.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let source_id = *data.first()?;
        let source_address_type = *data.get(1)?;
        let source_address = Address::new(data.get(2..8)?.try_into().ok()?);
        let source_adv_sid = *data.get(8)?;
        let broadcast_id = read_u24_le(data.get(9..12)?);
        let pa_sync_state = PeriodicAdvertisingSyncState::from_u8(*data.get(12)?)?;
        let big_encryption = BigEncryption::from_u8(*data.get(13)?)?;
        let (bad_code, subgroups) = if big_encryption == BigEncryption::BadCode {
            (
                data.get(14..30)?.to_vec(),
                decode_subgroups(data.get(30..)?)?,
            )
        } else {
            (Vec::new(), decode_subgroups(data.get(14..)?)?)
        };
        Some(Self {
            source_id,
            source_address_type,
            source_address,
            source_adv_sid,
            broadcast_id,
            pa_sync_state,
            big_encryption,
            bad_code,
            subgroups,
        })
    }
}

/// Mutable Scan Delegator state shared between the service handle and the Control Point
/// handler (see [`BroadcastAudioScanService`]).
#[derive(Debug, Default)]
struct ScanDelegatorState {
    /// Whether a Broadcast Assistant told us it is scanning on our behalf
    /// (Remote Scan Started/Stopped, BASS Sections 3.1.1.2/3.1.1.1).
    assistant_scanning: bool,
    /// One slot per Broadcast Receive State characteristic; `None` while the slot's
    /// characteristic value is empty (no source).
    sources: Vec<Option<BroadcastReceiveState>>,
    /// Source IDs must be unique among current sources (BASS Section 3.1.1.4); a
    /// monotonically increasing counter keeps removed IDs from being reused while any
    /// client might still reference them.
    next_source_id: u8,
}

/// `AttributeHandler` for the Broadcast Audio Scan Control Point. Owns the operation
/// semantics; shares the delegator state with [`BroadcastAudioScanService`] via
/// `Arc<Mutex<_>>` because `GattDatabase` takes ownership of the handler box.
#[derive(Debug)]
struct ScanControlPointHandler {
    state: Arc<Mutex<ScanDelegatorState>>,
    receive_state_value_handles: Vec<u16>,
}

impl ScanControlPointHandler {
    /// Serializes slot `index` into its Broadcast Receive State characteristic — an
    /// empty value when the slot holds no source, per BASS Section 3.2.
    fn publish_slot(&self, db: &mut GattDatabase, state: &ScanDelegatorState, index: usize) {
        let value = match &state.sources[index] {
            Some(receive_state) => receive_state.to_bytes(),
            None => Vec::new(),
        };
        let _ = db.set_value(self.receive_state_value_handles[index], &value);
    }

    /// Maps a requested BIS_Sync onto the reported BIS_Sync_State: the simulator treats
    /// any requested sync as immediately achieved, and maps [`ANY_BIS`] ("no
    /// preference") to a concrete result of "no BIS synchronized" because 0xFFFFFFFF
    /// means "failed to synchronize" in a Broadcast Receive State (BASS Section 3.2).
    fn bis_sync_state(requested: u32) -> u32 {
        if requested == ANY_BIS { 0 } else { requested }
    }

    fn sync_states(
        pa_sync: PeriodicAdvertisingSyncParams,
        subgroups: Vec<SubgroupInfo>,
    ) -> (PeriodicAdvertisingSyncState, Vec<SubgroupInfo>) {
        let pa_sync_state = match pa_sync {
            PeriodicAdvertisingSyncParams::DoNotSynchronizeToPa => {
                PeriodicAdvertisingSyncState::NotSynchronizedToPa
            }
            // No real PA sync procedure exists in the simulator, so a sync request
            // succeeds immediately regardless of PAST availability.
            _ => PeriodicAdvertisingSyncState::SynchronizedToPa,
        };
        let subgroups = subgroups
            .into_iter()
            .map(|subgroup| SubgroupInfo {
                bis_sync: if pa_sync_state == PeriodicAdvertisingSyncState::SynchronizedToPa {
                    Self::bis_sync_state(subgroup.bis_sync)
                } else {
                    // Not synchronized to the PA means no BIS can be synchronized.
                    0
                },
                metadata: subgroup.metadata,
            })
            .collect();
        (pa_sync_state, subgroups)
    }

    fn slot_of(state: &ScanDelegatorState, source_id: u8) -> Result<usize, u8> {
        state
            .sources
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|s| s.source_id == source_id))
            .ok_or(error_code::INVALID_SOURCE_ID)
    }
}

impl AttributeHandler for ScanControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        // Distinguish "unknown opcode" (BASS application error 0x80) from "known opcode,
        // malformed parameters" (ATT invalid length) before full parsing.
        match value.first() {
            None => return Err(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH),
            Some(&op) if op > opcode::REMOVE_SOURCE => {
                return Err(error_code::OPCODE_NOT_SUPPORTED);
            }
            Some(_) => {}
        }
        let operation = ControlPointOperation::parse(value)
            .ok_or(att_error_code::INVALID_ATTRIBUTE_VALUE_LENGTH)?;

        let mut state = self.state.lock().expect("BASS state lock poisoned");
        match operation {
            ControlPointOperation::RemoteScanStopped => state.assistant_scanning = false,
            ControlPointOperation::RemoteScanStarted => state.assistant_scanning = true,
            ControlPointOperation::AddSource {
                advertiser_address_type,
                advertiser_address,
                advertising_sid,
                broadcast_id,
                pa_sync,
                pa_interval: _,
                subgroups,
            } => {
                let index = state
                    .sources
                    .iter()
                    .position(Option::is_none)
                    .ok_or(att_error_code::INSUFFICIENT_RESOURCES)?;
                let source_id = state.next_source_id;
                state.next_source_id = state.next_source_id.wrapping_add(1);
                let (pa_sync_state, subgroups) = Self::sync_states(pa_sync, subgroups);
                state.sources[index] = Some(BroadcastReceiveState {
                    source_id,
                    source_address_type: advertiser_address_type,
                    source_address: advertiser_address,
                    source_adv_sid: advertising_sid,
                    broadcast_id,
                    pa_sync_state,
                    big_encryption: BigEncryption::NotEncrypted,
                    bad_code: Vec::new(),
                    subgroups,
                });
                self.publish_slot(db, &state, index);
            }
            ControlPointOperation::ModifySource {
                source_id,
                pa_sync,
                pa_interval: _,
                subgroups,
            } => {
                let index = Self::slot_of(&state, source_id)?;
                let (pa_sync_state, subgroups) = Self::sync_states(pa_sync, subgroups);
                let source = state.sources[index].as_mut().expect("slot checked");
                source.pa_sync_state = pa_sync_state;
                source.subgroups = subgroups;
                self.publish_slot(db, &state, index);
            }
            ControlPointOperation::SetBroadcastCode {
                source_id,
                broadcast_code: _,
            } => {
                let index = Self::slot_of(&state, source_id)?;
                let source = state.sources[index].as_mut().expect("slot checked");
                // The simulator can't actually decrypt a BIG, so any code supplied while
                // one is required is accepted as correct (BASS Section 3.1.1.6).
                if source.big_encryption == BigEncryption::BroadcastCodeRequired {
                    source.big_encryption = BigEncryption::Decrypting;
                    self.publish_slot(db, &state, index);
                }
            }
            ControlPointOperation::RemoveSource { source_id } => {
                let index = Self::slot_of(&state, source_id)?;
                state.sources[index] = None;
                self.publish_slot(db, &state, index);
            }
        }
        Ok(())
    }
}

/// Broadcast Audio Scan Service GATT container (the Scan Delegator role).
#[derive(Debug)]
pub struct BroadcastAudioScanService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Value attribute handle of the Control Point characteristic.
    pub control_point_value_handle: u16,
    /// Receive State Value Handles.
    pub receive_state_value_handles: Vec<u16>,
    state: Arc<Mutex<ScanDelegatorState>>,
}

impl BroadcastAudioScanService {
    /// Registers BASS with `num_receive_states` Broadcast Receive State slots (the spec
    /// requires at least one; each slot can track one broadcast source).
    pub fn register(db: &mut GattDatabase, num_receive_states: usize) -> Self {
        let service_handle = db.add_service(bass_uuid::BROADCAST_AUDIO_SCAN_SERVICE, true);

        let (_, control_point_value_handle) = db.add_characteristic(
            bass_uuid::BROADCAST_AUDIO_SCAN_CONTROL_POINT,
            CharacteristicProperties(
                CharacteristicProperties::WRITE | CharacteristicProperties::WRITE_WITHOUT_RESPONSE,
            ),
            vec![],
            AttributePermissions::write_only(),
        );

        let receive_state_value_handles: Vec<u16> = (0..num_receive_states)
            .map(|_| {
                let (_, value_handle) = db.add_characteristic(
                    bass_uuid::BROADCAST_RECEIVE_STATE,
                    CharacteristicProperties(
                        CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
                    ),
                    // Empty value = no source in this slot (BASS Section 3.2).
                    vec![],
                    AttributePermissions::read_only(),
                );
                value_handle
            })
            .collect();

        let state = Arc::new(Mutex::new(ScanDelegatorState {
            sources: vec![None; num_receive_states],
            ..Default::default()
        }));
        db.set_handler(
            control_point_value_handle,
            Box::new(ScanControlPointHandler {
                state: Arc::clone(&state),
                receive_state_value_handles: receive_state_value_handles.clone(),
            }),
        )
        .expect("control point handle just allocated");

        Self {
            service_handle,
            control_point_value_handle,
            receive_state_value_handles,
            state,
        }
    }

    /// Whether a Broadcast Assistant reported that it is scanning on our behalf.
    pub fn assistant_scanning(&self) -> bool {
        self.state
            .lock()
            .expect("BASS state lock poisoned")
            .assistant_scanning
    }

    /// The Broadcast Receive State currently held in slot `index`, if any.
    pub fn receive_state(&self, index: usize) -> Option<BroadcastReceiveState> {
        self.state
            .lock()
            .expect("BASS state lock poisoned")
            .sources
            .get(index)?
            .clone()
    }

    /// Host-side: marks `source_id`'s BIG as encrypted and awaiting a Broadcast_Code —
    /// what a real Delegator reports after receiving an encrypted BIGInfo (BASS Section
    /// 3.2), which the simulator has no controller event for. A subsequent Set
    /// Broadcast Code operation then moves the source to [`BigEncryption::Decrypting`].
    pub fn require_broadcast_code(&self, db: &mut GattDatabase, source_id: u8) -> Result<(), u8> {
        let mut state = self.state.lock().expect("BASS state lock poisoned");
        let index = ScanControlPointHandler::slot_of(&state, source_id)?;
        let source = state.sources[index].as_mut().expect("slot checked");
        source.big_encryption = BigEncryption::BroadcastCodeRequired;
        let value = source.to_bytes();
        let _ = db.set_value(self.receive_state_value_handles[index], &value);
        Ok(())
    }
}
