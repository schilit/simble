// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! SDP client/server tests: data element encoding, service search, and
//! attribute retrieval, including continuation-loop and recursion-guard cases.

use simble::classic::sdp::{
    AttributeIdSpec, DataElement, SdpClient, SdpPdu, SdpServer, SdpUuid, Service, ServiceAttribute,
    attribute_id, error_code,
};
use simble::l2cap::L2capHeader;
use simble::l2cap::classic::{ClassicChannelManager, ClassicChannelSpec};
use std::collections::HashMap;

/// Service class ID used by `sample_records`: E6D55659-C8B4-4B85-96BB-B1143AF6D3AE.
const SAMPLE_CLASS_UUID: SdpUuid = SdpUuid::Uuid128([
    0xAE, 0xD3, 0xF6, 0x3A, 0x14, 0xB1, 0xBB, 0x96, 0x85, 0x4B, 0xB4, 0xC8, 0x59, 0x56, 0xD5, 0xE6,
]);

fn sample_records(count: u32) -> HashMap<u32, Service> {
    (0..count)
        .map(|i| {
            let handle = 0x0001_0001 + i;
            (
                handle,
                vec![
                    ServiceAttribute::new(
                        attribute_id::SERVICE_RECORD_HANDLE,
                        DataElement::unsigned_integer_32(0x0001_0001),
                    ),
                    ServiceAttribute::new(
                        attribute_id::BROWSE_GROUP_LIST,
                        DataElement::sequence(vec![DataElement::uuid(
                            SdpUuid::SDP_PUBLIC_BROWSE_ROOT,
                        )]),
                    ),
                    ServiceAttribute::new(
                        attribute_id::SERVICE_CLASS_ID_LIST,
                        DataElement::sequence(vec![DataElement::uuid(SAMPLE_CLASS_UUID)]),
                    ),
                    ServiceAttribute::new(
                        attribute_id::PROTOCOL_DESCRIPTOR_LIST,
                        DataElement::sequence(vec![DataElement::sequence(vec![
                            DataElement::uuid(SdpUuid::BT_L2CAP_PROTOCOL_ID),
                        ])]),
                    ),
                ],
            )
        })
        .collect()
}

/// Round-trips every `DataElement` variant through `to_bytes`/`from_bytes`.
#[test]
fn test_data_elements() {
    fn check(e: DataElement) {
        let serialized = e.to_bytes();
        let parsed = DataElement::from_bytes(&serialized).expect("parse");
        assert_eq!(
            parsed.to_bytes(),
            serialized,
            "re-serialized bytes must match"
        );
        assert_eq!(parsed, e);
    }

    check(DataElement::nil());
    check(DataElement::unsigned_integer(12, 1));
    check(DataElement::unsigned_integer(1234, 2));
    check(DataElement::unsigned_integer(0x123456, 4));
    check(DataElement::unsigned_integer(0x1_2345_6789, 8));
    check(DataElement::unsigned_integer(0x0000_FFFF, 4));
    check(DataElement::signed_integer(-12, 1));
    check(DataElement::signed_integer(-1234, 2));
    check(DataElement::signed_integer(-0x123456, 4));
    check(DataElement::signed_integer(-0x1234_5678, 8));
    check(DataElement::signed_integer(0x0000_FFFF, 4));
    check(DataElement::uuid(SdpUuid::Uuid16(1234)));
    check(DataElement::uuid(SdpUuid::Uuid32(123_456_789)));
    check(DataElement::uuid(SAMPLE_CLASS_UUID));
    check(DataElement::text_string(b"hello".to_vec()));
    check(DataElement::text_string(b"hello".repeat(60)));
    check(DataElement::text_string(b"hello".repeat(20000)));
    check(DataElement::boolean(true));
    check(DataElement::boolean(false));
    check(DataElement::sequence(vec![DataElement::boolean(true)]));
    check(DataElement::sequence(vec![
        DataElement::boolean(true),
        DataElement::text_string(b"hello".to_vec()),
    ]));
    check(DataElement::alternative(vec![DataElement::boolean(true)]));
    check(DataElement::alternative(vec![
        DataElement::boolean(true),
        DataElement::text_string(b"hello".to_vec()),
    ]));
    check(DataElement::unsigned_integer_16(1234));
    check(DataElement::signed_integer_16(-1234));
    check(DataElement::uuid(SdpUuid::Uuid16(1234)));
    check(DataElement::text_string(b"hello".to_vec()));
    check(DataElement::boolean(true));
    check(DataElement::sequence(vec![
        DataElement::signed_integer(0, 1),
        DataElement::text_string(b"hello".to_vec()),
    ]));
    check(DataElement::alternative(vec![
        DataElement::signed_integer(0, 1),
        DataElement::text_string(b"hello".to_vec()),
    ]));
    check(DataElement::url("http://foobar.com"));
}

/// Round-trips an `SdpPdu`'s parameter length through parse/serialize.
#[test]
fn test_pdu_parameter_length() {
    let pdu = SdpPdu::ErrorResponse {
        transaction_id: 0,
        error_code: error_code::INVALID_SDP_VERSION,
    };
    assert_eq!(SdpPdu::parse(&pdu.to_bytes()), Some(pdu));
}

/// Service search by UUID: no match, single UUID match, and multi-UUID match.
#[test]
fn test_service_search() {
    let mut server = SdpServer::new();
    server.service_records.extend(sample_records(1));
    let mut client = SdpClient::new();

    let mismatched = SdpUuid::Uuid128([
        0xAF, 0xD3, 0xF6, 0x3A, 0x14, 0xB1, 0xBB, 0x96, 0x85, 0x4B, 0xB4, 0xC8, 0x59, 0x56, 0xD5,
        0xE6,
    ]);
    let services = client
        .search_services(&[mismatched], |req| server.handle_request(req, 2048))
        .unwrap();
    assert_eq!(services.len(), 0);

    let services = client
        .search_services(&[SAMPLE_CLASS_UUID], |req| server.handle_request(req, 2048))
        .unwrap();
    assert_eq!(services, vec![0x0001_0001]);

    let services = client
        .search_services(
            &[
                SdpUuid::BT_L2CAP_PROTOCOL_ID,
                SdpUuid::SDP_PUBLIC_BROWSE_ROOT,
            ],
            |req| server.handle_request(req, 2048),
        )
        .unwrap();
    assert_eq!(services, vec![0x0001_0001]);
}

/// Service search across many records with a small MTU, forcing the
/// continuation-state response loop.
#[test]
fn test_service_search_with_continuation() {
    let mut server = SdpServer::new();
    let records = sample_records(100);
    server.service_records.extend(records.clone());
    let mut client = SdpClient::new();

    let mut services = client
        .search_services(&[SAMPLE_CLASS_UUID], |req| server.handle_request(req, 48))
        .unwrap();
    services.sort_unstable();
    assert_eq!(services.len(), records.len());
    for (i, handle) in services.iter().enumerate() {
        assert_eq!(*handle, 0x0001_0001 + i as u32);
    }
}

/// Attribute retrieval by ID: no match, then a real attribute.
#[test]
fn test_service_attributes() {
    let mut server = SdpServer::new();
    server.service_records.extend(sample_records(1));
    let mut client = SdpClient::new();

    let attributes = client
        .get_attributes(0x0001_0001, &[AttributeIdSpec::Single(1234)], |req| {
            server.handle_request(req, 2048)
        })
        .unwrap();
    assert_eq!(attributes.len(), 0);

    let attributes = client
        .get_attributes(
            0x0001_0001,
            &[AttributeIdSpec::Single(attribute_id::SERVICE_RECORD_HANDLE)],
            |req| server.handle_request(req, 2048),
        )
        .unwrap();
    assert_eq!(attributes.len(), 1);
    assert_eq!(
        attributes[0].value,
        DataElement::unsigned_integer_32(0x0001_0001)
    );
}

/// Attribute retrieval across 100 attributes with a small MTU, forcing the
/// continuation-state response loop.
#[test]
fn test_service_attributes_with_continuation() {
    let mut server = SdpServer::new();
    server.service_records.insert(
        0x0001_0001,
        (0..100)
            .map(|i| ServiceAttribute::new(i, DataElement::unsigned_integer_32(0x0001_0001)))
            .collect(),
    );
    let mut client = SdpClient::new();

    let attribute_ids: Vec<AttributeIdSpec> = (0..100).map(AttributeIdSpec::Single).collect();
    let attributes = client
        .get_attributes(0x0001_0001, &attribute_ids, |req| {
            server.handle_request(req, 48)
        })
        .unwrap();
    assert_eq!(attributes.len(), 100);
    for (i, attribute) in attributes.iter().enumerate() {
        assert_eq!(attribute.id, i as u16);
    }
}

/// Combined search+attribute retrieval: a range spec returns all matching
/// attributes in ID order, a set of `Single` specs returns just those.
#[test]
fn test_service_search_attribute() {
    let mut server = SdpServer::new();
    server.service_records.insert(
        0x0001_0001,
        vec![
            ServiceAttribute::new(
                4,
                DataElement::sequence(vec![DataElement::uuid(SAMPLE_CLASS_UUID)]),
            ),
            ServiceAttribute::new(
                3,
                DataElement::sequence(vec![DataElement::uuid(SAMPLE_CLASS_UUID)]),
            ),
            ServiceAttribute::new(
                1,
                DataElement::sequence(vec![DataElement::uuid(SAMPLE_CLASS_UUID)]),
            ),
        ],
    );
    let mut client = SdpClient::new();

    let results = client
        .search_attributes(
            &[SAMPLE_CLASS_UUID],
            &[AttributeIdSpec::Range(0, 0xFFFF)],
            |req| server.handle_request(req, 2048),
        )
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].len(), 3);
    assert_eq!(results[0][0].id, 1);
    assert_eq!(results[0][1].id, 3);
    assert_eq!(results[0][2].id, 4);

    let results = client
        .search_attributes(
            &[SAMPLE_CLASS_UUID],
            &[
                AttributeIdSpec::Single(1),
                AttributeIdSpec::Single(2),
                AttributeIdSpec::Single(3),
            ],
            |req| server.handle_request(req, 2048),
        )
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].len(), 2);
    assert_eq!(results[0][0].id, 1);
    assert_eq!(results[0][1].id, 3);
}

/// Combined search+attribute retrieval across 100 attributes with a small
/// MTU, forcing the continuation-state response loop.
#[test]
fn test_service_search_attribute_with_continuation() {
    let mut server = SdpServer::new();
    server.service_records.insert(
        0x0001_0001,
        (0..100)
            .map(|i| {
                ServiceAttribute::new(
                    i,
                    DataElement::sequence(vec![DataElement::uuid(SAMPLE_CLASS_UUID)]),
                )
            })
            .collect(),
    );
    let mut client = SdpClient::new();

    let results = client
        .search_attributes(
            &[SAMPLE_CLASS_UUID],
            &[AttributeIdSpec::Range(0, 0xFFFF)],
            |req| server.handle_request(req, 48),
        )
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].len(), 100);
    for (i, attribute) in results[0].iter().enumerate() {
        assert_eq!(attribute.id, i as u16);
    }
}

/// Exercises the `ClassicChannelManager` connect/teardown lifecycle a real
/// SDP client connect/disconnect would drive an L2CAP channel through.
#[test]
fn test_client_async_context() {
    let mut server_mgr = ClassicChannelManager::new();
    let server = SdpServer::new();
    server.register(&mut server_mgr).unwrap();

    let mut client_mgr = ClassicChannelManager::new();
    let (client_cid, request) = client_mgr
        .connect(&ClassicChannelSpec::new(simble::classic::sdp::SDP_PSM))
        .unwrap();
    let response = server_mgr.on_connection_request(&request, 2048).unwrap();
    client_mgr
        .on_connection_response(client_cid, &response)
        .unwrap();
    assert!(client_mgr.get_channel(client_cid).is_some());

    client_mgr.remove_channel(client_cid);
    assert!(client_mgr.get_channel(client_cid).is_none());
}

/// A deeply-nested SEQUENCE must not blow the stack; parsing fails past the
/// nesting limit.
#[test]
fn test_nested_sequence_recursion_guard() {
    let mut inner = vec![0x35, 0x00]; // empty SEQUENCE terminator
    for _ in 0..1500 {
        let size = inner.len();
        if size >= 0xFFFF {
            break;
        }
        let mut next = vec![0x36, (size >> 8) as u8, size as u8];
        next.extend(inner);
        inner = next;
    }
    assert_eq!(DataElement::parse(&inner), None);
}

/// A SEQUENCE nested within the recursion limit still parses correctly.
#[test]
fn test_nested_sequence_within_limit_still_works() {
    let leaf = DataElement::unsigned_integer(1, 2);
    let mut payload = leaf;
    for _ in 0..16 {
        payload = DataElement::sequence(vec![payload]);
    }
    let raw = payload.to_bytes();
    let (mut cur, _) = DataElement::parse(&raw).expect("parses within depth limit");
    for _ in 0..16 {
        let items = cur.as_sequence().expect("expected sequence").to_vec();
        assert_eq!(items.len(), 1);
        cur = items.into_iter().next().unwrap();
    }
    assert_eq!(cur, DataElement::unsigned_integer(1, 2));
}

/// Demonstrates SDP running end-to-end over a Classic Basic Mode L2CAP
/// channel — connection + configuration handshake via `ClassicChannelManager`,
/// then SDP PDUs framed with `L2capHeader` across the two sides.
#[test]
fn test_sdp_over_classic_l2cap_channel() {
    let mut client_mgr = ClassicChannelManager::new();
    let mut server_mgr = ClassicChannelManager::new();
    let mut server = SdpServer::new();
    server.register(&mut server_mgr).unwrap();
    server.service_records.extend(sample_records(1));

    let (client_cid, request) = client_mgr
        .connect(&ClassicChannelSpec::new(simble::classic::sdp::SDP_PSM))
        .unwrap();
    let response = server_mgr.on_connection_request(&request, 2048).unwrap();
    let server_cid = response.destination_cid.get();
    client_mgr
        .on_connection_response(client_cid, &response)
        .unwrap();

    let (client_cfg_req, client_options) =
        client_mgr.make_configuration_request(client_cid).unwrap();
    let server_cfg_rsp = server_mgr
        .on_configuration_request(client_cfg_req.destination_cid.get(), &client_options)
        .unwrap();
    client_mgr
        .on_configuration_response(client_cid, &server_cfg_rsp)
        .unwrap();

    let (server_cfg_req, server_options) =
        server_mgr.make_configuration_request(server_cid).unwrap();
    let client_cfg_rsp = client_mgr
        .on_configuration_request(server_cfg_req.destination_cid.get(), &server_options)
        .unwrap();
    server_mgr
        .on_configuration_response(server_cid, &client_cfg_rsp)
        .unwrap();

    assert!(client_mgr.get_channel(client_cid).unwrap().is_open());
    assert!(server_mgr.get_channel(server_cid).unwrap().is_open());

    let mut client = SdpClient::new();
    let services = client
        .search_services(&[SAMPLE_CLASS_UUID], |req| {
            let framed = L2capHeader::serialize(server_cid, req);
            let (_, l2cap_payload) = L2capHeader::parse(&framed).unwrap();
            let response = server.handle_request(l2cap_payload, 2048);
            let framed_response = L2capHeader::serialize(client_cid, &response);
            let (_, response_payload) = L2capHeader::parse(&framed_response).unwrap();
            response_payload.to_vec()
        })
        .unwrap();
    assert_eq!(services, vec![0x0001_0001]);
}
