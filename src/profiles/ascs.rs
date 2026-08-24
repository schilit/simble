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
//! CIS establishment (which Bumble's `AseStateMachine` drives asynchronously off controller
//! events, e.g. auto-transitioning a Sink ASE to Streaming once its CIS is established) has
//! no equivalent here since Simble has no CIS/controller simulation yet;
//! `on_receiver_start_ready` models the client-driven half of that transition synchronously
//! instead.
//!
//! The two *server-initiated* halves of ASCS that do not ride a Control Point write are
//! explicit entry points on [`AudioStreamControlService`] rather than background tasks,
//! because Simble has no event loop to run them on:
//!
//! * [`released`](AudioStreamControlService::released) - the ASCS 5.9 Released operation,
//!   which drains an ASE out of the Releasing state once teardown is done.
//! * [`on_cis_loss`](AudioStreamControlService::on_cis_loss) and
//!   [`on_acl_loss`](AudioStreamControlService::on_acl_loss) - the two ASCS 3.2 link-loss
//!   rules. Nothing in Simble currently detects LE Audio link loss and calls them; they are
//!   the profile-side half of that path, waiting for a caller.

use std::sync::{Arc, Mutex};

use crate::gatt::database::AttributeHandler;
use crate::gatt::{AttributePermissions, CharacteristicProperties, GattDatabase};
use crate::profiles::bap::{LC3_CODEC_ID, read_u24_le, write_u24_le};
use zerocopy::byteorder::little_endian::U16;
use zerocopy::{FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned};

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
// The SIG can add a value to this field; `#[non_exhaustive]` is what stops
// that being a breaking change for every downstream `match`.
#[non_exhaustive]
pub enum AseState {
    /// Idle.
    Idle = 0x00,
    /// Codec configured.
    CodecConfigured = 0x01,
    /// Qos configured.
    QosConfigured = 0x02,
    /// Enabling.
    Enabling = 0x03,
    /// Streaming.
    Streaming = 0x04,
    /// Disabling.
    Disabling = 0x05,
    /// Releasing.
    Releasing = 0x06,
}

/// Direction an Audio Stream Endpoint carries audio in, relative to this device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioRole {
    /// Sink.
    Sink,
    /// Source.
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

    /// ASCS 5.8 - Release Operation. Valid from Codec Configured, QoS Configured, Enabling,
    /// Streaming, or Disabling - i.e. every state but Idle (nothing to release) and
    /// Releasing (a release is already in flight).
    ///
    /// Release does **not** reach Idle by itself: it moves the ASE to Releasing, which is
    /// where it stays while the CIS is torn down. The ASE leaves Releasing only when the
    /// server performs the Released operation ([`Self::on_released`], ASCS 5.9). Bumble
    /// splits the same way - `on_release` sets `RELEASING` and an async task later sets
    /// `IDLE` - so a client sees two ASE notifications, not one.
    pub(crate) fn on_release(&mut self) -> (u8, u8) {
        if matches!(self.state, AseState::Idle | AseState::Releasing) {
            return (
                response_code::INVALID_ASE_STATE_MACHINE_TRANSITION,
                reason_code::NONE,
            );
        }
        self.state = AseState::Releasing;
        (response_code::SUCCESS, reason_code::NONE)
    }

    /// ASCS 5.9 - Released Operation. Server-initiated: it has no Control Point opcode and
    /// therefore no response code, so this reports only whether it applied.
    ///
    /// Valid only from Releasing. The destination is the server's choice: an ASE whose
    /// codec configuration the server caches lands in Codec Configured, otherwise Idle. QoS
    /// parameters and metadata are dropped either way - neither is part of an ASE's value
    /// in Codec Configured or Idle (see [`Self::to_bytes`]), so keeping them would let a
    /// stale CIG/CIS mapping survive a release.
    pub(crate) fn on_released(&mut self, cache_codec_configuration: bool) -> bool {
        if self.state != AseState::Releasing {
            return false;
        }
        self.cig_id = 0;
        self.cis_id = 0;
        self.sdu_interval = 0;
        self.framing = 0;
        self.phy = 0;
        self.max_sdu = 0;
        self.retransmission_number = 0;
        self.max_transport_latency = 0;
        self.presentation_delay = 0;
        self.metadata.clear();
        if cache_codec_configuration {
            self.state = AseState::CodecConfigured;
        } else {
            self.codec_id = LC3_CODEC_ID;
            self.codec_specific_configuration.clear();
            self.state = AseState::Idle;
        }
        true
    }

    /// ASCS 3.2 - loss of the CIS carrying this ASE returns it to QoS Configured. Only the
    /// three states in which a CIS can exist are affected: Enabling (a Source ASE sits here
    /// with an established CIS until the client sends Receiver Start Ready), Streaming, and
    /// Disabling. The QoS parameters survive - the ASE is still QoS-configured, it just has
    /// no CIS - so the client can Enable again without repeating Config QoS.
    ///
    /// Reports whether the state changed.
    pub(crate) fn on_cis_loss(&mut self) -> bool {
        if !matches!(
            self.state,
            AseState::Enabling | AseState::Streaming | AseState::Disabling
        ) {
            return false;
        }
        self.state = AseState::QosConfigured;
        true
    }

    /// ASCS 3.2 - loss of the ACL moves every ASE that is not already Idle to Releasing.
    /// The Released operation ([`Self::on_released`]) then takes it the rest of the way, so
    /// an ACL drop and a client-driven Release converge on the same two-step path.
    ///
    /// Reports whether the state changed.
    pub(crate) fn on_acl_loss(&mut self) -> bool {
        if matches!(self.state, AseState::Idle | AseState::Releasing) {
            return false;
        }
        self.state = AseState::Releasing;
        true
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

/// One fixed 16-byte Config QoS record (ASCS 5.2). `sdu_interval` and
/// `presentation_delay` are 24-bit little-endian values read with
/// [`read_u24_le`].
#[repr(C)]
#[derive(Copy, Clone, Debug, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout)]
struct ConfigQosRecord {
    ase_id: u8,
    cig_id: u8,
    cis_id: u8,
    sdu_interval: [u8; 3],
    framing: u8,
    phy: u8,
    max_sdu: U16,
    retransmission_number: u8,
    max_transport_latency: U16,
    presentation_delay: [u8; 3],
}

impl AudioStreamControlService {
    /// Registers ASCS into a GATT database with one characteristic per requested Sink and
    /// Source ASE ID, plus the shared ASE Control Point.
    pub fn register(db: &mut GattDatabase, sink_ase_ids: &[u8], source_ase_ids: &[u8]) -> Self {
        let service_handle = db.add_service(ascs_uuid::AUDIO_STREAM_CONTROL_SERVICE, true);

        let mut ases = Vec::with_capacity(sink_ase_ids.len() + source_ase_ids.len());
        for &ase_id in sink_ase_ids {
            let (_, value_handle) = db.add_characteristic_with_cccd(
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
            let (_, value_handle) = db.add_characteristic_with_cccd(
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

        let (control_point_handle, control_point_value_handle) = db.add_characteristic_with_cccd(
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

    /// ASCS 5.9 - performs the Released operation on every ASE currently in Releasing,
    /// publishing each one's new value into `db`. Returns the IDs of the ASEs that moved.
    ///
    /// `cache_codec_configuration` picks between the two destinations ASCS 5.9 allows: a
    /// server that keeps the codec configuration lands the ASE in Codec Configured (so the
    /// client can go straight back to Config QoS), one that does not lands it in Idle.
    ///
    /// This is server-initiated and carries no Control Point response, so it leaves
    /// [`control_point_notification`](Self::control_point_notification) alone.
    pub fn released(&mut self, db: &mut GattDatabase, cache_codec_configuration: bool) -> Vec<u8> {
        let mut state = self.state.lock().unwrap();
        let moved: Vec<usize> = (0..state.ases.len())
            .filter(|&i| state.ases[i].on_released(cache_codec_configuration))
            .collect();
        moved.iter().map(|&i| state.publish(db, i)).collect()
    }

    /// ASCS 3.2 - reports loss of the CIS identified by `cig_id`/`cis_id`. Every ASE mapped
    /// to that CIS and in Enabling, Streaming, or Disabling drops back to QoS Configured;
    /// its new value is published into `db`. Returns the IDs of the ASEs that moved.
    ///
    /// Nothing in Simble detects CIS loss yet - there is no CIS to lose. This is the
    /// profile-side half of that path: whatever grows the ability to notice a CIS
    /// disconnection calls this, and ASCS behaves correctly without knowing how it found
    /// out.
    pub fn on_cis_loss(&mut self, db: &mut GattDatabase, cig_id: u8, cis_id: u8) -> Vec<u8> {
        let mut state = self.state.lock().unwrap();
        let moved: Vec<usize> = (0..state.ases.len())
            .filter(|&i| {
                let ase = &mut state.ases[i];
                ase.cig_id == cig_id && ase.cis_id == cis_id && ase.on_cis_loss()
            })
            .collect();
        moved.iter().map(|&i| state.publish(db, i)).collect()
    }

    /// ASCS 3.2 - reports loss of the ACL to the client that owns these ASEs. Every ASE not
    /// already Idle moves to Releasing and has its new value published into `db`; the
    /// server then completes the teardown with [`released`](Self::released). Returns the
    /// IDs of the ASEs that moved.
    ///
    /// Same shape as [`on_cis_loss`](Self::on_cis_loss): the detector does not exist yet,
    /// this is the half that does.
    pub fn on_acl_loss(&mut self, db: &mut GattDatabase) -> Vec<u8> {
        let mut state = self.state.lock().unwrap();
        let moved: Vec<usize> = (0..state.ases.len())
            .filter(|&i| state.ases[i].on_acl_loss())
            .collect();
        moved.iter().map(|&i| state.publish(db, i)).collect()
    }
}

impl AscsState {
    fn ase_index(&self, ase_id: u8) -> Option<usize> {
        self.ases.iter().position(|ase| ase.ase_id == ase_id)
    }

    /// Pushes ASE `index`'s current value into its GATT characteristic - the bytes ASCS
    /// notifies subscribers with on every state change. Returns the ASE's ID so callers
    /// can report which ASEs they touched.
    fn publish(&self, db: &mut GattDatabase, index: usize) -> u8 {
        let ase = &self.ases[index];
        let _ = db.set_value(ase.value_handle, &ase.to_bytes());
        ase.ase_id
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
                self.publish(db, index);
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
            let Ok((rec, next)) = Ref::<_, ConfigQosRecord>::from_prefix(r) else {
                out.push((0, response_code::INVALID_LENGTH, reason_code::NONE, None));
                return out;
            };
            r = next;
            let ase_id = rec.ase_id;
            out.push(match self.ase_index(ase_id) {
                Some(index) => {
                    let (code, reason) = self.ases[index].on_config_qos(
                        rec.cig_id,
                        rec.cis_id,
                        read_u24_le(&rec.sdu_interval),
                        rec.framing,
                        rec.phy,
                        rec.max_sdu.get(),
                        rec.retransmission_number,
                        rec.max_transport_latency.get(),
                        read_u24_le(&rec.presentation_delay),
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
#[path = "ascs_tests.rs"]
mod tests;
