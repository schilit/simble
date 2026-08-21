// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! LMP peer-controller link establishment tests: connection setup, feature
//! exchange, rejection, and malformed-packet handling.

use simble::controller::lmp::{
    LmpAccepted, LmpFeaturesReq, LmpFeaturesRes, LmpHostConnectionReq, LmpLink, LmpLinkState,
    LmpNotAccepted, LmpRole, opcode,
};
use zerocopy::IntoBytes;

/// Shuttles bytes between two `LmpLink`s until both queues drain, mirroring
/// how a real controller pair would relay LMP PDUs to each other.
fn run_handshake(central: &mut LmpLink, peripheral: &mut LmpLink) {
    let mut to_peripheral = vec![central.build_connection_request().expect("central starts")];
    let mut to_central: Vec<Vec<u8>> = Vec::new();

    for _ in 0..8 {
        if to_peripheral.is_empty() && to_central.is_empty() {
            return;
        }
        let mut next_to_central = Vec::new();
        for pkt in to_peripheral.drain(..) {
            next_to_central.extend(peripheral.receive(&pkt).expect("peripheral accepts"));
        }
        let mut next_to_peripheral = Vec::new();
        for pkt in to_central.drain(..) {
            next_to_peripheral.extend(central.receive(&pkt).expect("central accepts"));
        }
        to_central = next_to_central;
        to_peripheral = next_to_peripheral;
    }
    panic!("LMP handshake did not converge");
}

#[test]
fn test_lmp_connection_establishment_between_two_links() {
    let central_features = [0xFF, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    let peripheral_features = [0x0F, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

    let mut central = LmpLink::new(LmpRole::Central, central_features);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, peripheral_features);

    assert_eq!(central.state, LmpLinkState::Idle);
    assert_eq!(peripheral.state, LmpLinkState::Idle);

    run_handshake(&mut central, &mut peripheral);

    assert_eq!(central.state, LmpLinkState::Connected);
    assert_eq!(peripheral.state, LmpLinkState::Connected);
    assert!(central.is_connected());
    assert!(peripheral.is_connected());

    assert_eq!(central.peer_features, Some(peripheral_features));
    assert_eq!(peripheral.peer_features, Some(central_features));
}

#[test]
fn test_lmp_feature_exchange_yields_intersected_feature_set() {
    // Central supports bits 7,6,5,4 of byte 0; peripheral supports bits 7,6,3,2.
    let central_features = [0b1111_0000, 0, 0, 0, 0, 0, 0, 0];
    let peripheral_features = [0b1100_1100, 0, 0, 0, 0, 0, 0, 0];
    let expected_intersection = [0b1100_0000, 0, 0, 0, 0, 0, 0, 0];

    let mut central = LmpLink::new(LmpRole::Central, central_features);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, peripheral_features);

    run_handshake(&mut central, &mut peripheral);

    assert_eq!(
        central.negotiated_features(),
        Some(expected_intersection),
        "central's usable feature set must be the AND of both sides' masks"
    );
    assert_eq!(
        peripheral.negotiated_features(),
        Some(expected_intersection),
        "peripheral's usable feature set must be the AND of both sides' masks"
    );
}

#[test]
fn test_lmp_connection_rejected_by_peripheral() {
    let mut central = LmpLink::new(LmpRole::Central, [0; 8]);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0; 8]);
    peripheral.accept_connections = false;

    let req = central.build_connection_request().unwrap();
    let peripheral_responses = peripheral.receive(&req).unwrap();
    assert_eq!(peripheral_responses.len(), 1);

    let (not_accepted, _) = LmpNotAccepted::parse(&peripheral_responses[0])
        .expect("peripheral must reply with LMP_not_accepted");
    assert_eq!(not_accepted.rejected_opcode, opcode::HOST_CONNECTION_REQ);

    let central_outcome = central.receive(&peripheral_responses[0]).unwrap();
    assert!(central_outcome.is_empty());

    assert_eq!(central.state, LmpLinkState::Rejected);
    assert_eq!(peripheral.state, LmpLinkState::Rejected);
    assert!(!central.is_connected());
    assert!(!peripheral.is_connected());
}

#[test]
fn test_lmp_malformed_and_out_of_order_packets_rejected() {
    let mut link = LmpLink::new(LmpRole::Peripheral, [0; 8]);

    assert!(link.receive(&[]).is_err(), "empty packet must be rejected");
    assert!(
        link.receive(&[0x00]).is_err(),
        "unrecognized opcode must be rejected"
    );

    // A features_req arriving before the connection is established must be rejected.
    let premature = LmpFeaturesReq::new(0);
    assert!(link.receive(premature.as_bytes()).is_err());

    // Only the central may build a connection request.
    assert!(link.build_connection_request().is_err());
}

#[test]
fn test_lmp_packet_round_trip_parsing() {
    let conn_req = LmpHostConnectionReq::new(0);
    let (parsed, rest) =
        LmpHostConnectionReq::parse(conn_req.as_bytes()).expect("valid connection request");
    assert_eq!(parsed.tid(), 0);
    assert!(rest.is_empty());

    let accepted = LmpAccepted::new(1, opcode::HOST_CONNECTION_REQ);
    let (parsed, _) = LmpAccepted::parse(accepted.as_bytes()).expect("valid accepted");
    assert_eq!(parsed.accepted_opcode, opcode::HOST_CONNECTION_REQ);
    assert_eq!(parsed.tid(), 1);

    let features = [0xAA; 8];
    let features_res = LmpFeaturesRes::new(0, features);
    let (parsed, _) =
        LmpFeaturesRes::parse(features_res.as_bytes()).expect("valid features response");
    assert_eq!(parsed.features, features);

    // Parsing a features_res as a features_req must fail: opcode mismatch.
    assert!(LmpFeaturesReq::parse(features_res.as_bytes()).is_none());
}
