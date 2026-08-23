// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! **PROTOTYPE — offered as the cheaper alternative in the Gherkin
//! investigation, not yet a proposal.** The whole-axis version of the
//! regression test added with the one-octet ATT panic.
//!
//! `test_truncated_list_responses_are_rejected` in `src/packets/att.rs` names
//! five short PDUs by hand, which is five of the 4 352 (opcode, length) pairs a
//! peer can put on the wire. The bug it was written for — `payload.is_empty()`
//! proving length >= 1 where `payload[1]` needs 2 — is a *shape* of bug, not a
//! fact about `READ_BY_GROUP_TYPE_RSP`, and the same shape can appear the next
//! time a PDU gets a typed header. So sweep the axis instead of sampling it.
//!
//! Two levels, because the panic was reachable at both:
//!
//! * `AttPdu::parse` over every opcode and every truncation, which is pure and
//!   costs nothing;
//! * a connected `LeCentral` fed the same PDUs inside a real ACL frame, which
//!   is where the crash actually happened — `central.rs` dispatches on `att[0]`
//!   and hands the payload straight in.
//!
//! Neither asserts *what* the answer is. The claim is only that a remote peer
//! cannot end the process, which is the claim that was false.

use simble::device::central::LeCentral;
use simble::l2cap::{AclPacketBoundary, HciAclHeader, L2capHeader};
use simble::packets::att::AttPdu;
use simble::types::Address;
use zerocopy::IntoBytes;

/// Every ATT opcode, at every truncation a peer could send. `AttPdu::parse`
/// must return, whatever it returns.
#[test]
fn parsing_any_opcode_at_any_truncation_returns_rather_than_panics() {
    let mut parsed = 0usize;
    for opcode in 0u8..=0xFF {
        for length in 0usize..=16 {
            let mut pdu = vec![opcode];
            // A recognizable filler: 0xFF handles and lengths are the values
            // most likely to overflow an offset computation.
            pdu.extend(std::iter::repeat_n(0xFF, length));
            if AttPdu::parse(&pdu).is_some() {
                parsed += 1;
            }
        }
    }
    // Not an assertion about which ones parse — only that the sweep really ran
    // and is not silently matching nothing.
    assert!(
        parsed > 0,
        "the sweep parsed nothing at all, so it proves nothing"
    );
}

/// A trailing partial entry in a list response is the other half of the same
/// bug: the header declares an entry length, and the data is not a multiple of
/// it. Walk every declared length against every data length.
#[test]
fn a_list_response_whose_data_is_not_a_multiple_of_its_entry_length_returns() {
    for opcode in [0x11u8, 0x09, 0x05] {
        for item_len in 0u8..=32 {
            for data_len in 0usize..=48 {
                let mut pdu = vec![opcode, item_len];
                pdu.extend(std::iter::repeat_n(0xAB, data_len));
                if let Some(parsed) = AttPdu::parse(&pdu) {
                    // Walking the entries must also terminate. Formatting is
                    // the cheapest way to force every field to be read.
                    let _ = format!("{parsed:?}");
                }
            }
        }
    }
}

/// The level the crash was actually on. A central in the middle of discovery
/// is handed each malformed PDU as a real ACL frame, the way a peer would send
/// it, and must survive.
#[test]
fn a_central_in_discovery_survives_any_malformed_att_pdu() {
    for opcode in 0u8..=0xFF {
        for length in 0usize..=8 {
            let mut central = connected_central();
            let mut att = vec![opcode];
            att.extend(std::iter::repeat_n(0xFF, length));
            central.on_packet(&acl(CONNECTION_HANDLE, &att));
            // Still usable afterwards: a malformed PDU must not have wedged it
            // either. `pump` is what the scene calls every tick.
            let _ = central.pump();
        }
    }
}

const CONNECTION_HANDLE: u16 = 0x0040;

/// A central that has connected and is discovering, driven the way
/// `central.rs`'s own unit tests drive it.
fn connected_central() -> LeCentral {
    let mut central = LeCentral::new();
    let target: Address = "AA:BB:CC:00:00:01".parse().expect("a valid address");
    central.connect_with_type(target, 0x00);
    // Answer every command it asks for with success until it stops asking.
    let mut pending = central.pump();
    for _ in 0..32 {
        if pending.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for packet in pending {
            if packet.first() == Some(&0x01) && packet.len() >= 3 {
                next.extend(central.on_packet(&command_complete([packet[1], packet[2]], &[0x00])));
            }
        }
        pending = next;
    }
    central.on_packet(&connection_complete(target));
    central
}

fn command_complete(opcode: [u8; 2], params: &[u8]) -> Vec<u8> {
    let mut packet = vec![0x04, 0x0E, (3 + params.len()) as u8, 0x01];
    packet.extend_from_slice(&opcode);
    packet.extend_from_slice(params);
    packet
}

/// LE Connection Complete, status 0x00, as a central.
fn connection_complete(peer: Address) -> Vec<u8> {
    let mut wire = peer.to_be_bytes();
    wire.reverse();
    let mut body = vec![0x01, 0x00];
    body.extend_from_slice(&CONNECTION_HANDLE.to_le_bytes());
    body.push(0x00); // role: central
    body.push(0x00); // peer address type: public
    body.extend_from_slice(&wire);
    body.extend_from_slice(&[0x28, 0x00]); // connection interval
    body.extend_from_slice(&[0x00, 0x00]); // latency
    body.extend_from_slice(&[0x48, 0x00]); // supervision timeout
    body.push(0x00); // clock accuracy
    let mut packet = vec![0x04, 0x3E, body.len() as u8];
    packet.extend_from_slice(&body);
    packet
}

/// One ACL frame carrying `att` on the ATT channel.
fn acl(handle: u16, att: &[u8]) -> Vec<u8> {
    let l2cap = L2capHeader::serialize(simble::l2cap::cid::ATT, att);
    let mut packet = vec![0x02];
    packet.extend_from_slice(
        HciAclHeader::new(
            handle,
            AclPacketBoundary::FirstAutoFlushable,
            l2cap.len() as u16,
        )
        .as_bytes(),
    );
    packet.extend_from_slice(&l2cap);
    packet
}
