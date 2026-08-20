// Copyright 2026 The Android Open Source Project
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
    pub const fn from_bytes(bytes: [u8; 2]) -> Self {
        Self(U16::from_bytes(bytes))
    }

    pub fn get(&self) -> u16 {
        self.0.get()
    }
}

/// Trait for static HCI Command definitions.
pub trait HciCommand {
    const OP_CODE: OpCode;
}

/// Channel Sounding OpCodes.
pub mod cs_opcode {
    use super::OpCode;

    pub const LE_CS_READ_LOCAL_SUPPORTED_CAPABILITIES: OpCode = OpCode::from_bytes([0x89, 0x20]);
    pub const LE_CS_READ_REMOTE_SUPPORTED_CAPABILITIES: OpCode = OpCode::from_bytes([0x8A, 0x20]);
    pub const LE_CS_SECURITY_ENABLE: OpCode = OpCode::from_bytes([0x8C, 0x20]);
    pub const LE_CS_CREATE_CONFIG: OpCode = OpCode::from_bytes([0x90, 0x20]);
    pub const LE_CS_REMOVE_CONFIG: OpCode = OpCode::from_bytes([0x91, 0x20]);
    pub const LE_CS_SET_PROCEDURE_PARAMETERS: OpCode = OpCode::from_bytes([0x93, 0x20]);
    pub const LE_CS_PROCEDURE_ENABLE: OpCode = OpCode::from_bytes([0x94, 0x20]);
}

/// Channel Sounding LE Meta Subevent Codes (Event Code 0x3E).
pub mod cs_subevent_code {
    pub const LE_CS_READ_REMOTE_SUPPORTED_CAPABILITIES_COMPLETE: u8 = 0x2C;
    pub const LE_CS_READ_REMOTE_FAE_TABLE_COMPLETE: u8 = 0x2D;
    pub const LE_CS_SECURITY_ENABLE_COMPLETE: u8 = 0x2E;
    pub const LE_CS_CONFIG_COMPLETE: u8 = 0x2F;
    pub const LE_CS_PROCEDURE_ENABLE_COMPLETE: u8 = 0x30;
    pub const LE_CS_SUBEVENT_RESULT: u8 = 0x31;
    pub const LE_CS_SUBEVENT_RESULT_CONTINUE: u8 = 0x32;
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
    pub connection_handle: U16<LittleEndian>,
}

impl HciCommand for LeCsReadRemoteSupportedCapabilities {
    const OP_CODE: OpCode = cs_opcode::LE_CS_READ_REMOTE_SUPPORTED_CAPABILITIES;
}

/// LE CS Create Config Command.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsCreateConfig {
    pub connection_handle: U16<LittleEndian>,
    pub config_id: u8,
    pub create_context: u8,
    pub main_mode_type: u8, // 1: RTT, 2: PBR, 3: RTT + PBR
    pub sub_mode_type: u8,
    pub min_main_mode_steps: u8,
    pub max_main_mode_steps: u8,
    pub main_mode_repetition: u8,
    pub mode_0_steps: u8,
    pub role: u8, // 0: Initiator, 1: Reflector
    pub rtt_type: u8,
    pub cs_sync_phy: u8,
    pub channel_map: [u8; 10],
    pub channel_map_repetition: u8,
    pub channel_selection_type: u8,
    pub ch3c_shape: u8,
    pub ch3c_jump: u8,
    pub companion_signal_status: u8,
}

impl HciCommand for LeCsCreateConfig {
    const OP_CODE: OpCode = cs_opcode::LE_CS_CREATE_CONFIG;
}

/// LE CS Remove Config Command.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsRemoveConfig {
    pub connection_handle: U16<LittleEndian>,
    pub config_id: u8,
}

impl HciCommand for LeCsRemoveConfig {
    const OP_CODE: OpCode = cs_opcode::LE_CS_REMOVE_CONFIG;
}

/// LE CS Procedure Enable Command.
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsProcedureEnable {
    pub connection_handle: U16<LittleEndian>,
    pub config_id: u8,
    pub enable: u8,
}

impl HciCommand for LeCsProcedureEnable {
    const OP_CODE: OpCode = cs_opcode::LE_CS_PROCEDURE_ENABLE;
}

/// LE CS Config Complete Event (Subevent 0x2F).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsConfigCompleteEvent {
    pub status: u8,
    pub connection_handle: U16<LittleEndian>,
    pub config_id: u8,
    pub action: u8,
    pub main_mode_type: u8,
    pub sub_mode_type: u8,
    pub min_main_mode_steps: u8,
    pub max_main_mode_steps: u8,
    pub main_mode_repetition: u8,
    pub mode_0_steps: u8,
    pub role: u8,
    pub rtt_type: u8,
    pub cs_sync_phy: u8,
    pub channel_map: [u8; 10],
    pub channel_map_repetition: u8,
    pub channel_selection_type: u8,
    pub ch3c_shape: u8,
    pub ch3c_jump: u8,
    pub companion_signal_status: u8,
}

/// LE CS Subevent Result Header (Subevent 0x31).
#[repr(C)]
#[derive(FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout, Debug, Clone, Copy)]
pub struct LeCsSubeventResultHeader {
    pub connection_handle: U16<LittleEndian>,
    pub config_id: u8,
    pub start_acl_conn_event: U16<LittleEndian>,
    pub procedure_counter: U16<LittleEndian>,
    pub frequency_compensation: U16<LittleEndian>,
    pub reference_power_level: i8,
    pub procedure_done_status: u8,
    pub subevent_done_status: u8,
    pub procedure_abort_reason: u8,
    pub subevent_abort_reason: u8,
    pub num_antenna_paths: u8,
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
