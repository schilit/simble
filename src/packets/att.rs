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

    /// Every ATT PDU struct is a wire format: byte-aligned, densely packed, and
    /// exactly as long as the spec says. A struct that silently gained padding
    /// would parse garbage, so pin down both size and alignment.
    #[test]
    fn test_wire_layout_has_no_padding() {
        macro_rules! assert_layout {
            ($t:ty, $size:expr) => {
                assert_eq!(
                    core::mem::size_of::<$t>(),
                    $size,
                    concat!(stringify!($t), " wire size")
                );
                assert_eq!(
                    core::mem::align_of::<$t>(),
                    1,
                    concat!(stringify!($t), " must be unaligned")
                );
            };
        }

        assert_layout!(AttErrorRsp, 5);
        assert_layout!(AttExchangeMtuReq, 3);
        assert_layout!(AttExchangeMtuRsp, 3);
        assert_layout!(AttFindInformationReq, 5);
        assert_layout!(AttReadByTypeReqHeader, 5);
        assert_layout!(AttReadByGroupTypeReqHeader, 5);
        assert_layout!(AttReadReq, 3);
        assert_layout!(AttReadBlobReq, 5);
        assert_layout!(AttWriteReqHeader, 3);
        assert_layout!(AttPrepareWriteReqHeader, 5);
        assert_layout!(AttExecuteWriteReq, 2);
        assert_layout!(AttHandleValueHeader, 3);
    }

    /// Exact on-the-wire bytes for every fixed-size PDU, little-endian per
    /// Core Spec Vol 3, Part F. These are the byte sequences a real peer sees.
    #[test]
    fn test_exact_wire_bytes_round_trip() {
        // Error Response: Read Request on handle 0x0002 failed, Invalid Handle.
        let err = AttErrorRsp::new(opcode::READ_REQ, 0x0002, error_code::INVALID_HANDLE);
        assert_eq!(err.as_bytes(), &[0x01, 0x0A, 0x02, 0x00, 0x01]);

        // Exchange MTU Request, 517 (0x0205) — the LE ATT maximum.
        assert_eq!(
            AttExchangeMtuReq::new(517).as_bytes(),
            &[0x02, 0x05, 0x02],
            "client_rx_mtu must be little-endian"
        );

        // Exchange MTU Response, 23 (0x0017) — the LE ATT default.
        assert_eq!(AttExchangeMtuRsp::new(23).as_bytes(), &[0x03, 0x17, 0x00]);

        // Read Blob Request: handle 0x0004, offset 22 (0x0016).
        assert_eq!(
            AttReadBlobReq::new(0x0004, 22).as_bytes(),
            &[0x0C, 0x04, 0x00, 0x16, 0x00]
        );

        // Write Request header for handle 0x0010.
        assert_eq!(
            AttWriteReqHeader::new(opcode::WRITE_REQ, 0x0010).as_bytes(),
            &[0x12, 0x10, 0x00]
        );

        // Write Command shares the header struct but carries opcode 0x52.
        assert_eq!(
            AttWriteReqHeader::new(opcode::WRITE_CMD, 0x0010).as_bytes(),
            &[0x52, 0x10, 0x00]
        );

        // Prepare Write Request: handle 0x0010 at offset 5.
        assert_eq!(
            AttPrepareWriteReqHeader::new(0x0010, 0x0005).as_bytes(),
            &[0x16, 0x10, 0x00, 0x05, 0x00]
        );

        // Execute Write Request: commit, then cancel.
        assert_eq!(
            AttExecuteWriteReq::new(AttExecuteWriteReq::WRITE).as_bytes(),
            &[0x18, 0x01]
        );
        assert_eq!(
            AttExecuteWriteReq::new(AttExecuteWriteReq::CANCEL).as_bytes(),
            &[0x18, 0x00]
        );

        // Handle Value Notification / Indication header for handle 0x000C.
        assert_eq!(
            AttHandleValueHeader::new(opcode::HANDLE_VALUE_NTF, 0x000C).as_bytes(),
            &[0x1B, 0x0C, 0x00]
        );
        assert_eq!(
            AttHandleValueHeader::new(opcode::HANDLE_VALUE_IND, 0x000C).as_bytes(),
            &[0x1D, 0x0C, 0x00]
        );
    }

    /// Handle-range requests carry two little-endian handles that must not be
    /// transposed; 0x0001..=0xFFFF is the classic "discover everything" range.
    #[test]
    fn test_handle_range_requests_parse_from_wire() {
        let wire = [0x04, 0x01, 0x00, 0xFF, 0xFF];
        let (req, rest) = AttFindInformationReq::parse(&wire).expect("find info req");
        assert_eq!(req.start_handle.get(), 0x0001);
        assert_eq!(req.end_handle.get(), 0xFFFF);
        assert!(rest.is_empty());
        assert_eq!(req.as_bytes(), &wire);

        // Read By Type Request for the Characteristic declaration UUID (0x2803).
        let wire = [0x08, 0x01, 0x00, 0xFF, 0xFF, 0x03, 0x28];
        let (header, uuid) = AttReadByTypeReqHeader::parse(&wire).expect("read by type req");
        assert_eq!(header.start_handle.get(), 0x0001);
        assert_eq!(header.end_handle.get(), 0xFFFF);
        assert_eq!(uuid, &[0x03, 0x28]);

        // Read By Group Type Request for the Primary Service UUID (0x2800).
        let wire = [0x10, 0x01, 0x00, 0xFF, 0xFF, 0x00, 0x28];
        let (header, group) = AttReadByGroupTypeReqHeader::parse(&wire).expect("group type req");
        assert_eq!(header.start_handle.get(), 0x0001);
        assert_eq!(header.end_handle.get(), 0xFFFF);
        assert_eq!(group, &[0x00, 0x28]);

        // The same request with a 128-bit vendor UUID: the trailing slice is
        // whatever remains, so a 16-byte UUID falls out without a length field.
        let mut wire = vec![0x08, 0x01, 0x00, 0x0F, 0x00];
        let uuid128 = [
            0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x0D, 0x18,
            0x00, 0x00,
        ];
        wire.extend_from_slice(&uuid128);
        let (header, uuid) = AttReadByTypeReqHeader::parse(&wire).expect("128-bit uuid req");
        assert_eq!(header.end_handle.get(), 0x000F);
        assert_eq!(uuid, &uuid128);
    }

    /// Variable-length PDUs must split cleanly into fixed header plus trailing
    /// value, with the value handed back verbatim including empty and 0x00 bytes.
    #[test]
    fn test_variable_length_pdus_split_header_and_value() {
        // Write Request enabling notifications on a CCCD (value 0x0001).
        let wire = [0x12, 0x10, 0x00, 0x01, 0x00];
        match AttPdu::parse(&wire).expect("write req") {
            AttPdu::WriteReq { header, value } => {
                assert_eq!(header.handle.get(), 0x0010);
                assert_eq!(value, &[0x01, 0x00]);
            }
            other => panic!("Expected WriteReq, got {other:?}"),
        }

        // Notification carrying a 3-byte Heart Rate Measurement value.
        let wire = [0x1B, 0x0C, 0x00, 0x06, 0x4B, 0x00];
        match AttPdu::parse(&wire).expect("notification") {
            AttPdu::HandleValueNotify { header, value } => {
                assert_eq!(header.handle.get(), 0x000C);
                assert_eq!(value, &[0x06, 0x4B, 0x00]);
            }
            other => panic!("Expected HandleValueNotify, got {other:?}"),
        }

        // Indication uses the same header struct, different opcode.
        let wire = [0x1D, 0x0C, 0x00, 0xAA];
        match AttPdu::parse(&wire).expect("indication") {
            AttPdu::HandleValueInd { header, value } => {
                assert_eq!(header.handle.get(), 0x000C);
                assert_eq!(value, &[0xAA]);
            }
            other => panic!("Expected HandleValueInd, got {other:?}"),
        }

        // A zero-length write is legal: the trailing slice is simply empty.
        let wire = [0x12, 0x10, 0x00];
        match AttPdu::parse(&wire).expect("empty write") {
            AttPdu::WriteReq { header, value } => {
                assert_eq!(header.handle.get(), 0x0010);
                assert!(value.is_empty());
            }
            other => panic!("Expected WriteReq, got {other:?}"),
        }

        // Prepare Write queues a fragment at an offset.
        let wire = [0x16, 0x10, 0x00, 0x05, 0x00, 0xDE, 0xAD];
        match AttPdu::parse(&wire).expect("prepare write") {
            AttPdu::PrepareWriteReq { header, part_value } => {
                assert_eq!(header.handle.get(), 0x0010);
                assert_eq!(header.offset.get(), 5);
                assert_eq!(part_value, &[0xDE, 0xAD]);
            }
            other => panic!("Expected PrepareWriteReq, got {other:?}"),
        }

        // Read Response and Read Blob Response are bare value payloads.
        assert_eq!(
            AttPdu::parse(&[0x0B, 0x01, 0x02, 0x03]),
            Some(AttPdu::ReadRsp(&[0x01, 0x02, 0x03]))
        );
        assert_eq!(
            AttPdu::parse(&[0x0D, 0xFF]),
            Some(AttPdu::ReadBlobRsp(&[0xFF]))
        );

        // Bare, single-byte PDUs carry no payload at all.
        assert_eq!(AttPdu::parse(&[0x13]), Some(AttPdu::WriteRsp));
        assert_eq!(AttPdu::parse(&[0x19]), Some(AttPdu::ExecuteWriteRsp));
        assert_eq!(AttPdu::parse(&[0x1E]), Some(AttPdu::HandleValueCfm));
    }

    /// Truncated PDUs must be rejected rather than read past the buffer, and a
    /// header's `parse` must refuse a PDU belonging to a different opcode.
    #[test]
    fn test_rejects_truncated_and_mismatched_pdus() {
        // Empty input is never a PDU.
        assert_eq!(AttPdu::parse(&[]), None);

        // One byte short of the fixed header in each case.
        assert!(AttErrorRsp::parse(&[0x01, 0x0A, 0x02, 0x00]).is_none());
        assert!(AttExchangeMtuReq::parse(&[0x02, 0x05]).is_none());
        assert!(AttReadBlobReq::parse(&[0x0C, 0x04, 0x00, 0x16]).is_none());
        assert!(AttReadReq::parse(&[0x0A, 0x03]).is_none());
        assert!(AttExecuteWriteReq::parse(&[0x18]).is_none());
        assert!(AttFindInformationReq::parse(&[0x04, 0x01, 0x00, 0xFF]).is_none());

        // Truncation surfaces through the top-level parser as None, not a panic.
        assert_eq!(AttPdu::parse(&[0x02, 0x05]), None);
        assert_eq!(AttPdu::parse(&[0x1B, 0x0C]), None);

        // Opcode mismatch: a Read Request is not a Read Blob Request, even
        // though it is long enough to be read as one.
        assert!(AttReadBlobReq::parse(&[0x0A, 0x03, 0x00, 0x00, 0x00]).is_none());
        assert!(AttErrorRsp::parse(&[0x02, 0x05, 0x02, 0x00, 0x00]).is_none());

        // The two-opcode headers accept either opcode and nothing else.
        assert!(AttWriteReqHeader::parse(&[0x12, 0x10, 0x00]).is_some());
        assert!(AttWriteReqHeader::parse(&[0x52, 0x10, 0x00]).is_some());
        assert!(AttWriteReqHeader::parse(&[0x13, 0x10, 0x00]).is_none());
        assert!(AttHandleValueHeader::parse(&[0x1B, 0x0C, 0x00]).is_some());
        assert!(AttHandleValueHeader::parse(&[0x1D, 0x0C, 0x00]).is_some());
        assert!(AttHandleValueHeader::parse(&[0x1E, 0x0C, 0x00]).is_none());
    }

    /// An opcode outside the spec's table still falls through to `Unknown`
    /// with the payload intact rather than being silently dropped.
    #[test]
    fn test_unhandled_opcodes_fall_through_to_unknown() {
        // 0x14 is not an assigned ATT opcode.
        let wire = [0x14, 0xDE, 0xAD];
        assert_eq!(
            AttPdu::parse(&wire),
            Some(AttPdu::Unknown {
                opcode: 0x14,
                payload: &wire[1..],
            })
        );

        // A single-byte unknown opcode yields an empty payload, not a panic.
        assert_eq!(
            AttPdu::parse(&[0xFF]),
            Some(AttPdu::Unknown {
                opcode: 0xFF,
                payload: &[],
            })
        );
    }

    /// The list-bearing response PDUs are wire formats too: byte-aligned,
    /// densely packed headers and entries with no padding to misalign a list.
    #[test]
    fn test_response_wire_layout_has_no_padding() {
        macro_rules! assert_layout {
            ($t:ty, $size:expr) => {
                assert_eq!(
                    core::mem::size_of::<$t>(),
                    $size,
                    concat!(stringify!($t), " wire size")
                );
                assert_eq!(
                    core::mem::align_of::<$t>(),
                    1,
                    concat!(stringify!($t), " must be unaligned")
                );
            };
        }

        assert_layout!(AttFindInformationRspHeader, 2);
        assert_layout!(AttFindInformationItemHeader, 2);
        assert_layout!(AttFindByTypeValueReqHeader, 7);
        assert_layout!(AttHandlesInformation, 4);
        assert_layout!(AttReadByTypeRspHeader, 2);
        assert_layout!(AttReadByTypeItemHeader, 2);
        assert_layout!(AttReadByGroupTypeRspHeader, 2);
        assert_layout!(AttReadByGroupTypeItemHeader, 4);
        assert_layout!(AttPrepareWriteRspHeader, 5);
    }

    /// Exact on-the-wire bytes for the response headers and list entries.
    #[test]
    fn test_response_exact_wire_bytes() {
        // Find Information Response header, 16-bit UUID format.
        assert_eq!(
            AttFindInformationRspHeader::new(AttFindInformationRspHeader::FORMAT_UUID16).as_bytes(),
            &[0x05, 0x01]
        );
        assert_eq!(
            AttFindInformationItemHeader::new(0x0011).as_bytes(),
            &[0x11, 0x00]
        );

        // Find By Type Value Request: 0x0001..=0xFFFF, Primary Service (0x2800).
        assert_eq!(
            AttFindByTypeValueReqHeader::new(0x0001, 0xFFFF, 0x2800).as_bytes(),
            &[0x06, 0x01, 0x00, 0xFF, 0xFF, 0x00, 0x28]
        );

        // Handles Information: the group 0x0001..=0x0009.
        assert_eq!(
            AttHandlesInformation::new(0x0001, 0x0009).as_bytes(),
            &[0x01, 0x00, 0x09, 0x00]
        );

        // Read By Type Response for a characteristic declaration with a
        // 16-bit UUID: 2 (handle) + 5 (properties, value handle, UUID) = 7.
        assert_eq!(AttReadByTypeRspHeader::new(5).as_bytes(), &[0x09, 0x07]);
        assert_eq!(
            AttReadByTypeItemHeader::new(0x0002).as_bytes(),
            &[0x02, 0x00]
        );

        // Read By Group Type Response for a 16-bit service UUID: 4 + 2 = 6.
        assert_eq!(
            AttReadByGroupTypeRspHeader::new(2).as_bytes(),
            &[0x11, 0x06]
        );
        assert_eq!(
            AttReadByGroupTypeItemHeader::new(0x0001, 0x0009).as_bytes(),
            &[0x01, 0x00, 0x09, 0x00]
        );

        // Prepare Write Response echoes the request: handle 0x0010, offset 5.
        assert_eq!(
            AttPrepareWriteRspHeader::new(0x0010, 0x0005).as_bytes(),
            &[0x17, 0x10, 0x00, 0x05, 0x00]
        );
    }

    /// The Read By Group Type Response bytes asserted in
    /// `client::gatt_client` and `tests/device_test.rs`: a Heart Rate Service
    /// (0x180D) occupying handles 0x0001..=0x0009, entries 6 octets each.
    #[test]
    fn test_read_by_group_type_rsp_walks_service_list() {
        let wire = [0x11, 0x06, 0x01, 0x00, 0x09, 0x00, 0x0D, 0x18];
        let AttPdu::ReadByGroupTypeRsp {
            header,
            attribute_data_list,
        } = AttPdu::parse(&wire).expect("group type rsp")
        else {
            panic!("Expected ReadByGroupTypeRsp");
        };
        assert_eq!(header.length, 6);

        let entries: Vec<_> = header.items(attribute_data_list).expect("6 >= 4").collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.attribute_handle.get(), 0x0001);
        assert_eq!(entries[0].0.end_group_handle.get(), 0x0009);
        assert_eq!(entries[0].1, &[0x0D, 0x18]);

        // Two entries, and a trailing partial third that must be dropped
        // rather than read past the end of the buffer.
        let wire = [
            0x11, 0x06, 0x01, 0x00, 0x09, 0x00, 0x0D, 0x18, 0x0A, 0x00, 0x0D, 0x00, 0x0F, 0x18,
            0x0E, 0x00,
        ];
        let (header, list) = AttReadByGroupTypeRspHeader::parse(&wire).expect("group type rsp");
        let entries: Vec<_> = header.items(list).expect("6 >= 4").collect();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[1].0.attribute_handle.get(), 0x000A);
        assert_eq!(entries[1].1, &[0x0F, 0x18]);

        // A length that cannot hold the group's two handles is not walkable.
        let (header, list) = AttReadByGroupTypeRspHeader::parse(&[0x11, 0x03, 0x01, 0x00, 0x09])
            .expect("parses; length is nonsense");
        assert!(header.items(list).is_none());
    }

    /// The Read By Type Response bytes asserted in `client::gatt_client`: one
    /// Heart Rate Measurement characteristic (0x2A37), notify-only, declared
    /// at handle 0x0002 with its value at 0x0003. Entries are 7 octets.
    #[test]
    fn test_read_by_type_rsp_walks_characteristic_list() {
        let wire = [0x09, 0x07, 0x02, 0x00, 0x10, 0x03, 0x00, 0x37, 0x2A];
        let AttPdu::ReadByTypeRsp {
            header,
            attribute_data_list,
        } = AttPdu::parse(&wire).expect("read by type rsp")
        else {
            panic!("Expected ReadByTypeRsp");
        };
        assert_eq!(header.length, 7);

        let entries: Vec<_> = header.items(attribute_data_list).expect("7 >= 2").collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.attribute_handle.get(), 0x0002);
        // Declaration value: properties, value handle, characteristic UUID.
        assert_eq!(entries[0].1, &[0x10, 0x03, 0x00, 0x37, 0x2A]);
    }

    /// Find Information Response: `format` alone decides the entry width, so
    /// both widths must walk and a bogus format must refuse to walk at all.
    #[test]
    fn test_find_information_rsp_walks_both_formats() {
        // Format 0x01: handle 0x0011 -> CCCD (0x2902), 4-octet entries.
        let wire = [0x05, 0x01, 0x11, 0x00, 0x02, 0x29];
        let AttPdu::FindInformationRsp {
            header,
            information_data,
        } = AttPdu::parse(&wire).expect("find info rsp")
        else {
            panic!("Expected FindInformationRsp");
        };
        assert_eq!(header.item_len(), Some(4));
        let entries: Vec<_> = header
            .items(information_data)
            .expect("known format")
            .collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0.attribute_handle.get(), 0x0011);
        assert_eq!(entries[0].1, &[0x02, 0x29]);

        // Format 0x02: 18-octet entries carrying a 128-bit UUID.
        let uuid128 = [
            0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x02, 0x29,
            0x00, 0x00,
        ];
        let mut wire = vec![0x05, 0x02, 0x11, 0x00];
        wire.extend_from_slice(&uuid128);
        let (header, data) = AttFindInformationRspHeader::parse(&wire).expect("find info rsp");
        assert_eq!(header.item_len(), Some(18));
        let entries: Vec<_> = header.items(data).expect("known format").collect();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1, &uuid128);

        // Format 0x03 does not exist; there is no way to know the entry width.
        let (header, data) = AttFindInformationRspHeader::parse(&[0x05, 0x03, 0x11, 0x00])
            .expect("parses; format is nonsense");
        assert_eq!(header.item_len(), None);
        assert!(header.items(data).is_none());
    }

    /// The remaining response/request PDUs that used to fall through to
    /// `Unknown`, each with the bytes a real peer would send.
    #[test]
    fn test_remaining_list_pdus_parse_from_wire() {
        // Find By Type Value Request: "which primary service is 0x180D?"
        let wire = [0x06, 0x01, 0x00, 0xFF, 0xFF, 0x00, 0x28, 0x0D, 0x18];
        match AttPdu::parse(&wire).expect("find by type value req") {
            AttPdu::FindByTypeValueReq {
                header,
                attribute_value,
            } => {
                assert_eq!(header.start_handle.get(), 0x0001);
                assert_eq!(header.end_handle.get(), 0xFFFF);
                assert_eq!(header.attribute_type.get(), 0x2800);
                assert_eq!(attribute_value, &[0x0D, 0x18]);
            }
            other => panic!("Expected FindByTypeValueReq, got {other:?}"),
        }

        // Find By Type Value Response: two matching groups.
        let wire = [0x07, 0x01, 0x00, 0x09, 0x00, 0x0A, 0x00, 0x0D, 0x00];
        match AttPdu::parse(&wire).expect("find by type value rsp") {
            AttPdu::FindByTypeValueRsp(handles) => {
                assert_eq!(
                    handles,
                    &[
                        AttHandlesInformation::new(0x0001, 0x0009),
                        AttHandlesInformation::new(0x000A, 0x000D),
                    ]
                );
            }
            other => panic!("Expected FindByTypeValueRsp, got {other:?}"),
        }

        // A trailing partial entry is dropped, not misread.
        let wire = [0x07, 0x01, 0x00, 0x09, 0x00, 0x0A];
        assert_eq!(
            AttPdu::parse(&wire),
            Some(AttPdu::FindByTypeValueRsp(&[AttHandlesInformation::new(
                0x0001, 0x0009
            )]))
        );

        // Read Multiple Request: handles 0x0003 and 0x0005.
        let wire = [0x0E, 0x03, 0x00, 0x05, 0x00];
        match AttPdu::parse(&wire).expect("read multiple req") {
            AttPdu::ReadMultipleReq(handles) => {
                assert_eq!(handles.len(), 2);
                assert_eq!(handles[0].get(), 0x0003);
                assert_eq!(handles[1].get(), 0x0005);
            }
            other => panic!("Expected ReadMultipleReq, got {other:?}"),
        }

        // Read Multiple Response is an undelimited concatenation of values.
        assert_eq!(
            AttPdu::parse(&[0x0F, 0x06, 0x4B, 0x00, 0x01, 0x00]),
            Some(AttPdu::ReadMultipleRsp(&[0x06, 0x4B, 0x00, 0x01, 0x00]))
        );

        // Prepare Write Response: the server's echo of handle 0x0010 at
        // offset 5 with the queued fragment.
        let wire = [0x17, 0x10, 0x00, 0x05, 0x00, 0xDE, 0xAD];
        match AttPdu::parse(&wire).expect("prepare write rsp") {
            AttPdu::PrepareWriteRsp { header, part_value } => {
                assert_eq!(header.handle.get(), 0x0010);
                assert_eq!(header.offset.get(), 5);
                assert_eq!(part_value, &[0xDE, 0xAD]);
            }
            other => panic!("Expected PrepareWriteRsp, got {other:?}"),
        }
    }

    /// A response whose fixed header is truncated is rejected, the same way
    /// the request PDUs are — a bare opcode is not a parsable list response.
    #[test]
    fn test_truncated_list_responses_are_rejected() {
        assert_eq!(AttPdu::parse(&[opcode::READ_BY_GROUP_TYPE_RSP]), None);
        assert_eq!(AttPdu::parse(&[opcode::READ_BY_TYPE_RSP]), None);
        assert_eq!(AttPdu::parse(&[opcode::FIND_INFORMATION_RSP]), None);
        assert_eq!(AttPdu::parse(&[0x06, 0x01, 0x00, 0xFF, 0xFF, 0x00]), None);
        assert_eq!(AttPdu::parse(&[0x17, 0x10, 0x00, 0x05]), None);

        // A Prepare Write Response is not a Prepare Write Request and vice
        // versa, even though the two layouts are identical.
        let rsp = [0x17, 0x10, 0x00, 0x05, 0x00];
        assert!(AttPrepareWriteReqHeader::parse(&rsp).is_none());
        assert!(AttPrepareWriteRspHeader::parse(&[0x16, 0x10, 0x00, 0x05, 0x00]).is_none());
    }
}
