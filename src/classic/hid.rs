// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! HID over Bluetooth Classic (HID Profile v1.1.1): the HIDP transaction
//! protocol carried over two L2CAP Basic Mode channels — Control (PSM
//! 0x0011) for request/response transactions and Interrupt (PSM 0x0013) for
//! asynchronous report DATA — plus the HID SDP service record.
//!
//! [`HidMessage`] is the wire format: a 1-byte header with the transaction
//! type in the high nibble and a type-specific parameter in the low nibble
//! (HID Profile v1.1.1, 7.1), followed by an optional payload. [`HidDevice`]
//! and [`HidHost`] are plain synchronous state machines in the same
//! `receive -> (outgoing, events)` style as `rfcomm::Multiplexer`: the
//! device answers control-channel transactions from an internally stored
//! report set, the host builds requests and interprets responses.
//!
//! Report descriptors and boot report formats are transport independent, so
//! they come from `crate::devices::helpers::hid_reports` (shared with BLE
//! HOGP) rather than being redefined here; only the Classic transport
//! machinery lives in this module.

use crate::classic::sdp::{
    AttributeIdSpec, DataElement, SdpClient, SdpUuid, ServiceAttribute,
    attribute_id as sdp_attribute_id,
};
use crate::l2cap::classic::{ClassicChannelManager, ClassicChannelSpec};
use crate::packets::l2cap_signaling::ConnectionRequestHeader;
use crate::types::SimbleError;
use std::collections::HashMap;

/// Well-known PSM for the HID Control channel (Bluetooth Assigned Numbers).
pub const HID_CONTROL_PSM: u16 = 0x0011;
/// Well-known PSM for the HID Interrupt channel (Bluetooth Assigned Numbers).
pub const HID_INTERRUPT_PSM: u16 = 0x0013;

/// HumanInterfaceDeviceService service class UUID (0x1124).
pub const HID_SERVICE_CLASS: SdpUuid = SdpUuid::Uuid16(0x1124);
/// HIDP protocol UUID (0x0011), used in protocol descriptor lists.
pub const HIDP_PROTOCOL_ID: SdpUuid = SdpUuid::Uuid16(0x0011);

/// HID Profile version v1.1 (BluetoothProfileDescriptorList value).
pub(crate) const HID_PROFILE_VERSION: u16 = 0x0101;
/// HIDParserVersion v1.1.1 (HID Profile v1.1.1, 5.3.4.4).
pub(crate) const HID_PARSER_VERSION: u16 = 0x0111;
/// HID class descriptor type tag for a Report Descriptor (USB HID 1.11, 7.1).
pub const REPORT_DESCRIPTOR_TYPE: u16 = 0x22;
/// HIDDeviceSubclass for a combo keyboard/pointing device (Class of Device
/// minor device class encoding, HID Profile v1.1.1, 5.3.4.5).
pub const DEVICE_SUBCLASS_COMBO: u8 = 0xC0;

/// HIDP message types (high nibble of the transaction header, HID Profile
/// v1.1.1, 7.1).
pub mod message_type {
    /// HANDSHAKE transaction.
    pub(crate) const HANDSHAKE: u8 = 0x00;
    /// HID_CONTROL transaction.
    pub(crate) const CONTROL: u8 = 0x01;
    /// GET_REPORT transaction.
    pub const GET_REPORT: u8 = 0x04;
    /// SET_REPORT transaction.
    pub const SET_REPORT: u8 = 0x05;
    /// GET_PROTOCOL transaction.
    pub(crate) const GET_PROTOCOL: u8 = 0x06;
    /// SET_PROTOCOL transaction.
    pub(crate) const SET_PROTOCOL: u8 = 0x07;
    /// DATA transaction (report transfer).
    pub const DATA: u8 = 0x0A;
}

/// Report types (low 2 bits of the header parameter, HID Profile v1.1.1, 7.4.3).
pub mod report_type {
    /// Other (untyped) report.
    pub const OTHER: u8 = 0x00;
    /// Input report (device to host).
    pub const INPUT: u8 = 0x01;
    /// Output report (host to device).
    pub const OUTPUT: u8 = 0x02;
    /// Feature report.
    pub const FEATURE: u8 = 0x03;
}

/// HANDSHAKE result codes (HID Profile v1.1.1, 7.4.1).
pub mod handshake_code {
    /// Transaction completed successfully.
    pub const SUCCESSFUL: u8 = 0x00;
    /// Device not ready.
    pub const NOT_READY: u8 = 0x01;
    /// Invalid report ID.
    pub const ERR_INVALID_REPORT_ID: u8 = 0x02;
    /// Unsupported request.
    pub const ERR_UNSUPPORTED_REQUEST: u8 = 0x03;
    /// Invalid parameter.
    pub const ERR_INVALID_PARAMETER: u8 = 0x04;
    /// Unspecified error.
    pub const ERR_UNKNOWN: u8 = 0x0E;
    /// Fatal, unrecoverable error.
    pub const ERR_FATAL: u8 = 0x0F;
}

/// Protocol modes carried by GET_/SET_PROTOCOL (HID Profile v1.1.1, 7.4.5).
pub mod protocol_mode {
    /// Boot Protocol mode.
    pub const BOOT: u8 = 0x00;
    /// Report Protocol mode.
    pub const REPORT: u8 = 0x01;
}

/// HID_CONTROL operations (HID Profile v1.1.1, 7.4.2).
pub(crate) mod control_command {
    /// Suspend the device.
    pub(crate) const SUSPEND: u8 = 0x03;
    /// Resume from suspend.
    pub(crate) const EXIT_SUSPEND: u8 = 0x04;
    /// Tear down the virtual cable.
    pub(crate) const VIRTUAL_CABLE_UNPLUG: u8 = 0x05;
}

/// HID-specific SDP attribute IDs (HID Profile v1.1.1, 5.3.4). Generic
/// attribute IDs live in `sdp::attribute_id`.
pub mod attribute_id {
    /// HIDParserVersion attribute.
    pub(crate) const HID_PARSER_VERSION: u16 = 0x0201;
    /// HIDDeviceSubclass attribute.
    pub(crate) const HID_DEVICE_SUBCLASS: u16 = 0x0202;
    /// HIDCountryCode attribute.
    pub(crate) const HID_COUNTRY_CODE: u16 = 0x0203;
    /// HIDVirtualCable attribute.
    pub(crate) const HID_VIRTUAL_CABLE: u16 = 0x0204;
    /// HIDReconnectInitiate attribute.
    pub(crate) const HID_RECONNECT_INITIATE: u16 = 0x0205;
    /// HIDDescriptorList attribute (carries the report descriptors).
    pub const HID_DESCRIPTOR_LIST: u16 = 0x0206;
    /// HIDLANGIDBaseList attribute.
    pub(crate) const HID_LANGID_BASE_LIST: u16 = 0x0207;
    /// HIDBootDevice attribute.
    pub(crate) const HID_BOOT_DEVICE: u16 = 0x020E;
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// One HIDP transaction message (HID Profile v1.1.1, 7.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidMessage {
    /// HANDSHAKE: result code for a preceding transaction.
    Handshake {
        /// Result code.
        result_code: u8,
    },
    /// HID_CONTROL: a control operation such as suspend or unplug.
    Control {
        /// Operation.
        operation: u8,
    },
    /// GET_REPORT: request a stored report.
    GetReport {
        /// Report type.
        report_type: u8,
        /// Report id.
        report_id: u8,
        /// When set, the header's buffer-size flag (bit 3 of the parameter)
        /// is raised and a 2-byte little-endian size field follows the
        /// report ID (HID Profile v1.1.1, 7.4.3).
        buffer_size: Option<u16>,
    },
    /// `payload` is the report ID followed by the report data.
    SetReport {
        /// Report type.
        report_type: u8,
        /// Payload.
        payload: Vec<u8>,
    },
    /// GET_PROTOCOL: query the current protocol mode.
    GetProtocol,
    /// SET_PROTOCOL: switch between Boot and Report protocol modes.
    SetProtocol {
        /// Mode.
        mode: u8,
    },
    /// On the control channel: a GET_REPORT or GET_PROTOCOL response. On the
    /// interrupt channel: an input (device-to-host) or output (host-to-device)
    /// report transfer.
    Data {
        /// Report type.
        report_type: u8,
        /// Payload.
        payload: Vec<u8>,
    },
}

impl HidMessage {
    fn header(msg_type: u8, param: u8) -> u8 {
        (msg_type << 4) | (param & 0x0F)
    }

    /// Serializes this message to its HIDP wire bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        match self {
            Self::Handshake { result_code } => {
                vec![Self::header(message_type::HANDSHAKE, *result_code)]
            }
            Self::Control { operation } => vec![Self::header(message_type::CONTROL, *operation)],
            Self::GetReport {
                report_type,
                report_id,
                buffer_size,
            } => match buffer_size {
                Some(size) => {
                    let mut out = vec![
                        Self::header(message_type::GET_REPORT, 0x08 | (report_type & 0x03)),
                        *report_id,
                    ];
                    out.extend(size.to_le_bytes());
                    out
                }
                None => vec![
                    Self::header(message_type::GET_REPORT, report_type & 0x03),
                    *report_id,
                ],
            },
            Self::SetReport {
                report_type,
                payload,
            } => {
                let mut out = vec![Self::header(message_type::SET_REPORT, report_type & 0x03)];
                out.extend_from_slice(payload);
                out
            }
            Self::GetProtocol => vec![Self::header(message_type::GET_PROTOCOL, 0)],
            Self::SetProtocol { mode } => {
                vec![Self::header(message_type::SET_PROTOCOL, mode & 0x01)]
            }
            Self::Data {
                report_type,
                payload,
            } => {
                let mut out = vec![Self::header(message_type::DATA, report_type & 0x03)];
                out.extend_from_slice(payload);
                out
            }
        }
    }

    /// Parses one HIDP message. Returns `None` on an empty buffer, a
    /// reserved message type, or a truncated GET_REPORT.
    pub fn parse(data: &[u8]) -> Option<Self> {
        let header = *data.first()?;
        let param = header & 0x0F;
        match header >> 4 {
            message_type::HANDSHAKE => Some(Self::Handshake { result_code: param }),
            message_type::CONTROL => Some(Self::Control { operation: param }),
            message_type::GET_REPORT => {
                let report_id = *data.get(1)?;
                let buffer_size = if param & 0x08 != 0 {
                    Some(u16::from_le_bytes([*data.get(2)?, *data.get(3)?]))
                } else {
                    None
                };
                Some(Self::GetReport {
                    report_type: param & 0x03,
                    report_id,
                    buffer_size,
                })
            }
            message_type::SET_REPORT => Some(Self::SetReport {
                report_type: param & 0x03,
                payload: data[1..].to_vec(),
            }),
            message_type::GET_PROTOCOL => Some(Self::GetProtocol),
            message_type::SET_PROTOCOL => Some(Self::SetProtocol { mode: param & 0x01 }),
            message_type::DATA => Some(Self::Data {
                report_type: param & 0x03,
                payload: data[1..].to_vec(),
            }),
            _ => None,
        }
    }
}

fn handshake(result_code: u8) -> Vec<u8> {
    HidMessage::Handshake { result_code }.to_bytes()
}

// ---------------------------------------------------------------------------
// Interrupt channel
// ---------------------------------------------------------------------------

/// A report transfer received on the interrupt channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterruptData {
    /// Report type (`report_type::*`).
    pub report_type: u8,
    /// Report payload bytes.
    pub payload: Vec<u8>,
}

/// Processes one incoming interrupt-channel PDU, identical for both roles.
/// Only DATA is valid on the interrupt channel (HID Profile v1.1.1, 7.4.6);
/// any other message type is silently ignored (`Ok(None)`).
pub fn receive_interrupt(pdu: &[u8]) -> Result<Option<InterruptData>, SimbleError> {
    let message = HidMessage::parse(pdu)
        .ok_or_else(|| SimbleError::PacketParseError("HID: malformed interrupt PDU".into()))?;
    match message {
        HidMessage::Data {
            report_type,
            payload,
        } => Ok(Some(InterruptData {
            report_type,
            payload,
        })),
        _ => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// Device role
// ---------------------------------------------------------------------------

/// Result of feeding one control-channel PDU into [`HidDevice::receive_control`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidDeviceEvent {
    /// The host requested SUSPEND.
    Suspend,
    /// The host requested EXIT_SUSPEND.
    ExitSuspend,
    /// The host requested VIRTUAL_CABLE_UNPLUG.
    VirtualCableUnplug,
    /// DATA arrived on the control channel (host-initiated report transfer).
    ControlData {
        /// Report type.
        report_type: u8,
        /// Payload.
        payload: Vec<u8>,
    },
    /// A SET_REPORT was accepted and stored.
    ReportSet {
        /// Report type.
        report_type: u8,
        /// Report id.
        report_id: u8,
        /// Data.
        data: Vec<u8>,
    },
    /// A SET_PROTOCOL was accepted.
    ProtocolSet(u8),
}

/// Device role (the keyboard/mouse side): answers control-channel
/// transactions from a stored report set and sends input reports on the
/// interrupt channel. Reports are keyed by `(report_type, report_id)`;
/// requests for undeclared IDs get a `HANDSHAKE(ERR_INVALID_REPORT_ID)`.
#[derive(Debug, Clone)]
pub struct HidDevice {
    /// Current protocol mode (`protocol_mode::*`).
    pub protocol_mode: u8,
    reports: HashMap<(u8, u8), Vec<u8>>,
}

impl Default for HidDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl HidDevice {
    /// Creates a device that boots into Report Protocol mode.
    pub fn new() -> Self {
        Self {
            // Devices boot into Report Protocol mode (HID Profile v1.1.1, 7.4.5).
            protocol_mode: protocol_mode::REPORT,
            reports: HashMap::new(),
        }
    }

    /// Declares (or overwrites) the report served for `(report_type,
    /// report_id)`. Only declared IDs are readable/writable by the host.
    pub fn put_report(&mut self, report_type: u8, report_id: u8, data: impl Into<Vec<u8>>) {
        self.reports.insert((report_type, report_id), data.into());
    }

    /// Current data for a declared report, if any.
    pub fn report(&self, report_type: u8, report_id: u8) -> Option<&[u8]> {
        self.reports
            .get(&(report_type, report_id))
            .map(Vec::as_slice)
    }

    /// Builds a `DATA(input report)` PDU to send on the interrupt channel.
    pub fn send_input_report(&self, payload: impl Into<Vec<u8>>) -> Vec<u8> {
        HidMessage::Data {
            report_type: report_type::INPUT,
            payload: payload.into(),
        }
        .to_bytes()
    }

    /// Builds a `HID_CONTROL(VIRTUAL_CABLE_UNPLUG)` PDU for the control channel.
    pub fn virtual_cable_unplug(&self) -> Vec<u8> {
        HidMessage::Control {
            operation: control_command::VIRTUAL_CABLE_UNPLUG,
        }
        .to_bytes()
    }

    /// Processes one incoming control-channel PDU, returning the response
    /// PDU to send back (if the transaction demands one) plus any events.
    pub fn receive_control(
        &mut self,
        pdu: &[u8],
    ) -> Result<(Option<Vec<u8>>, Vec<HidDeviceEvent>), SimbleError> {
        let Some(message) = HidMessage::parse(pdu) else {
            if pdu.is_empty() {
                return Err(SimbleError::PacketParseError(
                    "HID: empty control PDU".into(),
                ));
            }
            // Reserved/unsupported message types are answered, not dropped
            // (HID Profile v1.1.1, 7.4.1).
            return Ok((
                Some(handshake(handshake_code::ERR_UNSUPPORTED_REQUEST)),
                Vec::new(),
            ));
        };

        let mut events = Vec::new();
        let response = match message {
            HidMessage::GetReport {
                report_type,
                report_id,
                buffer_size,
            } => Some(match self.reports.get(&(report_type, report_id)) {
                Some(data) => {
                    let mut payload = vec![report_id];
                    payload.extend_from_slice(data);
                    if let Some(size) = buffer_size {
                        payload.truncate(usize::from(size));
                    }
                    HidMessage::Data {
                        report_type,
                        payload,
                    }
                    .to_bytes()
                }
                None => handshake(handshake_code::ERR_INVALID_REPORT_ID),
            }),
            HidMessage::SetReport {
                report_type,
                payload,
            } => Some(match payload.split_first() {
                None => handshake(handshake_code::ERR_INVALID_PARAMETER),
                Some((&report_id, data)) => {
                    if let Some(stored) = self.reports.get_mut(&(report_type, report_id)) {
                        *stored = data.to_vec();
                        events.push(HidDeviceEvent::ReportSet {
                            report_type,
                            report_id,
                            data: data.to_vec(),
                        });
                        handshake(handshake_code::SUCCESSFUL)
                    } else {
                        handshake(handshake_code::ERR_INVALID_REPORT_ID)
                    }
                }
            }),
            HidMessage::GetProtocol => Some(
                HidMessage::Data {
                    report_type: report_type::OTHER,
                    payload: vec![self.protocol_mode],
                }
                .to_bytes(),
            ),
            HidMessage::SetProtocol { mode } => {
                self.protocol_mode = mode;
                events.push(HidDeviceEvent::ProtocolSet(mode));
                Some(handshake(handshake_code::SUCCESSFUL))
            }
            HidMessage::Data {
                report_type,
                payload,
            } => {
                events.push(HidDeviceEvent::ControlData {
                    report_type,
                    payload,
                });
                None
            }
            HidMessage::Control { operation } => {
                match operation {
                    control_command::SUSPEND => events.push(HidDeviceEvent::Suspend),
                    control_command::EXIT_SUSPEND => events.push(HidDeviceEvent::ExitSuspend),
                    control_command::VIRTUAL_CABLE_UNPLUG => {
                        events.push(HidDeviceEvent::VirtualCableUnplug);
                    }
                    _ => {}
                }
                None
            }
            HidMessage::Handshake { .. } => None,
        };
        Ok((response, events))
    }
}

// ---------------------------------------------------------------------------
// Host role
// ---------------------------------------------------------------------------

/// Result of feeding one control-channel PDU into [`HidHost::receive_control`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HidHostEvent {
    /// The device's HANDSHAKE result code for our last request.
    Handshake(u8),
    /// DATA arrived on the control channel: a GET_REPORT response (payload
    /// is report ID + data) or a GET_PROTOCOL response (payload is the mode).
    ControlData {
        /// Report type.
        report_type: u8,
        /// Payload.
        payload: Vec<u8>,
    },
    /// The device requested VIRTUAL_CABLE_UNPLUG.
    VirtualCableUnplug,
}

/// Host role (the computer/phone side): builds control-channel request PDUs
/// and interprets the device's responses.
#[derive(Debug, Clone, Copy, Default)]
pub struct HidHost;

impl HidHost {
    /// Creates a host-role helper.
    pub fn new() -> Self {
        Self
    }

    /// Builds a GET_REPORT request. `buffer_size`, when given, caps how many
    /// payload bytes the device may return.
    pub fn get_report(&self, report_type: u8, report_id: u8, buffer_size: Option<u16>) -> Vec<u8> {
        HidMessage::GetReport {
            report_type,
            report_id,
            buffer_size,
        }
        .to_bytes()
    }

    /// Builds a SET_REPORT request carrying `report_id` and `data`.
    pub fn set_report(&self, report_type: u8, report_id: u8, data: &[u8]) -> Vec<u8> {
        let mut payload = vec![report_id];
        payload.extend_from_slice(data);
        HidMessage::SetReport {
            report_type,
            payload,
        }
        .to_bytes()
    }

    /// Builds a GET_PROTOCOL request.
    pub fn get_protocol(&self) -> Vec<u8> {
        HidMessage::GetProtocol.to_bytes()
    }

    /// Builds a SET_PROTOCOL request selecting `mode`.
    pub fn set_protocol(&self, mode: u8) -> Vec<u8> {
        HidMessage::SetProtocol { mode }.to_bytes()
    }

    /// Builds a `HID_CONTROL(SUSPEND)` PDU.
    pub fn suspend(&self) -> Vec<u8> {
        HidMessage::Control {
            operation: control_command::SUSPEND,
        }
        .to_bytes()
    }

    /// Builds a `HID_CONTROL(EXIT_SUSPEND)` PDU.
    pub fn exit_suspend(&self) -> Vec<u8> {
        HidMessage::Control {
            operation: control_command::EXIT_SUSPEND,
        }
        .to_bytes()
    }

    /// Builds a `HID_CONTROL(VIRTUAL_CABLE_UNPLUG)` PDU.
    pub fn virtual_cable_unplug(&self) -> Vec<u8> {
        HidMessage::Control {
            operation: control_command::VIRTUAL_CABLE_UNPLUG,
        }
        .to_bytes()
    }

    /// Builds a `DATA(output report)` PDU to send on the interrupt channel.
    pub fn send_output_report(&self, payload: impl Into<Vec<u8>>) -> Vec<u8> {
        HidMessage::Data {
            report_type: report_type::OUTPUT,
            payload: payload.into(),
        }
        .to_bytes()
    }

    /// Processes one incoming control-channel PDU. Message types a host
    /// never receives (requests) are ignored; a malformed PDU is an error.
    pub fn receive_control(&self, pdu: &[u8]) -> Result<Vec<HidHostEvent>, SimbleError> {
        let Some(message) = HidMessage::parse(pdu) else {
            if pdu.is_empty() {
                return Err(SimbleError::PacketParseError(
                    "HID: empty control PDU".into(),
                ));
            }
            return Ok(Vec::new());
        };
        Ok(match message {
            HidMessage::Handshake { result_code } => vec![HidHostEvent::Handshake(result_code)],
            HidMessage::Data {
                report_type,
                payload,
            } => vec![HidHostEvent::ControlData {
                report_type,
                payload,
            }],
            HidMessage::Control { operation }
                if operation == control_command::VIRTUAL_CABLE_UNPLUG =>
            {
                vec![HidHostEvent::VirtualCableUnplug]
            }
            _ => Vec::new(),
        })
    }
}

// ---------------------------------------------------------------------------
// L2CAP integration
// ---------------------------------------------------------------------------

/// Registers both HID PSMs on `manager`. Both roles listen on both PSMs:
/// either side may initiate reconnection (HID Profile v1.1.1, 5.3.4.13).
pub fn register_psms(manager: &mut ClassicChannelManager) -> Result<(), SimbleError> {
    manager.register_server(HID_CONTROL_PSM)?;
    manager.register_server(HID_INTERRUPT_PSM)
}

/// Allocates the HID Control L2CAP channel (client role) and returns its
/// local CID plus the `Connection Request` to send. The control channel
/// must be established before the interrupt channel (HID Profile v1.1.1, 5.2.2).
pub fn connect_control_channel(
    manager: &mut ClassicChannelManager,
    mtu: u16,
) -> Result<(u16, ConnectionRequestHeader), SimbleError> {
    manager.connect(&ClassicChannelSpec::with_mtu(HID_CONTROL_PSM, mtu))
}

/// Allocates the HID Interrupt L2CAP channel (client role); see
/// [`connect_control_channel`].
pub fn connect_interrupt_channel(
    manager: &mut ClassicChannelManager,
    mtu: u16,
) -> Result<(u16, ConnectionRequestHeader), SimbleError> {
    manager.connect(&ClassicChannelSpec::with_mtu(HID_INTERRUPT_PSM, mtu))
}

// ---------------------------------------------------------------------------
// SDP integration
// ---------------------------------------------------------------------------

/// Builds the SDP service record for a HID device (HID Profile v1.1.1, 5.3):
/// the mandatory attributes plus BootDevice, advertising `report_descriptor`
/// in the HIDDescriptorList and both HID PSMs in the (additional) protocol
/// descriptor lists.
pub fn make_service_sdp_records(
    service_record_handle: u32,
    report_descriptor: &[u8],
    device_subclass: u8,
) -> Vec<ServiceAttribute> {
    vec![
        ServiceAttribute::new(
            sdp_attribute_id::SERVICE_RECORD_HANDLE,
            DataElement::unsigned_integer_32(service_record_handle),
        ),
        ServiceAttribute::new(
            sdp_attribute_id::BROWSE_GROUP_LIST,
            DataElement::sequence(vec![DataElement::uuid(SdpUuid::SDP_PUBLIC_BROWSE_ROOT)]),
        ),
        ServiceAttribute::new(
            sdp_attribute_id::SERVICE_CLASS_ID_LIST,
            DataElement::sequence(vec![DataElement::uuid(HID_SERVICE_CLASS)]),
        ),
        ServiceAttribute::new(
            sdp_attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID),
                    DataElement::unsigned_integer_16(HID_CONTROL_PSM),
                ]),
                DataElement::sequence(vec![DataElement::uuid(HIDP_PROTOCOL_ID)]),
            ]),
        ),
        ServiceAttribute::new(
            sdp_attribute_id::ADDITIONAL_PROTOCOL_DESCRIPTOR_LIST,
            DataElement::sequence(vec![DataElement::sequence(vec![
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID),
                    DataElement::unsigned_integer_16(HID_INTERRUPT_PSM),
                ]),
                DataElement::sequence(vec![DataElement::uuid(HIDP_PROTOCOL_ID)]),
            ])]),
        ),
        ServiceAttribute::new(
            sdp_attribute_id::BLUETOOTH_PROFILE_DESCRIPTOR_LIST,
            DataElement::sequence(vec![DataElement::sequence(vec![
                DataElement::uuid(HID_SERVICE_CLASS),
                DataElement::unsigned_integer_16(HID_PROFILE_VERSION),
            ])]),
        ),
        ServiceAttribute::new(
            sdp_attribute_id::LANGUAGE_BASE_ATTRIBUTE_ID_LIST,
            // "en", UTF-8, PrimaryLanguageBaseID 0x0100 (Core Vol 3, Part B, 5.1.8).
            DataElement::sequence(vec![
                DataElement::unsigned_integer_16(0x656E),
                DataElement::unsigned_integer_16(0x006A),
                DataElement::unsigned_integer_16(0x0100),
            ]),
        ),
        ServiceAttribute::new(
            attribute_id::HID_PARSER_VERSION,
            DataElement::unsigned_integer_16(HID_PARSER_VERSION),
        ),
        ServiceAttribute::new(
            attribute_id::HID_DEVICE_SUBCLASS,
            DataElement::unsigned_integer_8(device_subclass),
        ),
        ServiceAttribute::new(
            // 0x00 = hardware not localized (USB HID 1.11, 6.2.1).
            attribute_id::HID_COUNTRY_CODE,
            DataElement::unsigned_integer_8(0x00),
        ),
        ServiceAttribute::new(attribute_id::HID_VIRTUAL_CABLE, DataElement::boolean(true)),
        ServiceAttribute::new(
            attribute_id::HID_RECONNECT_INITIATE,
            DataElement::boolean(true),
        ),
        ServiceAttribute::new(
            attribute_id::HID_DESCRIPTOR_LIST,
            DataElement::sequence(vec![DataElement::sequence(vec![
                DataElement::unsigned_integer_16(REPORT_DESCRIPTOR_TYPE),
                DataElement::text_string(report_descriptor.to_vec()),
            ])]),
        ),
        ServiceAttribute::new(
            attribute_id::HID_LANGID_BASE_LIST,
            // LANGID 0x0409 (English/US), Bluetooth string offset 0x0100
            // (HID Profile v1.1.1, 5.3.4.12).
            DataElement::sequence(vec![DataElement::sequence(vec![
                DataElement::unsigned_integer_16(0x0409),
                DataElement::unsigned_integer_16(0x0100),
            ])]),
        ),
        ServiceAttribute::new(attribute_id::HID_BOOT_DEVICE, DataElement::boolean(true)),
    ]
}

/// Searches for a HID service via `sdp_client`/`transport`, returning its
/// report descriptor (the first HIDDescriptorList entry of type
/// [`REPORT_DESCRIPTOR_TYPE`]), if any.
pub fn find_hid_report_descriptor(
    sdp_client: &mut SdpClient,
    mut transport: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<Option<Vec<u8>>, SimbleError> {
    let results = sdp_client.search_attributes(
        &[HID_SERVICE_CLASS],
        &[AttributeIdSpec::Single(attribute_id::HID_DESCRIPTOR_LIST)],
        &mut transport,
    )?;
    for attrs in results {
        for attr in &attrs {
            if attr.id != attribute_id::HID_DESCRIPTOR_LIST {
                continue;
            }
            let Some(descriptors) = attr.value.as_sequence() else {
                continue;
            };
            for descriptor in descriptors {
                let Some(pair) = descriptor.as_sequence() else {
                    continue;
                };
                let descriptor_type = pair
                    .first()
                    .and_then(DataElement::as_unsigned_integer)
                    .map(|(value, _)| value);
                if descriptor_type == Some(u64::from(REPORT_DESCRIPTOR_TYPE))
                    && let Some(DataElement::TextString(bytes)) = pair.get(1)
                {
                    return Ok(Some(bytes.clone()));
                }
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_report_round_trip_without_buffer_size() {
        let msg = HidMessage::GetReport {
            report_type: report_type::INPUT,
            report_id: 2,
            buffer_size: None,
        };
        let bytes = msg.to_bytes();
        assert_eq!(bytes, vec![0x41, 0x02]);
        assert_eq!(HidMessage::parse(&bytes), Some(msg));
    }

    #[test]
    fn test_get_report_round_trip_with_buffer_size() {
        let msg = HidMessage::GetReport {
            report_type: report_type::FEATURE,
            report_id: 1,
            buffer_size: Some(0x1234),
        };
        let bytes = msg.to_bytes();
        assert_eq!(bytes, vec![0x4B, 0x01, 0x34, 0x12]);
        assert_eq!(HidMessage::parse(&bytes), Some(msg));
    }

    #[test]
    fn test_set_report_and_data_round_trip() {
        let msg = HidMessage::SetReport {
            report_type: report_type::OUTPUT,
            payload: vec![0x01, 0xAA, 0xBB],
        };
        let bytes = msg.to_bytes();
        assert_eq!(bytes[0], 0x52);
        assert_eq!(HidMessage::parse(&bytes), Some(msg));

        let msg = HidMessage::Data {
            report_type: report_type::INPUT,
            payload: vec![0x01, 0x02],
        };
        let bytes = msg.to_bytes();
        assert_eq!(bytes[0], 0xA1);
        assert_eq!(HidMessage::parse(&bytes), Some(msg));
    }

    #[test]
    fn test_handshake_control_and_protocol_round_trips() {
        for msg in [
            HidMessage::Handshake {
                result_code: handshake_code::ERR_INVALID_REPORT_ID,
            },
            HidMessage::Control {
                operation: control_command::SUSPEND,
            },
            HidMessage::GetProtocol,
            HidMessage::SetProtocol {
                mode: protocol_mode::BOOT,
            },
        ] {
            assert_eq!(HidMessage::parse(&msg.to_bytes()), Some(msg));
        }
    }

    #[test]
    fn test_parse_rejects_reserved_type_and_truncation() {
        // 0x2- and 0x3- are reserved message types.
        assert_eq!(HidMessage::parse(&[0x20]), None);
        assert_eq!(HidMessage::parse(&[]), None);
        // GET_REPORT missing its report ID byte.
        assert_eq!(HidMessage::parse(&[0x41]), None);
        // GET_REPORT with the buffer-size flag but a truncated size field.
        assert_eq!(HidMessage::parse(&[0x49, 0x01, 0x34]), None);
    }
}
