// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy GATT Client protocol engine for service and characteristic discovery.

use zerocopy::{FromBytes, IntoBytes};

use crate::att::{
    AttErrorRsp, AttExchangeMtuReq, AttReadBlobReq, AttReadByGroupTypeRspHeader,
    AttReadByTypeItemHeader, AttReadByTypeReqHeader, AttReadByTypeRspHeader, AttReadReq,
    AttWriteReqHeader, opcode,
};
use crate::gatt::CharacteristicDecl;
use crate::l2cap::{L2capHeader, cid};
use crate::types::{Address, SimbleError, Uuid};

/// Discovered GATT Service on a remote peripheral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredService {
    /// Start handle.
    pub start_handle: u16,
    /// End handle.
    pub end_handle: u16,
    /// Uuid.
    pub uuid: Uuid,
    /// Characteristics.
    pub characteristics: Vec<DiscoveredCharacteristic>,
}

/// Discovered GATT Characteristic on a remote peripheral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCharacteristic {
    /// Declaration handle.
    pub declaration_handle: u16,
    /// Value handle.
    pub value_handle: u16,
    /// Properties.
    pub properties: u8,
    /// Uuid.
    pub uuid: Uuid,
    /// Descriptors.
    pub descriptors: Vec<DiscoveredDescriptor>,
}

/// Discovered GATT Descriptor on a remote peripheral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDescriptor {
    /// Handle.
    pub handle: u16,
    /// Uuid.
    pub uuid: Uuid,
}

/// Helper function to construct ATT handle-range requests with a 16-bit UUID.
fn create_range_request(opcode: u8, start_handle: u16, end_handle: u16, uuid_16: u16) -> Vec<u8> {
    let mut pdu = Vec::with_capacity(7);
    let header = AttReadByTypeReqHeader {
        opcode,
        start_handle: zerocopy::byteorder::little_endian::U16::from_bytes(
            start_handle.to_le_bytes(),
        ),
        end_handle: zerocopy::byteorder::little_endian::U16::from_bytes(end_handle.to_le_bytes()),
    };
    pdu.extend_from_slice(header.as_bytes());
    pdu.extend_from_slice(&uuid_16.to_le_bytes());
    L2capHeader::serialize(cid::ATT, &pdu)
}

/// A lightweight, zero-copy GATT Client discovery manager.
#[derive(Debug, Clone)]
pub struct GattClient {
    /// Connection handle.
    pub connection_handle: u16,
    /// Peer address.
    pub peer_address: Address,
    /// Mtu.
    pub mtu: u16,
    /// Services.
    pub services: Vec<DiscoveredService>,
}

impl GattClient {
    /// Creates a new GATT client for a connection.
    pub fn new(connection_handle: u16, peer_address: Address) -> Self {
        Self {
            connection_handle,
            peer_address,
            mtu: 23,
            services: Vec::new(),
        }
    }

    /// Generates an Exchange MTU Request L2CAP packet.
    pub fn create_exchange_mtu_request(&self, client_mtu: u16) -> Vec<u8> {
        let req = AttExchangeMtuReq::new(client_mtu);
        L2capHeader::serialize(cid::ATT, req.as_bytes())
    }

    /// Handles an Exchange MTU Response from the peripheral.
    pub fn on_exchange_mtu_response(&mut self, server_mtu: u16, client_mtu: u16) {
        self.mtu = server_mtu.min(client_mtu).max(23);
    }

    /// Generates a Primary Service Discovery (Read By Group Type) Request.
    pub fn create_discover_services_request(&self, start_handle: u16, end_handle: u16) -> Vec<u8> {
        create_range_request(
            opcode::READ_BY_GROUP_TYPE_REQ,
            start_handle,
            end_handle,
            0x2800,
        )
    }

    /// Processes a Read By Group Type Response containing primary services.
    pub fn on_discover_services_response(&mut self, payload: &[u8]) -> Result<(), SimbleError> {
        let Some((header, attribute_data_list)) = AttReadByGroupTypeRspHeader::parse(payload)
        else {
            return Err(SimbleError::PacketParseError(
                "Invalid ReadByGroupType response".into(),
            ));
        };

        // Each entry is a handle range plus the service UUID, so anything
        // shorter than the range itself cannot be walked.
        let Some(items) = header.items(attribute_data_list) else {
            return Err(SimbleError::PacketParseError(
                "Invalid item length in service rsp".into(),
            ));
        };

        for (entry, uuid_bytes) in items {
            let Some(uuid) = Uuid::from_bytes(uuid_bytes) else {
                continue;
            };

            self.services.push(DiscoveredService {
                start_handle: entry.attribute_handle.get(),
                end_handle: entry.end_group_handle.get(),
                uuid,
                characteristics: Vec::new(),
            });
        }

        Ok(())
    }

    /// Generates a Characteristic Discovery (Read By Type) Request for a service range.
    pub fn create_discover_characteristics_request(
        &self,
        start_handle: u16,
        end_handle: u16,
    ) -> Vec<u8> {
        create_range_request(opcode::READ_BY_TYPE_REQ, start_handle, end_handle, 0x2803)
    }

    /// Processes a Read By Type Response containing characteristics.
    pub fn on_discover_characteristics_response(
        &mut self,
        service_uuid: Uuid,
        payload: &[u8],
    ) -> Result<(), SimbleError> {
        let Some((header, attribute_data_list)) = AttReadByTypeRspHeader::parse(payload) else {
            return Err(SimbleError::PacketParseError(
                "Invalid ReadByType response".into(),
            ));
        };

        // A characteristic declaration value is properties (1) + value handle
        // (2) + UUID, so an entry below the declaration handle plus those
        // three octets is not a characteristic list at all.
        let min_item_len = size_of::<AttReadByTypeItemHeader>() + size_of::<CharacteristicDecl>();
        if usize::from(header.length) < min_item_len {
            return Err(SimbleError::PacketParseError(
                "Invalid item length in char rsp".into(),
            ));
        }
        let items = header
            .items(attribute_data_list)
            .expect("length checked above");

        let service = self
            .services
            .iter_mut()
            .find(|s| s.uuid == service_uuid)
            .ok_or_else(|| SimbleError::Gatt(format!("Service {service_uuid} not discovered")))?;

        for (entry, value) in items {
            // The declaration's own value: properties, value handle, UUID.
            let Ok((decl, uuid_bytes)) = CharacteristicDecl::ref_from_prefix(value) else {
                continue;
            };
            let Some(uuid) = Uuid::from_bytes(uuid_bytes) else {
                continue;
            };

            service.characteristics.push(DiscoveredCharacteristic {
                declaration_handle: entry.attribute_handle.get(),
                value_handle: decl.value_handle.get(),
                properties: decl.properties,
                uuid,
                descriptors: Vec::new(),
            });
        }

        Ok(())
    }

    /// Generates a Read Request L2CAP packet for an attribute handle.
    pub fn create_read_request(&self, handle: u16) -> Vec<u8> {
        let req = AttReadReq {
            opcode: opcode::READ_REQ,
            handle: zerocopy::byteorder::little_endian::U16::from_bytes(handle.to_le_bytes()),
        };
        L2capHeader::serialize(cid::ATT, req.as_bytes())
    }

    /// Generates a Read Blob Request L2CAP packet for reading long attributes at an offset.
    pub fn create_read_blob_request(&self, handle: u16, offset: u16) -> Vec<u8> {
        let req = AttReadBlobReq::new(handle, offset);
        L2capHeader::serialize(cid::ATT, req.as_bytes())
    }

    fn create_write_pdu(&self, opcode: u8, handle: u16, value: &[u8]) -> Vec<u8> {
        let mut pdu = Vec::with_capacity(3 + value.len());
        let header = AttWriteReqHeader::new(opcode, handle);
        pdu.extend_from_slice(header.as_bytes());
        pdu.extend_from_slice(value);
        L2capHeader::serialize(cid::ATT, &pdu)
    }

    /// Generates a Write Request L2CAP packet (with response).
    pub fn create_write_request(&self, handle: u16, value: &[u8]) -> Vec<u8> {
        self.create_write_pdu(opcode::WRITE_REQ, handle, value)
    }

    /// Generates a Write Command L2CAP packet (without response).
    pub fn create_write_command(&self, handle: u16, value: &[u8]) -> Vec<u8> {
        self.create_write_pdu(opcode::WRITE_CMD, handle, value)
    }

    /// Finds a discovered characteristic by UUID across all discovered services.
    pub fn find_characteristic(&self, uuid: Uuid) -> Option<&DiscoveredCharacteristic> {
        for service in &self.services {
            if let Some(ch) = service.characteristics.iter().find(|c| c.uuid == uuid) {
                return Some(ch);
            }
        }
        None
    }

    /// Processes an ATT Error Response PDU.
    pub fn parse_error_response(payload: &[u8]) -> Option<AttErrorRsp> {
        if payload.len() < 5 || payload[0] != opcode::ERROR_RSP {
            return None;
        }
        let (rsp, _) = AttErrorRsp::parse(payload)?;
        Some(*rsp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gatt_client_service_and_char_discovery() {
        let addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let mut client = GattClient::new(0x0001, addr);

        // 1. Discover Services Request
        let req = client.create_discover_services_request(0x0001, 0xFFFF);
        let (_, payload) = L2capHeader::parse(&req).expect("Valid L2CAP");
        assert_eq!(payload[0], opcode::READ_BY_GROUP_TYPE_REQ);

        // 2. Parse Services Response (HRS 0x180D from handle 0x0001 to 0x0005)
        let mut svc_rsp = vec![opcode::READ_BY_GROUP_TYPE_RSP, 6]; // item_len = 6
        svc_rsp.extend_from_slice(&1u16.to_le_bytes()); // start
        svc_rsp.extend_from_slice(&5u16.to_le_bytes()); // end
        svc_rsp.extend_from_slice(&0x180Du16.to_le_bytes()); // HRS UUID
        client
            .on_discover_services_response(&svc_rsp)
            .expect("Valid parse");
        assert_eq!(client.services.len(), 1);
        assert_eq!(client.services[0].uuid, Uuid::from_u16(0x180D));

        // 3. Discover Characteristics Response
        let mut char_rsp = vec![opcode::READ_BY_TYPE_RSP, 7]; // item_len = 7
        char_rsp.extend_from_slice(&2u16.to_le_bytes()); // decl handle
        char_rsp.push(0x10); // NOTIFY
        char_rsp.extend_from_slice(&3u16.to_le_bytes()); // val handle
        char_rsp.extend_from_slice(&0x2A37u16.to_le_bytes()); // Heart Rate Measurement UUID
        client
            .on_discover_characteristics_response(Uuid::from_u16(0x180D), &char_rsp)
            .expect("Valid char parse");

        assert_eq!(client.services[0].characteristics.len(), 1);
        assert_eq!(
            client.services[0].characteristics[0].uuid,
            Uuid::from_u16(0x2A37)
        );
        assert_eq!(client.services[0].characteristics[0].value_handle, 3);
    }

    /// A one-octet ATT PDU used to **panic the process**.
    ///
    /// Both discovery handlers guarded with `payload.is_empty()`, which only
    /// proves length >= 1, and then indexed `payload[1]` for the item length.
    /// A peer sending a bare `[0x11]` or `[0x09]` therefore crashed a simble
    /// central mid-discovery with an index-out-of-bounds -- reachable from the
    /// wire, since `central.rs` and `ranging_scene.rs` dispatch on `att[0]`
    /// and hand the payload straight in. Any buggy or hostile remote device
    /// could do it.
    ///
    /// The typed headers need two octets to parse, so a truncated PDU is now
    /// a parse error. This test exists because that is a remote crash, and a
    /// remote crash deserves a test that fails loudly if the guard regresses.
    #[test]
    fn truncated_discovery_responses_are_rejected_not_panicked() {
        for pdu in [
            vec![opcode::READ_BY_GROUP_TYPE_RSP],
            vec![opcode::READ_BY_TYPE_RSP],
        ] {
            let addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
            let mut client = GattClient::new(0x0001, addr);
            let result = if pdu[0] == opcode::READ_BY_GROUP_TYPE_RSP {
                client.on_discover_services_response(&pdu)
            } else {
                client.on_discover_characteristics_response(Uuid::from_u16(0x180D), &pdu)
            };
            assert!(
                result.is_err(),
                "a 1-octet {pdu:02X?} must be a parse error, not a panic",
            );
        }
        // And the empty PDU, which the original guard did handle.
        let addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
        let mut client = GattClient::new(0x0001, addr);
        assert!(client.on_discover_services_response(&[]).is_err());
    }
}
