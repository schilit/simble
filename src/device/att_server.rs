// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! ATT Server protocol engine processing incoming requests and generating responses.

use zerocopy::IntoBytes;

use crate::att::{AttErrorRsp, AttExchangeMtuRsp, AttPdu, error_code, opcode};
use crate::device::VirtualDevice;
use crate::device::connection::PrepareWriteChunk;
use crate::types::{SimbleError, Uuid};

#[inline]
fn att_error(req_opcode: u8, handle: u16, code: u8) -> Vec<u8> {
    AttErrorRsp::new(req_opcode, handle, code)
        .as_bytes()
        .to_vec()
}

impl VirtualDevice {
    /// Processes an incoming ATT PDU parsed via `simble::packets`.
    pub(crate) fn process_att_packet(
        &mut self,
        connection_handle: u16,
        att_pdu: &[u8],
    ) -> Result<Option<Vec<u8>>, SimbleError> {
        let Some(parsed) = AttPdu::parse(att_pdu) else {
            return Ok(Some(att_error(
                if !att_pdu.is_empty() { att_pdu[0] } else { 0 },
                0x0000,
                error_code::INVALID_PDU,
            )));
        };

        match parsed {
            AttPdu::ExchangeMtuReq(req) => {
                let client_rx_mtu = req.client_rx_mtu.get();
                let server_rx_mtu = 512u16;
                let negotiated_mtu = client_rx_mtu.min(server_rx_mtu).max(23);
                if let Some(conn) = self.connections.get_mut(&connection_handle) {
                    conn.mtu = negotiated_mtu;
                }
                if let Some(observer) = self.observer.as_deref_mut() {
                    observer.on_mtu_changed(connection_handle, negotiated_mtu);
                }
                let resp = AttExchangeMtuRsp::new(server_rx_mtu);
                Ok(Some(resp.as_bytes().to_vec()))
            }
            AttPdu::FindInformationReq(req) => {
                let start_handle = req.start_handle.get();
                let end_handle = req.end_handle.get();
                let info = self.gatt_db.find_information(start_handle, end_handle);
                if info.is_empty() {
                    return Ok(Some(att_error(
                        opcode::FIND_INFORMATION_REQ,
                        start_handle,
                        error_code::ATTRIBUTE_NOT_FOUND,
                    )));
                }

                let mut resp = Vec::new();
                resp.push(opcode::FIND_INFORMATION_RSP);
                let format = if info[0].1.len() == 2 { 0x01 } else { 0x02 };
                resp.push(format);

                for (handle, uuid) in info {
                    if (format == 0x01 && uuid.len() == 2) || (format == 0x02 && uuid.len() == 16) {
                        resp.extend_from_slice(&handle.to_le_bytes());
                        resp.extend_from_slice(&uuid.to_att_bytes());
                    }
                }

                Ok(Some(resp))
            }
            AttPdu::ReadReq(req) => {
                let handle = req.handle.get();
                if let Some(observer) = self.observer.as_deref_mut() {
                    observer.on_characteristic_read(connection_handle, handle, 0);
                }
                match self.gatt_db.read(handle, 0) {
                    Ok(val) => {
                        let mut resp = Vec::with_capacity(1 + val.len());
                        resp.push(opcode::READ_RSP);
                        resp.extend_from_slice(val);
                        Ok(Some(resp))
                    }
                    Err(code) => Ok(Some(att_error(opcode::READ_REQ, handle, code))),
                }
            }
            AttPdu::ReadBlobReq(req) => {
                let handle = req.handle.get();
                let offset = req.offset.get();
                if let Some(observer) = self.observer.as_deref_mut() {
                    observer.on_characteristic_read(connection_handle, handle, offset);
                }
                match self.gatt_db.read(handle, offset as usize) {
                    Ok(val) => {
                        let mut resp = Vec::with_capacity(1 + val.len());
                        resp.push(opcode::READ_BLOB_RSP);
                        resp.extend_from_slice(val);
                        Ok(Some(resp))
                    }
                    Err(code) => Ok(Some(att_error(opcode::READ_BLOB_REQ, handle, code))),
                }
            }
            AttPdu::WriteReq { header, value } => {
                let handle = header.handle.get();
                if let Some(observer) = self.observer.as_deref_mut() {
                    observer.on_characteristic_write(connection_handle, handle, value, true);
                }
                // Captured before the write so the subscription event can
                // report the prev -> cur bit transition (NimBLE
                // `BLE_GAP_EVENT_SUBSCRIBE` pattern).
                let cccd_prev = self.cccd_value(handle);
                match self.gatt_db.write(handle, value) {
                    Ok(()) => {
                        if let Some(prev) = cccd_prev {
                            self.on_cccd_written(connection_handle, handle, prev);
                        }
                        Ok(Some(vec![opcode::WRITE_RSP]))
                    }
                    Err(code) => Ok(Some(att_error(opcode::WRITE_REQ, handle, code))),
                }
            }
            AttPdu::WriteCmd { header, value } => {
                let handle = header.handle.get();
                if let Some(observer) = self.observer.as_deref_mut() {
                    observer.on_characteristic_write(connection_handle, handle, value, false);
                }
                let cccd_prev = self.cccd_value(handle);
                if self.gatt_db.write(handle, value).is_ok()
                    && let Some(prev) = cccd_prev
                {
                    self.on_cccd_written(connection_handle, handle, prev);
                }
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

                let mut resp = Vec::with_capacity(5 + part_value.len());
                resp.push(opcode::PREPARE_WRITE_RSP);
                resp.extend_from_slice(&handle.to_le_bytes());
                resp.extend_from_slice(&offset.to_le_bytes());
                resp.extend_from_slice(part_value);
                Ok(Some(resp))
            }
            AttPdu::ExecuteWriteReq(req) => {
                let flags = req.flags;
                if let Some(conn) = self.connections.get_mut(&connection_handle) {
                    let queue = std::mem::take(&mut conn.prepare_write_queue);
                    if flags == 0x01 {
                        for chunk in queue {
                            let _ = self.gatt_db.write_offset(
                                chunk.handle,
                                chunk.offset as usize,
                                &chunk.data,
                            );
                        }
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
                // ATT 3.4.4.9: an invalid handle range gets an Error Response,
                // and a walking client treats it as end-of-discovery.
                if start_handle == 0 || start_handle > end_handle {
                    return Ok(Some(att_error(
                        opcode::READ_BY_GROUP_TYPE_REQ,
                        start_handle,
                        error_code::INVALID_HANDLE,
                    )));
                }
                let Some(uuid) = Uuid::from_bytes(group_type_bytes) else {
                    return Ok(Some(att_error(
                        opcode::READ_BY_GROUP_TYPE_REQ,
                        start_handle,
                        error_code::INVALID_PDU,
                    )));
                };

                let matches = self.gatt_db.read_by_type(start_handle, end_handle, uuid);
                if matches.is_empty() {
                    return Ok(Some(att_error(
                        opcode::READ_BY_GROUP_TYPE_REQ,
                        start_handle,
                        error_code::ATTRIBUTE_NOT_FOUND,
                    )));
                }

                // Every entry in one Attribute Data List must be the same
                // length (Core Spec Vol 3, Part F, 3.4.4.10). A 16-bit
                // service followed by a 128-bit one must therefore stop at
                // the boundary; the client re-requests from the last handle
                // it saw. Emitting both under one length header makes the
                // client slice a 128-bit UUID into phantom 16-bit services.
                let value_len = matches[0].1.len();
                let mut resp = Vec::new();
                resp.push(opcode::READ_BY_GROUP_TYPE_RSP);
                resp.push((4 + value_len) as u8);

                for (handle, value) in matches {
                    if value.len() != value_len {
                        break;
                    }
                    resp.extend_from_slice(&handle.to_le_bytes());
                    resp.extend_from_slice(&self.gatt_db.group_end_handle(handle).to_le_bytes());
                    resp.extend_from_slice(value);
                }

                Ok(Some(resp))
            }
            AttPdu::ReadByTypeReq { header, uuid_bytes } => {
                let start_handle = header.start_handle.get();
                let end_handle = header.end_handle.get();
                // ATT 3.4.4.1: same invalid-range rule as Read By Group Type.
                if start_handle == 0 || start_handle > end_handle {
                    return Ok(Some(att_error(
                        opcode::READ_BY_TYPE_REQ,
                        start_handle,
                        error_code::INVALID_HANDLE,
                    )));
                }
                let Some(uuid) = Uuid::from_bytes(uuid_bytes) else {
                    return Ok(Some(att_error(
                        opcode::READ_BY_TYPE_REQ,
                        start_handle,
                        error_code::INVALID_PDU,
                    )));
                };

                let matches = self.gatt_db.read_by_type(start_handle, end_handle, uuid);
                if matches.is_empty() {
                    return Ok(Some(att_error(
                        opcode::READ_BY_TYPE_REQ,
                        start_handle,
                        error_code::ATTRIBUTE_NOT_FOUND,
                    )));
                }

                // Same equal-length rule as Read By Group Type (Core Spec
                // Vol 3, Part F, 3.4.4.2): a characteristic declaration with
                // a 128-bit UUID is longer than a 16-bit one, so the list
                // stops at the first entry of a different size.
                let value_len = matches[0].1.len();
                let mut resp = Vec::new();
                resp.push(opcode::READ_BY_TYPE_RSP);
                resp.push((2 + value_len) as u8);

                for (handle, value) in matches {
                    if value.len() != value_len {
                        break;
                    }
                    resp.extend_from_slice(&handle.to_le_bytes());
                    resp.extend_from_slice(value);
                }

                Ok(Some(resp))
            }
            _ => Ok(Some(att_error(
                att_pdu[0],
                0x0000,
                error_code::REQUEST_NOT_SUPPORTED,
            ))),
        }
    }
}
