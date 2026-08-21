// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Bluetooth Core Specification 6.0 Channel Sounding (CS) HCI packets.
//!
//! Provides zero-copy data structures for high-accuracy distance measurement (HADM),
//! Phase-Based Ranging (PBR), and Round-Trip Time (RTT) procedures.

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, U16, Unaligned, byteorder::LittleEndian,
};

/// HCI OpCode transparent wrapper.
#[repr(transparent)]
#[derive(
    FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy, PartialEq, Eq,
)]
pub struct OpCode(pub U16<LittleEndian>);

impl OpCode {
    /// Builds this value from a bytes.
    pub const fn from_bytes(bytes: [u8; 2]) -> Self {
        Self(U16::from_bytes(bytes))
    }

    /// Get.
    pub fn get(&self) -> u16 {
        self.0.get()
    }
}

/// Trait for static HCI Command definitions.
pub trait HciCommand {
    /// Op code.
    const OP_CODE: OpCode;
}

/// Channel Sounding OpCodes.
pub mod cs_opcode {
    use super::OpCode;

    /// Le cs read local supported capabilities.
    pub const LE_CS_READ_LOCAL_SUPPORTED_CAPABILITIES: OpCode = OpCode::from_bytes([0x89, 0x20]);
    /// Le cs read remote supported capabilities.
    pub const LE_CS_READ_REMOTE_SUPPORTED_CAPABILITIES: OpCode = OpCode::from_bytes([0x8A, 0x20]);
    /// Le cs security enable.
    pub const LE_CS_SECURITY_ENABLE: OpCode = OpCode::from_bytes([0x8C, 0x20]);
    /// Le cs create config.
    pub const LE_CS_CREATE_CONFIG: OpCode = OpCode::from_bytes([0x90, 0x20]);
    /// Le cs remove config.
    pub const LE_CS_REMOVE_CONFIG: OpCode = OpCode::from_bytes([0x91, 0x20]);
    /// Le cs set procedure parameters.
    pub const LE_CS_SET_PROCEDURE_PARAMETERS: OpCode = OpCode::from_bytes([0x93, 0x20]);
    /// Le cs procedure enable.
    pub const LE_CS_PROCEDURE_ENABLE: OpCode = OpCode::from_bytes([0x94, 0x20]);
}

/// Channel Sounding LE Meta Subevent Codes (Event Code 0x3E).
pub mod cs_subevent_code {
    /// Le cs read remote supported capabilities complete.
    pub const LE_CS_READ_REMOTE_SUPPORTED_CAPABILITIES_COMPLETE: u8 = 0x2C;
    /// Le cs read remote fae table complete.
    pub const LE_CS_READ_REMOTE_FAE_TABLE_COMPLETE: u8 = 0x2D;
    /// Le cs security enable complete.
    pub const LE_CS_SECURITY_ENABLE_COMPLETE: u8 = 0x2E;
    /// Le cs config complete.
    pub const LE_CS_CONFIG_COMPLETE: u8 = 0x2F;
    /// Le cs procedure enable complete.
    pub const LE_CS_PROCEDURE_ENABLE_COMPLETE: u8 = 0x30;
    /// Le cs subevent result.
    pub const LE_CS_SUBEVENT_RESULT: u8 = 0x31;
    /// Le cs subevent result continue.
    pub const LE_CS_SUBEVENT_RESULT_CONTINUE: u8 = 0x32;
    /// Le cs test end complete.
    pub const LE_CS_TEST_END_COMPLETE: u8 = 0x33;
}

/// LE CS Read Local Supported Capabilities Command.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Default, Clone, Copy)]
pub struct LeCsReadLocalSupportedCapabilities {}

impl HciCommand for LeCsReadLocalSupportedCapabilities {
    const OP_CODE: OpCode = cs_opcode::LE_CS_READ_LOCAL_SUPPORTED_CAPABILITIES;
}

/// LE CS Read Remote Supported Capabilities Command.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsReadRemoteSupportedCapabilities {
    pub(crate) connection_handle: U16<LittleEndian>,
}

impl HciCommand for LeCsReadRemoteSupportedCapabilities {
    const OP_CODE: OpCode = cs_opcode::LE_CS_READ_REMOTE_SUPPORTED_CAPABILITIES;
}

/// LE CS Create Config Command.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsCreateConfig {
    /// Connection handle.
    pub connection_handle: U16<LittleEndian>,
    /// Config id.
    pub config_id: u8,
    /// Create context.
    pub create_context: u8,
    /// Main mode type (1: RTT, 2: PBR, 3: RTT + PBR).
    pub main_mode_type: u8, // 1: RTT, 2: PBR, 3: RTT + PBR
    /// Sub mode type.
    pub sub_mode_type: u8,
    /// Min main mode steps.
    pub min_main_mode_steps: u8,
    /// Max main mode steps.
    pub max_main_mode_steps: u8,
    /// Main mode repetition.
    pub main_mode_repetition: u8,
    /// Mode 0 steps.
    pub mode_0_steps: u8,
    /// Role (0: Initiator, 1: Reflector).
    pub role: u8, // 0: Initiator, 1: Reflector
    /// Rtt type.
    pub rtt_type: u8,
    /// Cs sync phy.
    pub cs_sync_phy: u8,
    /// Channel map.
    pub channel_map: [u8; 10],
    /// Channel map repetition.
    pub channel_map_repetition: u8,
    /// Channel selection type.
    pub channel_selection_type: u8,
    /// Ch3c shape.
    pub ch3c_shape: u8,
    /// Ch3c jump.
    pub ch3c_jump: u8,
    /// Companion signal status.
    pub companion_signal_status: u8,
}

impl HciCommand for LeCsCreateConfig {
    const OP_CODE: OpCode = cs_opcode::LE_CS_CREATE_CONFIG;
}

/// LE CS Remove Config Command.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsRemoveConfig {
    pub(crate) connection_handle: U16<LittleEndian>,
    pub(crate) config_id: u8,
}

impl HciCommand for LeCsRemoveConfig {
    const OP_CODE: OpCode = cs_opcode::LE_CS_REMOVE_CONFIG;
}

/// LE CS Procedure Enable Command.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsProcedureEnable {
    /// Connection handle.
    pub connection_handle: U16<LittleEndian>,
    /// Config id.
    pub config_id: u8,
    /// Enable.
    pub enable: u8,
}

impl HciCommand for LeCsProcedureEnable {
    const OP_CODE: OpCode = cs_opcode::LE_CS_PROCEDURE_ENABLE;
}

/// LE CS Config Complete Event (Subevent 0x2F).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsConfigCompleteEvent {
    pub(crate) status: u8,
    pub(crate) connection_handle: U16<LittleEndian>,
    pub(crate) config_id: u8,
    pub(crate) action: u8,
    pub(crate) main_mode_type: u8,
    pub(crate) sub_mode_type: u8,
    pub(crate) min_main_mode_steps: u8,
    pub(crate) max_main_mode_steps: u8,
    pub(crate) main_mode_repetition: u8,
    pub(crate) mode_0_steps: u8,
    pub(crate) role: u8,
    pub(crate) rtt_type: u8,
    pub(crate) cs_sync_phy: u8,
    pub(crate) channel_map: [u8; 10],
    pub(crate) channel_map_repetition: u8,
    pub(crate) channel_selection_type: u8,
    pub(crate) ch3c_shape: u8,
    pub(crate) ch3c_jump: u8,
    pub(crate) companion_signal_status: u8,
}

/// LE CS Subevent Result Header (Subevent 0x31).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsSubeventResultHeader {
    /// Connection handle.
    pub connection_handle: U16<LittleEndian>,
    /// Config id.
    pub config_id: u8,
    /// Start acl conn event.
    pub start_acl_conn_event: U16<LittleEndian>,
    /// Procedure counter.
    pub procedure_counter: U16<LittleEndian>,
    /// Frequency compensation.
    pub frequency_compensation: U16<LittleEndian>,
    /// Reference power level.
    pub reference_power_level: i8,
    /// Procedure done status.
    pub procedure_done_status: u8,
    /// Subevent done status.
    pub subevent_done_status: u8,
    /// Procedure abort reason.
    pub procedure_abort_reason: u8,
    /// Subevent abort reason.
    pub subevent_abort_reason: u8,
    /// Num antenna paths.
    pub num_antenna_paths: u8,
    /// Num steps reported.
    pub num_steps_reported: u8,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cs_command_opcodes() {
        assert_eq!(LeCsReadLocalSupportedCapabilities::OP_CODE.get(), 0x2089);
        assert_eq!(LeCsCreateConfig::OP_CODE.get(), 0x2090);
        assert_eq!(LeCsProcedureEnable::OP_CODE.get(), 0x2094);
    }
}
