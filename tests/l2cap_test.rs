// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Port of Bumble's l2cap_test.py test suite.
//!
//! Validates L2CAP B-frame encapsulation, fixed channel multiplexing, and Credit-Based CoC.

use simble::l2cap::{CoCManager, L2capHeader, cid};
use simble::packets::l2cap_signaling::{
    L2capSignalingHeader, LeCreditBasedConnectionRequestHeader,
    LeCreditBasedConnectionResponseHeader, LeFlowControlCredit, signaling_code,
};
use zerocopy::{FromBytes, IntoBytes, U16};

#[test]
fn test_l2cap_att_channel_framing() {
    let att_payload = [0x02, 0x00, 0x02]; // Exchange MTU (Client MTU 512)
    let framed = L2capHeader::serialize(cid::ATT, &att_payload);

    assert_eq!(framed.len(), 4 + 3);
    let (header, payload) = L2capHeader::parse(&framed).expect("Valid parse");

    assert_eq!(header.length.get(), 3);
    assert_eq!(header.cid.get(), cid::ATT);
    assert_eq!(payload, &att_payload);
}

#[test]
fn test_l2cap_smp_channel_framing() {
    let smp_payload = [0x01, 0x03, 0x00, 0x01, 0x10, 0x07, 0x07]; // Pairing Request
    let framed = L2capHeader::serialize(cid::SMP, &smp_payload);

    assert_eq!(framed.len(), 4 + 7);
    let (header, payload) = L2capHeader::parse(&framed).expect("Valid parse");

    assert_eq!(header.length.get(), 7);
    assert_eq!(header.cid.get(), cid::SMP);
    assert_eq!(payload, &smp_payload);
}

#[test]
fn test_l2cap_truncated_packet_rejected() {
    let raw = [0x05, 0x00, 0x04, 0x00, 0x01, 0x02]; // Declares length 5, only provides 2 bytes
    assert!(L2capHeader::parse(&raw).is_none());
}

#[test]
fn test_l2cap_exact_length_matching() {
    let payload = vec![0xAB; 250];
    let packet = L2capHeader::serialize(cid::ATT, &payload);
    let (header, parsed_payload) = L2capHeader::parse(&packet).expect("Valid parse");

    assert_eq!(header.length.get(), 250);
    assert_eq!(parsed_payload.len(), 250);
    assert_eq!(parsed_payload, payload.as_slice());
}

#[test]
fn test_l2cap_credit_based_connection_request() {
    let sig_header = L2capSignalingHeader {
        code: signaling_code::LE_CREDIT_BASED_CONNECTION_REQUEST,
        identifier: 1,
        length: U16::from(10), // 8 byte header + 1 source CID (2 bytes)
    };
    let req_body = LeCreditBasedConnectionRequestHeader {
        spsm: U16::from(0x0025),
        mtu: U16::from(512),
        mps: U16::from(251),
        initial_credits: U16::from(10),
    };
    let source_cid: U16<zerocopy::byteorder::LittleEndian> = U16::from(0x0040);

    let mut packet = Vec::new();
    packet.extend_from_slice(sig_header.as_bytes());
    packet.extend_from_slice(req_body.as_bytes());
    packet.extend_from_slice(source_cid.as_bytes());

    let (parsed_sig, rest) = L2capSignalingHeader::ref_from_prefix(&packet).unwrap();
    assert_eq!(
        parsed_sig.code,
        signaling_code::LE_CREDIT_BASED_CONNECTION_REQUEST
    );
    assert_eq!(parsed_sig.identifier, 1);

    let (parsed_req, cid_bytes) =
        LeCreditBasedConnectionRequestHeader::ref_from_prefix(rest).unwrap();
    assert_eq!(parsed_req.spsm.get(), 0x0025);
    assert_eq!(parsed_req.mtu.get(), 512);
    assert_eq!(parsed_req.mps.get(), 251);
    assert_eq!(parsed_req.initial_credits.get(), 10);
    assert_eq!(cid_bytes, &[0x40, 0x00]);
}

#[test]
fn test_l2cap_credit_based_connection_response() {
    let resp = LeCreditBasedConnectionResponseHeader {
        mtu: U16::from(256),
        mps: U16::from(100),
        initial_credits: U16::from(5),
        result: U16::from(0), // Success
    };
    let bytes = resp.as_bytes();
    let parsed = LeCreditBasedConnectionResponseHeader::ref_from_bytes(bytes).unwrap();
    assert_eq!(parsed.result.get(), 0);
    assert_eq!(parsed.mtu.get(), 256);
}

#[test]
fn test_l2cap_flow_control_credit_and_manager() {
    let mut mgr = CoCManager::new();
    let cid = mgr.create_channel(0x0050, 0x0025, 512, 251, 1).unwrap();

    let channel = mgr.get_channel_mut(cid).unwrap();
    assert!(channel.can_send());
    channel.consume_tx_credit().unwrap();
    assert!(!channel.can_send());

    // Receive LE Flow Control Credit packet
    let credit_pkt = LeFlowControlCredit {
        cid: U16::from(cid),
        credits: U16::from(4),
    };
    let parsed = LeFlowControlCredit::ref_from_bytes(credit_pkt.as_bytes()).unwrap();
    channel.add_tx_credits(parsed.credits.get());

    assert!(channel.can_send());
    assert_eq!(channel.peer_credits, 4);
}
