// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Simble's host stack against a **real Rootcanal controller, in-process**.
//!
//! Simble's other foreign-oracle tests need a subprocess and a TCP port, which
//! is why `SIMBLE_INTEROP_SETTLE` exists and why those tests live outside
//! `cargo test`. `rootcanal-rs` is a library: controllers are created in
//! memory, so the same real C++ controller can be an ordinary unit test — no
//! subprocess, no port, no listening socket.
//!
//! **Ticking is not a simulated clock.** `Rootcanal::tick()` calls upstream's
//! `DualModeController::Tick()`, and Rootcanal schedules advertising and
//! timeouts against `std::chrono::steady_clock` — real wall-clock time. HCI
//! command/response is synchronous and needs no elapsed time, but anything on
//! a timer does: ticking in a tight loop advances nothing, and the advertiser
//! never transmits. [`scan_for`] therefore sleeps ~1 ms per tick. That is a
//! bounded, deterministic wait against a local in-memory controller, not a
//! settle delay against a subprocess that may or may not have come up yet.
//!
//! Off by default, because the dependency builds upstream Rootcanal's C++ with
//! Bazel (minutes, once) and because a published crate cannot depend on a
//! git/path dependency. See the `Cargo.toml` entry for why this is a
//! `cfg`-gated dev-dependency rather than a Cargo feature.
//!
//! ```sh
//! RUSTFLAGS="--cfg rootcanal_oracle" cargo test --test rootcanal_oracle_test
//! ```

#![cfg(rootcanal_oracle)]

use std::str::FromStr;
use std::time::Duration;

use bytes::Bytes;
use rootcanal::{Rootcanal, types::Address as RcAddress};
use simble::transport::HciChannel;
use simble::types::Address;

/// The scanning controller — the one Simble's host drives.
const HOST_CTRL: u32 = 1;
/// The advertising controller, on the far end of Rootcanal's link layer.
const PEER_CTRL: u32 = 2;

/// The address string handed to `rootcanal-rs` for the advertiser.
///
/// Note the reversal below. `rootcanal-rs`'s `Address::from_str` stores the
/// octets in written order and passes them straight to the FFI, but upstream
/// Rootcanal treats octet 0 as the *least* significant (`Address::ToString`
/// iterates `rbegin()..rend()`). So the controller's real BD_ADDR is this
/// string reversed, and that is what a spec-correct HCI host — Simble — sees
/// in an advertising report. This is a byte-order bug in `rootcanal-rs`, not
/// in Simble; the test asserts what Simble genuinely observes rather than
/// papering over it.
const PEER_ADDR_AS_WRITTEN: &str = "AA:BB:CC:DD:EE:02";
/// The same controller as Simble sees it.
const PEER_ADDR_AS_SEEN: &str = "02:EE:DD:CC:BB:AA";

/// The one local name the advertiser puts in its AD.
const PEER_NAME: &str = "rootcanal-oracle";

// HCI opcodes used below.
const OP_RESET: u16 = 0x0C03;
const OP_SET_EVENT_MASK: u16 = 0x0C01;
const OP_READ_LOCAL_SUPPORTED_COMMANDS: u16 = 0x1002;
const OP_LE_SET_EVENT_MASK: u16 = 0x2001;
const OP_LE_SET_ADV_PARAMETERS: u16 = 0x2006;
const OP_LE_SET_ADV_DATA: u16 = 0x2008;
const OP_LE_SET_ADV_ENABLE: u16 = 0x200A;
const OP_LE_SET_SCAN_PARAMETERS: u16 = 0x200B;
const OP_LE_SET_SCAN_ENABLE: u16 = 0x200C;

/// A Simble host wired to one controller inside a shared `Rootcanal`.
struct Harness {
    rootcanal: Rootcanal,
    /// Simble's in-memory HCI transport, standing in for a real link.
    channel: HciChannel,
}

impl Harness {
    fn new() -> Self {
        assert!(
            !rootcanal::is_stub(),
            "rootcanal-rs is linked against its stub, which answers every \
             command with a bare success and simulates nothing. This test \
             would be asserting against a fake; refusing to run."
        );

        let mut rootcanal = Rootcanal::new(false);
        rootcanal
            .add_controller(
                HOST_CTRL,
                RcAddress::from_str("AA:BB:CC:DD:EE:01").unwrap(),
                None,
            )
            .unwrap();
        rootcanal
            .add_controller(
                PEER_CTRL,
                RcAddress::from_str(PEER_ADDR_AS_WRITTEN).unwrap(),
                None,
            )
            .unwrap();

        Self {
            rootcanal,
            channel: HciChannel::new(),
        }
    }

    /// Moves packets between Simble's `HciChannel` and the host's controller,
    /// then advances the simulation. Both sides speak H4, so this is a byte
    /// pass-through — no translation layer to get wrong.
    fn pump(&mut self) {
        while let Some(h4) = self.channel.poll_host_packet() {
            self.rootcanal.send_hci(HOST_CTRL, Bytes::from(h4)).unwrap();
        }
        self.rootcanal.tick();
        while let Some(evt) = self.rootcanal.recv_hci(HOST_CTRL).unwrap() {
            self.channel.receive_from_controller(evt.to_vec()).unwrap();
        }
    }

    /// Sends `opcode` from the Simble host and returns the Command Complete
    /// return parameters that follow the status byte.
    fn host_command(&mut self, opcode: u16, params: &[u8]) -> Vec<u8> {
        let mut cmd = vec![opcode as u8, (opcode >> 8) as u8, params.len() as u8];
        cmd.extend_from_slice(params);
        self.channel.send_command(&cmd).unwrap();

        for _ in 0..200 {
            self.pump();
            while let Some(pkt) = self.channel.poll_controller_packet() {
                // 04 | 0e | plen | num_pkts | op_lo | op_hi | status | params
                if pkt.first() != Some(&0x04) || pkt.get(1) != Some(&0x0E) {
                    continue;
                }
                let got = u16::from(pkt[4]) | (u16::from(pkt[5]) << 8);
                if got != opcode {
                    continue;
                }
                assert_eq!(
                    pkt[6], 0x00,
                    "opcode {opcode:#06x} -> status {:#04x}",
                    pkt[6]
                );
                return pkt[7..].to_vec();
            }
        }
        panic!("no Command Complete for opcode {opcode:#06x}");
    }

    /// Sends `opcode` straight to the peer controller, bypassing the Simble
    /// host — the peer is scenery, not the thing under test.
    fn peer_command(&mut self, opcode: u16, params: &[u8]) {
        let mut cmd = vec![0x01, opcode as u8, (opcode >> 8) as u8, params.len() as u8];
        cmd.extend_from_slice(params);
        self.rootcanal
            .send_hci(PEER_CTRL, Bytes::from(cmd))
            .unwrap();
        for _ in 0..10 {
            self.rootcanal.tick();
            let _ = self.rootcanal.drain_hci(PEER_CTRL);
        }
    }

    /// Scans for up to `ticks` milliseconds of real time, returning the first
    /// LE Advertising Report as (advertiser address, local name from its AD).
    fn scan_for(&mut self, ticks: u32) -> Option<(Address, String)> {
        for _ in 0..ticks {
            self.pump();
            // Rootcanal's advertiser runs on steady_clock; time must pass.
            std::thread::sleep(Duration::from_millis(1));
            while let Some(pkt) = self.channel.poll_controller_packet() {
                if let Some(found) = parse_advertising_report(&pkt) {
                    return Some(found);
                }
            }
        }
        None
    }
}

/// Parses an LE Advertising Report into (advertiser address, local name).
///
/// `04 | 3e | plen | 02 | num_reports | evt_type | addr_type | addr[6]
///     | data_len | data.. | rssi`
fn parse_advertising_report(pkt: &[u8]) -> Option<(Address, String)> {
    if pkt.first() != Some(&0x04) || pkt.get(1) != Some(&0x3E) || pkt.get(3) != Some(&0x02) {
        return None;
    }
    let mut be: [u8; 6] = pkt.get(7..13)?.try_into().ok()?;
    be.reverse(); // little-endian on the wire; Address::from_be_bytes wants big
    let data_len = *pkt.get(13)? as usize;
    let data = pkt.get(14..14 + data_len)?;
    Some((Address::from_be_bytes(be), name_from_ad(data)?))
}

/// Pulls the Complete Local Name (AD type 0x09) out of advertising data.
fn name_from_ad(data: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 1 < data.len() {
        let len = data[i] as usize;
        if len == 0 {
            return None;
        }
        // `get` rather than indexing: a truncated final AD structure is a
        // wrong answer to report, not a panic in the test harness.
        let field = data.get(i + 1..i + 1 + len)?;
        if field[0] == 0x09 {
            return String::from_utf8(field[1..].to_vec()).ok();
        }
        i += len + 1;
    }
    None
}

/// Advertising data: Flags + Complete Local Name, in the fixed 31-byte field.
fn advertising_data() -> Vec<u8> {
    let mut ad = vec![0x02, 0x01, 0x06]; // Flags: LE General Discoverable
    ad.push(PEER_NAME.len() as u8 + 1);
    ad.push(0x09); // Complete Local Name
    ad.extend_from_slice(PEER_NAME.as_bytes());
    let mut params = vec![ad.len() as u8];
    params.extend_from_slice(&ad);
    params.resize(32, 0);
    params
}

/// `Read_Local_Supported_Commands` owes a 64-byte bitmap, and the individual
/// bits must be right. The stub answers with **zero** return-parameter bytes
/// and could only fake this by implementing the real table.
#[test]
fn real_controller_answers_with_a_64_byte_supported_commands_bitmap() {
    let mut h = Harness::new();
    h.host_command(OP_RESET, &[]);

    let commands = h.host_command(OP_READ_LOCAL_SUPPORTED_COMMANDS, &[]);
    assert_eq!(
        commands.len(),
        64,
        "answered with {} return-parameter byte(s); a real controller owes a \
         64-byte bitmap. This is exactly what a stub looks like.",
        commands.len()
    );
    assert_ne!(commands, vec![0u8; 64], "bitmap is all zeroes");

    // Named bits, per Core Spec Vol 4 Part E 6.27. These are the very
    // commands the scan test goes on to use, so a wrong bitmap and a working
    // scan cannot both be true.
    assert_eq!(commands[0] & 0x20, 0x20, "octet 0 bit 5: HCI_Disconnect");
    assert_eq!(
        commands[26] & 0x20,
        0x20,
        "octet 26 bit 5: LE_Set_Advertising_Parameters"
    );
    assert_eq!(
        commands[27] & 0x02,
        0x02,
        "octet 27 bit 1: LE_Set_Advertising_Enable"
    );
    assert_eq!(
        commands[27] & 0x08,
        0x08,
        "octet 27 bit 3: LE_Set_Scan_Enable"
    );
}

/// The headline: a Simble host scanning through a real Rootcanal controller
/// discovers a second real controller advertising on the other side of
/// Rootcanal's link layer.
///
/// Nothing here is fakeable by a stub that answers commands uniformly. The
/// advertising report has to be *generated* by the peer's link layer,
/// *routed* by Rootcanal's RF model, and *reassembled* into an LE Meta event
/// by the scanner — carrying the peer's address and the name from its AD.
#[test]
fn simble_host_scans_a_real_rootcanal_advertiser() {
    let mut h = Harness::new();
    h.host_command(OP_RESET, &[]);
    h.peer_command(OP_RESET, &[]);

    // Peer: legacy connectable undirected advertising at the 20 ms minimum,
    // so the test needs as little real time as possible.
    let mut adv_params = vec![0x20, 0x00, 0x20, 0x00, 0x00, 0x00, 0x00];
    adv_params.extend_from_slice(&[0u8; 6]); // peer address (unused)
    adv_params.extend_from_slice(&[0x07, 0x00]); // all channels, allow all
    h.peer_command(OP_LE_SET_ADV_PARAMETERS, &adv_params);
    h.peer_command(OP_LE_SET_ADV_DATA, &advertising_data());
    h.peer_command(OP_LE_SET_ADV_ENABLE, &[0x01]);

    // Host: the LE Meta event is masked OFF in the default event mask, so a
    // scanner that skips this sees nothing no matter how long it waits.
    h.host_command(OP_SET_EVENT_MASK, &[0xFF; 8]);
    h.host_command(OP_LE_SET_EVENT_MASK, &[0xFF, 0, 0, 0, 0, 0, 0, 0]);

    // Passive scanning, window == interval so it never misses.
    h.host_command(
        OP_LE_SET_SCAN_PARAMETERS,
        &[0x00, 0x10, 0x00, 0x10, 0x00, 0x00, 0x00],
    );
    h.host_command(OP_LE_SET_SCAN_ENABLE, &[0x01, 0x00]);

    let (address, name) = h
        .scan_for(500)
        .unwrap_or_else(|| panic!("Simble host received no LE Advertising Report in ~500 ms"));

    assert_eq!(
        address.to_string(),
        PEER_ADDR_AS_SEEN,
        "wrong advertiser address"
    );
    assert_eq!(name, PEER_NAME, "wrong Complete Local Name");
}
