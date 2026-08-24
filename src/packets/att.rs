// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy Bluetooth Attribute Protocol (ATT) Packet Data Units (PDUs).
//!
//! Implements Bluetooth Core Specification Vol 3, Part F (Attribute Protocol)
//! using `zerocopy` for allocation-free parsing and framing.

use core::marker::PhantomData;

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

/// Reinterprets `bytes` as a slice of fixed-size wire items.
///
/// A trailing partial item is dropped rather than misread, which is the same
/// rule `slice::chunks_exact` applies to the hand-rolled walks this replaces.
fn typed_list<T>(bytes: &[u8]) -> &[T]
where
    T: FromBytes + Immutable + KnownLayout + Unaligned,
{
    let whole = bytes.len() - bytes.len() % size_of::<T>();
    <[T]>::ref_from_bytes(&bytes[..whole]).unwrap_or(&[])
}

/// Iterator over the entries of an ATT Attribute Data List.
///
/// The list responses (Find Information, Read By Type, Read By Group Type)
/// pack N entries of one common length into the PDU; that length comes from a
/// header field, not from the entry itself. Each entry is a fixed typed prefix
/// `H` followed by the entry's variable tail (an attribute value or a UUID).
///
/// A trailing partial entry is dropped rather than misread.
#[derive(Debug, Clone)]
pub struct AttDataListIter<'a, H> {
    data: &'a [u8],
    item_len: usize,
    _marker: PhantomData<H>,
}

impl<'a, H> AttDataListIter<'a, H> {
    /// Walks `data` as entries of exactly `item_len` octets each.
    fn new(data: &'a [u8], item_len: usize) -> Self {
        Self {
            data,
            item_len,
            _marker: PhantomData,
        }
    }
}

impl<'a, H> Iterator for AttDataListIter<'a, H>
where
    H: FromBytes + Immutable + KnownLayout + Unaligned,
{
    /// The entry's fixed prefix, then whatever the entry's length leaves over.
    type Item = (Ref<&'a [u8], H>, &'a [u8]);

    fn next(&mut self) -> Option<Self::Item> {
        if self.item_len == 0 || self.data.len() < self.item_len {
            self.data = &[];
            return None;
        }
        let (item, rest) = self.data.split_at(self.item_len);
        self.data = rest;
        Ref::<&'a [u8], H>::from_prefix(item).ok()
    }
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

impl AttFindInformationReq {
    /// Builds a Find Information Request over the given handle range.
    pub fn new(start_handle: u16, end_handle: u16) -> Self {
        Self {
            opcode: opcode::FIND_INFORMATION_REQ,
            start_handle: U16::from_bytes(start_handle.to_le_bytes()),
            end_handle: U16::from_bytes(end_handle.to_le_bytes()),
        }
    }
}
impl_att_parse!(AttFindInformationReq, opcode::FIND_INFORMATION_REQ);

/// ATT Find Information Response Header (followed by the Information Data
/// list: `format` decides whether entries are 4 or 18 octets long).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttFindInformationRspHeader {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Entry format: `FORMAT_UUID16` or `FORMAT_UUID128`.
    pub format: u8,
}

impl AttFindInformationRspHeader {
    /// Handle and 16-bit Bluetooth UUID pairs (4 octets per entry).
    pub const FORMAT_UUID16: u8 = 0x01;
    /// Handle and 128-bit UUID pairs (18 octets per entry).
    pub const FORMAT_UUID128: u8 = 0x02;

    /// Builds a Find Information Response header with the given entry format.
    pub fn new(format: u8) -> Self {
        Self {
            opcode: opcode::FIND_INFORMATION_RSP,
            format,
        }
    }

    /// Length in octets of one Information Data entry, or `None` if `format`
    /// is neither of the two the spec defines (Vol 3, Part F, 3.4.3.2).
    pub fn item_len(&self) -> Option<usize> {
        let uuid_len = match self.format {
            Self::FORMAT_UUID16 => 2,
            Self::FORMAT_UUID128 => 16,
            _ => return None,
        };
        Some(size_of::<AttFindInformationItemHeader>() + uuid_len)
    }

    /// Walks the Information Data list, yielding each entry's handle and the
    /// UUID bytes that follow it. `None` if `format` is unrecognized.
    pub fn items<'a>(
        &self,
        information_data: &'a [u8],
    ) -> Option<AttDataListIter<'a, AttFindInformationItemHeader>> {
        Some(AttDataListIter::new(information_data, self.item_len()?))
    }
}
impl_att_parse!(AttFindInformationRspHeader, opcode::FIND_INFORMATION_RSP);

/// Fixed prefix of one Find Information Response entry: the attribute handle,
/// followed by a 2- or 16-byte UUID as the response's `format` dictates.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttFindInformationItemHeader {
    /// Handle of the attribute this entry describes.
    pub attribute_handle: U16,
}

impl AttFindInformationItemHeader {
    /// Builds an Information Data entry prefix for the given handle.
    pub fn new(attribute_handle: u16) -> Self {
        Self {
            attribute_handle: U16::from_bytes(attribute_handle.to_le_bytes()),
        }
    }
}

/// ATT Find By Type Value Request Header (followed by the attribute value to
/// match against). The attribute type is always a 16-bit UUID here.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttFindByTypeValueReqHeader {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// First handle in the requested range.
    pub start_handle: U16,
    /// Last handle in the requested range.
    pub end_handle: U16,
    /// 16-bit UUID of the attribute type to match.
    pub attribute_type: U16,
}

impl AttFindByTypeValueReqHeader {
    /// Builds a Find By Type Value Request header for a handle range and type.
    pub fn new(start_handle: u16, end_handle: u16, attribute_type: u16) -> Self {
        Self {
            opcode: opcode::FIND_BY_TYPE_VALUE_REQ,
            start_handle: U16::from_bytes(start_handle.to_le_bytes()),
            end_handle: U16::from_bytes(end_handle.to_le_bytes()),
            attribute_type: U16::from_bytes(attribute_type.to_le_bytes()),
        }
    }
}
impl_att_parse!(AttFindByTypeValueReqHeader, opcode::FIND_BY_TYPE_VALUE_REQ);

/// One Handles Information entry of a Find By Type Value Response: the handle
/// that matched and the last handle of the group it heads.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttHandlesInformation {
    /// Handle of the attribute whose value matched.
    pub found_attribute_handle: U16,
    /// Last handle of the group; equal to `found_attribute_handle` when the
    /// matched attribute is not a grouping attribute.
    pub group_end_handle: U16,
}

impl AttHandlesInformation {
    /// Builds a Handles Information entry.
    pub fn new(found_attribute_handle: u16, group_end_handle: u16) -> Self {
        Self {
            found_attribute_handle: U16::from_bytes(found_attribute_handle.to_le_bytes()),
            group_end_handle: U16::from_bytes(group_end_handle.to_le_bytes()),
        }
    }
}

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

/// ATT Read By Type Response Header (followed by the Attribute Data List).
///
/// Every entry in one response is `length` octets long — handle plus value —
/// so a value of a different size ends the list (Vol 3, Part F, 3.4.4.2).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadByTypeRspHeader {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Size in octets of each Attribute Data entry (handle plus value).
    pub length: u8,
}

impl AttReadByTypeRspHeader {
    /// Builds a Read By Type Response header for entries carrying a value of
    /// `value_len` octets.
    pub fn new(value_len: usize) -> Self {
        Self {
            opcode: opcode::READ_BY_TYPE_RSP,
            length: (size_of::<AttReadByTypeItemHeader>() + value_len) as u8,
        }
    }

    /// Walks the Attribute Data List, yielding each entry's handle and value.
    /// `None` if `length` cannot even hold the handle.
    pub fn items<'a>(
        &self,
        attribute_data_list: &'a [u8],
    ) -> Option<AttDataListIter<'a, AttReadByTypeItemHeader>> {
        let item_len = usize::from(self.length);
        if item_len < size_of::<AttReadByTypeItemHeader>() {
            return None;
        }
        Some(AttDataListIter::new(attribute_data_list, item_len))
    }
}
impl_att_parse!(AttReadByTypeRspHeader, opcode::READ_BY_TYPE_RSP);

/// Fixed prefix of one Read By Type Response entry: the attribute handle,
/// followed by `length - 2` octets of attribute value.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadByTypeItemHeader {
    /// Handle of the attribute whose value follows.
    pub attribute_handle: U16,
}

impl AttReadByTypeItemHeader {
    /// Builds an Attribute Data entry prefix for the given handle.
    pub fn new(attribute_handle: u16) -> Self {
        Self {
            attribute_handle: U16::from_bytes(attribute_handle.to_le_bytes()),
        }
    }
}

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

/// ATT Read By Group Type Response Header (followed by the Attribute Data
/// List).
///
/// Every entry in one response is `length` octets long — handle, group end
/// handle, value — so a value of a different size ends the list (Vol 3,
/// Part F, 3.4.4.10).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadByGroupTypeRspHeader {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Size in octets of each Attribute Data entry.
    pub length: u8,
}

impl AttReadByGroupTypeRspHeader {
    /// Builds a Read By Group Type Response header for entries carrying a
    /// value of `value_len` octets.
    pub fn new(value_len: usize) -> Self {
        Self {
            opcode: opcode::READ_BY_GROUP_TYPE_RSP,
            length: (size_of::<AttReadByGroupTypeItemHeader>() + value_len) as u8,
        }
    }

    /// Walks the Attribute Data List, yielding each entry's handle range and
    /// value. `None` if `length` cannot even hold the two handles.
    pub fn items<'a>(
        &self,
        attribute_data_list: &'a [u8],
    ) -> Option<AttDataListIter<'a, AttReadByGroupTypeItemHeader>> {
        let item_len = usize::from(self.length);
        if item_len < size_of::<AttReadByGroupTypeItemHeader>() {
            return None;
        }
        Some(AttDataListIter::new(attribute_data_list, item_len))
    }
}
impl_att_parse!(AttReadByGroupTypeRspHeader, opcode::READ_BY_GROUP_TYPE_RSP);

/// Fixed prefix of one Read By Group Type Response entry: the group's handle
/// range, followed by `length - 4` octets of attribute value.
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttReadByGroupTypeItemHeader {
    /// Handle of the grouping attribute (the group's first handle).
    pub attribute_handle: U16,
    /// Last handle of the group.
    pub end_group_handle: U16,
}

impl AttReadByGroupTypeItemHeader {
    /// Builds an Attribute Data entry prefix for the given group range.
    pub fn new(attribute_handle: u16, end_group_handle: u16) -> Self {
        Self {
            attribute_handle: U16::from_bytes(attribute_handle.to_le_bytes()),
            end_group_handle: U16::from_bytes(end_group_handle.to_le_bytes()),
        }
    }
}

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

/// ATT Prepare Write Response Header (followed by the part value).
///
/// The server echoes the request back verbatim so the client can verify what
/// was queued, so the layout matches [`AttPrepareWriteReqHeader`] exactly —
/// only the opcode differs (Vol 3, Part F, 3.4.6.2).
#[repr(C)]
#[derive(
    Copy, Clone, Debug, PartialEq, Eq, FromBytes, IntoBytes, Unaligned, Immutable, KnownLayout,
)]
pub struct AttPrepareWriteRspHeader {
    /// ATT opcode identifying this PDU.
    pub opcode: u8,
    /// Attribute handle.
    pub handle: U16,
    /// Byte offset into the attribute value.
    pub offset: U16,
}

impl AttPrepareWriteRspHeader {
    /// Builds a Prepare Write Response header for the given handle and offset.
    pub fn new(handle: u16, offset: u16) -> Self {
        Self {
            opcode: opcode::PREPARE_WRITE_RSP,
            handle: U16::from_bytes(handle.to_le_bytes()),
            offset: U16::from_bytes(offset.to_le_bytes()),
        }
    }
}
impl_att_parse!(AttPrepareWriteRspHeader, opcode::PREPARE_WRITE_RSP);

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
    /// Find Information Response (header plus the Information Data list).
    FindInformationRsp {
        /// Fixed header (opcode and entry format).
        header: Ref<&'a [u8], AttFindInformationRspHeader>,
        /// The handle/UUID entries; walk them with
        /// [`AttFindInformationRspHeader::items`].
        information_data: &'a [u8],
    },
    /// Find By Type Value Request (header plus the value to match).
    FindByTypeValueReq {
        /// Fixed request header (opcode, handle range, and 16-bit type UUID).
        header: Ref<&'a [u8], AttFindByTypeValueReqHeader>,
        /// The attribute value to compare against.
        attribute_value: &'a [u8],
    },
    /// Find By Type Value Response: the list of matching handle ranges.
    FindByTypeValueRsp(&'a [AttHandlesInformation]),
    /// Read By Type Request (header plus the attribute-type UUID bytes).
    ReadByTypeReq {
        /// Fixed request header (opcode and handle range).
        header: Ref<&'a [u8], AttReadByTypeReqHeader>,
        /// The 2- or 16-byte attribute-type UUID being searched for.
        uuid_bytes: &'a [u8],
    },
    /// Read By Type Response (header plus the Attribute Data List).
    ReadByTypeRsp {
        /// Fixed header (opcode and per-entry length).
        header: Ref<&'a [u8], AttReadByTypeRspHeader>,
        /// The handle/value entries; walk them with
        /// [`AttReadByTypeRspHeader::items`].
        attribute_data_list: &'a [u8],
    },
    /// Read Request.
    ReadReq(Ref<&'a [u8], AttReadReq>),
    /// Read Response, carrying the attribute value bytes.
    ReadRsp(&'a [u8]),
    /// Read Blob Request.
    ReadBlobReq(Ref<&'a [u8], AttReadBlobReq>),
    /// Read Blob Response, carrying a portion of a long attribute value.
    ReadBlobRsp(&'a [u8]),
    /// Read Multiple Request: the set of handles to read, in order.
    ReadMultipleReq(&'a [U16]),
    /// Read Multiple Response: the requested values concatenated with no
    /// delimiters, so only the client's own handle list can split them.
    ReadMultipleRsp(&'a [u8]),
    /// Read By Group Type Request (header plus the group-type UUID bytes).
    ReadByGroupTypeReq {
        /// Fixed request header (opcode and handle range).
        header: Ref<&'a [u8], AttReadByGroupTypeReqHeader>,
        /// The 2- or 16-byte grouping-attribute-type UUID.
        group_type_bytes: &'a [u8],
    },
    /// Read By Group Type Response (header plus the Attribute Data List).
    ReadByGroupTypeRsp {
        /// Fixed header (opcode and per-entry length).
        header: Ref<&'a [u8], AttReadByGroupTypeRspHeader>,
        /// The handle-range/value entries; walk them with
        /// [`AttReadByGroupTypeRspHeader::items`].
        attribute_data_list: &'a [u8],
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
    /// Prepare Write Response (the server's verbatim echo of the request).
    PrepareWriteRsp {
        /// Fixed header (opcode, handle, and offset).
        header: Ref<&'a [u8], AttPrepareWriteRspHeader>,
        /// The value fragment that was queued at this offset.
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
            opcode::FIND_INFORMATION_RSP => {
                let (h, data) = AttFindInformationRspHeader::parse(bytes)?;
                Some(Self::FindInformationRsp {
                    header: h,
                    information_data: data,
                })
            }
            opcode::FIND_BY_TYPE_VALUE_REQ => {
                let (h, value) = AttFindByTypeValueReqHeader::parse(bytes)?;
                Some(Self::FindByTypeValueReq {
                    header: h,
                    attribute_value: value,
                })
            }
            opcode::FIND_BY_TYPE_VALUE_RSP => {
                Some(Self::FindByTypeValueRsp(typed_list(&bytes[1..])))
            }
            opcode::READ_BY_TYPE_REQ => {
                let (h, uuid) = AttReadByTypeReqHeader::parse(bytes)?;
                Some(Self::ReadByTypeReq {
                    header: h,
                    uuid_bytes: uuid,
                })
            }
            opcode::READ_BY_TYPE_RSP => {
                let (h, list) = AttReadByTypeRspHeader::parse(bytes)?;
                Some(Self::ReadByTypeRsp {
                    header: h,
                    attribute_data_list: list,
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
            opcode::READ_MULTIPLE_REQ => Some(Self::ReadMultipleReq(typed_list(&bytes[1..]))),
            opcode::READ_MULTIPLE_RSP => Some(Self::ReadMultipleRsp(&bytes[1..])),
            opcode::READ_BY_GROUP_TYPE_REQ => {
                let (h, group_type) = AttReadByGroupTypeReqHeader::parse(bytes)?;
                Some(Self::ReadByGroupTypeReq {
                    header: h,
                    group_type_bytes: group_type,
                })
            }
            opcode::READ_BY_GROUP_TYPE_RSP => {
                let (h, list) = AttReadByGroupTypeRspHeader::parse(bytes)?;
                Some(Self::ReadByGroupTypeRsp {
                    header: h,
                    attribute_data_list: list,
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
            opcode::PREPARE_WRITE_RSP => {
                let (h, part) = AttPrepareWriteRspHeader::parse(bytes)?;
                Some(Self::PrepareWriteRsp {
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
#[path = "att_tests.rs"]
mod tests;
