// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy Bluetooth Attribute Protocol (ATT) Packet Data Units (PDUs).
//!
//! Implements Bluetooth Core Specification Vol 3, Part F (Attribute Protocol)
//! using `zerocopy` for allocation-free parsing and framing.

use zerocopy::{
    FromBytes, Immutable, IntoBytes, KnownLayout, Ref, Unaligned, byteorder::little_endian::U16,
};

/// ATT OpCodes (Core Spec Vol 3, Part F, Section 3.4.8).
pub mod opcode {
    pub const ERROR_RSP: u8 = 0x01;
    pub const EXCHANGE_MTU_REQ: u8 = 0x02;
    pub const EXCHANGE_MTU_RSP: u8 = 0x03;
    pub const FIND_INFORMATION_REQ: u8 = 0x04;
    pub const FIND_INFORMATION_RSP: u8 = 0x05;
    pub const FIND_BY_TYPE_VALUE_REQ: u8 = 0x06;
    pub const FIND_BY_TYPE_VALUE_RSP: u8 = 0x07;
    pub const READ_BY_TYPE_REQ: u8 = 0x08;
    pub const READ_BY_TYPE_RSP: u8 = 0x09;
    pub const READ_REQ: u8 = 0x0A;
    pub const READ_RSP: u8 = 0x0B;
    pub const READ_BLOB_REQ: u8 = 0x0C;
    pub const READ_BLOB_RSP: u8 = 0x0D;
    pub const READ_MULTIPLE_REQ: u8 = 0x0E;
    pub const READ_MULTIPLE_RSP: u8 = 0x0F;
    pub const READ_BY_GROUP_TYPE_REQ: u8 = 0x10;
    pub const READ_BY_GROUP_TYPE_RSP: u8 = 0x11;
    pub const WRITE_REQ: u8 = 0x12;
    pub const WRITE_RSP: u8 = 0x13;
    pub const WRITE_CMD: u8 = 0x52;
    pub const PREPARE_WRITE_REQ: u8 = 0x16;
    pub const PREPARE_WRITE_RSP: u8 = 0x17;
    pub const EXECUTE_WRITE_REQ: u8 = 0x18;
    pub const EXECUTE_WRITE_RSP: u8 = 0x19;
    pub const HANDLE_VALUE_NTF: u8 = 0x1B;
    pub const HANDLE_VALUE_IND: u8 = 0x1D;
    pub const HANDLE_VALUE_CFM: u8 = 0x1E;
}

/// ATT Error Codes (Core Spec Vol 3, Part F, Section 3.4.1.1).
pub mod error_code {
    pub const INVALID_HANDLE: u8 = 0x01;
    pub const READ_NOT_PERMITTED: u8 = 0x02;
    pub const WRITE_NOT_PERMITTED: u8 = 0x03;
    pub const INVALID_PDU: u8 = 0x04;
    pub const INSUFFICIENT_AUTHENTICATION: u8 = 0x05;
    pub const REQUEST_NOT_SUPPORTED: u8 = 0x06;
    pub const INVALID_OFFSET: u8 = 0x07;
    pub const INSUFFICIENT_AUTHORIZATION: u8 = 0x08;
    pub const PREPARE_QUEUE_FULL: u8 = 0x09;
    pub const ATTRIBUTE_NOT_FOUND: u8 = 0x0A;
    pub const ATTRIBUTE_NOT_LONG: u8 = 0x0B;
    pub const INSUFFICIENT_KEY_SIZE: u8 = 0x0C;
    pub const INVALID_ATTRIBUTE_VALUE_LENGTH: u8 = 0x0D;
    pub const UNLIKELY_ERROR: u8 = 0x0E;
    pub const INSUFFICIENT_ENCRYPTION: u8 = 0x0F;
    pub const UNSUPPORTED_GROUP_TYPE: u8 = 0x10;
    pub const INSUFFICIENT_RESOURCES: u8 = 0x11;
    pub const DATABASE_OUT_OF_SYNC: u8 = 0x12;
    pub const VALUE_NOT_ALLOWED: u8 = 0x13;
}

macro_rules! impl_att_parse {
    ($struct_name:ident, $expected_opcode:expr) => {
        impl $struct_name {
            pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
                let (ref_val, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
                if ref_val.opcode != $expected_opcode {
                    return None;
                }
                Some((ref_val, rest))
            }
        }
    };
    ($struct_name:ident, $op1:expr, $op2:expr) => {
        impl $struct_name {
            pub fn parse(bytes: &[u8]) -> Option<(Ref<&[u8], Self>, &[u8])> {
                let (ref_val, rest) = Ref::<&[u8], Self>::from_prefix(bytes).ok()?;
                if ref_val.opcode != $op1 && ref_val.opcode != $op2 {
                    return None;
                }
                Some((ref_val, rest))
            }
        }
    };
}

/// ATT Error Response PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttErrorRsp {
    pub opcode: u8,
    pub request_opcode: u8,
    pub attribute_handle: U16,
    pub error_code: u8,
}

impl AttErrorRsp {
    pub fn new(request_opcode: u8, handle: u16, error_code: u8) -> Self {
        Self {
            opcode: opcode::ERROR_RSP,
            request_opcode,
            attribute_handle: U16::from_bytes(handle.to_le_bytes()),
            error_code,
        }
    }
}
impl_att_parse!(AttErrorRsp, opcode::ERROR_RSP);

/// ATT Exchange MTU Request PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttExchangeMtuReq {
    pub opcode: u8,
    pub client_rx_mtu: U16,
}

impl AttExchangeMtuReq {
    pub fn new(client_rx_mtu: u16) -> Self {
        Self {
            opcode: opcode::EXCHANGE_MTU_REQ,
            client_rx_mtu: U16::from_bytes(client_rx_mtu.to_le_bytes()),
        }
    }
}
impl_att_parse!(AttExchangeMtuReq, opcode::EXCHANGE_MTU_REQ);

/// ATT Exchange MTU Response PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttExchangeMtuRsp {
    pub opcode: u8,
    pub server_rx_mtu: U16,
}

impl AttExchangeMtuRsp {
    pub fn new(server_rx_mtu: u16) -> Self {
        Self {
            opcode: opcode::EXCHANGE_MTU_RSP,
            server_rx_mtu: U16::from_bytes(server_rx_mtu.to_le_bytes()),
        }
    }
}
impl_att_parse!(AttExchangeMtuRsp, opcode::EXCHANGE_MTU_RSP);

/// ATT Find Information Request PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttFindInformationReq {
    pub opcode: u8,
    pub start_handle: U16,
    pub end_handle: U16,
}
impl_att_parse!(AttFindInformationReq, opcode::FIND_INFORMATION_REQ);

/// ATT Read By Type Request Header (followed by 2 or 16 byte UUID).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadByTypeReqHeader {
    pub opcode: u8,
    pub start_handle: U16,
    pub end_handle: U16,
}
impl_att_parse!(AttReadByTypeReqHeader, opcode::READ_BY_TYPE_REQ);

/// ATT Read By Group Type Request Header (followed by 2 or 16 byte UUID).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadByGroupTypeReqHeader {
    pub opcode: u8,
    pub start_handle: U16,
    pub end_handle: U16,
}
impl_att_parse!(AttReadByGroupTypeReqHeader, opcode::READ_BY_GROUP_TYPE_REQ);

/// ATT Read Request PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadReq {
    pub opcode: u8,
    pub handle: U16,
}
impl_att_parse!(AttReadReq, opcode::READ_REQ);

/// ATT Read Blob Request PDU (for reading long attributes).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadBlobReq {
    pub opcode: u8,
    pub handle: U16,
    pub offset: U16,
}

impl AttReadBlobReq {
    pub fn new(handle: u16, offset: u16) -> Self {
        Self {
            opcode: opcode::READ_BLOB_REQ,
            handle: U16::from_bytes(handle.to_le_bytes()),
            offset: U16::from_bytes(offset.to_le_bytes()),
        }
    }
}
impl_att_parse!(AttReadBlobReq, opcode::READ_BLOB_REQ);

/// ATT Write Request / Command Header (followed by value payload).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttWriteReqHeader {
    pub opcode: u8,
    pub handle: U16,
}

impl AttWriteReqHeader {
    pub fn new(opcode: u8, handle: u16) -> Self {
        Self {
            opcode,
            handle: U16::from_bytes(handle.to_le_bytes()),
        }
    }
}
impl_att_parse!(AttWriteReqHeader, opcode::WRITE_REQ, opcode::WRITE_CMD);

/// ATT Prepare Write Request Header (followed by part value).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttPrepareWriteReqHeader {
    pub opcode: u8,
    pub handle: U16,
    pub offset: U16,
}

impl AttPrepareWriteReqHeader {
    pub fn new(handle: u16, offset: u16) -> Self {
        Self {
            opcode: opcode::PREPARE_WRITE_REQ,
            handle: U16::from_bytes(handle.to_le_bytes()),
            offset: U16::from_bytes(offset.to_le_bytes()),
        }
    }
}
impl_att_parse!(AttPrepareWriteReqHeader, opcode::PREPARE_WRITE_REQ);

/// ATT Execute Write Request PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttExecuteWriteReq {
    pub opcode: u8,
    pub flags: u8,
}

impl AttExecuteWriteReq {
    pub const CANCEL: u8 = 0x00;
    pub const WRITE: u8 = 0x01;

    pub fn new(flags: u8) -> Self {
        Self {
            opcode: opcode::EXECUTE_WRITE_REQ,
            flags,
        }
    }
}
impl_att_parse!(AttExecuteWriteReq, opcode::EXECUTE_WRITE_REQ);

/// ATT Handle Value Notification / Indication Header (followed by value payload).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttHandleValueHeader {
    pub opcode: u8,
    pub handle: U16,
}

impl AttHandleValueHeader {
    pub fn new(opcode: u8, handle: u16) -> Self {
        Self {
            opcode,
            handle: U16::from_bytes(handle.to_le_bytes()),
        }
    }
}
impl_att_parse!(
    AttHandleValueHeader,
    opcode::HANDLE_VALUE_NTF,
    opcode::HANDLE_VALUE_IND
);

/// Complete parsed ATT PDU enum.
#[derive(Debug, PartialEq, Eq)]
pub enum AttPdu<'a> {
    ErrorRsp(Ref<&'a [u8], AttErrorRsp>),
    ExchangeMtuReq(Ref<&'a [u8], AttExchangeMtuReq>),
    ExchangeMtuRsp(Ref<&'a [u8], AttExchangeMtuRsp>),
    FindInformationReq(Ref<&'a [u8], AttFindInformationReq>),
    ReadByTypeReq {
        header: Ref<&'a [u8], AttReadByTypeReqHeader>,
        uuid_bytes: &'a [u8],
    },
    ReadReq(Ref<&'a [u8], AttReadReq>),
    ReadRsp(&'a [u8]),
    ReadBlobReq(Ref<&'a [u8], AttReadBlobReq>),
    ReadBlobRsp(&'a [u8]),
    ReadByGroupTypeReq {
        header: Ref<&'a [u8], AttReadByGroupTypeReqHeader>,
        group_type_bytes: &'a [u8],
    },
    WriteReq {
        header: Ref<&'a [u8], AttWriteReqHeader>,
        value: &'a [u8],
    },
    WriteRsp,
    WriteCmd {
        header: Ref<&'a [u8], AttWriteReqHeader>,
        value: &'a [u8],
    },
    PrepareWriteReq {
        header: Ref<&'a [u8], AttPrepareWriteReqHeader>,
        part_value: &'a [u8],
    },
    ExecuteWriteReq(Ref<&'a [u8], AttExecuteWriteReq>),
    ExecuteWriteRsp,
    HandleValueNotify {
        header: Ref<&'a [u8], AttHandleValueHeader>,
        value: &'a [u8],
    },
    HandleValueInd {
        header: Ref<&'a [u8], AttHandleValueHeader>,
        value: &'a [u8],
    },
    HandleValueCfm,
    Unknown {
        opcode: u8,
        payload: &'a [u8],
    },
}

impl<'a> AttPdu<'a> {
    /// Parses any ATT PDU from a raw byte slice.
    pub fn parse(bytes: &'a [u8]) -> Option<Self> {
        if bytes.is_empty() {
            return None;
        }
        let op = bytes[0];
        match op {
            opcode::ERROR_RSP => {
                let (r, _) = AttErrorRsp::parse(bytes)?;
                Some(Self::ErrorRsp(r))
            }
            opcode::EXCHANGE_MTU_REQ => {
                let (r, _) = AttExchangeMtuReq::parse(bytes)?;
                Some(Self::ExchangeMtuReq(r))
            }
            opcode::EXCHANGE_MTU_RSP => {
                let (r, _) = AttExchangeMtuRsp::parse(bytes)?;
                Some(Self::ExchangeMtuRsp(r))
            }
            opcode::FIND_INFORMATION_REQ => {
                let (r, _) = AttFindInformationReq::parse(bytes)?;
                Some(Self::FindInformationReq(r))
            }
            opcode::READ_BY_TYPE_REQ => {
                let (h, uuid) = AttReadByTypeReqHeader::parse(bytes)?;
                Some(Self::ReadByTypeReq {
                    header: h,
                    uuid_bytes: uuid,
                })
            }
            opcode::READ_REQ => {
                let (r, _) = AttReadReq::parse(bytes)?;
                Some(Self::ReadReq(r))
            }
            opcode::READ_RSP => Some(Self::ReadRsp(&bytes[1..])),
            opcode::READ_BLOB_REQ => {
                let (r, _) = AttReadBlobReq::parse(bytes)?;
                Some(Self::ReadBlobReq(r))
            }
            opcode::READ_BLOB_RSP => Some(Self::ReadBlobRsp(&bytes[1..])),
            opcode::READ_BY_GROUP_TYPE_REQ => {
                let (h, group_type) = AttReadByGroupTypeReqHeader::parse(bytes)?;
                Some(Self::ReadByGroupTypeReq {
                    header: h,
                    group_type_bytes: group_type,
                })
            }
            opcode::WRITE_REQ => {
                let (h, val) = AttWriteReqHeader::parse(bytes)?;
                Some(Self::WriteReq {
                    header: h,
                    value: val,
                })
            }
            opcode::WRITE_RSP => Some(Self::WriteRsp),
            opcode::WRITE_CMD => {
                let (h, val) = AttWriteReqHeader::parse(bytes)?;
                Some(Self::WriteCmd {
                    header: h,
                    value: val,
                })
            }
            opcode::PREPARE_WRITE_REQ => {
                let (h, part) = AttPrepareWriteReqHeader::parse(bytes)?;
                Some(Self::PrepareWriteReq {
                    header: h,
                    part_value: part,
                })
            }
            opcode::EXECUTE_WRITE_REQ => {
                let (r, _) = AttExecuteWriteReq::parse(bytes)?;
                Some(Self::ExecuteWriteReq(r))
            }
            opcode::EXECUTE_WRITE_RSP => Some(Self::ExecuteWriteRsp),
            opcode::HANDLE_VALUE_NTF => {
                let (h, val) = AttHandleValueHeader::parse(bytes)?;
                Some(Self::HandleValueNotify {
                    header: h,
                    value: val,
                })
            }
            opcode::HANDLE_VALUE_IND => {
                let (h, val) = AttHandleValueHeader::parse(bytes)?;
                Some(Self::HandleValueInd {
                    header: h,
                    value: val,
                })
            }
            opcode::HANDLE_VALUE_CFM => Some(Self::HandleValueCfm),
            _ => Some(Self::Unknown {
                opcode: op,
                payload: &bytes[1..],
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zerocopy::IntoBytes;

    #[test]
    fn test_att_pdu_parser() {
        let req = AttExchangeMtuReq::new(512);
        let bytes = req.as_bytes();
        let pdu = AttPdu::parse(bytes).expect("Valid PDU");
        match pdu {
            AttPdu::ExchangeMtuReq(mtu_req) => {
                assert_eq!(mtu_req.client_rx_mtu.get(), 512);
            }
            _ => panic!("Expected ExchangeMtuReq"),
        }
    }

    #[test]
    fn test_read_blob_and_execute_write() {
        let blob_req = AttReadBlobReq::new(0x0004, 22);
        let blob_bytes = blob_req.as_bytes();
        let pdu = AttPdu::parse(blob_bytes).expect("Valid Blob");
        match pdu {
            AttPdu::ReadBlobReq(r) => {
                assert_eq!(r.handle.get(), 0x0004);
                assert_eq!(r.offset.get(), 22);
            }
            _ => panic!("Expected ReadBlobReq"),
        }

        let exec_req = AttExecuteWriteReq::new(AttExecuteWriteReq::WRITE);
        let exec_bytes = exec_req.as_bytes();
        let pdu2 = AttPdu::parse(exec_bytes).expect("Valid Exec");
        match pdu2 {
            AttPdu::ExecuteWriteReq(r) => {
                assert_eq!(r.flags, 1);
            }
            _ => panic!("Expected ExecuteWriteReq"),
        }
    }
}
