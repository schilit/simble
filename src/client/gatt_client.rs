// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! GATT Client role implementation for discovering remote services, characteristics,
//! and performing reads, writes, and subscriptions.

use zerocopy::IntoBytes;

use crate::att::{
    AttExchangeMtuReq, AttFindInformationReq, AttReadByGroupTypeReqHeader, AttReadByTypeReqHeader,
    AttReadReq, AttWriteReqHeader, opcode,
};
use crate::gatt::CharacteristicProperties;
use crate::l2cap::{L2capHeader, cid};
use crate::types::{Address, SimbleError, Uuid};

/// A descriptor discovered on a remote peripheral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDescriptor {
    pub handle: u16,
    pub uuid: Uuid,
}

/// A characteristic discovered on a remote peripheral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCharacteristic {
    pub declaration_handle: u16,
    pub value_handle: u16,
    pub properties: CharacteristicProperties,
    pub uuid: Uuid,
    pub descriptors: Vec<DiscoveredDescriptor>,
}

/// A primary or secondary service discovered on a remote peripheral.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredService {
    pub start_handle: u16,
    pub end_handle: u16,
    pub uuid: Uuid,
    pub characteristics: Vec<DiscoveredCharacteristic>,
}

/// GATT Client instance managing discovery and state for a remote peripheral connection.
#[derive(Debug, Clone)]
pub struct GattClient {
    pub connection_handle: u16,
    pub peer_address: Address,
    pub mtu: u16,
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
        let mut pdu = Vec::with_capacity(7);
        let header = AttReadByGroupTypeReqHeader {
            opcode: opcode::READ_BY_GROUP_TYPE_REQ,
            start_handle: zerocopy::byteorder::little_endian::U16::from_bytes(
                start_handle.to_le_bytes(),
            ),
            end_handle: zerocopy::byteorder::little_endian::U16::from_bytes(
                end_handle.to_le_bytes(),
            ),
        };
        pdu.extend_from_slice(header.as_bytes());
        pdu.extend_from_slice(&0x2800u16.to_le_bytes()); // Primary Service UUID
        L2capHeader::serialize(cid::ATT, &pdu)
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
            let uuid = if item_len == 6 {
                Uuid::from_u16(u16::from_le_bytes([chunk[4], chunk[5]]))
            } else if item_len == 20 {
                let mut b = [0u8; 16];
                b.copy_from_slice(&chunk[4..20]);
                Uuid::from_u128_bytes(b)
            } else {
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
        let mut pdu = Vec::with_capacity(7);
        let header = AttReadByTypeReqHeader {
            opcode: opcode::READ_BY_TYPE_REQ,
            start_handle: zerocopy::byteorder::little_endian::U16::from_bytes(
                start_handle.to_le_bytes(),
            ),
            end_handle: zerocopy::byteorder::little_endian::U16::from_bytes(
                end_handle.to_le_bytes(),
            ),
        };
        pdu.extend_from_slice(header.as_bytes());
        pdu.extend_from_slice(&0x2803u16.to_le_bytes()); // Characteristic Declaration UUID
        L2capHeader::serialize(cid::ATT, &pdu)
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
            .ok_or_else(|| SimbleError::DeviceError(format!("Service {service_uuid} not found")))?;

        let items_data = &payload[2..];
        for chunk in items_data.chunks_exact(item_len) {
            let decl_h = u16::from_le_bytes([chunk[0], chunk[1]]);
            let props = CharacteristicProperties(chunk[2]);
            let val_h = u16::from_le_bytes([chunk[3], chunk[4]]);
            let uuid = if item_len == 7 {
                Uuid::from_u16(u16::from_le_bytes([chunk[5], chunk[6]]))
            } else if item_len == 21 {
                let mut b = [0u8; 16];
                b.copy_from_slice(&chunk[5..21]);
                Uuid::from_u128_bytes(b)
            } else {
                continue;
            };

            service.characteristics.push(DiscoveredCharacteristic {
                declaration_handle: decl_h,
                value_handle: val_h,
                properties: props,
                uuid,
                descriptors: Vec::new(),
            });
        }

        Ok(())
    }

    /// Generates a Descriptor Discovery (Find Information) Request for a characteristic handle range.
    pub fn create_discover_descriptors_request(
        &self,
        start_handle: u16,
        end_handle: u16,
    ) -> Vec<u8> {
        let req = AttFindInformationReq {
            opcode: opcode::FIND_INFORMATION_REQ,
            start_handle: zerocopy::byteorder::little_endian::U16::from_bytes(
                start_handle.to_le_bytes(),
            ),
            end_handle: zerocopy::byteorder::little_endian::U16::from_bytes(
                end_handle.to_le_bytes(),
            ),
        };
        L2capHeader::serialize(cid::ATT, req.as_bytes())
    }

    /// Generates a Read Characteristic Value Request.
    pub fn create_read_request(&self, handle: u16) -> Vec<u8> {
        let req = AttReadReq {
            opcode: opcode::READ_REQ,
            handle: zerocopy::byteorder::little_endian::U16::from_bytes(handle.to_le_bytes()),
        };
        L2capHeader::serialize(cid::ATT, req.as_bytes())
    }

    /// Generates a Write Characteristic Request.
    pub fn create_write_request(&self, handle: u16, value: &[u8]) -> Vec<u8> {
        let mut pdu = Vec::with_capacity(3 + value.len());
        let header = AttWriteReqHeader::new(opcode::WRITE_REQ, handle);
        pdu.extend_from_slice(header.as_bytes());
        pdu.extend_from_slice(value);
        L2capHeader::serialize(cid::ATT, &pdu)
    }

    /// Generates a Write Characteristic Command (unacknowledged).
    pub fn create_write_command(&self, handle: u16, value: &[u8]) -> Vec<u8> {
        let mut pdu = Vec::with_capacity(3 + value.len());
        let header = AttWriteReqHeader::new(opcode::WRITE_CMD, handle);
        pdu.extend_from_slice(header.as_bytes());
        pdu.extend_from_slice(value);
        L2capHeader::serialize(cid::ATT, &pdu)
    }

    /// Generates a Write to CCCD to enable notifications ([0x01, 0x00]).
    pub fn create_subscribe_notification(&self, cccd_handle: u16) -> Vec<u8> {
        self.create_write_request(cccd_handle, &[0x01, 0x00])
    }

    /// Finds a characteristic across discovered services.
    pub fn find_characteristic(&self, uuid: Uuid) -> Option<&DiscoveredCharacteristic> {
        for s in &self.services {
            for c in &s.characteristics {
                if c.uuid == uuid {
                    return Some(c);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gatt_client_service_and_char_discovery() {
        let peer_addr = Address::from_be_bytes([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        let mut client = GattClient::new(0x0001, peer_addr);

        // 1. Process Service Discovery Response
        let mut svc_rsp = vec![opcode::READ_BY_GROUP_TYPE_RSP, 6];
        svc_rsp.extend_from_slice(&1u16.to_le_bytes()); // start 1
        svc_rsp.extend_from_slice(&10u16.to_le_bytes()); // end 10
        svc_rsp.extend_from_slice(&0x180Du16.to_le_bytes()); // Heart Rate Service

        client.on_discover_services_response(&svc_rsp).unwrap();
        assert_eq!(client.services.len(), 1);
        assert_eq!(client.services[0].uuid, Uuid::from_u16(0x180D));

        // 2. Process Characteristic Discovery Response
        let mut char_rsp = vec![opcode::READ_BY_TYPE_RSP, 7];
        char_rsp.extend_from_slice(&2u16.to_le_bytes()); // decl handle 2
        char_rsp.push(CharacteristicProperties::NOTIFY); // props
        char_rsp.extend_from_slice(&3u16.to_le_bytes()); // value handle 3
        char_rsp.extend_from_slice(&0x2A37u16.to_le_bytes()); // HRM UUID

        client
            .on_discover_characteristics_response(Uuid::from_u16(0x180D), &char_rsp)
            .unwrap();

        let ch = client
            .find_characteristic(Uuid::from_u16(0x2A37))
            .expect("Found char");
        assert_eq!(ch.value_handle, 3);
        assert_eq!(
            ch.properties,
            CharacteristicProperties(CharacteristicProperties::NOTIFY)
        );
    }
}
