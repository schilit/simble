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
        0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x0D, 0x18, 0x00,
        0x00,
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
        0x11, 0x06, 0x01, 0x00, 0x09, 0x00, 0x0D, 0x18, 0x0A, 0x00, 0x0D, 0x00, 0x0F, 0x18, 0x0E,
        0x00,
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
        0xFB, 0x34, 0x9B, 0x5F, 0x80, 0x00, 0x00, 0x80, 0x00, 0x10, 0x00, 0x00, 0x02, 0x29, 0x00,
        0x00,
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
