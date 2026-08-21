// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! AV/C (Audio/Video Control) frame model: the command/response format that
//! AVRCP (`crate::classic::avrcp`) carries inside AVCTP messages
//! (`crate::classic::avctp`).
//!
//! Wire format (AV/C Digital Interface Command Set General Specification
//! 4.1, section 7): a frame starts with the ctype (command) or response
//! code in the low nibble of byte 0 (the high nibble must be zero), then a
//! byte packing the subunit type (5 bits) and subunit ID (3 bits, with an
//! escape to extension bytes for IDs above 4), the opcode, and
//! opcode-specific operands. Two opcodes get structured views here because
//! AVRCP is built on them: VENDOR DEPENDENT ([`VendorDependent`], which for
//! AVRCP always carries the Bluetooth SIG company ID) and PASS THROUGH
//! ([`PassThrough`], the remote-control key operations from the AV/C Panel
//! Subunit Specification 1.1, section 9.4).

/// The Bluetooth SIG company ID carried in AVRCP vendor-dependent frames
/// (AVRCP spec 6.1).
pub const BLUETOOTH_SIG_COMPANY_ID: u32 = 0x00_1958;

/// Subunit types (AV/C General Specification 4.1, Table 7.4).
pub mod subunit_type {
    pub const MONITOR: u8 = 0x00;
    pub const AUDIO: u8 = 0x01;
    pub const PRINTER: u8 = 0x02;
    pub const DISC: u8 = 0x03;
    pub const TAPE_RECORDER_OR_PLAYER: u8 = 0x04;
    pub const TUNER: u8 = 0x05;
    pub const CA: u8 = 0x06;
    pub const CAMERA: u8 = 0x07;
    pub const PANEL: u8 = 0x09;
    pub const BULLETIN_BOARD: u8 = 0x0A;
    pub const VENDOR_UNIQUE: u8 = 0x1C;
    pub const EXTENDED: u8 = 0x1E;
    pub const UNIT: u8 = 0x1F;
}

/// Operation codes (AV/C General Specification 4.1, section 7.3.1).
pub mod opcode {
    // 0x00 - 0x0F: unit and subunit commands
    pub const VENDOR_DEPENDENT: u8 = 0x00;
    pub const RESERVE: u8 = 0x01;
    pub const PLUG_INFO: u8 = 0x02;

    // 0x10 - 0x3F: unit commands
    pub const DIGITAL_OUTPUT: u8 = 0x10;
    pub const DIGITAL_INPUT: u8 = 0x11;
    pub const CHANNEL_USAGE: u8 = 0x12;
    pub const OUTPUT_PLUG_SIGNAL_FORMAT: u8 = 0x18;
    pub const INPUT_PLUG_SIGNAL_FORMAT: u8 = 0x19;
    pub const GENERAL_BUS_SETUP: u8 = 0x1F;
    pub const CONNECT_AV: u8 = 0x20;
    pub const DISCONNECT_AV: u8 = 0x21;
    pub const CONNECTIONS: u8 = 0x22;
    pub const CONNECT: u8 = 0x24;
    pub const DISCONNECT: u8 = 0x25;
    pub const UNIT_INFO: u8 = 0x30;
    pub const SUBUNIT_INFO: u8 = 0x31;

    // 0x40 - 0x7F: subunit commands
    pub const PASS_THROUGH: u8 = 0x7C;
    pub const GUI_UPDATE: u8 = 0x7D;
    pub const PUSH_GUI_DATA: u8 = 0x7E;
    pub const USER_ACTION: u8 = 0x7F;

    // 0xA0 - 0xBF: unit and subunit commands
    pub const VERSION: u8 = 0xB0;
    pub const POWER: u8 = 0xB2;
}

/// PASS THROUGH operation IDs (AV/C Panel Subunit Specification 1.1,
/// Table 9.21).
pub mod operation_id {
    pub const SELECT: u8 = 0x00;
    pub const UP: u8 = 0x01;
    pub const DOWN: u8 = 0x02;
    pub const LEFT: u8 = 0x03;
    pub const RIGHT: u8 = 0x04;
    pub const RIGHT_UP: u8 = 0x05;
    pub const RIGHT_DOWN: u8 = 0x06;
    pub const LEFT_UP: u8 = 0x07;
    pub const LEFT_DOWN: u8 = 0x08;
    pub const ROOT_MENU: u8 = 0x09;
    pub const SETUP_MENU: u8 = 0x0A;
    pub const CONTENTS_MENU: u8 = 0x0B;
    pub const FAVORITE_MENU: u8 = 0x0C;
    pub const EXIT: u8 = 0x0D;
    pub const NUMBER_0: u8 = 0x20;
    pub const NUMBER_1: u8 = 0x21;
    pub const NUMBER_2: u8 = 0x22;
    pub const NUMBER_3: u8 = 0x23;
    pub const NUMBER_4: u8 = 0x24;
    pub const NUMBER_5: u8 = 0x25;
    pub const NUMBER_6: u8 = 0x26;
    pub const NUMBER_7: u8 = 0x27;
    pub const NUMBER_8: u8 = 0x28;
    pub const NUMBER_9: u8 = 0x29;
    pub const DOT: u8 = 0x2A;
    pub const ENTER: u8 = 0x2B;
    pub const CLEAR: u8 = 0x2C;
    pub const CHANNEL_UP: u8 = 0x30;
    pub const CHANNEL_DOWN: u8 = 0x31;
    pub const PREVIOUS_CHANNEL: u8 = 0x32;
    pub const SOUND_SELECT: u8 = 0x33;
    pub const INPUT_SELECT: u8 = 0x34;
    pub const DISPLAY_INFORMATION: u8 = 0x35;
    pub const HELP: u8 = 0x36;
    pub const PAGE_UP: u8 = 0x37;
    pub const PAGE_DOWN: u8 = 0x38;
    pub const POWER: u8 = 0x40;
    pub const VOLUME_UP: u8 = 0x41;
    pub const VOLUME_DOWN: u8 = 0x42;
    pub const MUTE: u8 = 0x43;
    pub const PLAY: u8 = 0x44;
    pub const STOP: u8 = 0x45;
    pub const PAUSE: u8 = 0x46;
    pub const RECORD: u8 = 0x47;
    pub const REWIND: u8 = 0x48;
    pub const FAST_FORWARD: u8 = 0x49;
    pub const EJECT: u8 = 0x4A;
    pub const FORWARD: u8 = 0x4B;
    pub const BACKWARD: u8 = 0x4C;
    pub const ANGLE: u8 = 0x50;
    pub const SUBPICTURE: u8 = 0x51;
    pub const F1: u8 = 0x71;
    pub const F2: u8 = 0x72;
    pub const F3: u8 = 0x73;
    pub const F4: u8 = 0x74;
    pub const F5: u8 = 0x75;
    pub const VENDOR_UNIQUE: u8 = 0x7E;
}

/// Command types (AV/C General Specification 4.1, Table 7.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandType {
    Control,
    Status,
    SpecificInquiry,
    Notify,
    GeneralInquiry,
}

impl CommandType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x00 => Some(CommandType::Control),
            0x01 => Some(CommandType::Status),
            0x02 => Some(CommandType::SpecificInquiry),
            0x03 => Some(CommandType::Notify),
            0x04 => Some(CommandType::GeneralInquiry),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            CommandType::Control => 0x00,
            CommandType::Status => 0x01,
            CommandType::SpecificInquiry => 0x02,
            CommandType::Notify => 0x03,
            CommandType::GeneralInquiry => 0x04,
        }
    }
}

/// Response codes (AV/C General Specification 4.1, Table 7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResponseCode {
    NotImplemented,
    Accepted,
    Rejected,
    InTransition,
    ImplementedOrStable,
    Changed,
    Interim,
}

impl ResponseCode {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0x08 => Some(ResponseCode::NotImplemented),
            0x09 => Some(ResponseCode::Accepted),
            0x0A => Some(ResponseCode::Rejected),
            0x0B => Some(ResponseCode::InTransition),
            0x0C => Some(ResponseCode::ImplementedOrStable),
            0x0D => Some(ResponseCode::Changed),
            0x0F => Some(ResponseCode::Interim),
            _ => None,
        }
    }

    pub fn as_u8(self) -> u8 {
        match self {
            ResponseCode::NotImplemented => 0x08,
            ResponseCode::Accepted => 0x09,
            ResponseCode::Rejected => 0x0A,
            ResponseCode::InTransition => 0x0B,
            ResponseCode::ImplementedOrStable => 0x0C,
            ResponseCode::Changed => 0x0D,
            ResponseCode::Interim => 0x0F,
        }
    }
}

// ---------------------------------------------------------------------------
// Frames
// ---------------------------------------------------------------------------

/// An AV/C command frame. `subunit_id` is `u16` because the 3-bit wire field
/// escapes to extension bytes for IDs 5 and above (AV/C General
/// Specification 4.1, section 7.3.4), reaching up to 514.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandFrame {
    pub ctype: CommandType,
    pub subunit_type: u8,
    pub subunit_id: u16,
    pub opcode: u8,
    pub operands: Vec<u8>,
}

/// An AV/C response frame; see [`CommandFrame`] for field notes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResponseFrame {
    pub response: ResponseCode,
    pub subunit_type: u8,
    pub subunit_id: u16,
    pub opcode: u8,
    pub operands: Vec<u8>,
}

/// A parsed AV/C frame: the low nibble of byte 0 below 8 is a command type,
/// 8 and above is a response code (AV/C General Specification 4.1, 7.1/7.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    Command(CommandFrame),
    Response(ResponseFrame),
}

/// Parses the subunit byte (and any extension bytes), returning
/// `(subunit_type, subunit_id, opcode offset)`.
fn parse_subunit(data: &[u8]) -> Option<(u8, u16, usize)> {
    let subunit_type = data.get(1)? >> 3;
    if subunit_type == subunit_type::EXTENDED {
        // Extended subunit types are not used by any Bluetooth profile.
        return None;
    }
    let subunit_id = u16::from(data.get(1)? & 0x07);
    match subunit_id {
        0..=4 | 7 => Some((subunit_type, subunit_id, 2)),
        5 => {
            // ID 5 escapes to one extension byte; extension 0xFF escapes to
            // a second one (AV/C General Specification 4.1, section 7.3.4).
            let extension = *data.get(2)?;
            match extension {
                0x00 => None, // reserved
                0xFF => Some((subunit_type, 5 + 254 + u16::from(*data.get(3)?), 4)),
                _ => Some((subunit_type, 5 + u16::from(extension), 3)),
            }
        }
        _ => None, // 6 is reserved
    }
}

impl Frame {
    /// Parses one AV/C frame; `None` on any malformed or reserved encoding.
    pub fn parse(data: &[u8]) -> Option<Frame> {
        let first = *data.first()?;
        if first >> 4 != 0 {
            return None;
        }
        let (subunit_type, subunit_id, opcode_offset) = parse_subunit(data)?;
        let opcode = *data.get(opcode_offset)?;
        let operands = data.get(opcode_offset + 1..)?.to_vec();
        let ctype_or_response = first & 0x0F;
        if ctype_or_response < 8 {
            Some(Frame::Command(CommandFrame {
                ctype: CommandType::from_u8(ctype_or_response)?,
                subunit_type,
                subunit_id,
                opcode,
                operands,
            }))
        } else {
            Some(Frame::Response(ResponseFrame {
                response: ResponseCode::from_u8(ctype_or_response)?,
                subunit_type,
                subunit_id,
                opcode,
                operands,
            }))
        }
    }
}

// Serialization supports only basic subunit IDs (0-7): AVRCP addresses only
// the PANEL subunit with ID 0 and the unit (ID 7), so the extension-byte
// forms never need to be emitted.
fn frame_to_bytes(
    ctype_or_response: u8,
    subunit_type: u8,
    subunit_id: u16,
    opcode: u8,
    operands: &[u8],
) -> Vec<u8> {
    let mut out = vec![
        ctype_or_response,
        (subunit_type << 3) | (subunit_id as u8 & 0x07),
        opcode,
    ];
    out.extend_from_slice(operands);
    out
}

impl CommandFrame {
    pub fn to_bytes(&self) -> Vec<u8> {
        frame_to_bytes(
            self.ctype.as_u8(),
            self.subunit_type,
            self.subunit_id,
            self.opcode,
            &self.operands,
        )
    }

    /// Builds a VENDOR DEPENDENT command frame.
    pub fn vendor_dependent(
        ctype: CommandType,
        subunit_type: u8,
        subunit_id: u16,
        company_id: u32,
        data: Vec<u8>,
    ) -> Self {
        Self {
            ctype,
            subunit_type,
            subunit_id,
            opcode: opcode::VENDOR_DEPENDENT,
            operands: VendorDependent { company_id, data }.to_operands(),
        }
    }

    /// Builds a PASS THROUGH command frame.
    pub fn pass_through(
        ctype: CommandType,
        subunit_type: u8,
        subunit_id: u16,
        pass_through: &PassThrough,
    ) -> Self {
        Self {
            ctype,
            subunit_type,
            subunit_id,
            opcode: opcode::PASS_THROUGH,
            operands: pass_through.to_operands(),
        }
    }

    /// Structured view of the operands when this is a VENDOR DEPENDENT frame.
    pub fn as_vendor_dependent(&self) -> Option<VendorDependent> {
        (self.opcode == opcode::VENDOR_DEPENDENT)
            .then(|| VendorDependent::parse(&self.operands))
            .flatten()
    }

    /// Structured view of the operands when this is a PASS THROUGH frame.
    pub fn as_pass_through(&self) -> Option<PassThrough> {
        (self.opcode == opcode::PASS_THROUGH)
            .then(|| PassThrough::parse(&self.operands))
            .flatten()
    }
}

impl ResponseFrame {
    pub fn to_bytes(&self) -> Vec<u8> {
        frame_to_bytes(
            self.response.as_u8(),
            self.subunit_type,
            self.subunit_id,
            self.opcode,
            &self.operands,
        )
    }

    /// Builds a VENDOR DEPENDENT response frame.
    pub fn vendor_dependent(
        response: ResponseCode,
        subunit_type: u8,
        subunit_id: u16,
        company_id: u32,
        data: Vec<u8>,
    ) -> Self {
        Self {
            response,
            subunit_type,
            subunit_id,
            opcode: opcode::VENDOR_DEPENDENT,
            operands: VendorDependent { company_id, data }.to_operands(),
        }
    }

    /// Builds a PASS THROUGH response frame.
    pub fn pass_through(
        response: ResponseCode,
        subunit_type: u8,
        subunit_id: u16,
        pass_through: &PassThrough,
    ) -> Self {
        Self {
            response,
            subunit_type,
            subunit_id,
            opcode: opcode::PASS_THROUGH,
            operands: pass_through.to_operands(),
        }
    }

    /// Structured view of the operands when this is a VENDOR DEPENDENT frame.
    pub fn as_vendor_dependent(&self) -> Option<VendorDependent> {
        (self.opcode == opcode::VENDOR_DEPENDENT)
            .then(|| VendorDependent::parse(&self.operands))
            .flatten()
    }

    /// Structured view of the operands when this is a PASS THROUGH frame.
    pub fn as_pass_through(&self) -> Option<PassThrough> {
        (self.opcode == opcode::PASS_THROUGH)
            .then(|| PassThrough::parse(&self.operands))
            .flatten()
    }
}

// ---------------------------------------------------------------------------
// Structured operand views
// ---------------------------------------------------------------------------

/// VENDOR DEPENDENT operands: a 24-bit big-endian company ID followed by
/// vendor data (AV/C General Specification 4.1, section 9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendorDependent {
    pub company_id: u32,
    pub data: Vec<u8>,
}

impl VendorDependent {
    pub fn parse(operands: &[u8]) -> Option<Self> {
        let id = operands.get(..3)?;
        Some(Self {
            company_id: u32::from_be_bytes([0, id[0], id[1], id[2]]),
            data: operands[3..].to_vec(),
        })
    }

    pub fn to_operands(&self) -> Vec<u8> {
        let id = self.company_id.to_be_bytes();
        let mut out = vec![id[1], id[2], id[3]];
        out.extend_from_slice(&self.data);
        out
    }
}

/// PASS THROUGH operands: the state flag (bit 7 of the first operand,
/// 0 = pressed, 1 = released), a 7-bit operation ID, and a length-prefixed
/// operation data field (AV/C Panel Subunit Specification 1.1, section 9.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PassThrough {
    pub pressed: bool,
    pub operation_id: u8,
    pub operation_data: Vec<u8>,
}

impl PassThrough {
    pub fn parse(operands: &[u8]) -> Option<Self> {
        let first = *operands.first()?;
        let length = usize::from(*operands.get(1)?);
        Some(Self {
            pressed: first & 0x80 == 0,
            operation_id: first & 0x7F,
            operation_data: operands.get(2..2 + length)?.to_vec(),
        })
    }

    pub fn to_operands(&self) -> Vec<u8> {
        let state_bit = if self.pressed { 0x00 } else { 0x80 };
        let mut out = vec![
            state_bit | (self.operation_id & 0x7F),
            self.operation_data.len() as u8,
        ];
        out.extend_from_slice(&self.operation_data);
        out
    }
}
