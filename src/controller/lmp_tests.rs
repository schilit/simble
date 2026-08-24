use super::*;

fn drive_to_connected(central: &mut LmpLink, peripheral: &mut LmpLink) {
    let mut to_peripheral = vec![central.build_connection_request().unwrap()];
    let mut to_central: Vec<Vec<u8>> = Vec::new();

    for _ in 0..8 {
        if to_peripheral.is_empty() && to_central.is_empty() {
            break;
        }
        let mut next_to_central = Vec::new();
        for pkt in to_peripheral.drain(..) {
            next_to_central.extend(peripheral.receive(&pkt).unwrap());
        }
        let mut next_to_peripheral = Vec::new();
        for pkt in to_central.drain(..) {
            next_to_peripheral.extend(central.receive(&pkt).unwrap());
        }
        to_central = next_to_central;
        to_peripheral = next_to_peripheral;
    }
}

#[test]
fn test_connection_establishment_and_feature_exchange() {
    let central_features = [0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    let peripheral_features = [0x0F, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];

    let mut central = LmpLink::new(LmpRole::Central, central_features);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, peripheral_features);

    drive_to_connected(&mut central, &mut peripheral);

    assert!(central.is_connected());
    assert!(peripheral.is_connected());
    assert_eq!(central.peer_features, Some(peripheral_features));
    assert_eq!(peripheral.peer_features, Some(central_features));
}

#[test]
fn test_negotiated_features_is_intersection() {
    let central_features = [0b1111_0000, 0, 0, 0, 0, 0, 0, 0];
    let peripheral_features = [0b1100_1100, 0, 0, 0, 0, 0, 0, 0];

    let mut central = LmpLink::new(LmpRole::Central, central_features);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, peripheral_features);
    drive_to_connected(&mut central, &mut peripheral);

    let expected = [0b1100_0000, 0, 0, 0, 0, 0, 0, 0];
    assert_eq!(central.negotiated_features(), Some(expected));
    assert_eq!(peripheral.negotiated_features(), Some(expected));
}

#[test]
fn test_connection_rejected() {
    let mut central = LmpLink::new(LmpRole::Central, [0; 8]);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0; 8]);
    peripheral.accept_connections = false;

    let req = central.build_connection_request().unwrap();
    let responses = peripheral.receive(&req).unwrap();
    assert_eq!(responses.len(), 1);

    let outcome = central.receive(&responses[0]).unwrap();
    assert!(outcome.is_empty());
    assert_eq!(central.state, LmpLinkState::Rejected);
    assert_eq!(peripheral.state, LmpLinkState::Rejected);
    assert!(!central.is_connected());
}

#[test]
fn test_malformed_packet_rejected() {
    let mut link = LmpLink::new(LmpRole::Peripheral, [0; 8]);
    assert!(link.receive(&[]).is_err());

    // opcode 0 is unassigned.
    assert!(link.receive(&[0x00]).is_err());
}

#[test]
fn test_features_req_before_connection_established_is_rejected() {
    let mut link = LmpLink::new(LmpRole::Peripheral, [0; 8]);
    let req = LmpFeaturesReq::new(0);
    assert!(link.receive(req.as_bytes()).is_err());
}

#[test]
fn test_only_central_builds_connection_request() {
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0; 8]);
    assert!(peripheral.build_connection_request().is_err());
}

/// A deferring link is what a *controller* needs: the decision to answer a
/// page belongs to the host above HCI, not to the link manager. Without
/// this, an `LmpLink` accepts on the host's behalf and the host is told
/// about a connection it never agreed to.
#[test]
fn test_a_deferring_peripheral_answers_nothing_until_its_host_decides() {
    let mut central = LmpLink::new(LmpRole::Central, [0xFF; 8]);
    let mut peripheral = LmpLink::deferring([0x0F; 8]);

    let request = central.build_connection_request().unwrap();
    let answer = peripheral.receive(&request).unwrap();
    assert!(
        answer.is_empty(),
        "a deferring link must say nothing at all — the Connection \
         Request event goes to the host, and the host answers"
    );
    assert_eq!(peripheral.state, LmpLinkState::ConnectionPending);
    assert!(peripheral.has_pending_connection());
    assert_eq!(
        central.state,
        LmpLinkState::ConnectionRequested,
        "and the central is still waiting, not connected"
    );
}

#[test]
fn test_a_deferred_connection_completes_once_the_host_accepts() {
    let central_features = [0xFF, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    let peripheral_features = [0x0F, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
    let mut central = LmpLink::new(LmpRole::Central, central_features);
    let mut peripheral = LmpLink::deferring(peripheral_features);

    let request = central.build_connection_request().unwrap();
    peripheral.receive(&request).unwrap();

    // The host says yes — the same PDUs a non-deferring link would have
    // sent straight away, just later.
    let mut to_central = peripheral.accept_pending_connection().unwrap();
    let mut to_peripheral: Vec<Vec<u8>> = Vec::new();
    for _ in 0..8 {
        if to_central.is_empty() && to_peripheral.is_empty() {
            break;
        }
        let mut next_to_peripheral = Vec::new();
        for pdu in to_central.drain(..) {
            next_to_peripheral.extend(central.receive(&pdu).unwrap());
        }
        let mut next_to_central = Vec::new();
        for pdu in to_peripheral.drain(..) {
            next_to_central.extend(peripheral.receive(&pdu).unwrap());
        }
        to_central = next_to_central;
        to_peripheral = next_to_peripheral;
    }

    assert!(central.is_connected());
    assert!(peripheral.is_connected());
    assert_eq!(central.peer_features, Some(peripheral_features));
    assert_eq!(peripheral.peer_features, Some(central_features));
}

#[test]
fn test_a_deferred_connection_the_host_refuses_ends_rejected_at_both_ends() {
    let mut central = LmpLink::new(LmpRole::Central, [0xFF; 8]);
    let mut peripheral = LmpLink::deferring([0x0F; 8]);
    let request = central.build_connection_request().unwrap();
    peripheral.receive(&request).unwrap();

    let pdus = peripheral
        .reject_pending_connection(reject_reason::CONNECTION_REJECTED_LIMITED_RESOURCES)
        .unwrap();
    for pdu in pdus {
        central.receive(&pdu).unwrap();
    }

    assert_eq!(peripheral.state, LmpLinkState::Rejected);
    assert_eq!(central.state, LmpLinkState::Rejected);
    assert_eq!(
        central.rejected_reason,
        Some(reject_reason::CONNECTION_REJECTED_LIMITED_RESOURCES),
        "the initiator learns *why*, which is what its host puts in the \
         Connection Complete's status"
    );
}

#[test]
fn test_accepting_a_connection_nobody_requested_is_an_error() {
    // The state-with-no-exit guard: accept/reject are only legal from
    // ConnectionPending, so a stray HCI Accept cannot invent a link.
    let mut peripheral = LmpLink::deferring([0; 8]);
    assert!(peripheral.accept_pending_connection().is_err());
    assert!(
        peripheral
            .reject_pending_connection(reject_reason::CONNECTION_REJECTED_LIMITED_RESOURCES)
            .is_err()
    );
}

#[test]
fn test_a_non_deferring_link_is_unchanged() {
    // The original peer-to-peer behaviour must survive: `deferring` is an
    // opt-in, and every existing caller relies on the immediate answer.
    let mut central = LmpLink::new(LmpRole::Central, [0xFF; 8]);
    let mut peripheral = LmpLink::new(LmpRole::Peripheral, [0x0F; 8]);
    let request = central.build_connection_request().unwrap();
    let answer = peripheral.receive(&request).unwrap();
    assert_eq!(
        answer.len(),
        2,
        "LMP_accepted plus this end's own LMP_features_req, immediately"
    );
    assert!(!peripheral.has_pending_connection());
}
