// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Builders for HCI packets that tests hand to a host as if a controller had
//! sent them.
//!
//! This exists because `command_complete` had been written seven times. Five
//! were the same function differing only in how the opcode arrives; one takes
//! a *credit count* rather than parameters and omits the H4 prefix, so it is a
//! different event wearing the same name (it lives in `transport/usb_tests.rs`
//! as `command_complete_granting`); and one is production code in
//! `controller/sim.rs`, which builds the event through the zerocopy header
//! rather than by hand and should stay separate.
//!
//! Available to integration tests as well as unit tests: `cargo test` turns on
//! the `testing` feature through the self-dev-dependency, so `tests/` can call
//! these instead of keeping private copies.

/// An HCI Command Complete event, H4-framed, granting one command credit.
///
/// Takes the opcode as a `u16` because that is what the packet carries.
/// Callers holding `[u8; 2]` use `u16::from_le_bytes`; callers holding an
/// [`crate::packets::hci::OpCode`] use `.get()`.
#[must_use]
pub fn command_complete(opcode: u16, params: &[u8]) -> Vec<u8> {
    let mut packet = vec![
        crate::transport::h4_type::HCI_EVENT,
        0x0E,
        (3 + params.len()) as u8,
        0x01, // Num_HCI_Command_Packets
    ];
    packet.extend_from_slice(&opcode.to_le_bytes());
    packet.extend_from_slice(params);
    packet
}
