// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! `RequestContinuingResponse`: what happens when a response does not fit in
//! one AV/C frame.
//!
//! `docs/gaps.md` §2 recorded this as "not modelled on the send path".
//! **Neither** path modelled it, and the two halves failed differently — worth
//! separating, because only one of them is silent.
//!
//! **The send path** emitted the whole response as one AVRCP PDU however long,
//! and let AVCTP fragment the over-long AV/C frame underneath. Between two
//! simble ends that *works*, which is exactly why `tests/avrcp_test.rs` — 1 287
//! lines of it — never noticed. It is not what AVRCP says: 4.4.1 caps the AV/C
//! frame on the control channel at 512 bytes and 6.3.1 puts the fragmentation
//! at the AVRCP layer, pulled across by the controller. A conforming
//! controller is entitled to drop the over-long frame, and a stack that
//! reassembles at AVCTP (Bumble does) would have hidden this indefinitely.
//!
//! **The receive path** is the silent one. The controller reassembled
//! `START`/`CONTINUE` fragments it had no way to ask for, so against any
//! target that fragments per spec — Android's does — a `GetElementAttributes`
//! for a track with real metadata simply never answers. No error, no event,
//! no rejection: indistinguishable from a slow peer, with the AVRCP layer not
//! the obvious suspect.
//!
//! Every test here is bounded by a fragment count or a delivery count, so
//! "it worked" cannot mean "it took an unbounded number of exchanges".

use simble::classic::avc::CommandType;
use simble::classic::avrcp::{
    AvrcpEvent, Command, MediaAttribute, Protocol, character_set_id, media_attribute_id, pdu_id,
    play_status, status_code, write_pdu,
};

/// The MTU an AVRCP control channel negotiates in every scene here. AVRCP
/// 4.4.1 requires a control channel to accept a 512-byte AV/C frame, so this
/// is above the point at which fragmentation is decided.
const MTU: u16 = 672;

/// AV/C ACCEPTED, in the low nibble of the frame's first byte (AV/C General
/// Specification 4.1, Table 7.2).
const AVC_ACCEPTED: u8 = 0x09;
/// AV/C REJECTED, likewise.
const AVC_REJECTED: u8 = 0x0A;

/// Sends `pdus` from `a` to `b` and ping-pongs until both go quiet, counting
/// the round trips. The count is the assertion that matters: a continuation
/// is exactly "one more round trip than a response that fitted".
fn exchange_counting(
    a: &mut Protocol,
    b: &mut Protocol,
    pdus: Vec<Vec<u8>>,
) -> (Vec<AvrcpEvent>, Vec<AvrcpEvent>, usize) {
    let mut a_events = Vec::new();
    let mut b_events = Vec::new();
    let mut to_b = pdus;
    let mut to_a: Vec<Vec<u8>> = Vec::new();
    let mut steps = 0;
    for _ in 0..64 {
        let mut next_to_a = Vec::new();
        for pdu in to_b.drain(..) {
            let (out, events) = b.receive(&pdu);
            next_to_a.extend(out);
            b_events.extend(events);
        }
        let mut next_to_b = Vec::new();
        for pdu in to_a.drain(..) {
            let (out, events) = a.receive(&pdu);
            next_to_b.extend(out);
            a_events.extend(events);
        }
        to_a = next_to_a;
        to_b = next_to_b;
        if to_a.is_empty() && to_b.is_empty() {
            break;
        }
        steps += 1;
    }
    (a_events, b_events, steps)
}

/// Track metadata whose serialized form is `bytes` long or a little more —
/// the point being that it exceeds one AV/C frame, which is what a real
/// player's title, artist, album and genre together routinely do.
fn oversized_metadata(bytes: usize) -> Vec<MediaAttribute> {
    vec![
        MediaAttribute {
            attribute_id: media_attribute_id::TITLE,
            character_set_id: character_set_id::UTF_8,
            value: "A".repeat(bytes),
        },
        MediaAttribute {
            attribute_id: media_attribute_id::ARTIST_NAME,
            character_set_id: character_set_id::UTF_8,
            value: "Verified Foreign Artist".into(),
        },
    ]
}

fn pair() -> (Protocol, Protocol) {
    (Protocol::new(MTU), Protocol::new(MTU))
}

// ---------------------------------------------------------------------------
// The defect
// ---------------------------------------------------------------------------

/// The size at which a response starts to be fragmented is a property of the
/// channel, not of a constant this test made up.
#[test]
fn test_the_fragmentation_threshold_comes_from_the_channel_mtu() {
    let protocol = Protocol::new(MTU);
    // 512-byte AV/C frame limit, less the AVCTP, AV/C, company-ID and PDU
    // headers in front of the parameters.
    assert_eq!(protocol.maximum_parameter_size(), 512 - 13);

    // A channel that negotiated less than the AVRCP minimum fragments
    // sooner: the frame has to fit what the peer said it would accept.
    let small = Protocol::new(128);
    assert_eq!(small.maximum_parameter_size(), 128 - 13);
    assert!(small.maximum_parameter_size() < protocol.maximum_parameter_size());
}

/// The shape of the bug, stated as bytes: metadata this size **cannot** go
/// out in one packet, so a target that only ever sent one packet was sending
/// a prefix and calling it the answer.
#[test]
fn test_real_metadata_does_not_fit_in_one_av_c_frame() {
    let target = Protocol::new(MTU);
    let attributes = oversized_metadata(600);
    let parameters =
        simble::classic::avrcp::Response::GetElementAttributes { attributes }.to_parameters();
    assert!(
        parameters.len() > target.maximum_parameter_size(),
        "the test's own metadata has to be too big or it proves nothing: {} bytes",
        parameters.len()
    );
    let fragments = write_pdu(
        pdu_id::GET_ELEMENT_ATTRIBUTES,
        &parameters,
        target.maximum_parameter_size(),
    );
    assert_eq!(fragments.len(), 2, "expected a START and an END");
    assert_eq!(fragments[0][1] & 3, 0b01, "first fragment must be START");
    assert_eq!(fragments[1][1] & 3, 0b11, "last fragment must be END");
}

// ---------------------------------------------------------------------------
// The fix, end to end
// ---------------------------------------------------------------------------

/// A `GetElementAttributes` whose answer needs three packets comes back
/// whole, and the controller paid two extra round trips to get it.
#[test]
fn test_oversized_metadata_is_pulled_across_in_fragments() {
    let (mut controller, mut target) = pair();
    let attributes = oversized_metadata(1100);
    target.element_attributes = attributes.clone();

    let pdus = controller.get_element_attributes(0, &[]).unwrap();
    let (controller_events, _, steps) = exchange_counting(&mut controller, &mut target, pdus);

    assert_eq!(
        controller_events,
        vec![AvrcpEvent::ElementAttributesReceived(attributes)],
        "the metadata must arrive intact, not as the prefix that fitted"
    );
    assert_eq!(
        controller.continuations_requested(),
        2,
        "1100 bytes of title is three fragments, so two continuations"
    );
    // One-way deliveries after the first: command, START, request,
    // fragment 2, request, fragment 3.
    assert_eq!(steps, 5);
    assert!(
        !target.has_pending_continuation(),
        "the target must not still be holding a tail nobody will ask for"
    );
}

/// A response that fits sends no continuation at all — the fix must not turn
/// every query into a two-step exchange.
#[test]
fn test_a_response_that_fits_costs_no_extra_round_trip() {
    let (mut controller, mut target) = pair();
    let attributes = vec![MediaAttribute {
        attribute_id: media_attribute_id::TITLE,
        character_set_id: character_set_id::UTF_8,
        value: "Short".into(),
    }];
    target.element_attributes = attributes.clone();

    let pdus = controller.get_element_attributes(0, &[]).unwrap();
    let (controller_events, _, steps) = exchange_counting(&mut controller, &mut target, pdus);

    assert_eq!(
        controller_events,
        vec![AvrcpEvent::ElementAttributesReceived(attributes)]
    );
    assert_eq!(controller.continuations_requested(), 0);
    assert_eq!(steps, 1);
    assert!(!target.has_pending_continuation());
}

/// The transaction label is not a detail: the controller's own assembler
/// drops any fragment whose label differs from the one the transaction
/// started on, so a continuation sent on a fresh label would discard exactly
/// the bytes it asked for.
#[test]
fn test_the_continuation_command_stays_on_the_original_transaction_label() {
    let (mut controller, mut target) = pair();
    target.element_attributes = oversized_metadata(700);

    let command = controller.get_element_attributes(0, &[]).unwrap();
    assert_eq!(command.len(), 1);
    let original_label = command[0][0] >> 4;

    // Drive one step by hand so the continuation command can be inspected.
    let (start_fragment, _) = target.receive(&command[0]);
    assert_eq!(start_fragment.len(), 1);
    let (continuation, events) = controller.receive(&start_fragment[0]);
    assert!(
        events.is_empty(),
        "a START fragment is not an answer, so it must raise no event"
    );
    assert_eq!(
        continuation.len(),
        1,
        "the controller must ask for the rest"
    );
    assert_eq!(continuation[0][0] >> 4, original_label);
    // AVCTP header (1) + PID (2) + AV/C ctype/subunit/opcode (3) + company
    // ID (3), then the AVRCP PDU header, whose first byte is the PDU ID.
    assert_eq!(continuation[0][9], pdu_id::REQUEST_CONTINUING_RESPONSE);
    // Its single parameter names the PDU being continued.
    assert_eq!(continuation[0][13], pdu_id::GET_ELEMENT_ATTRIBUTES);
}

/// A target holding a tail answers `AbortContinuingResponse` by dropping it,
/// and a second abort is refused rather than silently accepted — a
/// controller that aborts twice has lost the thread and needs to hear so.
#[test]
fn test_abort_discards_the_held_fragments() {
    let (mut controller, mut target) = pair();
    target.element_attributes = oversized_metadata(700);

    let command = controller.get_element_attributes(0, &[]).unwrap();
    let (start_fragment, _) = target.receive(&command[0]);
    // Feed the controller the START but throw away the continuation request:
    // this models a controller that gave up.
    let _ = controller.receive(&start_fragment[0]);
    assert!(target.has_pending_continuation());

    let abort = controller
        .send_avrcp_command(
            CommandType::Control,
            &Command::AbortContinuingResponse {
                continuing_pdu_id: pdu_id::GET_ELEMENT_ATTRIBUTES,
            },
        )
        .unwrap();
    let (accepted, _) = target.receive(&abort[0]);
    assert!(!target.has_pending_continuation());
    assert_eq!(accepted.len(), 1);
    // AV/C response code sits in the low nibble of the frame's first byte,
    // which is the fourth octet of the AVCTP packet.
    assert_eq!(accepted[0][3] & 0x0F, AVC_ACCEPTED);

    let abort_again = controller
        .send_avrcp_command(
            CommandType::Control,
            &Command::AbortContinuingResponse {
                continuing_pdu_id: pdu_id::GET_ELEMENT_ATTRIBUTES,
            },
        )
        .unwrap();
    let (refused, _) = target.receive(&abort_again[0]);
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0][3] & 0x0F, AVC_REJECTED);
    assert_eq!(*refused[0].last().unwrap(), status_code::INVALID_PARAMETER);
}

/// A continuation asked for out of nowhere is refused, not answered with
/// whatever happens to be in hand.
#[test]
fn test_a_continuation_nobody_started_is_refused() {
    let (mut controller, mut target) = pair();
    let request = controller
        .send_avrcp_command(
            CommandType::Control,
            &Command::RequestContinuingResponse {
                continuing_pdu_id: pdu_id::GET_ELEMENT_ATTRIBUTES,
            },
        )
        .unwrap();
    let (refused, _) = target.receive(&request[0]);
    assert_eq!(refused.len(), 1);
    assert_eq!(refused[0][3] & 0x0F, AVC_REJECTED);
    assert_eq!(*refused[0].last().unwrap(), status_code::INVALID_PARAMETER);
}

/// A CHANGED notification arriving between the START fragment and the
/// continuation request must not throw the held tail away. Two responses are
/// in flight on one connection here, which is ordinary AVRCP: notifications
/// are asynchronous by definition.
#[test]
fn test_a_notification_does_not_evict_a_held_continuation() {
    let (mut controller, mut target) = pair();
    target.supported_events = vec![simble::classic::avrcp::event_id::PLAYBACK_STATUS_CHANGED];
    target.element_attributes = oversized_metadata(700);

    // Register for playback status, so the target has a listener to fire at.
    let register = controller
        .register_notification(simble::classic::avrcp::event_id::PLAYBACK_STATUS_CHANGED, 0)
        .unwrap();
    let (interim, _) = target.receive(&register[0]);
    let _ = controller.receive(&interim[0]);

    // Start a fragmented metadata read but hold the continuation request.
    let command = controller.get_element_attributes(0, &[]).unwrap();
    let (start_fragment, _) = target.receive(&command[0]);
    let (continuation, _) = controller.receive(&start_fragment[0]);
    assert!(target.has_pending_continuation());

    // The track starts playing while the controller is mid-read.
    let changed = target.notify_playback_status_changed(play_status::PLAYING);
    assert_eq!(changed.len(), 1);
    assert!(
        target.has_pending_continuation(),
        "a small unrelated response must not evict the held fragments"
    );

    // Now finish the read; the metadata must still be whole.
    // The CHANGED response and the continuation request cross on the wire,
    // which is the whole point: deliver the notification first.
    let (_, controller_events, _) = exchange_counting(&mut target, &mut controller, changed);
    let (more_controller_events, _, _) =
        exchange_counting(&mut controller, &mut target, continuation);
    let controller_events: Vec<AvrcpEvent> = controller_events
        .into_iter()
        .chain(more_controller_events)
        .collect();
    let mut notified = false;
    let mut metadata = None;
    for event in controller_events {
        match event {
            AvrcpEvent::ElementAttributesReceived(attributes) => metadata = Some(attributes),
            AvrcpEvent::NotificationReceived { interim: false, .. } => notified = true,
            _ => {}
        }
    }
    assert!(
        notified,
        "the CHANGED notification must still reach the controller"
    );
    assert_eq!(
        metadata,
        Some(oversized_metadata(700)),
        "the notification must not have destroyed the half-read metadata"
    );
}
