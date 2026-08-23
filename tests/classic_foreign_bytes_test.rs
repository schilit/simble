// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! **Captured foreign bytes** from the BR/EDR initiator interop run.
//!
//! `tests/interop/classic_peer.py` points simble's BR/EDR initiator at a
//! *Bumble* classic device over netsim's rootcanal. That run is the only
//! thing that proves the inquiry, paging, Remote Name Request and SDP client
//! bytes are right — simble's own simulated controller and simble's own
//! responder agree with the initiator by construction — but it needs a live
//! `netsimd` and a Python environment, so it is not run by `cargo test` and
//! a regression can sit in the tree for weeks.
//!
//! This file closes that hole the way `docs/test-strategy.md` asks for: the
//! exact octets **rootcanal and Bumble put on the wire** during a passing
//! run, pinned as consts and parsed by the same code paths the live run
//! uses. Nothing here is constructed by simble, so no self-consistent
//! mistake can satisfy it.
//!
//! Two of these captures exist because the live run found a bug:
//!
//! - `SDP_ANSWER_CHUNK_0` / `CHUNK_1` — Bumble's SDP server caps a response
//!   at the negotiated L2CAP MTU less nine and returns the rest under a
//!   **continuation state**. The event-loop SDP client ignored the field,
//!   marked the truncated prefix as the whole answer, and reported the peer
//!   as offering no Serial Port service at all.
//! - `INQUIRY_RESULT_WITH_RSSI` / `EXTENDED_INQUIRY_RESULT` — rootcanal
//!   honours HCI Write Inquiry Mode, and the host understood only the reset
//!   default. With either of the other two modes set the inquiry completed
//!   having found nothing, with no error anywhere.
//!
//! Provenance: netsimd (Android emulator canary), Bumble 0.0.219, one run
//! per inquiry mode. rootcanal hands out a fresh BD_ADDR per session, which
//! is why the address differs between captures.

use simble::classic::rfcomm::RFCOMM_PSM;
use simble::classic::sdp::{SDP_PSM, SdpUuid};
use simble::device::classic_host::{inquiry_mode, scan_enable};
use simble::device::{ClassicHost, ProtocolHandler, SdpQueryHandler};
use simble::types::Address;
use std::str::FromStr;

// ---------------------------------------------------------------------------
// What Bumble was configured to be. Every assertion below is one of these
// facts arriving from the other side of a foreign stack.
// ---------------------------------------------------------------------------

/// `HCI_Write_Local_Name` on Bumble's side; rootcanal serves it to a Remote
/// Name Request and Bumble puts it in its EIR.
const PEER_NAME: &str = "Bumble SPP Peer";

/// `HCI_Write_Class_Of_Device` on Bumble's side, 0x2C0114 — a computer major
/// class. Deliberately nothing like the 0x240404 headset every simble
/// example uses, so a passing assertion cannot be simble reading back its
/// own constant. On the wire it is little-endian: `14 01 2C`.
const PEER_CLASS_OF_DEVICE: [u8; 3] = [0x14, 0x01, 0x2C];

/// The channel `rfcomm.Server.listen()` was asked for on Bumble's side, and
/// the number that has to come back out of its SDP record. Not 3 — the
/// channel simble's own examples hardcode.
const PEER_RFCOMM_CHANNEL: u8 = 7;

/// Serial Port Profile service class, what the SDP query searches for.
const SERIAL_PORT: SdpUuid = SdpUuid::Uuid16(0x1101);

// ---------------------------------------------------------------------------
// Controller events — rootcanal's bytes, carrying Bumble's configuration.
// ---------------------------------------------------------------------------

/// HCI Inquiry Result (event 0x02), the reset-default form. Num_Responses(1),
/// BD_ADDR(6), Page_Scan_Repetition_Mode(1), **Reserved(2)**,
/// Class_of_Device(3), Clock_Offset(2).
const INQUIRY_RESULT_STANDARD: &str = "04020F0100000000016100000014012C0000";

/// HCI Inquiry Result with RSSI (event 0x22), what the controller sends
/// after Write Inquiry Mode 0x01. Same 14-octet response, but with **one**
/// reserved octet and an RSSI at the end — so Class_of_Device sits one octet
/// earlier than above. Reading it at the standard offset yields 0x002C01.
const INQUIRY_RESULT_WITH_RSSI: &str = "04220F01000000000163000014012C0000F8";

/// HCI Extended Inquiry Result (event 0x2F), after Write Inquiry Mode 0x02.
/// One response only, followed by 240 octets of EIR — here a single AD
/// structure, `10 09 "Bumble SPP Peer"`, which is how a phone lists a name
/// it has not paged for. The 223-octet zero tail is restored by `padded`.
const EXTENDED_INQUIRY_RESULT: &str =
    "042FFF01000000000165000014012C0000F8100942756D626C65205350502050656572";

/// HCI Remote Name Request Complete (event 0x07): Status(1), BD_ADDR(6),
/// Remote_Name(248, NUL-padded). The 233-octet zero tail is restored by
/// `padded`.
const REMOTE_NAME_COMPLETE: &str = "0407FF0000000000016142756D626C65205350502050656572";

/// HCI Connection Complete (event 0x03) for the ACL link simble paged for.
const CONNECTION_COMPLETE: &str = "04030B0000000000000001610100";

// ---------------------------------------------------------------------------
// Commands — the bytes simble sent that rootcanal and Bumble accepted. A
// controller that dislikes an HCI parameter layout does not return an error
// here, it *dies*, so a run that got to the end is a strong statement about
// these octets in particular.
// ---------------------------------------------------------------------------

/// HCI Inquiry, LAP 0x9E8B33 (GIAC) little-endian, 4 × 1.28 s, unlimited
/// responses.
const INQUIRY_COMMAND: &str = "01010405338B9E0400";

/// HCI Remote Name Request: BD_ADDR(6), Page_Scan_Repetition_Mode(1),
/// Reserved(1), Clock_Offset(2).
const REMOTE_NAME_REQUEST_COMMAND: &str = "0119040A00000000017101000000";

/// HCI Create Connection: BD_ADDR(6), Packet_Type(2), PSRM(1), Reserved(1),
/// Clock_Offset(2), Allow_Role_Switch(1).
const CREATE_CONNECTION_COMMAND: &str = "0105040D00000000017118CC0100000001";

/// HCI Write Inquiry Mode 0x00, as sent in the standard-mode run.
const WRITE_INQUIRY_MODE_COMMAND: &str = "01450C0100";

// ---------------------------------------------------------------------------
// SDP — Bumble's own server, serialising its own records.
// ---------------------------------------------------------------------------

/// simble's SDP_ServiceSearchAttributeRequest, as Bumble's server accepted
/// it: search pattern `[SerialPort]`, maximum 0xFFFF bytes, attributes
/// `[ProtocolDescriptorList, ServiceClassIDList]`, null continuation state
/// (`00` — an InfoLength of zero).
const SDP_REQUEST_FIRST: &str = "06000100103503191101FFFF350609000409000100";

/// The follow-up, after Bumble asked for a continuation. Byte-for-byte the
/// same request with Bumble's `01 00` state echoed back — the server matches
/// the whole request, not just the state.
const SDP_REQUEST_CONTINUED: &str = "06000100113503191101FFFF35060900040900010100";

/// Bumble's SDP_ServiceSearchAttributeResponse when its database holds the
/// one SPP record: a complete answer with a null continuation state.
const SDP_ANSWER_ONE_SHOT: &str =
    "0700010020001D351B35190900013503191101090004350C35031901003505190003080700";

/// The first half of Bumble's answer when 26 records match: 663 bytes of
/// attribute list — the negotiated MTU less nine — and a continuation state
/// of `01 00`. Its data element is a *prefix* of a SEQUENCE and does not
/// parse; that is the whole trap.
const SDP_ANSWER_CHUNK_0: &str = concat!(
    "070001029B02973602BE35190900013503191101090004350C3503190100350519000308",
    "0735190900013503191101090004350C3503190100350519000308143519090001350319",
    "1101090004350C35031901003505190003081535190900013503191101090004350C3503",
    "1901003505190003081635190900013503191101090004350C3503190100350519000308",
    "1735190900013503191101090004350C3503190100350519000308183519090001350319",
    "1101090004350C35031901003505190003081935190900013503191101090004350C3503",
    "1901003505190003081A35190900013503191101090004350C3503190100350519000308",
    "1B35190900013503191101090004350C35031901003505190003081C3519090001350319",
    "1101090004350C35031901003505190003081D35190900013503191101090004350C3503",
    "1901003505190003081435190900013503191101090004350C3503190100350519000308",
    "1535190900013503191101090004350C3503190100350519000308163519090001350319",
    "1101090004350C35031901003505190003081735190900013503191101090004350C3503",
    "1901003505190003081835190900013503191101090004350C3503190100350519000308",
    "1935190900013503191101090004350C35031901003505190003081A3519090001350319",
    "1101090004350C35031901003505190003081B35190900013503191101090004350C3503",
    "1901003505190003081C35190900013503191101090004350C3503190100350519000308",
    "1D35190900013503191101090004350C3503190100350519000308143519090001350319",
    "1101090004350C35031901003505190003081535190900013503191101090004350C3503",
    "190100350519000308163519090001350319110109000100",
);

/// The rest of it, with a null continuation state. Note that it starts mid
/// data element: `04 35 0C ...` continues the record chunk 0 cut in half.
const SDP_ANSWER_CHUNK_1: &str = concat!(
    "070001002D002A04350C3503190100350519000308173519090001350319110109000435",
    "0C35031901003505190003081800",
);

// ---------------------------------------------------------------------------

/// Parses a captured hex string into bytes.
fn bytes(hex: &str) -> Vec<u8> {
    assert!(hex.len().is_multiple_of(2), "hex must be whole octets");
    (0..hex.len() / 2)
        .map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("valid hex"))
        .collect()
}

/// Restores a capture whose recorded tail was all zeros. HCI's fixed-width
/// name and EIR fields are mostly NUL fill; writing 466 literal zero
/// characters into this file would obscure the bytes that matter without
/// adding a single check.
fn padded(hex: &str, total: usize) -> Vec<u8> {
    let mut out = bytes(hex);
    assert!(
        out.len() <= total,
        "capture is longer than its declared size"
    );
    out.resize(total, 0);
    out
}

/// A client-side host: not discoverable, not connectable, with an SDP query
/// registered — the shape `examples/classic_initiator.rs` runs.
fn initiator() -> ClassicHost {
    ClassicHost::new("simble-initiator", [0x0C, 0x02, 0x5A])
}

/// The peer address a capture names, taken out of the capture itself so the
/// two can never drift apart.
fn address_from(event: &[u8], offset: usize) -> Address {
    Address::new(event[offset..offset + 6].try_into().expect("six octets"))
}

// ---------------------------------------------------------------------------
// Inquiry
// ---------------------------------------------------------------------------

#[test]
fn test_bumbles_standard_inquiry_result_names_the_peer_and_its_class() {
    let event = bytes(INQUIRY_RESULT_STANDARD);
    let mut host = initiator();
    host.handle_packet(&event).expect("the event is handled");

    let found = host.discovered();
    assert_eq!(found.len(), 1, "one device answered the inquiry: {found:?}");
    // BD_ADDR starts after H4(1) + code(1) + length(1) + Num_Responses(1).
    assert_eq!(found[0].address, address_from(&event, 4));
    assert_eq!(
        found[0].class_of_device, PEER_CLASS_OF_DEVICE,
        "the Class of Device Bumble wrote, read at the standard form's offset"
    );
    assert_eq!(
        found[0].name, None,
        "a standard inquiry result carries no name — that is what the \
         Remote Name Request is for, and what makes a phone say 'unknown \
         device' while it resolves one"
    );
}

#[test]
fn test_bumbles_inquiry_result_with_rssi_puts_the_class_one_octet_earlier() {
    let event = bytes(INQUIRY_RESULT_WITH_RSSI);
    let mut host = initiator();
    host.handle_packet(&event).expect("the event is handled");

    let found = host.discovered();
    assert_eq!(
        found.len(),
        1,
        "event 0x22 must be understood, not dropped: a host that ignores it \
         completes its inquiry having found nothing, with no error anywhere"
    );
    assert_eq!(found[0].address, address_from(&event, 4));
    assert_eq!(
        found[0].class_of_device, PEER_CLASS_OF_DEVICE,
        "this form has one reserved octet, not two"
    );
    // The precise trap: the standard form's offset, applied here, silently
    // produces a different and entirely plausible-looking Class of Device.
    let at_standard_offset = [event[13], event[14], event[15]];
    assert_ne!(
        at_standard_offset, PEER_CLASS_OF_DEVICE,
        "if these agreed the test would prove nothing about the offset"
    );
}

#[test]
fn test_bumbles_extended_inquiry_result_carries_the_name_in_its_eir() {
    // 3 header octets + Num_Responses(1) + 254-octet response.
    let event = padded(EXTENDED_INQUIRY_RESULT, 3 + 1 + 254);
    let mut host = initiator();
    host.handle_packet(&event).expect("the event is handled");

    let found = host.discovered();
    assert_eq!(found.len(), 1, "event 0x2F must be understood: {found:?}");
    assert_eq!(found[0].address, address_from(&event, 4));
    assert_eq!(found[0].class_of_device, PEER_CLASS_OF_DEVICE);
    assert_eq!(
        found[0].name.as_deref(),
        Some(PEER_NAME),
        "the EIR carries the name Bumble advertised, so no Remote Name \
         Request is needed at all — the reason a phone's device list has \
         names before anything is paired"
    );
}

// ---------------------------------------------------------------------------
// Remote name and paging
// ---------------------------------------------------------------------------

#[test]
fn test_bumbles_remote_name_response_is_read_back_intact() {
    // 3 header octets + Status(1) + BD_ADDR(6) + Remote_Name(248).
    let event = padded(REMOTE_NAME_COMPLETE, 3 + 1 + 6 + 248);
    let mut host = initiator();
    host.handle_packet(&event).expect("the event is handled");

    let address = address_from(&event, 4);
    assert_eq!(
        host.name_of(address),
        Some(PEER_NAME),
        "the NUL padding must be trimmed and nothing else: a name read one \
         octet off, or with the fill left on, still 'works' against a peer \
         that never checks it"
    );
}

#[test]
fn test_bumbles_connection_complete_opens_the_link() {
    let event = bytes(CONNECTION_COMPLETE);
    let mut host = initiator();
    host.handle_packet(&event).expect("the event is handled");

    let (handle, peer) = host.connection().expect("the ACL link is tracked");
    // Handle sits after H4(1) + code(1) + length(1) + Status(1).
    assert_eq!(handle, u16::from_le_bytes([event[4], event[5]]));
    assert_eq!(peer, address_from(&event, 6));
}

// ---------------------------------------------------------------------------
// The commands the far side accepted
// ---------------------------------------------------------------------------

#[test]
fn test_the_hci_commands_rootcanal_accepted_are_the_bytes_this_host_builds() {
    // rootcanal does not answer a malformed HCI command with an error — it
    // dies. A run that reached the end of the sequence is therefore a
    // statement about these exact octets, and pinning them here is what
    // keeps a later edit from having to rediscover it with a netsim crash.
    let mut host = initiator();
    assert_eq!(
        host.start_inquiry(4),
        vec![bytes(INQUIRY_COMMAND)],
        "HCI Inquiry: GIAC little-endian, then length and Num_Responses"
    );

    let target = Address::from_str("71:01:00:00:00:00").expect("valid address");
    assert_eq!(
        host.request_remote_name(target),
        vec![bytes(REMOTE_NAME_REQUEST_COMMAND)],
        "HCI Remote Name Request"
    );
    assert_eq!(
        host.create_connection(target),
        vec![bytes(CREATE_CONNECTION_COMMAND)],
        "HCI Create Connection"
    );
    assert_eq!(
        host.set_inquiry_mode(inquiry_mode::STANDARD),
        vec![bytes(WRITE_INQUIRY_MODE_COMMAND)],
        "HCI Write Inquiry Mode"
    );
    // A pure client is neither discoverable nor connectable; the initiator
    // run disables both after bring-up and rootcanal accepted that too.
    assert_eq!(
        host.set_scan_enable(scan_enable::NONE),
        vec![bytes("011A0C0100")],
        "HCI Write Scan Enable"
    );
}

// ---------------------------------------------------------------------------
// SDP
// ---------------------------------------------------------------------------

/// The query client, and the results handle to read its answer from.
fn sdp_query() -> (SdpQueryHandler, simble::device::SharedSdpQueryResults) {
    SdpQueryHandler::searching(SERIAL_PORT)
}

#[test]
fn test_the_sdp_request_bumbles_server_accepted_is_the_one_this_client_builds() {
    let (mut query, _results) = sdp_query();
    assert_eq!(query.psm(), SDP_PSM, "the query rides the SDP PSM");
    assert_eq!(
        query.poll_output(672),
        vec![bytes(SDP_REQUEST_FIRST)],
        "the ServiceSearchAttributeRequest Bumble's SDP server answered"
    );
    assert!(
        query.poll_output(672).is_empty(),
        "and it is asked once, not on every poll"
    );
}

#[test]
fn test_bumbles_one_shot_sdp_answer_yields_the_channel_it_listens_on() {
    let (mut query, results) = sdp_query();
    query.poll_output(672);
    let replies = query.on_data(&bytes(SDP_ANSWER_ONE_SHOT), 672);
    assert!(
        replies.is_empty(),
        "a complete answer needs no follow-up: {replies:?}"
    );

    let results = results.lock().expect("results readable");
    assert!(results.answered);
    assert_eq!(results.error, None);
    assert!(!results.truncated);
    assert_eq!(
        results.channel_for(SERIAL_PORT),
        Some(PEER_RFCOMM_CHANNEL),
        "the RFCOMM server channel out of Bumble's own record — opening any \
         other is refused with DM"
    );
}

#[test]
fn test_bumbles_continuation_sdp_answer_is_followed_to_the_end() {
    let (mut query, results) = sdp_query();
    query.poll_output(672);

    // Chunk 0 is a prefix, and the client's only correct response is to ask
    // again with the server's state echoed back.
    let follow_up = query.on_data(&bytes(SDP_ANSWER_CHUNK_0), 672);
    assert_eq!(
        follow_up,
        vec![bytes(SDP_REQUEST_CONTINUED)],
        "the continuation request must repeat the whole original request \
         with Bumble's `01 00` state appended"
    );
    assert!(
        !results.lock().expect("results readable").answered,
        "a prefix is not an answer; calling it one is the bug this capture \
         exists for"
    );

    let done = query.on_data(&bytes(SDP_ANSWER_CHUNK_1), 672);
    assert!(done.is_empty(), "the null state ends the exchange");

    let results = results.lock().expect("results readable");
    assert!(results.answered);
    assert!(!results.truncated);
    assert_eq!(
        results.channel_for(SERIAL_PORT),
        Some(PEER_RFCOMM_CHANNEL),
        "the two halves spliced back together parse, and the Serial Port \
         record in them still names channel {PEER_RFCOMM_CHANNEL}"
    );
    assert_eq!(
        results.rfcomm_channels.len(),
        26,
        "all 26 matching records survive the splice, not just those in the \
         chunk that happened to parse on its own"
    );
}

#[test]
fn test_a_continuation_chunk_alone_is_not_a_usable_answer() {
    // The regression guard, and a description of the original bug: treat
    // chunk 0 as complete and the peer looks like it offers nothing. The
    // failure is silent — the bytes are a well-formed SDP PDU, only their
    // payload is half a data element.
    let attribute_lists = &bytes(SDP_ANSWER_CHUNK_0)[7..];
    assert!(
        simble::classic::sdp::DataElement::from_bytes(attribute_lists).is_none(),
        "a prefix of a SEQUENCE does not parse, which is why the truncation \
         surfaced as 'the peer advertises no Serial Port service' rather \
         than as a parse error"
    );
}

#[test]
fn test_the_rfcomm_channel_bumble_advertised_is_not_the_one_simble_defaults_to() {
    // The check that keeps this whole file honest: if Bumble happened to
    // advertise the channel simble's own examples hardcode, every assertion
    // above would pass on a client that never read the answer.
    assert_ne!(
        PEER_RFCOMM_CHANNEL, 3,
        "3 is simble's own SPP channel; the peer must use another"
    );
    assert_eq!(RFCOMM_PSM, 0x0003, "RFCOMM's PSM, not its server channel");
}
