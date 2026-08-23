// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Service Discovery Protocol (SDP) client and server for Bluetooth Classic.
//!
//! Wire format: a 5-byte fixed PDU header (see [`SdpPdu::parse`]) followed by
//! parameters that embed one or more `DataElement` TLV trees — SDP's
//! self-describing type/length/value encoding for nil, integers, UUIDs,
//! strings, booleans, sequences, alternatives, and URLs. `DataElement` is
//! recursive and variable-length, so unlike the fixed-layout packet structs
//! elsewhere in Simble it is hand-encoded/decoded rather than zerocopy.

use crate::l2cap::classic::ClassicChannelManager;
use crate::types::SimbleError;
use std::collections::HashMap;

/// Well-known PSM for SDP (Bluetooth Assigned Numbers).
pub const SDP_PSM: u16 = 0x0001;

/// Safety cap on continuation round-trips for a single logical request.
pub(crate) const SDP_CONTINUATION_WATCHDOG: u32 = 64;

/// SDP data elements nest via SEQUENCE/ALTERNATIVE; cap recursion so a
/// malicious peer can't stack-overflow the parser with a deeply nested PDU.
const MAX_DATA_ELEMENT_NESTING: u32 = 32;

const CONTINUATION_MARKER: [u8; 2] = [0x01, 0x00];

/// SDP PDU identifiers (Bluetooth Core Vol 3, Part B, 4.2).
pub mod pdu_id {
    /// SDP_ErrorResponse.
    pub(crate) const ERROR_RESPONSE: u8 = 0x01;
    /// SDP_ServiceSearchRequest.
    pub(crate) const SERVICE_SEARCH_REQUEST: u8 = 0x02;
    /// SDP_ServiceSearchResponse.
    pub(crate) const SERVICE_SEARCH_RESPONSE: u8 = 0x03;
    /// SDP_ServiceAttributeRequest.
    pub(crate) const SERVICE_ATTRIBUTE_REQUEST: u8 = 0x04;
    /// SDP_ServiceAttributeResponse.
    pub(crate) const SERVICE_ATTRIBUTE_RESPONSE: u8 = 0x05;
    /// SDP_ServiceSearchAttributeRequest.
    pub(crate) const SERVICE_SEARCH_ATTRIBUTE_REQUEST: u8 = 0x06;
    /// SDP_ServiceSearchAttributeResponse.
    pub(crate) const SERVICE_SEARCH_ATTRIBUTE_RESPONSE: u8 = 0x07;
}

/// SDP error codes carried in an `SDP_ErrorResponse` PDU.
pub mod error_code {
    /// Invalid or unsupported SDP version.
    pub const INVALID_SDP_VERSION: u16 = 0x0001;
    /// Invalid service record handle.
    pub(crate) const INVALID_SERVICE_RECORD_HANDLE: u16 = 0x0002;
    /// Invalid request syntax.
    pub(crate) const INVALID_REQUEST_SYNTAX: u16 = 0x0003;
    /// Invalid PDU size.
    pub const INVALID_PDU_SIZE: u16 = 0x0004;
    /// Invalid continuation state.
    pub(crate) const INVALID_CONTINUATION_STATE: u16 = 0x0005;
    /// Insufficient resources to satisfy the request.
    pub const INSUFFICIENT_RESOURCES_TO_SATISFY_REQUEST: u16 = 0x0006;
}

/// Universal Service Attribute IDs (Bluetooth Assigned Numbers).
pub mod attribute_id {
    /// ServiceRecordHandle attribute.
    pub const SERVICE_RECORD_HANDLE: u16 = 0x0000;
    /// ServiceClassIDList attribute.
    pub const SERVICE_CLASS_ID_LIST: u16 = 0x0001;
    /// ServiceRecordState attribute.
    pub const SERVICE_RECORD_STATE: u16 = 0x0002;
    /// ServiceID attribute.
    pub const SERVICE_ID: u16 = 0x0003;
    /// ProtocolDescriptorList attribute.
    pub const PROTOCOL_DESCRIPTOR_LIST: u16 = 0x0004;
    /// BrowseGroupList attribute.
    pub const BROWSE_GROUP_LIST: u16 = 0x0005;
    /// LanguageBaseAttributeIDList attribute.
    pub(crate) const LANGUAGE_BASE_ATTRIBUTE_ID_LIST: u16 = 0x0006;
    /// ServiceInfoTimeToLive attribute.
    pub const SERVICE_INFO_TIME_TO_LIVE: u16 = 0x0007;
    /// ServiceAvailability attribute.
    pub const SERVICE_AVAILABILITY: u16 = 0x0008;
    /// BluetoothProfileDescriptorList attribute.
    pub(crate) const BLUETOOTH_PROFILE_DESCRIPTOR_LIST: u16 = 0x0009;
    /// DocumentationURL attribute.
    pub const DOCUMENTATION_URL: u16 = 0x000A;
    /// ClientExecutableURL attribute.
    pub const CLIENT_EXECUTABLE_URL: u16 = 0x000B;
    /// IconURL attribute.
    pub const ICON_URL: u16 = 0x000C;
    /// AdditionalProtocolDescriptorList attribute.
    pub const ADDITIONAL_PROTOCOL_DESCRIPTOR_LIST: u16 = 0x000D;
}

/// Range covering every possible attribute ID, for use in a search request.
pub const ALL_ATTRIBUTES_RANGE: (u16, u16) = (0x0000, 0xFFFF);

// ---------------------------------------------------------------------------
// UUID (SDP wire form: 16/32/128-bit, big-endian on the wire)
// ---------------------------------------------------------------------------

/// A Bluetooth UUID as carried in an SDP `DataElement`. Distinct from
/// `crate::types::Uuid` because SDP additionally allows a 32-bit form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SdpUuid {
    /// 16-bit UUID.
    Uuid16(u16),
    /// 32-bit UUID (SDP-only form).
    Uuid32(u32),
    /// 128-bit UUID, stored big-endian.
    Uuid128([u8; 16]),
}

impl SdpUuid {
    /// L2CAP protocol UUID (0x0100), used in protocol descriptor lists.
    pub const BT_L2CAP_PROTOCOL_ID: SdpUuid = SdpUuid::Uuid16(0x0100);
    /// RFCOMM protocol UUID (0x0003).
    pub(crate) const BT_RFCOMM_PROTOCOL_ID: SdpUuid = SdpUuid::Uuid16(0x0003);
    /// PublicBrowseRoot UUID (0x1002).
    pub const SDP_PUBLIC_BROWSE_ROOT: SdpUuid = SdpUuid::Uuid16(0x1002);

    /// Length in bytes of this UUID's wire form.
    fn wire_len(self) -> usize {
        match self {
            SdpUuid::Uuid16(..) => 2,
            SdpUuid::Uuid32(..) => 4,
            SdpUuid::Uuid128(..) => 16,
        }
    }

    fn to_wire_bytes(self) -> Vec<u8> {
        match self {
            SdpUuid::Uuid16(v) => v.to_be_bytes().to_vec(),
            SdpUuid::Uuid32(v) => v.to_be_bytes().to_vec(),
            SdpUuid::Uuid128(mut b) => {
                b.reverse();
                b.to_vec()
            }
        }
    }

    fn from_wire_bytes(bytes: &[u8]) -> Option<Self> {
        match bytes.len() {
            2 => Some(SdpUuid::Uuid16(u16::from_be_bytes(bytes.try_into().ok()?))),
            4 => Some(SdpUuid::Uuid32(u32::from_be_bytes(bytes.try_into().ok()?))),
            16 => {
                let mut b = [0u8; 16];
                b.copy_from_slice(bytes);
                b.reverse();
                Some(SdpUuid::Uuid128(b))
            }
            _ => None,
        }
    }
}

impl From<crate::types::Uuid> for SdpUuid {
    fn from(uuid: crate::types::Uuid) -> Self {
        match uuid {
            crate::types::Uuid::Uuid16(v) => SdpUuid::Uuid16(v),
            crate::types::Uuid::Uuid128(b) => SdpUuid::Uuid128(b),
        }
    }
}

// ---------------------------------------------------------------------------
// DataElement
// ---------------------------------------------------------------------------

/// An SDP TLV data element (Bluetooth Core Vol 3, Part B, 3.2).
#[derive(Debug, Clone, PartialEq)]
pub enum DataElement {
    /// Nil (no value).
    Nil,
    /// `(value, size_in_bytes)`, size is one of 1/2/4/8.
    UnsignedInteger(u64, u8),
    /// `(value, size_in_bytes)`, size is one of 1/2/4/8.
    SignedInteger(i64, u8),
    /// A UUID (16/32/128-bit).
    Uuid(SdpUuid),
    /// A text string (raw bytes).
    TextString(Vec<u8>),
    /// A boolean.
    Boolean(bool),
    /// An ordered sequence of elements.
    Sequence(Vec<DataElement>),
    /// A set of alternative elements.
    Alternative(Vec<DataElement>),
    /// A URL string.
    Url(String),
}

impl DataElement {
    /// Constructs a Nil element.
    pub fn nil() -> Self {
        DataElement::Nil
    }

    /// Constructs an unsigned integer of `size` bytes.
    pub fn unsigned_integer(value: u64, size: u8) -> Self {
        DataElement::UnsignedInteger(value, size)
    }

    /// Constructs a 1-byte unsigned integer.
    pub(crate) fn unsigned_integer_8(value: u8) -> Self {
        Self::unsigned_integer(value as u64, 1)
    }

    /// Constructs a 2-byte unsigned integer.
    pub fn unsigned_integer_16(value: u16) -> Self {
        Self::unsigned_integer(value as u64, 2)
    }

    /// Constructs a 4-byte unsigned integer.
    pub fn unsigned_integer_32(value: u32) -> Self {
        Self::unsigned_integer(value as u64, 4)
    }

    /// Constructs an 8-byte unsigned integer.
    pub fn unsigned_integer_64(value: u64) -> Self {
        Self::unsigned_integer(value, 8)
    }

    /// Constructs a signed integer of `size` bytes.
    pub fn signed_integer(value: i64, size: u8) -> Self {
        DataElement::SignedInteger(value, size)
    }

    /// Constructs a 1-byte signed integer.
    pub fn signed_integer_8(value: i8) -> Self {
        Self::signed_integer(value as i64, 1)
    }

    /// Constructs a 2-byte signed integer.
    pub fn signed_integer_16(value: i16) -> Self {
        Self::signed_integer(value as i64, 2)
    }

    /// Constructs a 4-byte signed integer.
    pub fn signed_integer_32(value: i32) -> Self {
        Self::signed_integer(value as i64, 4)
    }

    /// Constructs an 8-byte signed integer.
    pub fn signed_integer_64(value: i64) -> Self {
        Self::signed_integer(value, 8)
    }

    /// Constructs a UUID element.
    pub fn uuid(value: SdpUuid) -> Self {
        DataElement::Uuid(value)
    }

    /// Constructs a text-string element.
    pub fn text_string(value: impl Into<Vec<u8>>) -> Self {
        DataElement::TextString(value.into())
    }

    /// Constructs a boolean element.
    pub fn boolean(value: bool) -> Self {
        DataElement::Boolean(value)
    }

    /// Constructs a SEQUENCE element.
    pub fn sequence(items: impl Into<Vec<DataElement>>) -> Self {
        DataElement::Sequence(items.into())
    }

    /// Constructs an ALTERNATIVE element.
    pub fn alternative(items: impl Into<Vec<DataElement>>) -> Self {
        DataElement::Alternative(items.into())
    }

    /// Constructs a URL element.
    pub fn url(value: impl Into<String>) -> Self {
        DataElement::Url(value.into())
    }

    /// Returns the elements of a SEQUENCE, else `None`.
    pub fn as_sequence(&self) -> Option<&[DataElement]> {
        match self {
            DataElement::Sequence(items) => Some(items),
            _ => None,
        }
    }

    /// Returns the UUID of a Uuid element, else `None`.
    pub fn as_uuid(&self) -> Option<SdpUuid> {
        match self {
            DataElement::Uuid(u) => Some(*u),
            _ => None,
        }
    }

    /// Returns `(value, size)` of an unsigned integer, else `None`.
    pub fn as_unsigned_integer(&self) -> Option<(u64, u8)> {
        match self {
            DataElement::UnsignedInteger(v, s) => Some((*v, *s)),
            _ => None,
        }
    }

    fn type_code(&self) -> u8 {
        match self {
            DataElement::Nil => 0,
            DataElement::UnsignedInteger(..) => 1,
            DataElement::SignedInteger(..) => 2,
            DataElement::Uuid(..) => 3,
            DataElement::TextString(..) => 4,
            DataElement::Boolean(..) => 5,
            DataElement::Sequence(..) => 6,
            DataElement::Alternative(..) => 7,
            DataElement::Url(..) => 8,
        }
    }

    /// Serializes this element to its SDP TLV wire form.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.encoded_len());
        self.write_to(&mut out);
        out
    }

    /// Appends this element's SDP TLV wire form to `out` in a single pass.
    fn write_to(&self, out: &mut Vec<u8>) {
        let type_code = self.type_code();
        match self {
            DataElement::Nil => out.push(type_code << 3),
            DataElement::UnsignedInteger(v, size) => {
                out.push((type_code << 3) | size_index_fixed(*size as usize));
                out.extend_from_slice(&v.to_be_bytes()[8 - *size as usize..]);
            }
            DataElement::SignedInteger(v, size) => {
                out.push((type_code << 3) | size_index_fixed(*size as usize));
                out.extend_from_slice(&v.to_be_bytes()[8 - *size as usize..]);
            }
            DataElement::Uuid(u) => {
                out.push((type_code << 3) | size_index_fixed(u.wire_len()));
                out.extend_from_slice(&u.to_wire_bytes());
            }
            DataElement::Boolean(b) => out.extend_from_slice(&[type_code << 3, u8::from(*b)]),
            DataElement::TextString(s) => write_variable(type_code, s, out),
            DataElement::Url(s) => write_variable(type_code, s.as_bytes(), out),
            DataElement::Sequence(items) | DataElement::Alternative(items) => {
                let body_len: usize = items.iter().map(DataElement::encoded_len).sum();
                write_variable_header(type_code, body_len, out);
                for item in items {
                    item.write_to(out);
                }
            }
        }
    }

    /// Length in bytes of this element's wire form.
    fn encoded_len(&self) -> usize {
        match self {
            DataElement::Nil => 1,
            DataElement::UnsignedInteger(_, size) | DataElement::SignedInteger(_, size) => {
                1 + *size as usize
            }
            DataElement::Uuid(u) => 1 + u.wire_len(),
            DataElement::Boolean(..) => 2,
            DataElement::TextString(s) => variable_header_len(s.len()) + s.len(),
            DataElement::Url(s) => variable_header_len(s.len()) + s.len(),
            DataElement::Sequence(items) | DataElement::Alternative(items) => {
                let body_len: usize = items.iter().map(DataElement::encoded_len).sum();
                variable_header_len(body_len) + body_len
            }
        }
    }

    /// Parses one `DataElement` from the start of `data`, returning it along
    /// with the number of bytes consumed. Recursion (SEQUENCE/ALTERNATIVE)
    /// beyond `MAX_DATA_ELEMENT_NESTING` levels returns `None`.
    pub fn parse(data: &[u8]) -> Option<(DataElement, usize)> {
        parse_element(data, 0, 0)
    }

    /// Parses a `DataElement` starting at `data[0]`, ignoring any trailing bytes.
    pub fn from_bytes(data: &[u8]) -> Option<DataElement> {
        Self::parse(data).map(|(element, _)| element)
    }
}

fn size_index_fixed(size: usize) -> u8 {
    match size {
        2 => 1,
        4 => 2,
        8 => 3,
        16 => 4,
        _ => 0,
    }
}

/// Header (type descriptor + length field) size for a variable-length element.
fn variable_header_len(size: usize) -> usize {
    if size <= 0xFF {
        2
    } else if size <= 0xFFFF {
        3
    } else {
        5
    }
}

fn write_variable_header(type_code: u8, size: usize, out: &mut Vec<u8>) {
    if size <= 0xFF {
        out.push((type_code << 3) | 5);
        out.push(size as u8);
    } else if size <= 0xFFFF {
        out.push((type_code << 3) | 6);
        out.extend_from_slice(&(size as u16).to_be_bytes());
    } else {
        out.push((type_code << 3) | 7);
        out.extend_from_slice(&(size as u32).to_be_bytes());
    }
}

fn write_variable(type_code: u8, data: &[u8], out: &mut Vec<u8>) {
    write_variable_header(type_code, data.len(), out);
    out.extend_from_slice(data);
}

fn decode_uint_be(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    buf[8 - bytes.len()..].copy_from_slice(bytes);
    u64::from_be_bytes(buf)
}

fn decode_int_be(bytes: &[u8]) -> i64 {
    let mut buf = if bytes[0] & 0x80 != 0 {
        [0xFFu8; 8]
    } else {
        [0u8; 8]
    };
    buf[8 - bytes.len()..].copy_from_slice(bytes);
    i64::from_be_bytes(buf)
}

fn parse_element(data: &[u8], offset: usize, depth: u32) -> Option<(DataElement, usize)> {
    let byte0 = *data.get(offset)?;
    let type_code = byte0 >> 3;
    let size_index = byte0 & 0x07;
    let mut pos = offset + 1;

    let value_len: usize = match size_index {
        0 => usize::from(type_code != 0),
        1 => 2,
        2 => 4,
        3 => 8,
        4 => 16,
        5 => {
            let l = *data.get(pos)? as usize;
            pos += 1;
            l
        }
        6 => {
            let b = data.get(pos..pos + 2)?;
            pos += 2;
            u16::from_be_bytes([b[0], b[1]]) as usize
        }
        7 => {
            let b = data.get(pos..pos + 4)?;
            pos += 4;
            u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize
        }
        _ => return None,
    };

    let value_start = pos;
    let value_end = value_start.checked_add(value_len)?;
    if value_end > data.len() {
        return None;
    }
    let value_bytes = &data[value_start..value_end];

    let element = match type_code {
        0 => DataElement::Nil,
        1 => {
            if !matches!(value_len, 1 | 2 | 4 | 8) {
                return None;
            }
            DataElement::UnsignedInteger(decode_uint_be(value_bytes), value_len as u8)
        }
        2 => {
            if !matches!(value_len, 1 | 2 | 4 | 8) {
                return None;
            }
            DataElement::SignedInteger(decode_int_be(value_bytes), value_len as u8)
        }
        3 => DataElement::Uuid(SdpUuid::from_wire_bytes(value_bytes)?),
        4 => DataElement::TextString(value_bytes.to_vec()),
        5 => {
            if value_len != 1 {
                return None;
            }
            DataElement::Boolean(value_bytes[0] != 0)
        }
        6 | 7 => {
            if depth >= MAX_DATA_ELEMENT_NESTING {
                return None;
            }
            let mut items = Vec::new();
            let mut cursor = value_start;
            while cursor < value_end {
                let (item, next) = parse_element(data, cursor, depth + 1)?;
                items.push(item);
                cursor = next;
            }
            if cursor != value_end {
                return None;
            }
            if type_code == 6 {
                DataElement::Sequence(items)
            } else {
                DataElement::Alternative(items)
            }
        }
        8 => DataElement::Url(String::from_utf8(value_bytes.to_vec()).ok()?),
        _ => return None,
    };

    Some((element, value_end))
}

// ---------------------------------------------------------------------------
// Service records
// ---------------------------------------------------------------------------

/// One `(attribute ID, value)` pair within a service record.
#[derive(Debug, Clone, PartialEq)]
pub struct ServiceAttribute {
    /// Attribute ID.
    pub id: u16,
    /// Attribute value.
    pub value: DataElement,
}

/// A service record: an ordered set of attributes.
pub type Service = Vec<ServiceAttribute>;

impl ServiceAttribute {
    /// Creates an attribute from an ID and value.
    pub fn new(id: u16, value: DataElement) -> Self {
        Self { id, value }
    }

    /// Decodes `[id0, value0, id1, value1, ...]` pairs, as found in a
    /// flattened attribute-list `DataElement::Sequence`. Non-integer IDs are
    /// skipped rather than rejecting the whole list.
    pub(crate) fn list_from_data_elements(elements: &[DataElement]) -> Vec<ServiceAttribute> {
        elements
            .as_chunks::<2>()
            .0
            .iter()
            .filter_map(|pair| {
                let (id, _) = pair[0].as_unsigned_integer()?;
                Some(ServiceAttribute::new(id as u16, pair[1].clone()))
            })
            .collect()
    }

    /// Finds the value of attribute `id` in `list`, if present.
    pub fn find_attribute_in_list(list: &[ServiceAttribute], id: u16) -> Option<&DataElement> {
        list.iter().find(|a| a.id == id).map(|a| &a.value)
    }

    /// Checks whether `uuid` appears in `value`, recursing into sequences.
    pub(crate) fn is_uuid_in_value(uuid: SdpUuid, value: &DataElement) -> bool {
        match value {
            DataElement::Uuid(u) => *u == uuid,
            DataElement::Sequence(items) => items.iter().any(|e| Self::is_uuid_in_value(uuid, e)),
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// PDUs
// ---------------------------------------------------------------------------

/// An attribute ID, or an inclusive `(start, end)` range of attribute IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeIdSpec {
    /// A single attribute ID.
    Single(u16),
    /// An inclusive `(start, end)` range of attribute IDs.
    Range(u16, u16),
}

fn encode_attribute_id_list(ids: &[AttributeIdSpec]) -> DataElement {
    DataElement::sequence(
        ids.iter()
            .map(|id| match id {
                AttributeIdSpec::Single(v) => DataElement::unsigned_integer_16(*v),
                AttributeIdSpec::Range(start, end) => {
                    DataElement::unsigned_integer_32(((*start as u32) << 16) | *end as u32)
                }
            })
            .collect::<Vec<_>>(),
    )
}

/// An SDP request/response PDU (Bluetooth Core Vol 3, Part B, 4).
#[derive(Debug, Clone, PartialEq)]
pub enum SdpPdu {
    /// SDP_ErrorResponse: an error code for a failed request.
    ErrorResponse {
        /// Transaction id.
        transaction_id: u16,
        /// Error code.
        error_code: u16,
    },
    /// SDP_ServiceSearchRequest: search for records matching a UUID pattern.
    ServiceSearchRequest {
        /// Transaction id.
        transaction_id: u16,
        /// Service search pattern.
        service_search_pattern: DataElement,
        /// Maximum service record count.
        maximum_service_record_count: u16,
        /// Continuation state.
        continuation_state: Vec<u8>,
    },
    /// SDP_ServiceSearchResponse: matching service record handles.
    ServiceSearchResponse {
        /// Transaction id.
        transaction_id: u16,
        /// Total service record count.
        total_service_record_count: u16,
        /// Service record handle list.
        service_record_handle_list: Vec<u32>,
        /// Continuation state.
        continuation_state: Vec<u8>,
    },
    /// SDP_ServiceAttributeRequest: request attributes of one service record.
    ServiceAttributeRequest {
        /// Transaction id.
        transaction_id: u16,
        /// Service record handle.
        service_record_handle: u32,
        /// Maximum attribute byte count.
        maximum_attribute_byte_count: u16,
        /// Attribute id list.
        attribute_id_list: DataElement,
        /// Continuation state.
        continuation_state: Vec<u8>,
    },
    /// SDP_ServiceAttributeResponse: the requested attribute list.
    ServiceAttributeResponse {
        /// Transaction id.
        transaction_id: u16,
        /// Attribute list.
        attribute_list: Vec<u8>,
        /// Continuation state.
        continuation_state: Vec<u8>,
    },
    /// SDP_ServiceSearchAttributeRequest: combined search plus attribute request.
    ServiceSearchAttributeRequest {
        /// Transaction id.
        transaction_id: u16,
        /// Service search pattern.
        service_search_pattern: DataElement,
        /// Maximum attribute byte count.
        maximum_attribute_byte_count: u16,
        /// Attribute id list.
        attribute_id_list: DataElement,
        /// Continuation state.
        continuation_state: Vec<u8>,
    },
    /// SDP_ServiceSearchAttributeResponse: attribute lists for all matches.
    ServiceSearchAttributeResponse {
        /// Transaction id.
        transaction_id: u16,
        /// Attribute lists.
        attribute_lists: Vec<u8>,
        /// Continuation state.
        continuation_state: Vec<u8>,
    },
}

impl SdpPdu {
    fn pdu_id(&self) -> u8 {
        match self {
            SdpPdu::ErrorResponse { .. } => pdu_id::ERROR_RESPONSE,
            SdpPdu::ServiceSearchRequest { .. } => pdu_id::SERVICE_SEARCH_REQUEST,
            SdpPdu::ServiceSearchResponse { .. } => pdu_id::SERVICE_SEARCH_RESPONSE,
            SdpPdu::ServiceAttributeRequest { .. } => pdu_id::SERVICE_ATTRIBUTE_REQUEST,
            SdpPdu::ServiceAttributeResponse { .. } => pdu_id::SERVICE_ATTRIBUTE_RESPONSE,
            SdpPdu::ServiceSearchAttributeRequest { .. } => {
                pdu_id::SERVICE_SEARCH_ATTRIBUTE_REQUEST
            }
            SdpPdu::ServiceSearchAttributeResponse { .. } => {
                pdu_id::SERVICE_SEARCH_ATTRIBUTE_RESPONSE
            }
        }
    }

    /// Returns the PDU's transaction ID.
    pub fn transaction_id(&self) -> u16 {
        match self {
            SdpPdu::ErrorResponse { transaction_id, .. }
            | SdpPdu::ServiceSearchRequest { transaction_id, .. }
            | SdpPdu::ServiceSearchResponse { transaction_id, .. }
            | SdpPdu::ServiceAttributeRequest { transaction_id, .. }
            | SdpPdu::ServiceAttributeResponse { transaction_id, .. }
            | SdpPdu::ServiceSearchAttributeRequest { transaction_id, .. }
            | SdpPdu::ServiceSearchAttributeResponse { transaction_id, .. } => *transaction_id,
        }
    }

    fn parameters(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            SdpPdu::ErrorResponse { error_code, .. } => out.extend(error_code.to_be_bytes()),
            SdpPdu::ServiceSearchRequest {
                service_search_pattern,
                maximum_service_record_count,
                continuation_state,
                ..
            } => {
                out.extend(service_search_pattern.to_bytes());
                out.extend(maximum_service_record_count.to_be_bytes());
                out.extend(continuation_state);
            }
            SdpPdu::ServiceSearchResponse {
                total_service_record_count,
                service_record_handle_list,
                continuation_state,
                ..
            } => {
                out.extend(total_service_record_count.to_be_bytes());
                out.extend((service_record_handle_list.len() as u16).to_be_bytes());
                for handle in service_record_handle_list {
                    out.extend(handle.to_be_bytes());
                }
                out.extend(continuation_state);
            }
            SdpPdu::ServiceAttributeRequest {
                service_record_handle,
                maximum_attribute_byte_count,
                attribute_id_list,
                continuation_state,
                ..
            } => {
                out.extend(service_record_handle.to_be_bytes());
                out.extend(maximum_attribute_byte_count.to_be_bytes());
                out.extend(attribute_id_list.to_bytes());
                out.extend(continuation_state);
            }
            SdpPdu::ServiceAttributeResponse {
                attribute_list,
                continuation_state,
                ..
            } => {
                out.extend((attribute_list.len() as u16).to_be_bytes());
                out.extend(attribute_list);
                out.extend(continuation_state);
            }
            SdpPdu::ServiceSearchAttributeRequest {
                service_search_pattern,
                maximum_attribute_byte_count,
                attribute_id_list,
                continuation_state,
                ..
            } => {
                out.extend(service_search_pattern.to_bytes());
                out.extend(maximum_attribute_byte_count.to_be_bytes());
                out.extend(attribute_id_list.to_bytes());
                out.extend(continuation_state);
            }
            SdpPdu::ServiceSearchAttributeResponse {
                attribute_lists,
                continuation_state,
                ..
            } => {
                out.extend((attribute_lists.len() as u16).to_be_bytes());
                out.extend(attribute_lists);
                out.extend(continuation_state);
            }
        }
        out
    }

    /// Serializes the full PDU: 5-byte header + parameters.
    pub fn to_bytes(&self) -> Vec<u8> {
        let params = self.parameters();
        let mut out = Vec::with_capacity(5 + params.len());
        out.push(self.pdu_id());
        out.extend(self.transaction_id().to_be_bytes());
        out.extend((params.len() as u16).to_be_bytes());
        out.extend(params);
        out
    }

    /// Parses a full PDU (5-byte header + parameters) from `data`.
    pub fn parse(data: &[u8]) -> Option<SdpPdu> {
        if data.len() < 5 {
            return None;
        }
        let id = data[0];
        let transaction_id = u16::from_be_bytes([data[1], data[2]]);
        let parameters_length = u16::from_be_bytes([data[3], data[4]]) as usize;
        let params = data.get(5..5 + parameters_length)?;

        match id {
            pdu_id::ERROR_RESPONSE => {
                let error_code = u16::from_be_bytes(params.get(0..2)?.try_into().ok()?);
                Some(SdpPdu::ErrorResponse {
                    transaction_id,
                    error_code,
                })
            }
            pdu_id::SERVICE_SEARCH_REQUEST => {
                let (service_search_pattern, offset) = DataElement::parse(params)?;
                let maximum_service_record_count =
                    u16::from_be_bytes(params.get(offset..offset + 2)?.try_into().ok()?);
                let continuation_state = params.get(offset + 2..)?.to_vec();
                Some(SdpPdu::ServiceSearchRequest {
                    transaction_id,
                    service_search_pattern,
                    maximum_service_record_count,
                    continuation_state,
                })
            }
            pdu_id::SERVICE_SEARCH_RESPONSE => {
                let total_service_record_count =
                    u16::from_be_bytes(params.get(0..2)?.try_into().ok()?);
                let count = u16::from_be_bytes(params.get(2..4)?.try_into().ok()?) as usize;
                let mut service_record_handle_list = Vec::with_capacity(count);
                let mut offset = 4;
                for _ in 0..count {
                    service_record_handle_list.push(u32::from_be_bytes(
                        params.get(offset..offset + 4)?.try_into().ok()?,
                    ));
                    offset += 4;
                }
                let continuation_state = params.get(offset..)?.to_vec();
                Some(SdpPdu::ServiceSearchResponse {
                    transaction_id,
                    total_service_record_count,
                    service_record_handle_list,
                    continuation_state,
                })
            }
            pdu_id::SERVICE_ATTRIBUTE_REQUEST => {
                let service_record_handle = u32::from_be_bytes(params.get(0..4)?.try_into().ok()?);
                let maximum_attribute_byte_count =
                    u16::from_be_bytes(params.get(4..6)?.try_into().ok()?);
                let (attribute_id_list, rel_offset) = DataElement::parse(params.get(6..)?)?;
                let offset = 6 + rel_offset;
                let continuation_state = params.get(offset..)?.to_vec();
                Some(SdpPdu::ServiceAttributeRequest {
                    transaction_id,
                    service_record_handle,
                    maximum_attribute_byte_count,
                    attribute_id_list,
                    continuation_state,
                })
            }
            pdu_id::SERVICE_ATTRIBUTE_RESPONSE => {
                let len = u16::from_be_bytes(params.get(0..2)?.try_into().ok()?) as usize;
                let attribute_list = params.get(2..2 + len)?.to_vec();
                let continuation_state = params.get(2 + len..)?.to_vec();
                Some(SdpPdu::ServiceAttributeResponse {
                    transaction_id,
                    attribute_list,
                    continuation_state,
                })
            }
            pdu_id::SERVICE_SEARCH_ATTRIBUTE_REQUEST => {
                let (service_search_pattern, off1) = DataElement::parse(params)?;
                let maximum_attribute_byte_count =
                    u16::from_be_bytes(params.get(off1..off1 + 2)?.try_into().ok()?);
                let (attribute_id_list, off2) = DataElement::parse(params.get(off1 + 2..)?)?;
                let offset = off1 + 2 + off2;
                let continuation_state = params.get(offset..)?.to_vec();
                Some(SdpPdu::ServiceSearchAttributeRequest {
                    transaction_id,
                    service_search_pattern,
                    maximum_attribute_byte_count,
                    attribute_id_list,
                    continuation_state,
                })
            }
            pdu_id::SERVICE_SEARCH_ATTRIBUTE_RESPONSE => {
                let len = u16::from_be_bytes(params.get(0..2)?.try_into().ok()?) as usize;
                let attribute_lists = params.get(2..2 + len)?.to_vec();
                let continuation_state = params.get(2 + len..)?.to_vec();
                Some(SdpPdu::ServiceSearchAttributeResponse {
                    transaction_id,
                    attribute_lists,
                    continuation_state,
                })
            }
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// SDP client: builds request PDUs and drives the continuation loop over a
/// caller-supplied transport closure (`request bytes -> response bytes`),
/// synchronously — Simble has no async runtime, so the transport is whatever
/// moves bytes to the peer's `SdpServer::handle_request` and back (typically
/// an open `ClassicChannel`).
#[derive(Debug, Default)]
pub struct SdpClient {
    next_transaction_id: u16,
}

impl SdpClient {
    /// Creates a client with a fresh transaction-ID counter.
    pub fn new() -> Self {
        Self::default()
    }

    fn make_transaction_id(&mut self) -> u16 {
        let id = self.next_transaction_id;
        self.next_transaction_id = self.next_transaction_id.wrapping_add(1);
        id
    }

    /// Searches for services matching all of `uuids`, returning their record handles.
    pub fn search_services(
        &mut self,
        uuids: &[SdpUuid],
        mut transport: impl FnMut(&[u8]) -> Vec<u8>,
    ) -> Result<Vec<u32>, SimbleError> {
        let pattern = DataElement::sequence(
            uuids
                .iter()
                .map(|u| DataElement::uuid(*u))
                .collect::<Vec<_>>(),
        );
        let mut handles = Vec::new();
        let mut continuation_state = vec![0u8];

        for _ in 0..SDP_CONTINUATION_WATCHDOG {
            let request = SdpPdu::ServiceSearchRequest {
                transaction_id: self.make_transaction_id(),
                service_search_pattern: pattern.clone(),
                maximum_service_record_count: 0xFFFF,
                continuation_state,
            };
            let response = parse_response(&transport(&request.to_bytes()))?;
            match response {
                SdpPdu::ServiceSearchResponse {
                    service_record_handle_list,
                    continuation_state: next,
                    ..
                } => {
                    handles.extend(service_record_handle_list);
                    if is_final_continuation(&next) {
                        return Ok(handles);
                    }
                    continuation_state = next;
                }
                SdpPdu::ErrorResponse { error_code, .. } => return Err(sdp_error(error_code)),
                _ => return Err(unexpected_response()),
            }
        }
        Err(watchdog_exceeded())
    }

    /// Gets the requested attributes for a single service record.
    pub fn get_attributes(
        &mut self,
        service_record_handle: u32,
        attribute_ids: &[AttributeIdSpec],
        mut transport: impl FnMut(&[u8]) -> Vec<u8>,
    ) -> Result<Vec<ServiceAttribute>, SimbleError> {
        let attribute_id_list = encode_attribute_id_list(attribute_ids);
        let mut accumulator = Vec::new();
        let mut continuation_state = vec![0u8];

        for _ in 0..SDP_CONTINUATION_WATCHDOG {
            let request = SdpPdu::ServiceAttributeRequest {
                transaction_id: self.make_transaction_id(),
                service_record_handle,
                maximum_attribute_byte_count: 0xFFFF,
                attribute_id_list: attribute_id_list.clone(),
                continuation_state,
            };
            let response = parse_response(&transport(&request.to_bytes()))?;
            match response {
                SdpPdu::ServiceAttributeResponse {
                    attribute_list,
                    continuation_state: next,
                    ..
                } => {
                    accumulator.extend(attribute_list);
                    if is_final_continuation(&next) {
                        let sequence =
                            DataElement::from_bytes(&accumulator).ok_or_else(malformed_element)?;
                        let items = sequence.as_sequence().ok_or_else(unexpected_response)?;
                        return Ok(ServiceAttribute::list_from_data_elements(items));
                    }
                    continuation_state = next;
                }
                SdpPdu::ErrorResponse { error_code, .. } => return Err(sdp_error(error_code)),
                _ => return Err(unexpected_response()),
            }
        }
        Err(watchdog_exceeded())
    }

    /// Searches for services matching all of `uuids` and returns the
    /// requested attributes for each match.
    pub fn search_attributes(
        &mut self,
        uuids: &[SdpUuid],
        attribute_ids: &[AttributeIdSpec],
        mut transport: impl FnMut(&[u8]) -> Vec<u8>,
    ) -> Result<Vec<Vec<ServiceAttribute>>, SimbleError> {
        let pattern = DataElement::sequence(
            uuids
                .iter()
                .map(|u| DataElement::uuid(*u))
                .collect::<Vec<_>>(),
        );
        let attribute_id_list = encode_attribute_id_list(attribute_ids);
        let mut accumulator = Vec::new();
        let mut continuation_state = vec![0u8];

        for _ in 0..SDP_CONTINUATION_WATCHDOG {
            let request = SdpPdu::ServiceSearchAttributeRequest {
                transaction_id: self.make_transaction_id(),
                service_search_pattern: pattern.clone(),
                maximum_attribute_byte_count: 0xFFFF,
                attribute_id_list: attribute_id_list.clone(),
                continuation_state,
            };
            let response = parse_response(&transport(&request.to_bytes()))?;
            match response {
                SdpPdu::ServiceSearchAttributeResponse {
                    attribute_lists,
                    continuation_state: next,
                    ..
                } => {
                    accumulator.extend(attribute_lists);
                    if is_final_continuation(&next) {
                        let outer =
                            DataElement::from_bytes(&accumulator).ok_or_else(malformed_element)?;
                        let items = outer.as_sequence().ok_or_else(unexpected_response)?;
                        return Ok(items
                            .iter()
                            .filter_map(DataElement::as_sequence)
                            .map(ServiceAttribute::list_from_data_elements)
                            .collect());
                    }
                    continuation_state = next;
                }
                SdpPdu::ErrorResponse { error_code, .. } => return Err(sdp_error(error_code)),
                _ => return Err(unexpected_response()),
            }
        }
        Err(watchdog_exceeded())
    }
}

/// Whether a response's continuation state says "that was all of it".
///
/// The null continuation state is a single zero byte (Vol 3, Part B,
/// Section 4.3): an InfoLength of 0 with no bytes after it. Anything else
/// means the server is holding the rest of the answer and expects the same
/// request back with these bytes echoed into it. A client that ignores this
/// field gets a **truncated** answer that still parses as a well-formed PDU,
/// which is why the mistake survives every test against a peer whose records
/// happen to fit in one response.
pub(crate) fn is_final_continuation(state: &[u8]) -> bool {
    state.len() == 1 && state[0] == 0
}

fn parse_response(bytes: &[u8]) -> Result<SdpPdu, SimbleError> {
    SdpPdu::parse(bytes).ok_or_else(malformed_element)
}

fn malformed_element() -> SimbleError {
    SimbleError::PacketParseError("SDP: malformed response".into())
}

fn unexpected_response() -> SimbleError {
    SimbleError::PacketParseError("SDP: unexpected response PDU type".into())
}

fn watchdog_exceeded() -> SimbleError {
    SimbleError::DeviceError("SDP: continuation watchdog exceeded".into())
}

fn sdp_error(error_code: u16) -> SimbleError {
    SimbleError::DeviceError(format!("SDP: peer returned error {error_code:#06x}"))
}

// ---------------------------------------------------------------------------
// Server
// ---------------------------------------------------------------------------

enum PendingResponse {
    Handles(u16, Vec<u32>),
    Bytes(Vec<u8>),
}

/// SDP server: holds registered service records and answers request PDUs.
/// Continuation state (`current_response`) is per-server rather than
/// per-channel — one in-flight request/continuation sequence at a time.
#[derive(Default)]
pub struct SdpServer {
    /// Registered service records, keyed by record handle.
    pub service_records: HashMap<u32, Service>,
    current_response: Option<PendingResponse>,
}

impl SdpServer {
    /// Creates a server with no registered records.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers the SDP PSM with an L2CAP Classic channel manager.
    pub fn register(&self, manager: &mut ClassicChannelManager) -> Result<(), SimbleError> {
        manager.register_server(SDP_PSM)
    }

    /// Handles one raw request PDU and returns the raw response PDU bytes.
    /// `peer_mtu` bounds how much can be packed into a single response
    /// before continuation is required.
    pub fn handle_request(&mut self, request: &[u8], peer_mtu: u16) -> Vec<u8> {
        let Some(pdu) = SdpPdu::parse(request) else {
            return (SdpPdu::ErrorResponse {
                transaction_id: 0,
                error_code: error_code::INVALID_REQUEST_SYNTAX,
            })
            .to_bytes();
        };
        self.handle_pdu(pdu, peer_mtu).to_bytes()
    }

    fn handle_pdu(&mut self, pdu: SdpPdu, peer_mtu: u16) -> SdpPdu {
        match pdu {
            SdpPdu::ServiceSearchRequest {
                transaction_id,
                service_search_pattern,
                maximum_service_record_count,
                continuation_state,
            } => self.on_service_search_request(
                transaction_id,
                &service_search_pattern,
                maximum_service_record_count,
                &continuation_state,
                peer_mtu,
            ),
            SdpPdu::ServiceAttributeRequest {
                transaction_id,
                service_record_handle,
                maximum_attribute_byte_count,
                attribute_id_list,
                continuation_state,
            } => self.on_service_attribute_request(
                transaction_id,
                service_record_handle,
                maximum_attribute_byte_count,
                &attribute_id_list,
                &continuation_state,
                peer_mtu,
            ),
            SdpPdu::ServiceSearchAttributeRequest {
                transaction_id,
                service_search_pattern,
                maximum_attribute_byte_count,
                attribute_id_list,
                continuation_state,
            } => self.on_service_search_attribute_request(
                transaction_id,
                &service_search_pattern,
                maximum_attribute_byte_count,
                &attribute_id_list,
                &continuation_state,
                peer_mtu,
            ),
            other => SdpPdu::ErrorResponse {
                transaction_id: other.transaction_id(),
                error_code: error_code::INVALID_REQUEST_SYNTAX,
            },
        }
    }

    /// `Ok(true)` = this is a valid continuation of the pending response,
    /// `Ok(false)` = a fresh request, `Err(())` = invalid continuation state.
    fn check_continuation(&mut self, continuation_state: &[u8]) -> Result<bool, ()> {
        if continuation_state.len() > 1 {
            if self.current_response.is_none() || continuation_state != CONTINUATION_MARKER {
                return Err(());
            }
            return Ok(true);
        }
        self.current_response = None;
        Ok(false)
    }

    fn match_services(&self, pattern: &DataElement) -> Vec<u32> {
        let uuids = pattern.as_sequence().unwrap_or(&[]);
        let mut result = Vec::new();
        for (&handle, service) in &self.service_records {
            for id in uuids {
                let Some(target) = id.as_uuid() else { continue };
                if service
                    .iter()
                    .any(|attr| ServiceAttribute::is_uuid_in_value(target, &attr.value))
                {
                    result.push(handle);
                    break;
                }
            }
        }
        result
    }

    fn get_service_attributes(
        service: &[ServiceAttribute],
        attribute_ids: &[DataElement],
    ) -> DataElement {
        let mut attributes: Vec<&ServiceAttribute> = Vec::new();
        for id_element in attribute_ids {
            let Some((value, size)) = id_element.as_unsigned_integer() else {
                continue;
            };
            let (start, end) = if size == 4 {
                ((value >> 16) as u16, (value & 0xFFFF) as u16)
            } else {
                (value as u16, value as u16)
            };
            attributes.extend(service.iter().filter(|a| a.id >= start && a.id <= end));
        }
        attributes.sort_by_key(|a| a.id);

        let mut items = Vec::with_capacity(attributes.len() * 2);
        for attribute in attributes {
            items.push(DataElement::unsigned_integer_16(attribute.id));
            items.push(attribute.value.clone());
        }
        DataElement::sequence(items)
    }

    fn next_response_payload(&mut self, max_size: usize) -> (Vec<u8>, Vec<u8>) {
        let current = match self.current_response.take() {
            Some(PendingResponse::Bytes(b)) => b,
            _ => Vec::new(),
        };
        if current.len() > max_size {
            let (payload, rest) = current.split_at(max_size);
            self.current_response = Some(PendingResponse::Bytes(rest.to_vec()));
            (payload.to_vec(), CONTINUATION_MARKER.to_vec())
        } else {
            (current, vec![0])
        }
    }

    fn on_service_search_request(
        &mut self,
        transaction_id: u16,
        pattern: &DataElement,
        maximum_service_record_count: u16,
        continuation_state: &[u8],
        peer_mtu: u16,
    ) -> SdpPdu {
        let Ok(continuation) = self.check_continuation(continuation_state) else {
            return SdpPdu::ErrorResponse {
                transaction_id,
                error_code: error_code::INVALID_CONTINUATION_STATE,
            };
        };
        if !continuation {
            let mut handles = self.match_services(pattern);
            handles.sort_unstable();
            let total = handles.len() as u16;
            handles.truncate(maximum_service_record_count as usize);
            self.current_response = Some(PendingResponse::Handles(total, handles));
        }
        let (total, mut handles) = match self.current_response.take() {
            Some(PendingResponse::Handles(t, h)) => (t, h),
            _ => (0, Vec::new()),
        };
        let max_handles = (peer_mtu as usize).saturating_sub(11) / 4;
        let remaining = if handles.len() > max_handles {
            handles.split_off(max_handles)
        } else {
            Vec::new()
        };
        let continuation_state_out = if remaining.is_empty() {
            vec![0]
        } else {
            self.current_response = Some(PendingResponse::Handles(total, remaining));
            CONTINUATION_MARKER.to_vec()
        };
        SdpPdu::ServiceSearchResponse {
            transaction_id,
            total_service_record_count: total,
            service_record_handle_list: handles,
            continuation_state: continuation_state_out,
        }
    }

    fn on_service_attribute_request(
        &mut self,
        transaction_id: u16,
        service_record_handle: u32,
        maximum_attribute_byte_count: u16,
        attribute_id_list: &DataElement,
        continuation_state: &[u8],
        peer_mtu: u16,
    ) -> SdpPdu {
        let Ok(continuation) = self.check_continuation(continuation_state) else {
            return SdpPdu::ErrorResponse {
                transaction_id,
                error_code: error_code::INVALID_CONTINUATION_STATE,
            };
        };
        if !continuation {
            let Some(service) = self.service_records.get(&service_record_handle) else {
                return SdpPdu::ErrorResponse {
                    transaction_id,
                    error_code: error_code::INVALID_SERVICE_RECORD_HANDLE,
                };
            };
            let ids = attribute_id_list.as_sequence().unwrap_or(&[]);
            let attributes = Self::get_service_attributes(service, ids);
            self.current_response = Some(PendingResponse::Bytes(attributes.to_bytes()));
        }
        let max_bytes =
            (maximum_attribute_byte_count as usize).min((peer_mtu as usize).saturating_sub(9));
        let (attribute_list, continuation_state) = self.next_response_payload(max_bytes);
        SdpPdu::ServiceAttributeResponse {
            transaction_id,
            attribute_list,
            continuation_state,
        }
    }

    fn on_service_search_attribute_request(
        &mut self,
        transaction_id: u16,
        pattern: &DataElement,
        maximum_attribute_byte_count: u16,
        attribute_id_list: &DataElement,
        continuation_state: &[u8],
        peer_mtu: u16,
    ) -> SdpPdu {
        let Ok(continuation) = self.check_continuation(continuation_state) else {
            return SdpPdu::ErrorResponse {
                transaction_id,
                error_code: error_code::INVALID_CONTINUATION_STATE,
            };
        };
        if !continuation {
            let handles = self.match_services(pattern);
            let ids = attribute_id_list.as_sequence().unwrap_or(&[]);
            let mut lists = Vec::new();
            for handle in handles {
                if let Some(service) = self.service_records.get(&handle) {
                    let attributes = Self::get_service_attributes(service, ids);
                    if !matches!(&attributes, DataElement::Sequence(v) if v.is_empty()) {
                        lists.push(attributes);
                    }
                }
            }
            self.current_response = Some(PendingResponse::Bytes(
                DataElement::sequence(lists).to_bytes(),
            ));
        }
        let max_bytes =
            (maximum_attribute_byte_count as usize).min((peer_mtu as usize).saturating_sub(9));
        let (attribute_lists, continuation_state) = self.next_response_payload(max_bytes);
        SdpPdu::ServiceSearchAttributeResponse {
            transaction_id,
            attribute_lists,
            continuation_state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_element_round_trip_scalars() {
        for element in [
            DataElement::nil(),
            DataElement::unsigned_integer_8(12),
            DataElement::unsigned_integer_16(1234),
            DataElement::unsigned_integer_32(0x123456),
            DataElement::unsigned_integer_64(0x1_2345_6789),
            DataElement::signed_integer_8(-12),
            DataElement::signed_integer_16(-1234),
            DataElement::signed_integer_32(-0x123456),
            DataElement::boolean(true),
            DataElement::boolean(false),
            DataElement::uuid(SdpUuid::Uuid16(1234)),
            DataElement::uuid(SdpUuid::Uuid32(123_456_789)),
            DataElement::text_string(b"hello".to_vec()),
            DataElement::url("http://example.com"),
        ] {
            let bytes = element.to_bytes();
            let (parsed, consumed) = DataElement::parse(&bytes).expect("parse");
            assert_eq!(consumed, bytes.len());
            assert_eq!(parsed, element);
        }
    }

    #[test]
    fn test_data_element_uuid_128_round_trip() {
        let uuid = SdpUuid::Uuid128([
            0xC6, 0xAF, 0x7A, 0x66, 0x03, 0x0B, 0xA6, 0xA6, 0xDC, 0x4D, 0xBE, 0x09, 0x2C, 0x51,
            0xA3, 0x61,
        ]);
        let element = DataElement::uuid(uuid);
        let bytes = element.to_bytes();
        assert_eq!(DataElement::from_bytes(&bytes), Some(element));
    }

    #[test]
    fn test_data_element_large_text_string_round_trip() {
        let text = vec![b'h'; 20000];
        let element = DataElement::text_string(text);
        let bytes = element.to_bytes();
        assert_eq!(DataElement::from_bytes(&bytes), Some(element));
    }

    #[test]
    fn test_data_element_sequence_and_alternative_round_trip() {
        let seq = DataElement::sequence(vec![
            DataElement::boolean(true),
            DataElement::text_string(b"hello".to_vec()),
        ]);
        assert_eq!(DataElement::from_bytes(&seq.to_bytes()), Some(seq));

        let alt = DataElement::alternative(vec![
            DataElement::boolean(true),
            DataElement::text_string(b"hello".to_vec()),
        ]);
        assert_eq!(DataElement::from_bytes(&alt.to_bytes()), Some(alt));
    }
}
