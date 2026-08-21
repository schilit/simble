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
    /// Error Response.
    pub const ERROR_RSP: u8 = 0x01;
    /// Exchange MTU Request.
    pub const EXCHANGE_MTU_REQ: u8 = 0x02;
    /// Exchange MTU Response.
    pub const EXCHANGE_MTU_RSP: u8 = 0x03;
    /// Find Information Request.
    pub const FIND_INFORMATION_REQ: u8 = 0x04;
    /// Find Information Response.
    pub const FIND_INFORMATION_RSP: u8 = 0x05;
    /// Find By Type Value Request.
    pub const FIND_BY_TYPE_VALUE_REQ: u8 = 0x06;
    /// Find By Type Value Response.
    pub const FIND_BY_TYPE_VALUE_RSP: u8 = 0x07;
    /// Read By Type Request.
    pub const READ_BY_TYPE_REQ: u8 = 0x08;
    /// Read By Type Response.
    pub const READ_BY_TYPE_RSP: u8 = 0x09;
    /// Read Request.
    pub const READ_REQ: u8 = 0x0A;
    /// Read Response.
    pub const READ_RSP: u8 = 0x0B;
    /// Read Blob Request (long attribute).
    pub const READ_BLOB_REQ: u8 = 0x0C;
    /// Read Blob Response.
    pub const READ_BLOB_RSP: u8 = 0x0D;
    /// Read Multiple Request.
    pub const READ_MULTIPLE_REQ: u8 = 0x0E;
    /// Read Multiple Response.
    pub const READ_MULTIPLE_RSP: u8 = 0x0F;
    /// Read By Group Type Request.
    pub const READ_BY_GROUP_TYPE_REQ: u8 = 0x10;
    /// Read By Group Type Response.
    pub const READ_BY_GROUP_TYPE_RSP: u8 = 0x11;
    /// Write Request.
    pub const WRITE_REQ: u8 = 0x12;
    /// Write Response.
    pub const WRITE_RSP: u8 = 0x13;
    /// Write Command (no response).
    pub const WRITE_CMD: u8 = 0x52;
    /// Prepare Write Request.
    pub const PREPARE_WRITE_REQ: u8 = 0x16;
    /// Prepare Write Response.
    pub const PREPARE_WRITE_RSP: u8 = 0x17;
    /// Execute Write Request.
    pub const EXECUTE_WRITE_REQ: u8 = 0x18;
    /// Execute Write Response.
    pub const EXECUTE_WRITE_RSP: u8 = 0x19;
    /// Handle Value Notification.
    pub const HANDLE_VALUE_NTF: u8 = 0x1B;
    /// Handle Value Indication.
    pub const HANDLE_VALUE_IND: u8 = 0x1D;
    /// Handle Value Confirmation.
    pub const HANDLE_VALUE_CFM: u8 = 0x1E;
}

/// ATT Error Codes (Core Spec Vol 3, Part F, Section 3.4.1.1).
pub mod error_code {
    /// The attribute handle given was not valid.
    pub const INVALID_HANDLE: u8 = 0x01;
    /// The attribute cannot be read.
    pub const READ_NOT_PERMITTED: u8 = 0x02;
    /// The attribute cannot be written.
    pub const WRITE_NOT_PERMITTED: u8 = 0x03;
    /// The attribute PDU was invalid.
    pub const INVALID_PDU: u8 = 0x04;
    /// The attribute requires authentication before it can be read or written.
    pub const INSUFFICIENT_AUTHENTICATION: u8 = 0x05;
    /// The server does not support the request received from the client.
    pub const REQUEST_NOT_SUPPORTED: u8 = 0x06;
    /// Offset specified was past the end of the attribute.
    pub const INVALID_OFFSET: u8 = 0x07;
    /// The attribute requires authorization before it can be read or written.
    pub const INSUFFICIENT_AUTHORIZATION: u8 = 0x08;
    /// Too many prepare writes have been queued.
    pub const PREPARE_QUEUE_FULL: u8 = 0x09;
    /// No attribute found within the given attribute handle range.
    pub const ATTRIBUTE_NOT_FOUND: u8 = 0x0A;
    /// The attribute cannot be read using the Read Blob Request.
    pub const ATTRIBUTE_NOT_LONG: u8 = 0x0B;
    /// The encryption key size used is insufficient.
    pub const INSUFFICIENT_KEY_SIZE: u8 = 0x0C;
    /// The attribute value length is invalid for the operation.
    pub const INVALID_ATTRIBUTE_VALUE_LENGTH: u8 = 0x0D;
    /// The request encountered an unlikely error and could not be completed.
    pub const UNLIKELY_ERROR: u8 = 0x0E;
    /// The attribute requires encryption before it can be read or written.
    pub const INSUFFICIENT_ENCRYPTION: u8 = 0x0F;
    /// The attribute type is not a supported grouping attribute.
    pub const UNSUPPORTED_GROUP_TYPE: u8 = 0x10;
    /// Insufficient resources to complete the request.
    pub const INSUFFICIENT_RESOURCES: u8 = 0x11;
    /// The server database is out of sync with the client.
    pub const DATABASE_OUT_OF_SYNC: u8 = 0x12;
    /// The attribute value is not allowed.
    pub const VALUE_NOT_ALLOWED: u8 = 0x13;
}

macro_rules! impl_att_parse {
    ($struct_name:ident, $expected_opcode:expr) => {
        impl $struct_name {
            /// Parses this PDU from a byte slice, returning the fixed header and
            /// any trailing bytes, or `None` if the opcode does not match.
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
            /// Parses this PDU from a byte slice, returning the fixed header and
            /// any trailing bytes, or `None` if the opcode does not match.
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
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Opcode of the request that generated this error.
    pub request_opcode: u8,
    /// Handle of the attribute that caused the error (0x0000 if none).
    pub attribute_handle: U16,
    /// The reason for the error (see [`error_code`]).
    pub error_code: u8,
}

impl AttErrorRsp {
    /// Builds an Error Response for the given request opcode, handle, and error code.
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
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Client receive MTU size, in bytes.
    pub client_rx_mtu: U16,
}

impl AttExchangeMtuReq {
    /// Builds an Exchange MTU Request advertising the given client RX MTU.
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
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Server receive MTU size, in bytes.
    pub server_rx_mtu: U16,
}

impl AttExchangeMtuRsp {
    /// Builds an Exchange MTU Response advertising the given server RX MTU.
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
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// First handle in the requested range.
    pub start_handle: U16,
    /// Last handle in the requested range.
    pub end_handle: U16,
}
impl_att_parse!(AttFindInformationReq, opcode::FIND_INFORMATION_REQ);

/// ATT Read By Type Request Header (followed by 2 or 16 byte UUID).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadByTypeReqHeader {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// First handle in the requested range.
    pub start_handle: U16,
    /// Last handle in the requested range.
    pub end_handle: U16,
}
impl_att_parse!(AttReadByTypeReqHeader, opcode::READ_BY_TYPE_REQ);

/// ATT Read By Group Type Request Header (followed by 2 or 16 byte UUID).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadByGroupTypeReqHeader {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// First handle in the requested range.
    pub start_handle: U16,
    /// Last handle in the requested range.
    pub end_handle: U16,
}
impl_att_parse!(AttReadByGroupTypeReqHeader, opcode::READ_BY_GROUP_TYPE_REQ);

/// ATT Read Request PDU.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadReq {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Attribute handle.
    pub handle: U16,
}
impl_att_parse!(AttReadReq, opcode::READ_REQ);

/// ATT Read Blob Request PDU (for reading long attributes).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadBlobReq {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Attribute handle.
    pub handle: U16,
    /// Byte offset into the attribute value.
    pub offset: U16,
}

impl AttReadBlobReq {
    /// Builds a Read Blob Request for the given handle and offset.
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
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Attribute handle.
    pub handle: U16,
}

impl AttWriteReqHeader {
    /// Builds a Write Request/Command header with the given opcode and handle.
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
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Attribute handle.
    pub handle: U16,
    /// Byte offset into the attribute value.
    pub offset: U16,
}

impl AttPrepareWriteReqHeader {
    /// Builds a Prepare Write Request header for the given handle and offset.
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
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Execute-write flags (`CANCEL` or `WRITE`).
    pub flags: u8,
}

impl AttExecuteWriteReq {
    /// Cancel all pending prepared writes.
    pub const CANCEL: u8 = 0x00;
    /// Immediately write all pending prepared values.
    pub const WRITE: u8 = 0x01;

    /// Builds an Execute Write Request with the given flags.
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
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Attribute handle.
    pub handle: U16,
}

impl AttHandleValueHeader {
    /// Builds a Handle Value Notification/Indication header.
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
    /// Error Response.
    ErrorRsp(Ref<&'a [u8], AttErrorRsp>),
    /// Exchange MTU Request.
    ExchangeMtuReq(Ref<&'a [u8], AttExchangeMtuReq>),
    /// Exchange MTU Response.
    ExchangeMtuRsp(Ref<&'a [u8], AttExchangeMtuRsp>),
    /// Find Information Request.
    FindInformationReq(Ref<&'a [u8], AttFindInformationReq>),
    /// Read By Type Request (header plus the attribute-type UUID bytes).
    ReadByTypeReq {
        /// Fixed request header (opcode and handle range).
        header: Ref<&'a [u8], AttReadByTypeReqHeader>,
        /// The 2- or 16-byte attribute-type UUID being searched for.
        uuid_bytes: &'a [u8],
    },
    /// Read Request.
    ReadReq(Ref<&'a [u8], AttReadReq>),
    /// Read Response, carrying the attribute value bytes.
    ReadRsp(&'a [u8]),
    /// Read Blob Request.
    ReadBlobReq(Ref<&'a [u8], AttReadBlobReq>),
    /// Read Blob Response, carrying a portion of a long attribute value.
    ReadBlobRsp(&'a [u8]),
    /// Read By Group Type Request (header plus the group-type UUID bytes).
    ReadByGroupTypeReq {
        /// Fixed request header (opcode and handle range).
        header: Ref<&'a [u8], AttReadByGroupTypeReqHeader>,
        /// The 2- or 16-byte grouping-attribute-type UUID.
        group_type_bytes: &'a [u8],
    },
    /// Write Request (header plus the value to write).
    WriteReq {
        /// Fixed request header (opcode and attribute handle).
        header: Ref<&'a [u8], AttWriteReqHeader>,
        /// The attribute value to write.
        value: &'a [u8],
    },
    /// Write Response.
    WriteRsp,
    /// Write Command (unacknowledged write; header plus value).
    WriteCmd {
        /// Fixed command header (opcode and attribute handle).
        header: Ref<&'a [u8], AttWriteReqHeader>,
        /// The attribute value to write.
        value: &'a [u8],
    },
    /// Prepare Write Request (queues part of a long write).
    PrepareWriteReq {
        /// Fixed request header (opcode, handle, and offset).
        header: Ref<&'a [u8], AttPrepareWriteReqHeader>,
        /// The value fragment to queue at this offset.
        part_value: &'a [u8],
    },
    /// Execute Write Request (commit or cancel queued prepared writes).
    ExecuteWriteReq(Ref<&'a [u8], AttExecuteWriteReq>),
    /// Execute Write Response.
    ExecuteWriteRsp,
    /// Handle Value Notification (unacknowledged server-initiated update).
    HandleValueNotify {
        /// Fixed header (opcode and attribute handle).
        header: Ref<&'a [u8], AttHandleValueHeader>,
        /// The notified attribute value.
        value: &'a [u8],
    },
    /// Handle Value Indication (acknowledged server-initiated update).
    HandleValueInd {
        /// Fixed header (opcode and attribute handle).
        header: Ref<&'a [u8], AttHandleValueHeader>,
        /// The indicated attribute value.
        value: &'a [u8],
    },
    /// Handle Value Confirmation (client ack of an indication).
    HandleValueCfm,
    /// An opcode this parser does not recognize.
    Unknown {
        /// The unrecognized opcode byte.
        opcode: u8,
        /// The remaining PDU bytes after the opcode.
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
