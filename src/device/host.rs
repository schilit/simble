// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! The LE **host**: the layer between a [`VirtualDevice`]'s GATT/SMP state
//! and an HCI controller. It consumes HCI events and produces the commands
//! and ACL data the controller must send — connection bring-up, advertising
//! setup, the Long Term Key reply that starts encryption, ATT/SMP traffic,
//! and re-advertising after a disconnect.
//!
//! This is deliberately transport-free: `handle_packet` takes bytes in and
//! returns the H4 packets to send back, so the same host runs over the
//! in-process scene radio, netsim, a USB dongle, or a test harness. Events
//! are parsed through the typed views in [`crate::packets::hci_events`]
//! rather than by indexing raw bytes.

use crate::device::VirtualDevice;
use crate::gap::advertising::build_adv_payload_with_extras;
use crate::l2cap::{AclPacketBoundary, AclReassembler, HciAclHeader};
use crate::packets::hci_events::HciEvent;
use crate::types::{Address, AddressType, SimbleError};
use zerocopy::IntoBytes;

/// HCI command opcodes this host issues (Vol 4, Part E, Section 7.8).
pub(crate) mod opcode {
    /// Reset.
    pub const RESET: [u8; 2] = [0x03, 0x0C];
    /// Set Event Mask.
    pub const SET_EVENT_MASK: [u8; 2] = [0x01, 0x0C];
    /// LE Set Event Mask.
    pub const LE_SET_EVENT_MASK: [u8; 2] = [0x01, 0x20];
    /// LE Set Advertising Parameters.
    pub const LE_SET_ADVERTISING_PARAMETERS: [u8; 2] = [0x06, 0x20];
    /// LE Set Advertising Data.
    pub const LE_SET_ADVERTISING_DATA: [u8; 2] = [0x08, 0x20];
    /// LE Set Scan Response Data.
    pub const LE_SET_SCAN_RESPONSE_DATA: [u8; 2] = [0x09, 0x20];
    /// LE Set Advertising Enable.
    pub const LE_SET_ADVERTISING_ENABLE: [u8; 2] = [0x0A, 0x20];
    /// LE Long Term Key Request Reply.
    pub const LE_LTK_REQUEST_REPLY: [u8; 2] = [0x1A, 0x20];
    /// LE Long Term Key Request Negative Reply.
    pub const LE_LTK_REQUEST_NEGATIVE_REPLY: [u8; 2] = [0x1B, 0x20];
    /// LE Accept CIS Request.
    pub const LE_ACCEPT_CIS_REQUEST: [u8; 2] = [0x66, 0x20];
    /// LE Set CIG Parameters — a central defines the isochronous group and
    /// learns the CIS handles it may then create streams on.
    pub const LE_SET_CIG_PARAMETERS: [u8; 2] = [0x62, 0x20];
    /// LE Create CIS — a central opens the stream to a peer's ACL link.
    pub const LE_CREATE_CIS: [u8; 2] = [0x64, 0x20];
    /// LE Setup ISO Data Path.
    pub const LE_SETUP_ISO_DATA_PATH: [u8; 2] = [0x6E, 0x20];
    /// LE Set Host Feature.
    pub const LE_SET_HOST_FEATURE: [u8; 2] = [0x74, 0x20];
}

/// LE host feature bit numbers (Assigned Numbers, "LE Host Feature Bits").
pub(crate) mod le_host_feature {
    /// Connected Isochronous Stream, Host Support. A controller refuses
    /// LE Create CIS with Command Disallowed until both hosts have set
    /// this — which is why a CIS cannot be opened to a device that never
    /// declares it, however complete its ASCS looks.
    pub const CONNECTED_ISOCHRONOUS_STREAM: u8 = 32;
}

/// Data path directions for LE Setup ISO Data Path (Vol 4, Part E, 7.8.109).
pub(crate) mod iso_data_path {
    /// Host to Controller — the device is a source, sending audio.
    pub const INPUT: u8 = 0x00;
    /// Controller to Host — the device is a sink, receiving audio.
    pub const OUTPUT: u8 = 0x01;
    /// Data path over HCI (as opposed to a vendor path).
    pub const HCI: u8 = 0x00;
}

/// Advertising types for LE Set Advertising Parameters.
mod adv_type {
    /// Connectable undirected — a normal peripheral.
    pub const ADV_IND: u8 = 0x00;
    /// Non-connectable undirected — a pure beacon (iBeacon, Eddystone,
    /// Quick Share nudge): broadcast only, no connections, no scan response.
    pub const ADV_NONCONN_IND: u8 = 0x03;
}

/// The default LE ACL data length. A host must fragment L2CAP PDUs to this,
/// as real stacks do (Android fragments its own 65-byte SMP Public Key);
/// oversized fragments are dropped or mis-handled by real peers.
const LE_ACL_DATA_LEN: usize = 27;

/// Builds one H4 HCI command packet.
pub fn command(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
    let mut packet = Vec::with_capacity(4 + params.len());
    packet.push(crate::transport::h4_type::HCI_COMMAND);
    packet.extend_from_slice(&opcode);
    packet.push(params.len() as u8);
    packet.extend_from_slice(params);
    packet
}

/// Pads an advertising payload into the fixed 32-byte HCI parameter block
/// (significant length byte + 31 data bytes) of LE Set Advertising Data /
/// LE Set Scan Response Data.
pub fn adv_data_param(payload: &[u8]) -> Vec<u8> {
    let mut param = vec![payload.len() as u8];
    param.extend_from_slice(payload);
    param.resize(32, 0x00);
    param
}

/// Splits one L2CAP PDU into the H4 ACL packets that carry it, flagging the
/// first fragment and any continuations.
pub fn acl_packets(handle: u16, l2cap: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut first = true;
    for chunk in l2cap.chunks(LE_ACL_DATA_LEN) {
        let boundary = if first {
            AclPacketBoundary::FirstNonFlushable
        } else {
            AclPacketBoundary::Continuing
        };
        first = false;
        let header = HciAclHeader::new(handle, boundary, chunk.len() as u16);
        let mut packet = Vec::with_capacity(5 + chunk.len());
        packet.push(crate::transport::h4_type::HCI_ACL_DATA);
        packet.extend_from_slice(header.as_bytes());
        packet.extend_from_slice(chunk);
        packets.push(packet);
    }
    packets
}

/// The controller bring-up every host does first. The post-Reset default
/// event mask excludes LE Meta Events (Vol 4, Part E, Section 7.3.1, bit
/// 61), so both masks must be opened before any advertising report or
/// connection event can arrive.
pub fn init_commands() -> Vec<Vec<u8>> {
    vec![
        command(opcode::RESET, &[]),
        command(opcode::SET_EVENT_MASK, &[0xFF; 8]),
        command(opcode::LE_SET_EVENT_MASK, &[0xFF; 8]),
        // Declare CIS host support, or the controller disallows LE Create CIS
        // and no isochronous stream can ever be opened to this device.
        command(
            opcode::LE_SET_HOST_FEATURE,
            &[le_host_feature::CONNECTED_ISOCHRONOUS_STREAM, 0x01],
        ),
    ]
}

/// The LE host state for one device: ACL reassembly and the current
/// connection. Owns no transport — see the module docs.
#[derive(Debug, Default)]
pub struct LeHost {
    reassembler: AclReassembler,
    connection: Option<(u16, Address)>,
    /// The established isochronous stream's handle, once a CIS is up. ISO
    /// SDUs are addressed to this handle, never to the ACL handle.
    cis_handle: Option<u16>,
}

impl LeHost {
    /// Creates a host with no connection.
    pub fn new() -> Self {
        Self::default()
    }

    /// The current connection as `(handle, peer address)`, if any.
    pub fn connection(&self) -> Option<(u16, Address)> {
        self.connection
    }

    /// The established CIS handle, if an isochronous stream is up.
    pub fn cis_handle(&self) -> Option<u16> {
        self.cis_handle
    }

    /// Forgets the current connection (used when a scene tears a device
    /// down without a Disconnection Complete arriving).
    pub fn clear_connection(&mut self) {
        self.connection = None;
        self.cis_handle = None;
    }

    /// The full bring-up for advertising `device`: reset, event masks,
    /// advertising parameters (connectable or beacon), advertising data,
    /// an optional scan response, then advertising enable. `service_uuids`
    /// are the 16-bit primary services to advertise.
    pub fn start_advertising(
        &self,
        device: &VirtualDevice,
        service_uuids: &[u16],
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        let mut commands = init_commands();

        // 100ms interval, public own address, all channels, no filter.
        let advertising_type = if device.connectable {
            adv_type::ADV_IND
        } else {
            adv_type::ADV_NONCONN_IND
        };
        commands.push(command(
            opcode::LE_SET_ADVERTISING_PARAMETERS,
            &[
                0xA0,
                0x00,
                0xA0,
                0x00,
                advertising_type,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x00,
                0x07,
                0x00,
            ],
        ));

        let payload = build_adv_payload_with_extras(
            &device.name,
            service_uuids,
            device.advertising_data.as_ref(),
        )?;
        commands.push(command(
            opcode::LE_SET_ADVERTISING_DATA,
            &adv_data_param(&payload),
        ));

        // Only a scannable advertisement (ADV_IND) has a scan response; a
        // non-connectable beacon never solicits a scan request.
        if device.connectable {
            let scan_response = crate::gap::AdvertisingData::new()
                .with_name(&device.name)
                .to_bytes();
            commands.push(command(
                opcode::LE_SET_SCAN_RESPONSE_DATA,
                &adv_data_param(&scan_response),
            ));
        }

        commands.push(command(opcode::LE_SET_ADVERTISING_ENABLE, &[0x01]));
        Ok(commands)
    }

    /// Handles one H4 packet from the controller, updating `device` and
    /// returning the H4 packets to send back.
    pub fn handle_packet(
        &mut self,
        device: &mut VirtualDevice,
        packet: &[u8],
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        match packet.first() {
            Some(&crate::transport::h4_type::HCI_EVENT) => Ok(self.handle_event(device, packet)),
            Some(&crate::transport::h4_type::HCI_ACL_DATA) => self.handle_acl(device, packet),
            Some(&crate::transport::h4_type::HCI_ISO_DATA) => {
                // The media plane: an SDU from the peer goes to the device's
                // audio inbox. Nothing is sent back — ISO is unacknowledged.
                if let Some(sdu) = crate::packets::parse_iso_packet(packet) {
                    device.on_iso_sdu(sdu.payload);
                }
                Ok(Vec::new())
            }
            _ => Ok(Vec::new()),
        }
    }

    fn handle_event(&mut self, device: &mut VirtualDevice, packet: &[u8]) -> Vec<Vec<u8>> {
        let Some(event) = HciEvent::parse_h4(packet) else {
            return Vec::new();
        };
        match event {
            HciEvent::LeConnectionComplete(event) if event.status == 0x00 => {
                let handle = event.connection_handle.get() & 0x0FFF;
                let peer = Address::new(event.peer_address);
                // The address type is mixed into SMP's pairing crypto, so it
                // must reach the connection: 0x01/0x03 are random (0x02/0x03
                // being the Enhanced event's resolved identity addresses).
                let peer_type = match event.peer_address_type {
                    0x01 | 0x03 => AddressType::Random,
                    _ => AddressType::Public,
                };
                device.on_connected(handle, peer);
                device.set_peer_address_type(handle, peer_type);
                self.connection = Some((handle, peer));
                Vec::new()
            }
            HciEvent::LeLongTermKeyRequest(event) => {
                // The controller is starting encryption and wants the key:
                // the session's LTK (Secure Connections) or STK (legacy),
                // recorded on the connection during SMP. Without this reply
                // encryption never starts and the peer fails the pairing.
                let handle = event.connection_handle.get() & 0x0FFF;
                match device.connections.get(&handle).and_then(|c| c.ltk) {
                    Some(key) => {
                        let mut params = Vec::with_capacity(18);
                        params.extend_from_slice(&handle.to_le_bytes());
                        params.extend_from_slice(&key);
                        vec![command(opcode::LE_LTK_REQUEST_REPLY, &params)]
                    }
                    None => vec![command(
                        opcode::LE_LTK_REQUEST_NEGATIVE_REPLY,
                        &handle.to_le_bytes(),
                    )],
                }
            }
            HciEvent::EncryptionChange(event) if event.status == 0x00 => {
                // Phase-3 SMP key distribution runs over the encrypted link
                // only; telling the device unblocks a deferred session.
                let handle = event.connection_handle.get() & 0x0FFF;
                device.on_encryption_changed(handle, event.encryption_enabled != 0);
                self.drain_smp(device, handle)
            }
            HciEvent::LeCisRequest(event) => {
                // A central wants an isochronous stream. Accept it: the
                // controller answers with LE CIS Established, which is where
                // the data path gets set up.
                vec![command(
                    opcode::LE_ACCEPT_CIS_REQUEST,
                    &event.cis_connection_handle.get().to_le_bytes(),
                )]
            }
            HciEvent::LeCisEstablished(event) if event.status == 0x00 => {
                let handle = event.cis_connection_handle.get();
                self.cis_handle = Some(handle);
                device.cis_handle = Some(handle);
                // Open the receive path so the controller delivers SDUs up to
                // the host. A sink only needs Output; a source would also set
                // up Input. Codec ID 0x03 = "transparent": the codec runs in
                // the host, which is where a decoder would live.
                let mut params = Vec::with_capacity(13);
                params.extend_from_slice(&handle.to_le_bytes());
                params.push(iso_data_path::OUTPUT);
                params.push(iso_data_path::HCI);
                params.extend_from_slice(&[0x03, 0x00, 0x00, 0x00, 0x00]); // transparent codec
                params.extend_from_slice(&[0x00, 0x00, 0x00]); // controller delay
                params.push(0x00); // codec configuration length
                vec![command(opcode::LE_SETUP_ISO_DATA_PATH, &params)]
            }
            HciEvent::DisconnectionComplete(event) => {
                let handle = event.connection_handle.get() & 0x0FFF;
                device.on_disconnected(handle);
                self.reassembler.on_disconnected(handle);
                self.connection = None;
                // The controller stops advertising when a connection is
                // established (Vol 6, Part B, Section 4.4.2); re-enable so
                // the device is discoverable again.
                vec![command(opcode::LE_SET_ADVERTISING_ENABLE, &[0x01])]
            }
            _ => Vec::new(),
        }
    }

    fn handle_acl(
        &mut self,
        device: &mut VirtualDevice,
        packet: &[u8],
    ) -> Result<Vec<Vec<u8>>, SimbleError> {
        let Some((header, payload)) = HciAclHeader::parse(&packet[1..]) else {
            return Err(SimbleError::PacketParseError("Invalid ACL header".into()));
        };
        let handle = header.handle();
        let is_first = header.is_first_fragment();
        let Some(frame) = self.reassembler.push_fragment(handle, is_first, payload)? else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        if let Some(l2cap) = device.process_l2cap_packet(handle, &frame)? {
            out.extend(acl_packets(handle, &l2cap));
        }
        // An SMP exchange queues follow-on PDUs beyond the immediate reply
        // (the responder's Pairing Random, DHKey Check, key distribution) —
        // send them, or the peer's pairing stalls and times out.
        out.extend(self.drain_smp(device, handle));
        Ok(out)
    }

    /// Drains every SMP PDU the device has queued for `handle`.
    fn drain_smp(&mut self, device: &mut VirtualDevice, handle: u16) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        while let Some(pdu) = device.poll_smp_pdu(handle) {
            out.extend(acl_packets(handle, &pdu));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gatt::GattDatabase;

    fn test_device(connectable: bool) -> VirtualDevice {
        let mut device = VirtualDevice::new(
            "HostTest",
            "AA:BB:CC:00:00:01".parse().unwrap(),
            AddressType::Public,
        );
        device.gatt_db = GattDatabase::new();
        device.connectable = connectable;
        device
    }

    #[test]
    fn test_start_advertising_shapes_the_bring_up() {
        let host = LeHost::new();
        let commands = host.start_advertising(&test_device(true), &[0x180D]).unwrap();
        // Reset, both event masks, CIS host feature, adv params, adv data,
        // scan response, enable.
        assert_eq!(commands.len(), 8, "{commands:?}");
        assert_eq!(
            &commands[3][1..3],
            &[0x74, 0x20],
            "LE Set Host Feature declares CIS support"
        );
        assert_eq!(&commands[3][4..6], &[32, 0x01], "bit 32 = CIS host support");
        let params = &commands[4];
        assert_eq!(&params[1..3], &[0x06, 0x20], "LE Set Advertising Parameters");
        assert_eq!(params[8], adv_type::ADV_IND);
        assert!(
            commands.iter().any(|c| c[1..3] == [0x09, 0x20]),
            "connectable device sends a scan response"
        );

        let beacon = host
            .start_advertising(&test_device(false), &[0x180D])
            .unwrap();
        assert_eq!(beacon[4][8], adv_type::ADV_NONCONN_IND);
        assert!(
            !beacon.iter().any(|c| c[1..3] == [0x09, 0x20]),
            "a beacon sends no scan response"
        );
    }

    #[test]
    fn test_connection_complete_records_peer_identity() {
        let mut host = LeHost::new();
        let mut device = test_device(true);
        let mut packet = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x01];
        packet.extend_from_slice(&[0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        packet.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        let out = host.handle_packet(&mut device, &packet).unwrap();
        assert!(out.is_empty(), "a connection needs no reply");
        assert_eq!(host.connection().unwrap().0, 0x0040);
        let conn = device.connections.get(&0x0040).expect("connected");
        assert_eq!(conn.peer_address_type, AddressType::Random);
    }

    #[test]
    fn test_ltk_request_answers_with_the_session_key() {
        let mut host = LeHost::new();
        let mut device = test_device(true);
        let mut connect = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x00];
        connect.extend_from_slice(&[0x11; 6]);
        connect.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        host.handle_packet(&mut device, &connect).unwrap();

        // With no key, the host must answer with a negative reply…
        let mut request = vec![0x04, 0x3E, 0x0D, 0x05, 0x40, 0x00];
        request.extend_from_slice(&[0x00; 10]);
        let out = host.handle_packet(&mut device, &request).unwrap();
        assert_eq!(&out[0][1..3], &[0x1B, 0x20], "negative reply");

        // …and with one, the key itself.
        let key = [0xAB; 16];
        device.connections.get_mut(&0x0040).unwrap().ltk = Some(key);
        let out = host.handle_packet(&mut device, &request).unwrap();
        assert_eq!(&out[0][1..3], &[0x1A, 0x20], "LTK request reply");
        assert_eq!(&out[0][6..22], &key);
    }

    #[test]
    fn test_cis_request_is_accepted_and_opens_the_data_path() {
        // The LE Audio media plane's handshake: a central asks for an
        // isochronous stream, the sink accepts, and on establishment opens
        // the controller-to-host path so SDUs are delivered.
        let mut host = LeHost::new();
        let mut device = test_device(true);

        // LE CIS Request: acl handle 0x0040, cis handle 0x0060, CIG 1, CIS 1.
        let request = [
            0x04, 0x3E, 0x07, 0x1A, 0x40, 0x00, 0x60, 0x00, 0x01, 0x01,
        ];
        let out = host.handle_packet(&mut device, &request).unwrap();
        assert_eq!(&out[0][1..3], &[0x66, 0x20], "LE Accept CIS Request");
        assert_eq!(&out[0][4..6], &[0x60, 0x00], "for the offered CIS handle");
        assert!(host.cis_handle().is_none(), "not established yet");

        // LE CIS Established (status 0, cis handle 0x0060) + timing fields.
        let mut established = vec![0x04, 0x3E, 0x1D, 0x19, 0x00, 0x60, 0x00];
        established.extend_from_slice(&[0x00; 26]);
        let out = host.handle_packet(&mut device, &established).unwrap();
        assert_eq!(host.cis_handle(), Some(0x0060), "stream handle recorded");
        assert_eq!(&out[0][1..3], &[0x6E, 0x20], "LE Setup ISO Data Path");
        assert_eq!(&out[0][4..6], &[0x60, 0x00], "on the CIS handle");
        assert_eq!(out[0][6], 0x01, "direction: controller to host (a sink)");
        assert_eq!(out[0][7], 0x00, "data path over HCI");

        // A failed establishment must not record a handle.
        let mut failed = vec![0x04, 0x3E, 0x1D, 0x19, 0x01, 0x70, 0x00];
        failed.extend_from_slice(&[0x00; 26]);
        let mut fresh = LeHost::new();
        let out = fresh.handle_packet(&mut device, &failed).unwrap();
        assert!(out.is_empty());
        assert!(fresh.cis_handle().is_none());
    }

    #[test]
    fn test_disconnect_re_enables_advertising() {
        let mut host = LeHost::new();
        let mut device = test_device(true);
        let mut connect = vec![0x04, 0x3E, 0x13, 0x01, 0x00, 0x40, 0x00, 0x01, 0x00];
        connect.extend_from_slice(&[0x11; 6]);
        connect.extend_from_slice(&[0x18, 0x00, 0x00, 0x00, 0x48, 0x00, 0x00]);
        host.handle_packet(&mut device, &connect).unwrap();

        let out = host
            .handle_packet(&mut device, &[0x04, 0x05, 0x04, 0x00, 0x40, 0x00, 0x13])
            .unwrap();
        assert_eq!(&out[0][1..3], &[0x0A, 0x20], "advertising re-enabled");
        assert_eq!(out[0][4], 0x01, "enable = 1");
        assert!(host.connection().is_none());
    }

    #[test]
    fn test_acl_fragments_long_pdus() {
        let l2cap = vec![0xAB; 65]; // an SMP public key's worth
        let packets = acl_packets(0x0040, &l2cap);
        assert_eq!(packets.len(), 3, "27 + 27 + 11");
        // First fragment carries PB=0b00, the rest PB=0b01 (continuation).
        assert_eq!(packets[0][2] >> 4 & 0x03, 0b00);
        assert_eq!(packets[1][2] >> 4 & 0x03, 0b01);
        assert_eq!(packets[2][2] >> 4 & 0x03, 0b01);
        let total: usize = packets.iter().map(|p| p.len() - 5).sum();
        assert_eq!(total, l2cap.len());
    }

    #[test]
    fn test_unknown_and_malformed_packets_are_ignored() {
        let mut host = LeHost::new();
        let mut device = test_device(true);
        // A command-complete event the host doesn't act on.
        assert!(
            host.handle_packet(&mut device, &[0x04, 0x0E, 0x04, 0x01, 0x03, 0x0C, 0x00])
                .unwrap()
                .is_empty()
        );
        // Something that isn't an event or ACL data at all.
        assert!(host.handle_packet(&mut device, &[0x09, 0x01]).unwrap().is_empty());
    }
}
