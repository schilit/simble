// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! ATT Server protocol handler for dispatching incoming requests to the GATT Database.

use zerocopy::IntoBytes;

use crate::att::{
    AttErrorRsp, AttExchangeMtuRsp, AttExecuteWriteReq, AttPdu, AttPrepareWriteReqHeader,
    error_code, opcode,
};
use crate::device::VirtualDevice;
use crate::device::connection::PrepareWriteChunk;
use crate::types::{SimbleError, Uuid};

impl VirtualDevice {
    /// Processes an incoming ATT PDU parsed via `simble::packets`.
    pub(crate) fn process_att_packet(
        &mut self,
        connection_handle: u16,
        payload: &[u8],
    ) -> Result<Option<Vec<u8>>, SimbleError> {
        let pdu = AttPdu::parse(payload)
            .ok_or_else(|| SimbleError::PacketParseError("Invalid ATT PDU".into()))?;

        match pdu {
            AttPdu::ExchangeMtuReq(req) => {
                let client_mtu = req.client_rx_mtu.get();
                let server_mtu = 512u16;
                let negotiated_mtu = client_mtu.min(server_mtu).max(23);

                if let Some(conn) = self.connections.get_mut(&connection_handle) {
                    conn.mtu = negotiated_mtu;
                }

                Ok(Some(AttExchangeMtuRsp::new(server_mtu).as_bytes().to_vec()))
            }
            AttPdu::ReadReq(req) => {
                let handle = req.handle.get();
                match self.gatt_db.read(handle, 0) {
                    Ok(val) => {
                        let mut resp = Vec::with_capacity(1 + val.len());
                        resp.push(opcode::READ_RSP);
                        resp.extend_from_slice(val);
                        Ok(Some(resp))
                    }
                    Err(err) => Ok(Some(
                        AttErrorRsp::new(opcode::READ_REQ, handle, err)
                            .as_bytes()
                            .to_vec(),
                    )),
                }
            }
            AttPdu::ReadBlobReq(req) => {
                let handle = req.handle.get();
                let offset = req.offset.get() as usize;
                match self.gatt_db.read(handle, offset) {
                    Ok(val) => {
                        let mut resp = Vec::with_capacity(1 + val.len());
                        resp.push(opcode::READ_BLOB_RSP);
                        resp.extend_from_slice(val);
                        Ok(Some(resp))
                    }
                    Err(err) => Ok(Some(
                        AttErrorRsp::new(opcode::READ_BLOB_REQ, handle, err)
                            .as_bytes()
                            .to_vec(),
                    )),
                }
            }
            AttPdu::WriteReq { header, value } => {
                let handle = header.handle.get();
                match self.gatt_db.write(handle, value) {
                    Ok(()) => Ok(Some(vec![opcode::WRITE_RSP])),
                    Err(err) => Ok(Some(
                        AttErrorRsp::new(opcode::WRITE_REQ, handle, err)
                            .as_bytes()
                            .to_vec(),
                    )),
                }
            }
            AttPdu::WriteCmd { header, value } => {
                let handle = header.handle.get();
                let _ = self.gatt_db.write(handle, value);
                Ok(None)
            }
            AttPdu::PrepareWriteReq { header, part_value } => {
                let handle = header.handle.get();
                let offset = header.offset.get();
                if let Some(conn) = self.connections.get_mut(&connection_handle) {
                    conn.prepare_write_queue.push(PrepareWriteChunk {
                        handle,
                        offset,
                        data: part_value.to_vec(),
                    });
                }

                // Prepare write response is an echo of the request
                let mut resp = Vec::with_capacity(5 + part_value.len());
                let rsp_hdr = AttPrepareWriteReqHeader::new(handle, offset);
                resp.extend_from_slice(rsp_hdr.as_bytes());
                resp.extend_from_slice(part_value);
                Ok(Some(resp))
            }
            AttPdu::ExecuteWriteReq(req) => {
                if let Some(conn) = self.connections.get_mut(&connection_handle) {
                    if req.flags == AttExecuteWriteReq::WRITE {
                        let queue = std::mem::take(&mut conn.prepare_write_queue);
                        for chunk in queue {
                            let _ = self.gatt_db.write_offset(
                                chunk.handle,
                                chunk.offset as usize,
                                &chunk.data,
                            );
                        }
                    } else {
                        conn.prepare_write_queue.clear();
                    }
                }
                Ok(Some(vec![opcode::EXECUTE_WRITE_RSP]))
            }
            AttPdu::HandleValueCfm => {
                if let Some(conn) = self.connections.get_mut(&connection_handle) {
                    conn.pending_indication = false;
                }
                Ok(None)
            }
            AttPdu::ReadByGroupTypeReq {
                header,
                group_type_bytes,
            } => {
                let start_handle = header.start_handle.get();
                let end_handle = header.end_handle.get();
                let uuid = if group_type_bytes.len() == 2 {
                    Uuid::from_u16(u16::from_le_bytes([
                        group_type_bytes[0],
                        group_type_bytes[1],
                    ]))
                } else if group_type_bytes.len() == 16 {
                    let mut b = [0u8; 16];
                    b.copy_from_slice(group_type_bytes);
                    Uuid::from_u128_bytes(b)
                } else {
                    return Ok(Some(
                        AttErrorRsp::new(
                            opcode::READ_BY_GROUP_TYPE_REQ,
                            start_handle,
                            error_code::INVALID_PDU,
                        )
                        .as_bytes()
                        .to_vec(),
                    ));
                };

                let matches = self.gatt_db.read_by_type(start_handle, end_handle, uuid);
                if matches.is_empty() {
                    return Ok(Some(
                        AttErrorRsp::new(
                            opcode::READ_BY_GROUP_TYPE_REQ,
                            start_handle,
                            error_code::ATTRIBUTE_NOT_FOUND,
                        )
                        .as_bytes()
                        .to_vec(),
                    ));
                }

                let mut resp = Vec::new();
                resp.push(opcode::READ_BY_GROUP_TYPE_RSP);
                let item_len = 4 + matches[0].1.len();
                resp.push(item_len as u8);

                for (handle, value) in matches {
                    resp.extend_from_slice(&handle.to_le_bytes());
                    resp.extend_from_slice(&0xFFFFu16.to_le_bytes());
                    resp.extend_from_slice(value);
                }

                Ok(Some(resp))
            }
            AttPdu::ReadByTypeReq { header, uuid_bytes } => {
                let start_handle = header.start_handle.get();
                let end_handle = header.end_handle.get();
                let uuid = if uuid_bytes.len() == 2 {
                    Uuid::from_u16(u16::from_le_bytes([uuid_bytes[0], uuid_bytes[1]]))
                } else if uuid_bytes.len() == 16 {
                    let mut b = [0u8; 16];
                    b.copy_from_slice(uuid_bytes);
                    Uuid::from_u128_bytes(b)
                } else {
                    return Ok(Some(
                        AttErrorRsp::new(
                            opcode::READ_BY_TYPE_REQ,
                            start_handle,
                            error_code::INVALID_PDU,
                        )
                        .as_bytes()
                        .to_vec(),
                    ));
                };

                let matches = self.gatt_db.read_by_type(start_handle, end_handle, uuid);
                if matches.is_empty() {
                    return Ok(Some(
                        AttErrorRsp::new(
                            opcode::READ_BY_TYPE_REQ,
                            start_handle,
                            error_code::ATTRIBUTE_NOT_FOUND,
                        )
                        .as_bytes()
                        .to_vec(),
                    ));
                }

                let mut resp = Vec::new();
                resp.push(opcode::READ_BY_TYPE_RSP);
                let item_len = 2 + matches[0].1.len();
                resp.push(item_len as u8);

                for (handle, value) in matches {
                    resp.extend_from_slice(&handle.to_le_bytes());
                    resp.extend_from_slice(value);
                }

                Ok(Some(resp))
            }
            _ => {
                let op = payload[0];
                Ok(Some(
                    AttErrorRsp::new(op, 0, error_code::REQUEST_NOT_SUPPORTED)
                        .as_bytes()
                        .to_vec(),
                ))
            }
        }
    }
}
