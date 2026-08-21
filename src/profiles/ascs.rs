// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Audio Stream Control Service (ASCS, UUID 0x184E).
//!
//! Exposes one GATT characteristic per Audio Stream Endpoint (Sink or Source ASE) plus a
//! shared ASE Control Point characteristic clients write to drive each ASE's state machine
//! (BAP Section 5): Idle -> Codec Configured -> QoS Configured -> Enabling -> Streaming ->
//! Disabling/Releasing -> Idle. Unlike the other profiles in this crate, ASCS has real
//! per-ASE state beyond a static GATT value. That state is shared (`Arc<Mutex<_>>`, the
//! same bridge shape `crate::android::gatt_server` uses between its server and observer)
//! between `AudioStreamControlService` and the `AttributeHandler` `register()` attaches to
//! the ASE Control Point, so a real ATT write arriving through `GattDatabase::write` drives
//! the state machines and pushes each resulting ASE value into the `GattDatabase`.
//!
//! CIS establishment/teardown (which Bumble's `AseStateMachine` drives asynchronously off
//! controller events, e.g. auto-transitioning a Sink ASE to Streaming once its CIS is
//! established) has no equivalent here since Simble has no CIS/controller simulation yet;
//! `on_receiver_start_ready` and `on_release` model the client-driven half of those
//! transitions synchronously instead.

use std::sync::{Arc, Mutex};

use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use crate::profiles::bap::{LC3_CODEC_ID, read_u24_le, write_u24_le};

/// ASCS Service and characteristic UUIDs.
pub mod ascs_uuid {
    use crate::types::Uuid;

    /// Audio Stream Control Service UUID.
    pub const AUDIO_STREAM_CONTROL_SERVICE: Uuid = Uuid::Uuid16(0x184E);
    /// Sink Ase characteristic UUID.
    pub const SINK_ASE: Uuid = Uuid::Uuid16(0x2BC4);
    /// Source Ase characteristic UUID.
    pub const SOURCE_ASE: Uuid = Uuid::Uuid16(0x2BC5);
    /// Ase Control Point characteristic UUID.
    pub const ASE_CONTROL_POINT: Uuid = Uuid::Uuid16(0x2BC6);
}

/// ASE Control Point opcodes (Audio Stream Control Service, Section 5).
pub mod opcode {
    /// Config Codec control-point opcode.
    pub const CONFIG_CODEC: u8 = 0x01;
    /// Config Qos control-point opcode.
    pub const CONFIG_QOS: u8 = 0x02;
    /// Enable control-point opcode.
    pub const ENABLE: u8 = 0x03;
    /// Receiver Start Ready control-point opcode.
    pub const RECEIVER_START_READY: u8 = 0x04;
    /// Disable control-point opcode.
    pub const DISABLE: u8 = 0x05;
    /// Receiver Stop Ready control-point opcode.
    pub const RECEIVER_STOP_READY: u8 = 0x06;
    /// Update Metadata control-point opcode.
    pub const UPDATE_METADATA: u8 = 0x07;
    /// Release control-point opcode.
    pub const RELEASE: u8 = 0x08;
}

/// ASE Response Code values notified back on the Control Point (ASCS Table 5.1).
pub mod response_code {
    /// Success.
    pub const SUCCESS: u8 = 0x00;
    /// Unsupported Opcode.
    pub const UNSUPPORTED_OPCODE: u8 = 0x01;
    /// Invalid Length.
    pub const INVALID_LENGTH: u8 = 0x02;
    /// Invalid Ase Id.
    pub const INVALID_ASE_ID: u8 = 0x03;
    /// Invalid Ase State Machine Transition.
    pub const INVALID_ASE_STATE_MACHINE_TRANSITION: u8 = 0x04;
    /// Invalid Ase Direction.
    pub const INVALID_ASE_DIRECTION: u8 = 0x05;
    /// Unsupported Audio Capabilities.
    pub const UNSUPPORTED_AUDIO_CAPABILITIES: u8 = 0x06;
    /// Unsupported Configuration Parameter Value.
    pub const UNSUPPORTED_CONFIGURATION_PARAMETER_VALUE: u8 = 0x07;
    /// Rejected Configuration Parameter Value.
    pub const REJECTED_CONFIGURATION_PARAMETER_VALUE: u8 = 0x08;
    /// Invalid Configuration Parameter Value.
    pub const INVALID_CONFIGURATION_PARAMETER_VALUE: u8 = 0x09;
    /// Unsupported Metadata.
    pub const UNSUPPORTED_METADATA: u8 = 0x0A;
    /// Rejected Metadata.
    pub const REJECTED_METADATA: u8 = 0x0B;
    /// Invalid Metadata.
    pub const INVALID_METADATA: u8 = 0x0C;
    /// Insufficient Resources.
    pub const INSUFFICIENT_RESOURCES: u8 = 0x0D;
    /// Unspecified Error.
    pub const UNSPECIFIED_ERROR: u8 = 0x0E;
}

/// ASE Reason Code values, valid when `response_code` names a rejected parameter.
pub mod reason_code {
    /// None.
    pub const NONE: u8 = 0x00;
    /// Codec Id.
    pub const CODEC_ID: u8 = 0x01;
    /// Codec Specific Configuration.
    pub const CODEC_SPECIFIC_CONFIGURATION: u8 = 0x02;
    /// Sdu Interval.
    pub const SDU_INTERVAL: u8 = 0x03;
    /// Framing.
    pub const FRAMING: u8 = 0x04;
    /// Phy.
    pub const PHY: u8 = 0x05;
    /// Maximum Sdu Size.
    pub const MAXIMUM_SDU_SIZE: u8 = 0x06;
    /// Retransmission Number.
    pub const RETRANSMISSION_NUMBER: u8 = 0x07;
    /// Max Transport Latency.
    pub const MAX_TRANSPORT_LATENCY: u8 = 0x08;
    /// Presentation Delay.
    pub const PRESENTATION_DELAY: u8 = 0x09;
    /// Invalid Ase Cis Mapping.
    pub const INVALID_ASE_CIS_MAPPING: u8 = 0x0A;
}

const PREFERRED_FRAMING: u8 = 0;
const PREFERRED_PHY: u8 = 0;
const PREFERRED_RETRANSMISSION_NUMBER: u8 = 13;
const PREFERRED_MAX_TRANSPORT_LATENCY: u16 = 100;

/// ASE state machine states (ASCS Section 3, Table 3.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum AseState {
    Idle = 0x00,
    CodecConfigured = 0x01,
    QosConfigured = 0x02,
    Enabling = 0x03,
    Streaming = 0x04,
    Disabling = 0x05,
    Releasing = 0x06,
}

/// Direction an Audio Stream Endpoint carries audio in, relative to this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRole {
    Sink,
    Source,
}

/// One Audio Stream Endpoint: GATT handle plus the state ASCS Section 5 tracks for it.
#[derive(Debug, Clone)]
pub struct AudioStreamEndpoint {
    /// Ase Id.
    pub ase_id: u8,
    /// Role.
    pub role: AudioRole,
    /// Attribute handle of the Value.
    pub value_handle: u16,
    /// State.
    pub state: AseState,
    /// Codec Id.
    pub codec_id: [u8; 5],
    /// Codec Specific Configuration.
    pub codec_specific_configuration: Vec<u8>,
    /// Cig Id.
    pub cig_id: u8,
    /// Cis Id.
    pub cis_id: u8,
    /// Sdu Interval.
    pub sdu_interval: u32,
    /// Framing.
    pub framing: u8,
    /// Phy.
    pub phy: u8,
    /// Max Sdu.
    pub max_sdu: u16,
    /// Retransmission Number.
    pub retransmission_number: u8,
    /// Max Transport Latency.
    pub max_transport_latency: u16,
    /// Presentation Delay.
    pub presentation_delay: u32,
    /// Metadata.
    pub metadata: Vec<u8>,
}

impl AudioStreamEndpoint {
    fn new(ase_id: u8, role: AudioRole, value_handle: u16) -> Self {
        Self {
            ase_id,
            role,
            value_handle,
            state: AseState::Idle,
            codec_id: LC3_CODEC_ID,
            codec_specific_configuration: Vec::new(),
            cig_id: 0,
            cis_id: 0,
            sdu_interval: 0,
            framing: 0,
            phy: 0,
            max_sdu: 0,
            retransmission_number: 0,
            max_transport_latency: 0,
            presentation_delay: 0,
            metadata: Vec::new(),
        }
    }

    /// Serializes ASE_ID, ASE_State, and the additional parameters ASCS 5 defines per state.
    pub(crate) fn to_bytes(&self) -> Vec<u8> {
        let mut buf = vec![self.ase_id, self.state as u8];
        match self.state {
            AseState::CodecConfigured => {
                buf.push(PREFERRED_FRAMING);
                buf.push(PREFERRED_PHY);
                buf.push(PREFERRED_RETRANSMISSION_NUMBER);
                buf.extend_from_slice(&PREFERRED_MAX_TRANSPORT_LATENCY.to_le_bytes());
                buf.extend_from_slice(&write_u24_le(0)); // supported_presentation_delay_min
                buf.extend_from_slice(&write_u24_le(0)); // supported_presentation_delay_max
                buf.extend_from_slice(&write_u24_le(0)); // preferred_presentation_delay_min
                buf.extend_from_slice(&write_u24_le(0)); // preferred_presentation_delay_max
                buf.extend_from_slice(&self.codec_id);
                buf.push(self.codec_specific_configuration.len() as u8);
                buf.extend_from_slice(&self.codec_specific_configuration);
            }
            AseState::QosConfigured => {
                buf.push(self.cig_id);
                buf.push(self.cis_id);
                buf.extend_from_slice(&write_u24_le(self.sdu_interval));
                buf.push(self.framing);
                buf.push(self.phy);
                buf.extend_from_slice(&self.max_sdu.to_le_bytes());
                buf.push(self.retransmission_number);
                buf.extend_from_slice(&self.max_transport_latency.to_le_bytes());
                buf.extend_from_slice(&write_u24_le(self.presentation_delay));
            }
            AseState::Enabling | AseState::Streaming | AseState::Disabling => {
                buf.push(self.cig_id);
                buf.push(self.cis_id);
                buf.push(self.metadata.len() as u8);
                buf.extend_from_slice(&self.metadata);
            }
            AseState::Idle | AseState::Releasing => {}
        }
        buf
    }

    /// ASCS 5.1 - Config Codec Operation. Valid from Idle, Codec Configured, or QoS
    /// Configured (re-configuring before QoS/Enable is permitted).
    pub(crate) fn on_config_codec(
        &mut self,
        codec_id: [u8; 5],
        codec_specific_configuration: &[u8],
    ) -> (u8, u8) {
        if !matches!(
            self.state,
            AseState::Idle | AseState::CodecConfigured | AseState::QosConfigured
        ) {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.codec_id = codec_id;
        self.codec_specific_configuration = codec_specific_configuration.to_vec();
        self.state = AseState::CodecConfigured;
        (response_code::SUCCESS, reason_code::NONE)
    }

    /// ASCS 5.2 - Config QoS Operation. Valid from Codec Configured or QoS Configured.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn on_config_qos(
        &mut self,
        cig_id: u8,
        cis_id: u8,
        sdu_interval: u32,
        framing: u8,
        phy: u8,
        max_sdu: u16,
        retransmission_number: u8,
        max_transport_latency: u16,
        presentation_delay: u32,
    ) -> (u8, u8) {
        if !matches!(
            self.state,
            AseState::CodecConfigured | AseState::QosConfigured
        ) {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.cig_id = cig_id;
        self.cis_id = cis_id;
        self.sdu_interval = sdu_interval;
        self.framing = framing;
        self.phy = phy;
        self.max_sdu = max_sdu;
        self.retransmission_number = retransmission_number;
        self.max_transport_latency = max_transport_latency;
        self.presentation_delay = presentation_delay;
        self.state = AseState::QosConfigured;
        (response_code::SUCCESS, reason_code::NONE)
    }

    /// ASCS 5.3 - Enable Operation. Valid only from QoS Configured.
    pub(crate) fn on_enable(&mut self, metadata: &[u8]) -> (u8, u8) {
        if self.state != AseState::QosConfigured {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.metadata = metadata.to_vec();
        self.state = AseState::Enabling;
        (response_code::SUCCESS, reason_code::NONE)
    }

    /// ASCS 5.4 - Receiver Start Ready Operation. Valid only from Enabling.
    pub(crate) fn on_receiver_start_ready(&mut self) -> (u8, u8) {
        if self.state != AseState::Enabling {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.state = AseState::Streaming;
        (response_code::SUCCESS, reason_code::NONE)
    }

    /// ASCS 5.5 - Disable Operation. Valid from Enabling or Streaming; a Sink ASE returns
    /// straight to QoS Configured (it has no Receiver Stop Ready step), a Source ASE goes
    /// through Disabling until the client sends Receiver Stop Ready.
    pub(crate) fn on_disable(&mut self) -> (u8, u8) {
        if !matches!(self.state, AseState::Enabling | AseState::Streaming) {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.state = if self.role == AudioRole::Sink {
            AseState::QosConfigured
        } else {
            AseState::Disabling
        };
        (response_code::SUCCESS, reason_code::NONE)
    }

    /// ASCS 5.6 - Receiver Stop Ready Operation. Only meaningful for a Source ASE in
    /// Disabling (ASCS 3.4: Sink ASEs never enter Disabling).
    pub(crate) fn on_receiver_stop_ready(&mut self) -> (u8, u8) {
        if self.role != AudioRole::Source || self.state != AseState::Disabling {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.state = AseState::QosConfigured;
        (response_code::SUCCESS, reason_code::NONE)
    }

    /// ASCS 5.7 - Update Metadata Operation. Valid from Enabling or Streaming; state is
    /// unchanged.
    pub(crate) fn on_update_metadata(&mut self, metadata: &[u8]) -> (u8, u8) {
        if !matches!(self.state, AseState::Enabling | AseState::Streaming) {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.metadata = metadata.to_vec();
        (response_code::SUCCESS, reason_code::NONE)
    }

    /// ASCS 5.8 - Release Operation. Valid from any state but Idle. Real ASCS passes
    /// through Releasing while the CIS (if any) is torn down; Simble has no CIS to tear
    /// down, so Release resolves straight to Idle.
    pub(crate) fn on_release(&mut self) -> (u8, u8) {
        if self.state == AseState::Idle {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.state = AseState::Idle;
        (response_code::SUCCESS, reason_code::NONE)
    }
}

/// The mutable half of ASCS, shared between `AudioStreamControlService` (host-side
/// accessors) and the `AseControlPointHandler` the service registers into the
/// `GattDatabase`.
#[derive(Debug)]
struct AscsState {
    ases: Vec<AudioStreamEndpoint>,
    /// The Control Point response payload produced by the most recent write. ASCS 5
    /// responds via NOTIFICATION rather than the ATT write response, and Simble has no
    /// notification-dispatch mechanism yet, so the pending payload is parked here for
    /// callers (and a future dispatch mechanism) to pick up. It can't live in the Control
    /// Point attribute's stored value: `GattDatabase::write` detaches the handled
    /// attribute for the duration of `on_write` and reinserts it unchanged afterward, so
    /// a `set_value` on that handle from inside the handler wouldn't survive.
    control_point_notification: Vec<u8>,
}

/// Owns writes to the ASE Control Point value attribute (attached via
/// `GattDatabase::set_handler`), so a raw ATT write drives the ASE state machines instead
/// of overwriting the control point's stored bytes.
#[derive(Debug)]
struct AseControlPointHandler {
    state: Arc<Mutex<AscsState>>,
}

impl AttributeHandler for AseControlPointHandler {
    fn on_write(&mut self, db: &mut GattDatabase, value: &[u8]) -> Result<(), u8> {
        self.state.lock().unwrap().apply_control_point(db, value);
        // ASCS 5: the ATT write itself succeeds even when individual ASE operations are
        // rejected - per-ASE outcomes ride the Control Point notification instead.
        Ok(())
    }
}

/// Audio Stream Control Service GATT container plus the ASE state machines it owns.
#[derive(Debug, Clone)]
pub struct AudioStreamControlService {
    /// Attribute handle of the service declaration.
    pub service_handle: u16,
    /// Attribute handle of the Control Point.
    pub control_point_handle: u16,
    /// Value attribute handle of the Control Point characteristic.
    pub control_point_value_handle: u16,
    state: Arc<Mutex<AscsState>>,
}

/// Per-ASE Control Point outcome: `(ASE_ID, Response_Code, Reason)` plus,
/// on success, the index of the ASE in `ases` so the caller can publish its
/// refreshed value without rescanning by ID.
type AseOpResult = (u8, u8, u8, Option<usize>);

fn parse_count(data: &[u8]) -> Option<(u8, &[u8])> {
    let (&count, rest) = data.split_first()?;
    Some((count, rest))
}

impl AudioStreamControlService {
    /// Registers ASCS into a GATT database with one characteristic per requested Sink and
    /// Source ASE ID, plus the shared ASE Control Point.
    pub fn register(db: &mut GattDatabase, sink_ase_ids: &[u8], source_ase_ids: &[u8]) -> Self {
        let service_handle = db.add_service(ascs_uuid::AUDIO_STREAM_CONTROL_SERVICE, true);

        let mut ases = Vec::with_capacity(sink_ase_ids.len() + source_ase_ids.len());
        for &ase_id in sink_ase_ids {
            let (_, value_handle) = db.add_characteristic(
                ascs_uuid::SINK_ASE,
                CharacteristicProperties(
                    CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
                ),
                vec![ase_id, AseState::Idle as u8],
                AttributePermissions::read_only(),
            );
            ases.push(AudioStreamEndpoint::new(
                ase_id,
                AudioRole::Sink,
                value_handle,
            ));
        }
        for &ase_id in source_ase_ids {
            let (_, value_handle) = db.add_characteristic(
                ascs_uuid::SOURCE_ASE,
                CharacteristicProperties(
                    CharacteristicProperties::READ | CharacteristicProperties::NOTIFY,
                ),
                vec![ase_id, AseState::Idle as u8],
                AttributePermissions::read_only(),
            );
            ases.push(AudioStreamEndpoint::new(
                ase_id,
                AudioRole::Source,
                value_handle,
            ));
        }

        let (control_point_handle, control_point_value_handle) = db.add_characteristic(
            ascs_uuid::ASE_CONTROL_POINT,
            CharacteristicProperties(
                CharacteristicProperties::WRITE
                    | CharacteristicProperties::WRITE_WITHOUT_RESPONSE
                    | CharacteristicProperties::NOTIFY,
            ),
            vec![],
            AttributePermissions::write_only(),
        );

        let state = Arc::new(Mutex::new(AscsState {
            ases,
            control_point_notification: Vec::new(),
        }));
        db.set_handler(
            control_point_value_handle,
            Box::new(AseControlPointHandler {
                state: state.clone(),
            }),
        )
        .expect("control point handle was just allocated");

        Self {
            service_handle,
            control_point_handle,
            control_point_value_handle,
            state,
        }
    }

    /// Snapshot of the ASE with `ase_id`, if one was registered.
    pub fn ase(&self, ase_id: u8) -> Option<AudioStreamEndpoint> {
        let state = self.state.lock().unwrap();
        state.ases.iter().find(|ase| ase.ase_id == ase_id).cloned()
    }

    /// The Control Point response payload (`[Opcode, Number_of_ASEs, (ASE_ID,
    /// Response_Code, Reason)...]`) produced by the most recent Control Point write - the
    /// bytes ASCS 5 delivers as a Control Point NOTIFICATION, pending until Simble grows a
    /// notification-dispatch mechanism.
    pub fn control_point_notification(&self) -> Vec<u8> {
        self.state
            .lock()
            .unwrap()
            .control_point_notification
            .clone()
    }

    #[cfg(test)]
    fn set_ase_state(&self, ase_id: u8, new_state: AseState) {
        let mut state = self.state.lock().unwrap();
        let ase = state
            .ases
            .iter_mut()
            .find(|ase| ase.ase_id == ase_id)
            .unwrap();
        ase.state = new_state;
    }

    /// Host-side convenience for driving the ASE Control Point (ASCS 5): routes `data`
    /// through the same `GattDatabase::write` path a remote client's ATT write takes
    /// (dispatching to the handler `register()` attached) and returns the resulting
    /// Control Point notification payload.
    pub fn write_control_point(&mut self, db: &mut GattDatabase, data: &[u8]) -> Vec<u8> {
        let _ = db.write(self.control_point_value_handle, data);
        self.control_point_notification()
    }
}

impl AscsState {
    fn ase_index(&self, ase_id: u8) -> Option<usize> {
        self.ases.iter().position(|ase| ase.ase_id == ase_id)
    }

    /// Applies an ASE Control Point write (ASCS 5): parses the opcode and per-ASE
    /// operation list, drives each named ASE's state machine, publishes the resulting ASE
    /// value(s) into `db`, and parks the Control Point notification payload in
    /// `control_point_notification`.
    fn apply_control_point(&mut self, db: &mut GattDatabase, data: &[u8]) {
        let Some((&op, rest)) = data.split_first() else {
            self.control_point_notification = Vec::new();
            return;
        };

        let responses = match op {
            opcode::CONFIG_CODEC => self.apply_config_codec(rest),
            opcode::CONFIG_QOS => self.apply_config_qos(rest),
            opcode::ENABLE => self.apply_metadata_op(rest, AudioStreamEndpoint::on_enable),
            opcode::RECEIVER_START_READY => {
                self.apply_id_op(rest, AudioStreamEndpoint::on_receiver_start_ready)
            }
            opcode::DISABLE => self.apply_id_op(rest, AudioStreamEndpoint::on_disable),
            opcode::RECEIVER_STOP_READY => {
                self.apply_id_op(rest, AudioStreamEndpoint::on_receiver_stop_ready)
            }
            opcode::UPDATE_METADATA => {
                self.apply_metadata_op(rest, AudioStreamEndpoint::on_update_metadata)
            }
            opcode::RELEASE => self.apply_id_op(rest, AudioStreamEndpoint::on_release),
            _ => vec![(
                0,
                response_code::UNSUPPORTED_OPCODE,
                reason_code::NONE,
                None,
            )],
        };

        for &(_, code, _, index) in &responses {
            if code == response_code::SUCCESS
                && let Some(index) = index
            {
                let ase = &self.ases[index];
                let value = ase.to_bytes();
                let _ = db.set_value(ase.value_handle, &value);
            }
        }

        let mut notification = Vec::with_capacity(2 + responses.len() * 3);
        notification.push(op);
        notification.push(responses.len() as u8);
        for (ase_id, code, reason, _) in &responses {
            notification.extend_from_slice(&[*ase_id, *code, *reason]);
        }
        self.control_point_notification = notification;
    }

    /// Shared by Receiver Start/Stop Ready, Disable, and Release: `[N, ASE_ID(1)...N]`.
    fn apply_id_op(
        &mut self,
        rest: &[u8],
        f: impl Fn(&mut AudioStreamEndpoint) -> (u8, u8),
    ) -> Vec<AseOpResult> {
        let Some((count, mut r)) = parse_count(rest) else {
            return vec![(0, response_code::INVALID_LENGTH, reason_code::NONE, None)];
        };
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let Some((&ase_id, next)) = r.split_first() else {
                out.push((0, response_code::INVALID_LENGTH, reason_code::NONE, None));
                return out;
            };
            r = next;
            out.push(self.dispatch(ase_id, &f));
        }
        out
    }

    /// Shared by Enable and Update Metadata: `[N, (ASE_ID(1), Metadata_Length(1), Metadata)...N]`.
    fn apply_metadata_op(
        &mut self,
        rest: &[u8],
        f: impl Fn(&mut AudioStreamEndpoint, &[u8]) -> (u8, u8),
    ) -> Vec<AseOpResult> {
        let Some((count, mut r)) = parse_count(rest) else {
            return vec![(0, response_code::INVALID_LENGTH, reason_code::NONE, None)];
        };
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let Some((&ase_id, r1)) = r.split_first() else {
                out.push((0, response_code::INVALID_LENGTH, reason_code::NONE, None));
                return out;
            };
            let Some((&metadata_len, r2)) = r1.split_first() else {
                out.push((
                    ase_id,
                    response_code::INVALID_LENGTH,
                    reason_code::NONE,
                    None,
                ));
                return out;
            };
            if r2.len() < metadata_len as usize {
                out.push((
                    ase_id,
                    response_code::INVALID_LENGTH,
                    reason_code::NONE,
                    None,
                ));
                return out;
            }
            let (metadata, next) = r2.split_at(metadata_len as usize);
            r = next;
            out.push(match self.ase_index(ase_id) {
                Some(index) => {
                    let (code, reason) = f(&mut self.ases[index], metadata);
                    (ase_id, code, reason, Some(index))
                }
                None => (
                    ase_id,
                    response_code::INVALID_ASE_ID,
                    reason_code::NONE,
                    None,
                ),
            });
        }
        out
    }

    /// ASCS 5.1 - Config Codec: `[N, (ASE_ID(1), Target_Latency(1), Target_PHY(1),
    /// Codec_ID(5), Codec_Specific_Configuration_Length(1), Codec_Specific_Configuration)...N]`.
    fn apply_config_codec(&mut self, rest: &[u8]) -> Vec<AseOpResult> {
        let Some((count, mut r)) = parse_count(rest) else {
            return vec![(0, response_code::INVALID_LENGTH, reason_code::NONE, None)];
        };
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            if r.len() < 9 {
                out.push((0, response_code::INVALID_LENGTH, reason_code::NONE, None));
                return out;
            }
            let ase_id = r[0];
            let mut codec_id = [0u8; 5];
            codec_id.copy_from_slice(&r[3..8]);
            let csc_len = r[8] as usize;
            if r.len() < 9 + csc_len {
                out.push((
                    ase_id,
                    response_code::INVALID_LENGTH,
                    reason_code::NONE,
                    None,
                ));
                return out;
            }
            let csc = &r[9..9 + csc_len];
            r = &r[9 + csc_len..];
            out.push(match self.ase_index(ase_id) {
                Some(index) => {
                    let (code, reason) = self.ases[index].on_config_codec(codec_id, csc);
                    (ase_id, code, reason, Some(index))
                }
                None => (
                    ase_id,
                    response_code::INVALID_ASE_ID,
                    reason_code::NONE,
                    None,
                ),
            });
        }
        out
    }

    /// ASCS 5.2 - Config QoS: `[N, (ASE_ID(1), CIG_ID(1), CIS_ID(1), SDU_Interval(3),
    /// Framing(1), PHY(1), Max_SDU(2), Retransmission_Number(1), Max_Transport_Latency(2),
    /// Presentation_Delay(3))...N]`.
    fn apply_config_qos(&mut self, rest: &[u8]) -> Vec<AseOpResult> {
        let Some((count, mut r)) = parse_count(rest) else {
            return vec![(0, response_code::INVALID_LENGTH, reason_code::NONE, None)];
        };
        let mut out = Vec::with_capacity(count as usize);
        for _ in 0..count {
            if r.len() < 16 {
                out.push((0, response_code::INVALID_LENGTH, reason_code::NONE, None));
                return out;
            }
            let ase_id = r[0];
            let cig_id = r[1];
            let cis_id = r[2];
            let sdu_interval = read_u24_le(&r[3..]);
            let framing = r[6];
            let phy = r[7];
            let max_sdu = u16::from_le_bytes([r[8], r[9]]);
            let retransmission_number = r[10];
            let max_transport_latency = u16::from_le_bytes([r[11], r[12]]);
            let presentation_delay = read_u24_le(&r[13..]);
            r = &r[16..];
            out.push(match self.ase_index(ase_id) {
                Some(index) => {
                    let (code, reason) = self.ases[index].on_config_qos(
                        cig_id,
                        cis_id,
                        sdu_interval,
                        framing,
                        phy,
                        max_sdu,
                        retransmission_number,
                        max_transport_latency,
                        presentation_delay,
                    );
                    (ase_id, code, reason, Some(index))
                }
                None => (
                    ase_id,
                    response_code::INVALID_ASE_ID,
                    reason_code::NONE,
                    None,
                ),
            });
        }
        out
    }

    fn dispatch(
        &mut self,
        ase_id: u8,
        f: &impl Fn(&mut AudioStreamEndpoint) -> (u8, u8),
    ) -> AseOpResult {
        match self.ase_index(ase_id) {
            Some(index) => {
                let (code, reason) = f(&mut self.ases[index]);
                (ase_id, code, reason, Some(index))
            }
            None => (
                ase_id,
                response_code::INVALID_ASE_ID,
                reason_code::NONE,
                None,
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_codec_pdu(ase_ids: &[u8]) -> Vec<u8> {
        let mut buf = vec![opcode::CONFIG_CODEC, ase_ids.len() as u8];
        for &ase_id in ase_ids {
            buf.push(ase_id);
            buf.push(3); // target_latency
            buf.push(1); // target_phy
            buf.extend_from_slice(&LC3_CODEC_ID);
            buf.push(0); // codec_specific_configuration length
        }
        buf
    }

    #[test]
    fn test_ase_starts_idle() {
        let mut db = GattDatabase::new();
        let ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
        assert_eq!(ascs.ase(1).unwrap().state, AseState::Idle);
        assert_eq!(
            db.read(ascs.ase(1).unwrap().value_handle, 0).unwrap(),
            &[1, 0]
        );
    }

    #[test]
    fn test_full_valid_transition_sequence() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);

        let resp = ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
        assert_eq!(
            resp,
            vec![
                opcode::CONFIG_CODEC,
                1,
                1,
                response_code::SUCCESS,
                reason_code::NONE
            ]
        );
        assert_eq!(ascs.ase(1).unwrap().state, AseState::CodecConfigured);

        let qos_pdu = {
            let mut buf = vec![opcode::CONFIG_QOS, 1, 1, 1, 2];
            buf.extend_from_slice(&[10, 0, 0]); // sdu_interval
            buf.push(0); // framing
            buf.push(1); // phy
            buf.extend_from_slice(&40u16.to_le_bytes());
            buf.push(5); // retransmission_number
            buf.extend_from_slice(&20u16.to_le_bytes());
            buf.extend_from_slice(&[0, 0, 0]); // presentation_delay
            buf
        };
        let resp = ascs.write_control_point(&mut db, &qos_pdu);
        assert_eq!(resp[3], response_code::SUCCESS);
        assert_eq!(ascs.ase(1).unwrap().state, AseState::QosConfigured);

        let enable_pdu = vec![opcode::ENABLE, 1, 1, 0];
        let resp = ascs.write_control_point(&mut db, &enable_pdu);
        assert_eq!(resp[3], response_code::SUCCESS);
        assert_eq!(ascs.ase(1).unwrap().state, AseState::Enabling);

        let start_ready_pdu = vec![opcode::RECEIVER_START_READY, 1, 1];
        let resp = ascs.write_control_point(&mut db, &start_ready_pdu);
        assert_eq!(resp[3], response_code::SUCCESS);
        assert_eq!(ascs.ase(1).unwrap().state, AseState::Streaming);

        let release_pdu = vec![opcode::RELEASE, 1, 1];
        let resp = ascs.write_control_point(&mut db, &release_pdu);
        assert_eq!(resp[3], response_code::SUCCESS);
        assert_eq!(ascs.ase(1).unwrap().state, AseState::Idle);
        assert_eq!(
            db.read(ascs.ase(1).unwrap().value_handle, 0).unwrap(),
            &[1, 0]
        );
    }

    #[test]
    fn test_enable_before_qos_is_rejected() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
        ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));

        let enable_pdu = vec![opcode::ENABLE, 1, 1, 0];
        let resp = ascs.write_control_point(&mut db, &enable_pdu);
        assert_eq!(
            resp,
            vec![
                opcode::ENABLE,
                1,
                1,
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE
            ]
        );
        assert_eq!(ascs.ase(1).unwrap().state, AseState::CodecConfigured);
    }

    #[test]
    fn test_unknown_ase_id_is_rejected() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
        let resp = ascs.write_control_point(&mut db, &config_codec_pdu(&[99]));
        assert_eq!(
            resp,
            vec![
                opcode::CONFIG_CODEC,
                1,
                99,
                response_code::INVALID_ASE_ID,
                reason_code::NONE
            ]
        );
    }

    #[test]
    fn test_unsupported_opcode_is_rejected() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
        let resp = ascs.write_control_point(&mut db, &[0xFF, 0x00]);
        assert_eq!(
            resp,
            vec![
                0xFF,
                1,
                0,
                response_code::UNSUPPORTED_OPCODE,
                reason_code::NONE
            ]
        );
    }

    #[test]
    fn test_truncated_pdu_reports_invalid_length() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
        let resp = ascs.write_control_point(&mut db, &[opcode::CONFIG_CODEC, 1, 1]);
        assert_eq!(
            resp,
            vec![
                opcode::CONFIG_CODEC,
                1,
                0,
                response_code::INVALID_LENGTH,
                reason_code::NONE
            ]
        );
    }

    #[test]
    fn test_sink_disable_returns_to_qos_configured_without_receiver_stop_ready() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
        ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
        ascs.set_ase_state(1, AseState::QosConfigured);
        ascs.write_control_point(&mut db, &[opcode::ENABLE, 1, 1, 0]);

        let resp = ascs.write_control_point(&mut db, &[opcode::DISABLE, 1, 1]);
        assert_eq!(resp[3], response_code::SUCCESS);
        assert_eq!(ascs.ase(1).unwrap().state, AseState::QosConfigured);
    }

    #[test]
    fn test_source_disable_then_receiver_stop_ready() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[], &[2]);
        ascs.write_control_point(&mut db, &config_codec_pdu(&[2]));
        ascs.set_ase_state(2, AseState::QosConfigured);
        ascs.write_control_point(&mut db, &[opcode::ENABLE, 1, 2, 0]);

        let resp = ascs.write_control_point(&mut db, &[opcode::DISABLE, 1, 2]);
        assert_eq!(resp[3], response_code::SUCCESS);
        assert_eq!(ascs.ase(2).unwrap().state, AseState::Disabling);

        let resp = ascs.write_control_point(&mut db, &[opcode::RECEIVER_STOP_READY, 1, 2]);
        assert_eq!(resp[3], response_code::SUCCESS);
        assert_eq!(ascs.ase(2).unwrap().state, AseState::QosConfigured);
    }

    #[test]
    fn test_receiver_stop_ready_on_sink_is_rejected() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
        ascs.write_control_point(&mut db, &config_codec_pdu(&[1]));
        ascs.set_ase_state(1, AseState::QosConfigured);
        ascs.write_control_point(&mut db, &[opcode::ENABLE, 1, 1, 0]);
        ascs.write_control_point(&mut db, &[opcode::DISABLE, 1, 1]);

        let resp = ascs.write_control_point(&mut db, &[opcode::RECEIVER_STOP_READY, 1, 1]);
        assert_eq!(resp[3], response_code::INVALID_ASE_STATE_MACHINE_TRANSITION);
    }

    #[test]
    fn test_release_from_idle_is_rejected() {
        let mut db = GattDatabase::new();
        let mut ascs = AudioStreamControlService::register(&mut db, &[1], &[]);
        let resp = ascs.write_control_point(&mut db, &[opcode::RELEASE, 1, 1]);
        assert_eq!(resp[3], response_code::INVALID_ASE_STATE_MACHINE_TRANSITION);
    }
}
