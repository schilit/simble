// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Zero-copy GATT Client protocol engine for service and characteristic discovery.

use zerocopy::IntoBytes;

use crate::att::{
    AttErrorRsp, AttExchangeMtuReq, AttReadBlobReq, AttReadByTypeReqHeader, AttReadReq,
    AttWriteReqHeader, opcode,
};
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
        if payload.is_empty() || payload[0] != opcode::READ_BY_GROUP_TYPE_RSP {
            return Err(SimbleError::PacketParseError(
                "Invalid ReadByGroupType response".into(),
            ));
        }

        let item_len = payload[1] as usize;
        if item_len < 4 {
            return Err(SimbleError::PacketParseError(
                "Invalid item length in service rsp".into(),
            ));
        }

        let items_data = &payload[2..];
        for chunk in items_data.chunks_exact(item_len) {
            let start = u16::from_le_bytes([chunk[0], chunk[1]]);
            let end = u16::from_le_bytes([chunk[2], chunk[3]]);
            let Some(uuid) = Uuid::from_bytes(&chunk[4..]) else {
                continue;
            };

            self.services.push(DiscoveredService {
                start_handle: start,
                end_handle: end,
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
        if payload.is_empty() || payload[0] != opcode::READ_BY_TYPE_RSP {
            return Err(SimbleError::PacketParseError(
                "Invalid ReadByType response".into(),
            ));
        }

        let item_len = payload[1] as usize;
        if item_len < 5 {
            return Err(SimbleError::PacketParseError(
                "Invalid item length in char rsp".into(),
            ));
        }

        let service = self
            .services
            .iter_mut()
            .find(|s| s.uuid == service_uuid)
            .ok_or_else(|| SimbleError::Gatt(format!("Service {service_uuid} not discovered")))?;

        let items_data = &payload[2..];
        for chunk in items_data.chunks_exact(item_len) {
            let decl_handle = u16::from_le_bytes([chunk[0], chunk[1]]);
            let props = chunk[2];
            let val_handle = u16::from_le_bytes([chunk[3], chunk[4]]);
            let Some(uuid) = Uuid::from_bytes(&chunk[5..]) else {
                continue;
            };

            service.characteristics.push(DiscoveredCharacteristic {
                declaration_handle: decl_handle,
                value_handle: val_handle,
                properties: props,
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
}
