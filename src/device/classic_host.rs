// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The **BR/EDR host**: the layer that turns simble's Classic protocol
//! modules ([`crate::classic`]) and its L2CAP Classic channel manager
//! ([`crate::l2cap::classic::ClassicChannelManager`]) into a device a real stack can
//! find, connect to, and use.
//!
//! Those protocol modules were complete but unreachable: nothing accepted a
//! page, nothing routed ACL data to L2CAP signalling, and nothing answered
//! SDP. This host closes that gap, mirroring [`crate::device::LeHost`] — it
//! is transport-free, taking H4 packets in and returning the H4 packets to
//! send back, so the same host runs over netsim, a dongle, or a test harness.
//!
//! What it covers today: discoverable/connectable bring-up, accepting an
//! inbound ACL connection, the L2CAP signalling handshake (connect →
//! configure → open), and dispatching channel data to a protocol handler —
//! with SDP wired up, which is what lets a peer discover any profile at all.
//! Profile channels above SDP (RFCOMM/SPP, HID, AVDTP) plug into the same
//! [`ProtocolHandler`] seam; see the module tests for the shape.

use crate::classic::sdp::{
    DataElement, SDP_PSM, SdpServer, SdpUuid, Service, ServiceAttribute, attribute_id,
};
use crate::l2cap::classic::ClassicChannelManager;
use crate::l2cap::{L2capHeader, cid};
use crate::packets::{
    ConfigurationRequestHeader, ConfigurationResponseHeader, ConnectionRequestHeader, HciEvent,
    L2capSignalingHeader, signaling_code,
};
use crate::types::{Address, SimbleError};
use zerocopy::{FromBytes, IntoBytes};

/// HCI command opcodes this host issues (Vol 4, Part E, Section 7.3).
mod opcode {
    /// Reset.
    pub const RESET: [u8; 2] = [0x03, 0x0C];
    /// Set Event Mask.
    pub const SET_EVENT_MASK: [u8; 2] = [0x01, 0x0C];
    /// Write Local Name.
    pub const WRITE_LOCAL_NAME: [u8; 2] = [0x13, 0x0C];
    /// Write Class of Device.
    pub const WRITE_CLASS_OF_DEVICE: [u8; 2] = [0x24, 0x0C];
    /// Write Scan Enable.
    pub const WRITE_SCAN_ENABLE: [u8; 2] = [0x1A, 0x0C];
    /// Write Simple Pairing Mode.
    pub const WRITE_SIMPLE_PAIRING_MODE: [u8; 2] = [0x56, 0x0C];
    /// Accept Connection Request.
    pub const ACCEPT_CONNECTION_REQUEST: [u8; 2] = [0x09, 0x04];
}

/// Scan Enable values (Vol 4, Part E, Section 7.3.18). Inquiry scan makes a
/// device *discoverable*; page scan makes it *connectable*. A peripheral
/// needs both to appear in Android's pairing list and then be connected to.
pub mod scan_enable {
    /// No scans — invisible and unconnectable.
    pub const NONE: u8 = 0x00;
    /// Inquiry scan only: discoverable, not connectable.
    pub const INQUIRY_ONLY: u8 = 0x01;
    /// Page scan only: connectable, not discoverable.
    pub const PAGE_ONLY: u8 = 0x02;
    /// Both — what a pairable peripheral advertises with.
    pub const INQUIRY_AND_PAGE: u8 = 0x03;
}

/// The default L2CAP MTU this host offers on its channels.
const DEFAULT_L2CAP_MTU: u16 = 672;

/// Builds one H4 HCI command packet.
fn command(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(4 + params.len());
    packet.push(crate::transport::h4_type::HCI_COMMAND);
    packet.extend_from_slice(&opcode);
    packet.push(params.len() as u8);
    packet.extend_from_slice(params);
    packet
}

/// Wraps an L2CAP PDU in an H4 ACL packet for `handle`.
fn acl_packet(handle: u16, l2cap: &[u8]) -> Vec<u8> {
    use crate::l2cap::{AclPacketBoundary, HciAclHeader};
    let header = HciAclHeader::new(handle, AclPacketBoundary::FirstNonFlushable, l2cap.len() as u16);
    let mut packet = Vec::with_capacity(5 + l2cap.len());
    packet.push(crate::transport::h4_type::HCI_ACL_DATA);
    packet.extend_from_slice(header.as_bytes());
    packet.extend_from_slice(l2cap);
    packet
}

/// Frames an L2CAP signalling PDU (code + identifier + payload) on the
/// BR/EDR signalling channel.
fn signaling_pdu(code: u8, identifier: u8, payload: &[u8]) -> Vec<u8> {
    let header = L2capSignalingHeader {
        code,
        identifier,
        length: (payload.len() as u16).into(),
    };
    let mut body = Vec::with_capacity(4 + payload.len());
    body.extend_from_slice(header.as_bytes());
    body.extend_from_slice(payload);
    L2capHeader::serialize(cid::BR_SIGNALING, &body)
}

/// What a profile does with data on an open L2CAP channel: it sees the
/// payload and returns whatever should be sent back on the same channel.
/// SDP is one of these; RFCOMM, HID and AVDTP fit the same seam.
pub trait ProtocolHandler: std::fmt::Debug {
    /// The PSM this handler serves.
    fn psm(&self) -> u16;
    /// Handles one inbound SDU; returns the SDU to reply with, if any.
    fn on_data(&mut self, data: &[u8], peer_mtu: u16) -> Option<Vec<u8>>;
}

/// The SDP server as a channel handler — the one profile every other
/// profile depends on, since a peer discovers services through it.
#[derive(Debug, Default)]
pub struct SdpHandler {
    server: SdpServer,
}

impl SdpHandler {
    /// Wraps a configured SDP server (its records are already registered).
    pub fn new(server: SdpServer) -> Self {
        Self { server }
    }

    /// The underlying server, for registering records.
    pub fn server_mut(&mut self) -> &mut SdpServer {
        &mut self.server
    }
}

impl std::fmt::Debug for SdpServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdpServer")
            .field("records", &self.service_records.len())
            .finish()
    }
}

impl ProtocolHandler for SdpHandler {
    fn psm(&self) -> u16 {
        SDP_PSM
    }

    fn on_data(&mut self, data: &[u8], peer_mtu: u16) -> Option<Vec<u8>> {
        Some(self.server.handle_request(data, peer_mtu))
    }
}

/// Builds a Serial Port Profile service record for `rfcomm_channel`, named
/// `name` — the record a peer reads to learn that this device speaks SPP and
/// on which RFCOMM channel. The shape (ServiceClassIDList +
/// ProtocolDescriptorList of L2CAP then RFCOMM) is what every SPP record
/// carries; SPP spec §5.1.
pub fn spp_service_record(handle: u32, rfcomm_channel: u8, name: &str) -> Service {
    /// Serial Port service class (Assigned Numbers).
    const SERIAL_PORT_SERVICE_CLASS: u16 = 0x1101;

    vec![
        ServiceAttribute {
            id: attribute_id::SERVICE_RECORD_HANDLE,
            value: DataElement::unsigned_integer_32(handle),
        },
        ServiceAttribute {
            id: attribute_id::SERVICE_CLASS_ID_LIST,
            value: DataElement::sequence(vec![DataElement::uuid(SdpUuid::Uuid16(
                SERIAL_PORT_SERVICE_CLASS,
            ))]),
        },
        ServiceAttribute {
            id: attribute_id::PROTOCOL_DESCRIPTOR_LIST,
            value: DataElement::sequence(vec![
                DataElement::sequence(vec![DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID)]),
                DataElement::sequence(vec![
                    DataElement::uuid(SdpUuid::Uuid16(0x0003)), // RFCOMM
                    DataElement::unsigned_integer(u64::from(rfcomm_channel), 1),
                ]),
            ]),
        },
        ServiceAttribute {
            id: attribute_id::BROWSE_GROUP_LIST,
            value: DataElement::sequence(vec![DataElement::uuid(
                SdpUuid::SDP_PUBLIC_BROWSE_ROOT,
            )]),
        },
        ServiceAttribute {
            // ServiceName sits at the primary language base (0x0100) rather
            // than having a fixed id of its own.
            id: 0x0100,
            value: DataElement::text_string(name.as_bytes().to_vec()),
        },
    ]
}

/// A BR/EDR host: discoverability, inbound connections, L2CAP signalling,
/// and per-PSM protocol dispatch.
#[derive(Debug)]
pub struct ClassicHost {
    /// The device's advertised name, returned on a Remote Name Request.
    name: String,
    /// Class of Device (3 octets, little-endian on the wire).
    class_of_device: [u8; 3],
    channels: ClassicChannelManager,
    handlers: Vec<Box<dyn ProtocolHandler>>,
    /// The current ACL connection, if any.
    connection: Option<(u16, Address)>,
    /// Next signalling identifier to use for host-initiated requests.
    next_identifier: u8,
    /// Local CIDs this host has accepted, so channel state can be inspected
    /// (the CID allocator does not expose iteration).
    local_cids: Vec<u16>,
}

impl ClassicHost {
    /// Creates a host advertising `name` with `class_of_device`. A common
    /// Class of Device for an audio peripheral is `[0x04, 0x04, 0x24]`
    /// (rendering / audio-video, wearable headset).
    pub fn new(name: impl Into<String>, class_of_device: [u8; 3]) -> Self {
        Self {
            name: name.into(),
            class_of_device,
            channels: ClassicChannelManager::new(),
            handlers: Vec::new(),
            connection: None,
            next_identifier: 1,
            local_cids: Vec::new(),
        }
    }

    /// Registers a protocol handler and its PSM, so an inbound connection
    /// request for that PSM is accepted and its data routed here.
    pub fn register_handler(
        &mut self,
        handler: Box<dyn ProtocolHandler>,
    ) -> Result<(), SimbleError> {
        self.channels.register_server(handler.psm())?;
        self.handlers.push(handler);
        Ok(())
    }

    /// The current ACL connection as `(handle, peer address)`, if any.
    pub fn connection(&self) -> Option<(u16, Address)> {
        self.connection
    }

    /// Whether any L2CAP channel is open (i.e. a peer has completed the
    /// connect + configure handshake).
    pub fn has_open_channel(&self) -> bool {
        self.local_cids
            .iter()
            .filter_map(|cid| self.channels.get_channel(*cid))
            .any(|channel| channel.is_open())
    }

    /// The controller bring-up that makes this device visible: reset, event
    /// mask, name, Class of Device, Simple Pairing, then inquiry+page scan.
    /// Without Write Scan Enable a BR/EDR device is neither discoverable nor
    /// connectable, which is the usual reason a peer never sees it.
    pub fn start_commands(&self) -> Vec<Vec<u8>> {
        let mut name_param = [0u8; 248];
        let bytes = self.name.as_bytes();
        let len = bytes.len().min(name_param.len());
        name_param[..len].copy_from_slice(&bytes[..len]);

        vec![
            command(opcode::RESET, &[]),
            command(opcode::SET_EVENT_MASK, &[0xFF; 8]),
            command(opcode::WRITE_LOCAL_NAME, &name_param),
            command(opcode::WRITE_CLASS_OF_DEVICE, &self.class_of_device),
            command(opcode::WRITE_SIMPLE_PAIRING_MODE, &[0x01]),
            command(opcode::WRITE_SCAN_ENABLE, &[scan_enable::INQUIRY_AND_PAGE]),
        ]
    }

    /// Handles one H4 packet from the controller, returning what to send back.
    pub fn handle_packet(&mut self, packet: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        match packet.first() {
            Some(&crate::transport::h4_type::HCI_EVENT) => Ok(self.handle_event(packet)),
            Some(&crate::transport::h4_type::HCI_ACL_DATA) => self.handle_acl(packet),
            _ => Ok(Vec::new()),
        }
    }

    fn handle_event(&mut self, packet: &[u8]) -> Vec<Vec<u8>> {
        let Some(event) = HciEvent::parse_h4(packet) else {
            return Vec::new();
        };
        match event {
            HciEvent::ConnectionRequest(request) => {
                // Answer the page, or the peer's connection attempt times
                // out: Accept Connection Request with role 0x01 (remain
                // peripheral, letting the initiator stay central).
                let mut params = Vec::with_capacity(7);
                params.extend_from_slice(&request.bd_addr);
                params.push(0x01);
                vec![command(opcode::ACCEPT_CONNECTION_REQUEST, &params)]
            }
            HciEvent::ConnectionComplete(complete) if complete.status == 0x00 => {
                self.connection = Some((
                    complete.connection_handle.get(),
                    Address::new(complete.bd_addr),
                ));
                Vec::new()
            }
            HciEvent::DisconnectionComplete(_) => {
                self.connection = None;
                // Re-enable scanning so the device is findable again after
                // the peer goes away.
                vec![command(
                    opcode::WRITE_SCAN_ENABLE,
                    &[scan_enable::INQUIRY_AND_PAGE],
                )]
            }
            _ => Vec::new(),
        }
    }

    fn handle_acl(&mut self, packet: &[u8]) -> Result<Vec<Vec<u8>>, SimbleError> {
        use crate::l2cap::HciAclHeader;
        let Some((header, payload)) = HciAclHeader::parse(&packet[1..]) else {
            return Err(SimbleError::PacketParseError("Invalid ACL header".into()));
        };
        let handle = header.handle();
        let Some((l2cap_header, body)) = L2capHeader::ref_from_prefix(payload).ok() else {
            return Ok(Vec::new());
        };
        let channel_id = l2cap_header.cid.get();
        let length = usize::from(l2cap_header.length.get()).min(body.len());
        let body = &body[..length];

        if channel_id == cid::BR_SIGNALING {
            return Ok(self.handle_signaling(handle, body));
        }
        Ok(self.handle_channel_data(handle, channel_id, body))
    }

    /// Drives the L2CAP connect/configure/disconnect handshake through the
    /// channel manager, which owns the state machine.
    fn handle_signaling(&mut self, handle: u16, body: &[u8]) -> Vec<Vec<u8>> {
        let Ok((header, params)) = L2capSignalingHeader::ref_from_prefix(body) else {
            return Vec::new();
        };
        let identifier = header.identifier;
        let length = usize::from(header.length.get()).min(params.len());
        let params = &params[..length];
        let mut out = Vec::new();

        match header.code {
            signaling_code::CONNECTION_REQUEST => {
                let Ok((request, _)) = ConnectionRequestHeader::ref_from_prefix(params) else {
                    return out;
                };
                let Ok(response) = self
                    .channels
                    .on_connection_request(request, DEFAULT_L2CAP_MTU)
                else {
                    return out;
                };
                let local_cid = response.destination_cid.get();
                if local_cid != 0 {
                    self.local_cids.push(local_cid);
                }
                out.push(acl_packet(
                    handle,
                    &signaling_pdu(
                        signaling_code::CONNECTION_RESPONSE,
                        identifier,
                        response.as_bytes(),
                    ),
                ));
                // A server sends its own Configuration Request straight
                // after accepting; the channel opens once both sides have
                // configured.
                if local_cid != 0
                    && let Ok((config, mtu_option)) =
                        self.channels.make_configuration_request(local_cid)
                {
                    let mut payload = config.as_bytes().to_vec();
                    payload.extend_from_slice(&mtu_option);
                    out.push(acl_packet(
                        handle,
                        &signaling_pdu(
                            signaling_code::CONFIGURATION_REQUEST,
                            self.take_identifier(),
                            &payload,
                        ),
                    ));
                }
            }
            signaling_code::CONFIGURATION_REQUEST => {
                let Ok((request, options)) = ConfigurationRequestHeader::ref_from_prefix(params)
                else {
                    return out;
                };
                // The peer addresses our local CID in destination_cid.
                let local_cid = request.destination_cid.get();
                if let Ok(response) = self.channels.on_configuration_request(local_cid, options) {
                    out.push(acl_packet(
                        handle,
                        &signaling_pdu(
                            signaling_code::CONFIGURATION_RESPONSE,
                            identifier,
                            response.as_bytes(),
                        ),
                    ));
                }
            }
            signaling_code::CONFIGURATION_RESPONSE => {
                let Ok((response, _)) = ConfigurationResponseHeader::ref_from_prefix(params) else {
                    return out;
                };
                // source_cid echoes the CID we put in our request — ours.
                let local_cid = response.source_cid.get();
                let _ = self.channels.on_configuration_response(local_cid, response);
            }
            // Echo the request body back as the response, per spec, and
            // drop the channel.
            signaling_code::DISCONNECTION_REQUEST if params.len() >= 4 => {
                {
                    let local_cid = u16::from_le_bytes([params[0], params[1]]);
                    self.channels.remove_channel(local_cid);
                    self.local_cids.retain(|cid| *cid != local_cid);
                    out.push(acl_packet(
                        handle,
                        &signaling_pdu(
                            signaling_code::DISCONNECTION_RESPONSE,
                            identifier,
                            params,
                        ),
                    ));
                }
            }
            _ => {}
        }
        out
    }

    /// Routes an SDU on an open channel to the handler for its PSM.
    fn handle_channel_data(&mut self, handle: u16, cid: u16, data: &[u8]) -> Vec<Vec<u8>> {
        let Some(channel) = self.channels.get_channel(cid) else {
            return Vec::new();
        };
        let (psm, peer_cid, peer_mtu) = (channel.psm, channel.peer_cid, channel.peer_mtu);
        let Some(handler) = self.handlers.iter_mut().find(|h| h.psm() == psm) else {
            return Vec::new();
        };
        match handler.on_data(data, peer_mtu) {
            Some(reply) if !reply.is_empty() => {
                vec![acl_packet(handle, &L2capHeader::serialize(peer_cid, &reply))]
            }
            _ => Vec::new(),
        }
    }

    fn take_identifier(&mut self) -> u8 {
        let id = self.next_identifier;
        self.next_identifier = self.next_identifier.wrapping_add(1).max(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packets::ConnectionResponseHeader;

    fn connection_request_event(addr: [u8; 6]) -> Vec<u8> {
        let mut packet = vec![0x04, 0x04, 0x0A];
        packet.extend_from_slice(&addr);
        packet.extend_from_slice(&[0x04, 0x04, 0x24]); // class of device
        packet.push(0x01); // ACL
        packet
    }

    fn connection_complete_event(handle: u16, addr: [u8; 6]) -> Vec<u8> {
        let mut packet = vec![0x04, 0x03, 0x0B, 0x00];
        packet.extend_from_slice(&handle.to_le_bytes());
        packet.extend_from_slice(&addr);
        packet.push(0x01); // ACL
        packet.push(0x00); // encryption off
        packet
    }

    fn host() -> ClassicHost {
        let mut host = ClassicHost::new("SimbleClassic", [0x04, 0x04, 0x24]);
        host.register_handler(Box::new(SdpHandler::default()))
            .unwrap();
        host
    }

    #[test]
    fn test_bring_up_makes_the_device_discoverable_and_connectable() {
        let commands = host().start_commands();
        // The last command must enable both scans, or a peer never sees it.
        let scan = commands.last().expect("bring-up is not empty");
        assert_eq!(&scan[1..3], &opcode::WRITE_SCAN_ENABLE);
        assert_eq!(scan[4], scan_enable::INQUIRY_AND_PAGE);
        // The name and class of device are set before scanning starts.
        assert!(commands.iter().any(|c| c[1..3] == opcode::WRITE_LOCAL_NAME));
        assert!(
            commands
                .iter()
                .any(|c| c[1..3] == opcode::WRITE_CLASS_OF_DEVICE)
        );
    }

    #[test]
    fn test_inbound_page_is_accepted_and_tracked() {
        let mut host = host();
        let addr = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];

        let out = host.handle_packet(&connection_request_event(addr)).unwrap();
        assert_eq!(
            &out[0][1..3],
            &opcode::ACCEPT_CONNECTION_REQUEST,
            "an unanswered page times out"
        );

        assert!(host.connection().is_none());
        host.handle_packet(&connection_complete_event(0x0080, addr))
            .unwrap();
        let (handle, peer) = host.connection().expect("connection tracked");
        assert_eq!(handle, 0x0080);
        assert_eq!(peer, Address::new(addr));
    }

    #[test]
    fn test_l2cap_handshake_opens_an_sdp_channel() {
        let mut host = host();
        let addr = [0x11; 6];
        host.handle_packet(&connection_request_event(addr)).unwrap();
        host.handle_packet(&connection_complete_event(0x0080, addr))
            .unwrap();

        // Peer opens SDP: Connection Request for PSM 0x0001, source CID 0x0040.
        let request = ConnectionRequestHeader {
            psm: SDP_PSM.into(),
            source_cid: 0x0040u16.into(),
        };
        let pdu = signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes());
        // signaling_pdu already wraps in an L2CAP header; feed it as ACL.
        let out = host.handle_packet(&acl_packet(0x0080, &pdu)).unwrap();
        assert_eq!(out.len(), 2, "connection response, then our config request");

        // The response must accept (result 0x0000) and name our local CID.
        // H4(1) + ACL header(4) + L2CAP header(4) + signalling header(4).
        let response_body = &out[0][13..];
        let (response, _) = ConnectionResponseHeader::ref_from_prefix(response_body).unwrap();
        assert_eq!(response.result.get(), 0x0000, "SDP PSM must be accepted");
        let local_cid = response.destination_cid.get();
        assert_ne!(local_cid, 0);

        // Peer configures us, and acks our configuration: channel opens.
        let mut config = ConfigurationRequestHeader {
            destination_cid: local_cid.into(),
            flags: 0u16.into(),
        }
        .as_bytes()
        .to_vec();
        config.extend_from_slice(&[0x01, 0x02, 0xA0, 0x02]); // MTU option, 672
        host.handle_packet(&acl_packet(
            0x0080,
            &signaling_pdu(signaling_code::CONFIGURATION_REQUEST, 2, &config),
        ))
        .unwrap();

        let ack = ConfigurationResponseHeader {
            source_cid: local_cid.into(),
            flags: 0u16.into(),
            result: 0u16.into(),
        };
        host.handle_packet(&acl_packet(
            0x0080,
            &signaling_pdu(signaling_code::CONFIGURATION_RESPONSE, 1, ack.as_bytes()),
        ))
        .unwrap();

        assert!(
            host.has_open_channel(),
            "both sides configured — the channel must be open"
        );
    }

    #[test]
    fn test_sdp_request_on_an_open_channel_is_answered() {
        let mut host = host();
        let addr = [0x11; 6];
        host.handle_packet(&connection_request_event(addr)).unwrap();
        host.handle_packet(&connection_complete_event(0x0080, addr))
            .unwrap();
        let request = ConnectionRequestHeader {
            psm: SDP_PSM.into(),
            source_cid: 0x0040u16.into(),
        };
        let out = host
            .handle_packet(&acl_packet(
                0x0080,
                &signaling_pdu(signaling_code::CONNECTION_REQUEST, 1, request.as_bytes()),
            ))
            .unwrap();
        let (response, _) =
            ConnectionResponseHeader::ref_from_prefix(&out[0][13..]).unwrap();
        let local_cid = response.destination_cid.get();

        // A malformed SDP request still gets an SDP error response, which
        // proves the data path reaches the server and comes back.
        let out = host.handle_channel_data(0x0080, local_cid, &[0xFF, 0x00, 0x00]);
        assert_eq!(out.len(), 1, "SDP must answer on the same channel");
        let reply = &out[0][9..];
        assert_eq!(
            reply[0], 0x01,
            "SDP ErrorResponse PDU id, i.e. the server ran"
        );
    }

    #[test]
    fn test_spp_record_is_discoverable_through_sdp() {
        // A peer finds SPP by searching for the Serial Port service class;
        // the record must carry the class and the RFCOMM channel.
        let mut handler = SdpHandler::default();
        handler
            .server_mut()
            .service_records
            .insert(0x00010001, spp_service_record(0x00010001, 3, "Simble SPP"));

        let record = &handler.server_mut().service_records[&0x00010001];
        let class_list = record
            .iter()
            .find(|a| a.id == attribute_id::SERVICE_CLASS_ID_LIST)
            .expect("record names its service class");
        assert_eq!(
            class_list.value,
            DataElement::sequence(vec![DataElement::uuid(SdpUuid::Uuid16(0x1101))])
        );

        // The protocol descriptor must be L2CAP then RFCOMM/channel 3, or a
        // peer cannot work out where to connect.
        let protocols = record
            .iter()
            .find(|a| a.id == attribute_id::PROTOCOL_DESCRIPTOR_LIST)
            .expect("record names its protocol stack");
        let DataElement::Sequence(layers) = &protocols.value else {
            panic!("protocol descriptor list must be a sequence");
        };
        assert_eq!(layers.len(), 2);
        assert_eq!(
            layers[1],
            DataElement::sequence(vec![
                DataElement::uuid(SdpUuid::Uuid16(0x0003)),
                DataElement::unsigned_integer(3, 1),
            ])
        );
    }

    #[test]
    fn test_disconnect_restores_discoverability() {
        let mut host = host();
        let addr = [0x11; 6];
        host.handle_packet(&connection_request_event(addr)).unwrap();
        host.handle_packet(&connection_complete_event(0x0080, addr))
            .unwrap();

        let out = host
            .handle_packet(&[0x04, 0x05, 0x04, 0x00, 0x80, 0x00, 0x13])
            .unwrap();
        assert!(host.connection().is_none());
        assert_eq!(&out[0][1..3], &opcode::WRITE_SCAN_ENABLE);
        assert_eq!(out[0][4], scan_enable::INQUIRY_AND_PAGE);
    }
}
