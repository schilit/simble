// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Validates SMP pairing requests, responses, IO capabilities, failure PDUs,
//! LTK<->link-key conversion, identity address resolution, debug mode, and
//! the `PairingSession` state machine end to end (LE Legacy and LE Secure
//! Connections) driven through a real `VirtualDevice`.

use simble::VirtualDevice;
use simble::crypto::smp_crypto::rev;
use simble::device::MemoryBondStore;
use simble::l2cap::{L2capHeader, cid};
use simble::smp::{
    IdentityAddressPreference, KeyStore, PairingConfig, PairingSession, Role,
    SMP_DEBUG_KEY_PUBLIC_X, SMP_DEBUG_KEY_PUBLIC_Y, SMP_TIMEOUT_SECONDS, SmpPairingFailed,
    SmpPairingPacket, error_code, io_capability, opcode, resolve_identity_address,
};
use simble::types::{Address, AddressType};
use zerocopy::IntoBytes;

#[test]
fn test_smp_pairing_request_response_exchange() {
    // 1. Initiator sends Pairing Request
    let req = SmpPairingPacket {
        opcode: opcode::PAIRING_REQUEST,
        io_capability: io_capability::KEYBOARD_DISPLAY,
        oob_data_flag: 0x00,
        auth_req: 0x2D, // Bonding | MITM | SC | Keypress
        max_encryption_key_size: 16,
        initiator_key_distribution: 0x07, // EncKey | IdKey | SignKey
        responder_key_distribution: 0x07,
    };

    let (parsed_req, _) = SmpPairingPacket::parse(req.as_bytes()).expect("Parse Pairing Request");
    assert_eq!(parsed_req.opcode, opcode::PAIRING_REQUEST);
    assert_eq!(parsed_req.io_capability, io_capability::KEYBOARD_DISPLAY);
    assert_eq!(parsed_req.auth_req, 0x2D);

    // 2. Responder replies with Pairing Response
    let resp = SmpPairingPacket::new_response(io_capability::NO_INPUT_NO_OUTPUT, 0x2D, 0x07, 0x07);

    let (parsed_resp, _) =
        SmpPairingPacket::parse(resp.as_bytes()).expect("Parse Pairing Response");
    assert_eq!(parsed_resp.opcode, opcode::PAIRING_RESPONSE);
    assert_eq!(parsed_resp.io_capability, io_capability::NO_INPUT_NO_OUTPUT);
    assert_eq!(parsed_resp.max_encryption_key_size, 16);
}

#[test]
fn test_smp_pairing_failed_pdu() {
    let failed = SmpPairingFailed::new(0x05); // Pairing Not Supported
    let (parsed, _) = SmpPairingFailed::parse(failed.as_bytes()).expect("Parse Pairing Failed");

    assert_eq!(parsed.opcode, opcode::PAIRING_FAILED);
    assert_eq!(parsed.reason, 0x05);
}

/// Bluetooth Core Spec Vol 3, Part H, Section 2.4.2.4 cross-transport key
/// derivation test vectors, matching Bumble's `test_ltk_to_link_key`.
#[test]
fn test_ltk_to_link_key() {
    let ltk = [
        0x64, 0xBF, 0x4F, 0x33, 0x33, 0x6C, 0x06, 0xBD, 0x58, 0x4B, 0x26, 0xE3, 0xBC, 0xF9, 0x8D,
        0x36,
    ];
    assert_eq!(
        PairingSession::derive_link_key(&ltk, false),
        [
            0xB0, 0x8F, 0x38, 0xEE, 0xAF, 0x30, 0x82, 0x0D, 0xBD, 0xC1, 0x3F, 0x63, 0xEF, 0xA4,
            0x1C, 0xBC,
        ]
    );
    assert_eq!(
        PairingSession::derive_link_key(&ltk, true),
        [
            0x35, 0xB8, 0x47, 0x30, 0xF4, 0xF1, 0x39, 0x0A, 0x53, 0x02, 0xA4, 0xDC, 0x79, 0xD3,
            0x7A, 0x28,
        ]
    );
}

/// Matching Bumble's `test_link_key_to_ltk`.
#[test]
fn test_link_key_to_ltk() {
    let link_key = [
        0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x00, 0x01, 0x02, 0x03, 0x04,
        0x05,
    ];
    assert_eq!(
        PairingSession::derive_ltk(&link_key, false),
        [
            0x30, 0x0A, 0x0D, 0xF1, 0x43, 0x9A, 0x2C, 0x8A, 0xA1, 0xDF, 0xA3, 0xF1, 0x72, 0xFB,
            0x13, 0xA8,
        ]
    );
    assert_eq!(
        PairingSession::derive_ltk(&link_key, true),
        [
            0x79, 0xBC, 0x11, 0x32, 0x13, 0x8A, 0x41, 0x69, 0xE2, 0xB3, 0xCC, 0x5E, 0xEB, 0x09,
            0x5E, 0xE8,
        ]
    );
}

/// Matching Bumble's `test_send_identity_address_command` parametrized cases.
#[test]
fn test_send_identity_address_command() {
    let public = Address::from_be_bytes([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
    let random = Address::from_be_bytes([0xEE, 0xEE, 0xEE, 0xEE, 0xEE, 0xEE]);

    // No preference, a public address is available: prefer it.
    assert_eq!(resolve_identity_address(None, public, random), (0, public));
    // No preference, no public address: fall back to the random static one.
    assert_eq!(
        resolve_identity_address(None, Address::ANY, random),
        (1, random)
    );
    // Explicit preference always wins.
    assert_eq!(
        resolve_identity_address(Some(IdentityAddressPreference::Public), public, random),
        (0, public)
    );
    assert_eq!(
        resolve_identity_address(Some(IdentityAddressPreference::Random), public, random),
        (1, random)
    );
}

/// Matching Bumble's `test_smp_debug_mode`: debug mode uses the fixed,
/// publicly-known key pair from the spec so pairing traces are reproducible.
#[test]
fn test_smp_debug_mode() {
    let debug_session = PairingSession::new(
        Role::Initiator,
        PairingConfig {
            debug_mode: true,
            ..PairingConfig::default()
        },
        Address::ANY,
        AddressType::Random,
        Address::ANY,
        AddressType::Random,
    );
    assert_eq!(
        debug_session.local_public_key(),
        (rev(&SMP_DEBUG_KEY_PUBLIC_X), rev(&SMP_DEBUG_KEY_PUBLIC_Y)),
        "the session holds the debug key in SMP wire order (little-endian)"
    );

    let normal_session = PairingSession::new(
        Role::Initiator,
        PairingConfig {
            debug_mode: false,
            ..PairingConfig::default()
        },
        Address::ANY,
        AddressType::Random,
        Address::ANY,
        AddressType::Random,
    );
    assert_ne!(
        normal_session.local_public_key(),
        (rev(&SMP_DEBUG_KEY_PUBLIC_X), rev(&SMP_DEBUG_KEY_PUBLIC_Y)),
        "the session holds the debug key in SMP wire order (little-endian)"
    );
}

/// Shuttles PDUs between two `VirtualDevice`s over real `process_l2cap_packet`
/// calls (cid::SMP), draining each side's multi-PDU key-distribution queue
/// via `poll_smp_pdu`, until pairing converges or the round budget runs out.
/// Returns the two devices and their connection handles for the caller to
/// assert against.
fn run_virtual_device_pairing(sc: bool) -> (VirtualDevice, u16, VirtualDevice, u16) {
    let central_addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    let peripheral_addr = Address::from_be_bytes([0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    let mut central = VirtualDevice::new("central", central_addr, AddressType::Random);
    let mut peripheral = VirtualDevice::new("peripheral", peripheral_addr, AddressType::Random);

    // Both sides carry a bond store so completed pairings are recorded
    // (NimBLE `ble_store` pattern), letting the tests assert the bond landed.
    central.bond_store = Some(Box::new(MemoryBondStore::new()));
    peripheral.bond_store = Some(Box::new(MemoryBondStore::new()));

    let conn_c = 0x0001;
    let conn_p = 0x0002;
    central.on_connected(conn_c, peripheral_addr);
    peripheral.on_connected(conn_p, central_addr);
    // SMP mixes the peer's address type into its crypto, so both sides must
    // record what the (simulated) LE Connection Complete would have carried.
    central.set_peer_address_type(conn_c, AddressType::Random);
    peripheral.set_peer_address_type(conn_p, AddressType::Random);

    let config = PairingConfig {
        sc,
        ..PairingConfig::default()
    };
    let request = central
        .start_pairing_with_config(conn_c, config)
        .expect("central can start pairing");

    let mut to_peripheral = vec![request];
    let mut to_central: Vec<Vec<u8>> = Vec::new();

    for _ in 0..32 {
        if to_peripheral.is_empty() && to_central.is_empty() {
            break;
        }
        let mut next_to_central = Vec::new();
        for pdu in to_peripheral.drain(..) {
            if let Some(reply) = peripheral
                .process_l2cap_packet(conn_p, &pdu)
                .expect("peripheral accepts SMP PDU")
            {
                next_to_central.push(reply);
            }
            while let Some(more) = peripheral.poll_smp_pdu(conn_p) {
                next_to_central.push(more);
            }
        }
        let mut next_to_peripheral = Vec::new();
        for pdu in to_central.drain(..) {
            if let Some(reply) = central
                .process_l2cap_packet(conn_c, &pdu)
                .expect("central accepts SMP PDU")
            {
                next_to_peripheral.push(reply);
            }
            while let Some(more) = central.poll_smp_pdu(conn_c) {
                next_to_peripheral.push(more);
            }
        }
        to_central = next_to_central;
        to_peripheral = next_to_peripheral;
    }

    (central, conn_c, peripheral, conn_p)
}

/// Full pairing-session integration test: two `VirtualDevice`s exchange real
/// SMP PDUs over `process_l2cap_packet`, exactly as two connected peers
/// would, all the way through LE Legacy Confirm/Random (`c1`/`s1`) and key
/// distribution.
#[test]
fn test_virtual_device_le_legacy_pairing_end_to_end() {
    let (central, conn_c, peripheral, conn_p) = run_virtual_device_pairing(false);

    let central_conn = central
        .connections
        .get(&conn_c)
        .expect("central connection still tracked");
    let peripheral_conn = peripheral
        .connections
        .get(&conn_p)
        .expect("peripheral connection still tracked");

    assert!(
        central_conn.is_encrypted,
        "central must reach encrypted state"
    );
    assert!(
        peripheral_conn.is_encrypted,
        "peripheral must reach encrypted state"
    );
    let central_session = central_conn
        .pairing_session
        .as_ref()
        .expect("central has a pairing session");
    let peripheral_session = peripheral_conn
        .pairing_session
        .as_ref()
        .expect("peripheral has a pairing session");
    assert!(central_session.is_complete());
    assert!(peripheral_session.is_complete());
    assert!(!central_session.is_failed());
    assert!(!peripheral_session.is_failed());

    // LE Legacy pairing derives the link's STK identically on both sides.
    assert_eq!(central_conn.ltk, peripheral_conn.ltk);
    assert!(central_conn.ltk.is_some());

    // The session's negotiated bonding keys are ready to hand to a
    // Bumble-`JsonKeyStore`-compatible `KeyStore`.
    let keystore = KeyStore::new(None);
    keystore.update("peripheral", peripheral_session.pairing_keys());
    let stored = keystore.get("peripheral").expect("keys were stored");
    assert!(stored.ltk_central.is_some() || stored.ltk_peripheral.is_some());

    // Pairing completion also recorded the bond in each device's bond
    // store, keyed by the peer's distributed identity address (both use
    // random static addresses, so identity == connection address here).
    let central_addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    let peripheral_addr = Address::from_be_bytes([0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    let bond = peripheral
        .bond_store
        .as_deref()
        .unwrap()
        .load_security(central_addr)
        .expect("peripheral recorded the bond");
    assert!(!bond.secure_connections);
    assert_eq!(bond.key_size, 16);
    // LE Legacy distributes per-role LTKs: the responder holds its own
    // central-role key (with EDIV/RAND) plus the initiator's.
    let ltk_central = bond.keys.ltk_central.expect("responder's own legacy LTK");
    assert!(ltk_central.ediv.is_some());
    assert!(ltk_central.rand.is_some());
    assert!(bond.keys.ltk_peripheral.is_some());
    assert!(
        central
            .bond_store
            .as_deref()
            .unwrap()
            .load_security(peripheral_addr)
            .is_some(),
        "central recorded the bond too"
    );
}

/// Same end-to-end drive as above, but negotiating LE Secure Connections:
/// exercises Public Key exchange, Confirm/Random via `f4`, and DHKey Check
/// via `f5`/`f6` through the real `VirtualDevice` wiring.
#[test]
fn test_virtual_device_le_secure_connections_pairing_end_to_end() {
    let (central, conn_c, peripheral, conn_p) = run_virtual_device_pairing(true);

    let central_conn = central
        .connections
        .get(&conn_c)
        .expect("central connection still tracked");
    let peripheral_conn = peripheral
        .connections
        .get(&conn_p)
        .expect("peripheral connection still tracked");

    let central_session = central_conn
        .pairing_session
        .as_ref()
        .expect("central has a pairing session");
    let peripheral_session = peripheral_conn
        .pairing_session
        .as_ref()
        .expect("peripheral has a pairing session");
    assert!(central_session.is_complete());
    assert!(peripheral_session.is_complete());
    assert!(!central_session.is_failed());
    assert!(!peripheral_session.is_failed());

    // LE Secure Connections derives a single shared LTK (via f5) rather than
    // per-role legacy keys.
    assert_eq!(central_conn.ltk, peripheral_conn.ltk);
    assert!(central_conn.ltk.is_some());

    let keystore = KeyStore::new(None);
    keystore.update("peripheral", peripheral_session.pairing_keys());
    let stored = keystore.get("peripheral").expect("keys were stored");
    assert!(stored.ltk.is_some());

    // The bond landed in the peripheral's bond store with the link's LTK
    // and Secure Connections metadata.
    let central_addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    let link_ltk = central_conn.ltk.expect("link is keyed");
    let bond = peripheral
        .bond_store
        .as_deref()
        .unwrap()
        .load_security(central_addr)
        .expect("peripheral recorded the bond");
    assert!(bond.secure_connections);
    assert_eq!(bond.key_size, 16);
    assert_eq!(
        bond.keys.ltk.as_ref().expect("SC LTK stored").value,
        link_ltk
    );

    // Reconnect: the bond record survives the connection teardown, so the
    // stored LTK is available to re-encrypt the new link without pairing
    // again, and the new connection is findable by peer address.
    let mut peripheral = peripheral;
    peripheral.on_disconnected(conn_p);
    peripheral.on_connected(0x0033, central_addr);
    assert_eq!(
        peripheral
            .connection_by_address(central_addr)
            .expect("reconnected link found by address")
            .handle,
        0x0033
    );
    let bond = peripheral
        .bond_store
        .as_deref()
        .unwrap()
        .load_security(central_addr)
        .expect("bond survives reconnect");
    assert_eq!(bond.keys.ltk.expect("SC LTK still stored").value, link_ltk);
}

// ===========================================================================
//  Failure paths — the half of SMP that nothing exercised
// ===========================================================================
//
// Coverage analysis found `PairingSession::fail()` with an execution count of
// **zero**, and no test anywhere referencing `CONFIRM_VALUE_FAILED` or
// `DHKEY_CHECK_FAILED`. That matters more than an ordinary coverage hole: the
// confirm-value comparison IS the man-in-the-middle defence of LE pairing.
//
// Both peers compute the confirm with the same code, so the happy path agrees
// with itself whether or not the comparison is load-bearing. If the check
// returned `Ok(())` instead of failing, every other test in this repo would
// still pass. These tests exist to make that impossible: each corrupts a PDU
// in flight and asserts not just that a Pairing Failed comes back, but that
// **no key was derived and the link never became encrypted** — the thing an
// attacker would actually be after.

/// The SMP opcode inside an L2CAP frame, and the offset of its first payload
/// octet. `process_l2cap_packet` is given whole frames — 2 octets of length,
/// 2 of CID (0x0006 for SMP), then the SMP PDU — so a tamper function has to
/// look past the header rather than at byte 0.
const L2CAP_HEADER: usize = 4;

fn smp_opcode(frame: &[u8]) -> Option<u8> {
    frame.get(L2CAP_HEADER).copied()
}

/// Runs a pairing exchange, applying `tamper` to every PDU in flight.
///
/// `tamper` sees each PDU just before it is delivered and may rewrite it —
/// standing in for an active attacker on the link, which is exactly the threat
/// the confirm value defends against.
fn run_pairing_with_tampering(
    sc: bool,
    mut tamper: impl FnMut(&mut Vec<u8>),
) -> (VirtualDevice, u16, VirtualDevice, u16) {
    let central_addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
    let peripheral_addr = Address::from_be_bytes([0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
    let mut central = VirtualDevice::new("central", central_addr, AddressType::Random);
    let mut peripheral = VirtualDevice::new("peripheral", peripheral_addr, AddressType::Random);
    central.bond_store = Some(Box::new(MemoryBondStore::new()));
    peripheral.bond_store = Some(Box::new(MemoryBondStore::new()));

    let (conn_c, conn_p) = (0x0001, 0x0002);
    central.on_connected(conn_c, peripheral_addr);
    peripheral.on_connected(conn_p, central_addr);
    central.set_peer_address_type(conn_c, AddressType::Random);
    peripheral.set_peer_address_type(conn_p, AddressType::Random);

    let config = PairingConfig {
        sc,
        ..PairingConfig::default()
    };
    let request = central
        .start_pairing_with_config(conn_c, config)
        .expect("central can start pairing");

    let mut to_peripheral = vec![request];
    let mut to_central: Vec<Vec<u8>> = Vec::new();

    for _ in 0..32 {
        if to_peripheral.is_empty() && to_central.is_empty() {
            break;
        }
        let mut next_to_central = Vec::new();
        for mut pdu in to_peripheral.drain(..) {
            tamper(&mut pdu);
            if let Some(reply) = peripheral
                .process_l2cap_packet(conn_p, &pdu)
                .expect("peripheral must not error on a tampered PDU")
            {
                next_to_central.push(reply);
            }
            while let Some(more) = peripheral.poll_smp_pdu(conn_p) {
                next_to_central.push(more);
            }
        }
        let mut next_to_peripheral = Vec::new();
        for mut pdu in to_central.drain(..) {
            tamper(&mut pdu);
            if let Some(reply) = central
                .process_l2cap_packet(conn_c, &pdu)
                .expect("central must not error on a tampered PDU")
            {
                next_to_peripheral.push(reply);
            }
            while let Some(more) = central.poll_smp_pdu(conn_c) {
                next_to_peripheral.push(more);
            }
        }
        to_central = next_to_central;
        to_peripheral = next_to_peripheral;
    }
    (central, conn_c, peripheral, conn_p)
}

/// Neither side may end up encrypted or holding a key. This is the assertion
/// that makes the confirm check load-bearing — a Pairing Failed PDU alone
/// would still be satisfied by an implementation that failed *and then*
/// derived a key anyway.
fn assert_pairing_was_refused(
    central: &VirtualDevice,
    conn_c: u16,
    peripheral: &VirtualDevice,
    conn_p: u16,
) {
    for (name, device, conn) in [
        ("central", central, conn_c),
        ("peripheral", peripheral, conn_p),
    ] {
        let connection = device
            .connections
            .get(&conn)
            .unwrap_or_else(|| panic!("{name} connection still tracked"));
        assert!(
            !connection.is_encrypted,
            "{name} must NOT reach encrypted state after a failed pairing",
        );
        assert!(
            connection.ltk.is_none(),
            "{name} must NOT hold a key after a failed pairing",
        );
        if let Some(session) = connection.pairing_session.as_ref() {
            assert!(!session.is_complete(), "{name} pairing must not complete");
        }
    }
}

/// A tampered Pairing Confirm must be rejected — LE Legacy.
///
/// Flipping one bit of the confirm is the simplest possible active attack.
/// `c1` over the peer's random will not reproduce it, and the session must
/// stop rather than continue to key derivation.
#[test]
fn legacy_pairing_rejects_a_tampered_confirm_and_derives_no_key() {
    let mut corrupted = false;
    let (central, conn_c, peripheral, conn_p) = run_pairing_with_tampering(false, |pdu| {
        if smp_opcode(pdu) == Some(opcode::PAIRING_CONFIRM) && !corrupted {
            pdu[L2CAP_HEADER + 1] ^= 0x01;
            corrupted = true;
        }
    });
    assert!(
        corrupted,
        "the exchange must have carried a Pairing Confirm"
    );

    let failed = [&central, &peripheral].iter().any(|d| {
        d.connections
            .values()
            .filter_map(|c| c.pairing_session.as_ref())
            .any(|s| s.is_failed())
    });
    assert!(failed, "a tampered confirm must fail the pairing");
    assert_pairing_was_refused(&central, conn_c, &peripheral, conn_p);
}

/// The same attack against LE Secure Connections, where the confirm is `f4`
/// over the two public keys rather than `c1` over the TK.
#[test]
fn secure_connections_rejects_a_tampered_confirm_and_derives_no_key() {
    let mut corrupted = false;
    let (central, conn_c, peripheral, conn_p) = run_pairing_with_tampering(true, |pdu| {
        if smp_opcode(pdu) == Some(opcode::PAIRING_CONFIRM) && !corrupted {
            pdu[L2CAP_HEADER + 1] ^= 0x80;
            corrupted = true;
        }
    });
    assert!(
        corrupted,
        "the exchange must have carried a Pairing Confirm"
    );
    assert_pairing_was_refused(&central, conn_c, &peripheral, conn_p);
}

/// A tampered DH Key Check must be rejected.
///
/// This is the second half of Secure Connections' authentication: even with a
/// matching confirm, `Ea`/`Eb` prove both sides derived the same LTK from the
/// same DHKey. Corrupting it is how a downgrade would show up.
#[test]
fn secure_connections_rejects_a_tampered_dhkey_check_and_derives_no_key() {
    let mut corrupted = false;
    let (central, conn_c, peripheral, conn_p) = run_pairing_with_tampering(true, |pdu| {
        if smp_opcode(pdu) == Some(opcode::PAIRING_DHKEY_CHECK) && !corrupted {
            pdu[L2CAP_HEADER + 1] ^= 0x01;
            corrupted = true;
        }
    });
    assert!(corrupted, "the exchange must have carried a DH Key Check");
    assert_pairing_was_refused(&central, conn_c, &peripheral, conn_p);
}

/// A truncated SMP PDU must be answered, not panic.
///
/// Every guard in the state machine returns `INVALID_PARAMETERS`, and none of
/// them had ever executed. A peer can send any of these, so each must survive
/// a length one byte short of what its parser needs — the same class of bug as
/// the one-octet ATT PDU that panicked a central during discovery.
#[test]
fn truncated_smp_pdus_are_refused_rather_than_panicking() {
    for opcode_byte in [
        opcode::PAIRING_REQUEST,
        opcode::PAIRING_RESPONSE,
        opcode::PAIRING_CONFIRM,
        opcode::PAIRING_RANDOM,
        opcode::PAIRING_PUBLIC_KEY,
        opcode::PAIRING_DHKEY_CHECK,
    ] {
        for len in 1..4 {
            let central_addr = Address::from_be_bytes([0x11, 0x22, 0x33, 0x44, 0x55, 0x66]);
            let peer_addr = Address::from_be_bytes([0x66, 0x55, 0x44, 0x33, 0x22, 0x11]);
            let mut device = VirtualDevice::new("victim", central_addr, AddressType::Random);
            device.on_connected(0x0001, peer_addr);
            device.set_peer_address_type(0x0001, AddressType::Random);

            let mut pdu = vec![opcode_byte];
            pdu.resize(len, 0x00);
            // The assertion is that this returns at all.
            let _ = device.process_l2cap_packet(0x0001, &pdu);
        }
    }
}

// ===========================================================================
//  Section 3.4 — the Security Manager Timer
// ===========================================================================
//
// "To protect the Security Manager protocol from stalling, a Security Manager
// Timer is used... If the Security Manager Timer reaches 30 seconds, the
// procedure shall be considered to have failed, and the local higher layer
// shall be notified. No further SMP commands shall be sent over the L2CAP
// Security Manager Channel. A new Pairing process shall only be performed when
// a new physical link has been established."
//   — Bluetooth Core Spec Vol 3, Part H, Section 3.4
//
// A `PairingSession` has no clock of its own. It takes the same monotonic
// `t_seconds` the rest of the simulator ticks on, via `VirtualDevice::tick_smp`
// — so a runtime that never ticks never times a pairing out (which is what
// every other test in this file wants), and one that does can jump the whole
// 30 seconds in a single call.

const CENTRAL_ADDR: [u8; 6] = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66];
const PERIPHERAL_ADDR: [u8; 6] = [0x66, 0x55, 0x44, 0x33, 0x22, 0x11];

/// A central that has sent its Pairing Request and is waiting for a Pairing
/// Response that never comes — the stall Section 3.4 exists to break.
///
/// Returns the device, its connection handle, and the request PDU, so a test
/// that wants the exchange to make progress can deliver it by hand.
fn stalled_pairing(sc: bool) -> (VirtualDevice, u16, Vec<u8>) {
    let central_addr = Address::from_be_bytes(CENTRAL_ADDR);
    let peripheral_addr = Address::from_be_bytes(PERIPHERAL_ADDR);
    let mut central = VirtualDevice::new("central", central_addr, AddressType::Random);
    central.bond_store = Some(Box::new(MemoryBondStore::new()));
    let conn = 0x0001;
    central.on_connected(conn, peripheral_addr);
    central.set_peer_address_type(conn, AddressType::Random);
    let request = central
        .start_pairing_with_config(
            conn,
            PairingConfig {
                sc,
                ..PairingConfig::default()
            },
        )
        .expect("central can start pairing");
    (central, conn, request)
}

fn session_is_failed(device: &VirtualDevice, conn: u16) -> bool {
    device.connections[&conn]
        .pairing_session
        .as_ref()
        .expect("session exists")
        .is_failed()
}

fn session_timed_out(device: &VirtualDevice, conn: u16) -> bool {
    device.connections[&conn]
        .pairing_session
        .as_ref()
        .expect("session exists")
        .is_timed_out()
}

/// Wraps a bare SMP PDU in the L2CAP frame `process_l2cap_packet` expects:
/// two octets of length, two of CID (0x0006), then the PDU.
fn smp_frame(pdu: &[u8]) -> Vec<u8> {
    L2capHeader::serialize(cid::SMP, pdu)
}

/// A Pairing Response that would drive the central's state machine forward if
/// anything were still listening.
fn valid_pairing_response() -> Vec<u8> {
    SmpPairingPacket::new_response(io_capability::NO_INPUT_NO_OUTPUT, 0x09, 0x03, 0x03)
        .as_bytes()
        .to_vec()
}

/// The timer's whole point: a pairing that stalls is not left half-open
/// forever, and the key material does not survive it.
///
/// The 29-second half of this is not decoration. Without it, a timer that
/// fired the instant it was ticked at all would pass just as well, and the
/// "30 seconds" would be a number nothing checked.
#[test]
fn the_security_manager_timer_fails_a_stalled_pairing_after_thirty_seconds() {
    let (mut central, conn, _request) = stalled_pairing(true);

    central.tick_smp(0.0);
    central.tick_smp(SMP_TIMEOUT_SECONDS - 1.0);
    assert!(
        !session_is_failed(&central, conn),
        "one second short of the timeout, the pairing is still running"
    );

    central.tick_smp(SMP_TIMEOUT_SECONDS);
    assert!(
        session_is_failed(&central, conn),
        "the Security Manager Timer reached 30 seconds; the procedure has failed"
    );
    assert!(
        session_timed_out(&central, conn),
        "and it failed by timeout, which is not the same as a Pairing Failed"
    );

    let connection = &central.connections[&conn];
    assert!(
        !connection.is_encrypted,
        "a timed-out pairing must not leave the link encrypted"
    );
    assert!(
        connection.ltk.is_none(),
        "a timed-out pairing must not leave a key behind"
    );
    let session = connection.pairing_session.as_ref().expect("session exists");
    assert!(session.ltk().is_none(), "the session's key is gone too");
    assert!(!session.is_complete());
}

/// "No further SMP commands shall be sent over the L2CAP Security Manager
/// Channel." Not one — so the response that arrives late gets no reply at all,
/// not even a Pairing Failed explaining why.
#[test]
fn a_timed_out_session_ignores_every_later_smp_pdu() {
    let (mut central, conn, _request) = stalled_pairing(true);
    central.tick_smp(0.0);
    central.tick_smp(SMP_TIMEOUT_SECONDS);
    assert!(session_timed_out(&central, conn));

    for pdu in [
        valid_pairing_response(),
        vec![opcode::PAIRING_CONFIRM; 17],
        vec![opcode::PAIRING_RANDOM; 17],
        vec![opcode::PAIRING_REQUEST, 0x03, 0x00, 0x09, 0x10, 0x03, 0x03],
    ] {
        let reply = central
            .process_l2cap_packet(conn, &smp_frame(&pdu))
            .expect("a timed-out link must not error on a late PDU");
        assert!(
            reply.is_none(),
            "a timed-out session must send nothing at all in reply to {:#04X}",
            pdu[0]
        );
        assert!(
            central.poll_smp_pdu(conn).is_none(),
            "and must have queued nothing either, after {:#04X}",
            pdu[0]
        );
    }
    assert!(central.connections[&conn].ltk.is_none());
}

/// "The Security Manager Timer shall be reset when an L2CAP SMP command is
/// queued for transmission."
///
/// A pairing that keeps making progress must never time out, however long it
/// takes in total. Here every exchange lands 20 seconds after the last — well
/// inside the timeout individually, far past it cumulatively — and the pairing
/// still completes.
#[test]
fn every_queued_command_restarts_the_security_manager_timer() {
    let central_addr = Address::from_be_bytes(CENTRAL_ADDR);
    let peripheral_addr = Address::from_be_bytes(PERIPHERAL_ADDR);
    let mut central = VirtualDevice::new("central", central_addr, AddressType::Random);
    let mut peripheral = VirtualDevice::new("peripheral", peripheral_addr, AddressType::Random);
    let (conn_c, conn_p) = (0x0001, 0x0002);
    central.on_connected(conn_c, peripheral_addr);
    peripheral.on_connected(conn_p, central_addr);
    central.set_peer_address_type(conn_c, AddressType::Random);
    peripheral.set_peer_address_type(conn_p, AddressType::Random);

    let request = central
        .start_pairing_with_config(conn_c, PairingConfig::default())
        .expect("central can start pairing");

    let mut clock = 0.0f64;
    let mut to_peripheral = vec![request];
    let mut to_central: Vec<Vec<u8>> = Vec::new();
    for _ in 0..32 {
        if to_peripheral.is_empty() && to_central.is_empty() {
            break;
        }
        // A slow link: a third of the timeout goes by between rounds, so a
        // full request/reply round trip costs each side two thirds of it —
        // comfortably inside the timeout per exchange, far past it in total.
        clock += SMP_TIMEOUT_SECONDS / 3.0;
        central.tick_smp(clock);
        peripheral.tick_smp(clock);

        let mut next_to_central = Vec::new();
        for pdu in to_peripheral.drain(..) {
            if let Some(reply) = peripheral.process_l2cap_packet(conn_p, &pdu).unwrap() {
                next_to_central.push(reply);
            }
            while let Some(more) = peripheral.poll_smp_pdu(conn_p) {
                next_to_central.push(more);
            }
        }
        let mut next_to_peripheral = Vec::new();
        for pdu in to_central.drain(..) {
            if let Some(reply) = central.process_l2cap_packet(conn_c, &pdu).unwrap() {
                next_to_peripheral.push(reply);
            }
            while let Some(more) = central.poll_smp_pdu(conn_c) {
                next_to_peripheral.push(more);
            }
        }
        to_central = next_to_central;
        to_peripheral = next_to_peripheral;
    }

    assert!(
        clock > SMP_TIMEOUT_SECONDS,
        "the exchange has to outlast the timeout for this test to mean anything \
         (it ran {clock} simulated seconds)"
    );
    assert!(!session_is_failed(&central, conn_c));
    assert!(!session_is_failed(&peripheral, conn_p));
    assert!(central.connections[&conn_c].is_encrypted);
    assert_eq!(
        central.connections[&conn_c].ltk,
        peripheral.connections[&conn_p].ltk
    );
}

/// "When a Pairing process completes (whether successfully or not), the
/// Security Manager Timer shall be stopped."
///
/// A bonded connection outlives its pairing by hours. If the timer kept
/// running after key distribution finished, the session would time out on a
/// perfectly healthy link and throw away the LTK the link is encrypted with.
#[test]
fn the_timer_stops_when_pairing_completes_and_a_bonded_link_survives() {
    let (mut central, conn_c, mut peripheral, conn_p) = run_virtual_device_pairing(true);
    let ltk = central.connections[&conn_c].ltk;
    assert!(ltk.is_some(), "the pairing completed and keyed the link");

    // Hours of simulated uptime on an idle, already-encrypted link.
    for step in 0..10 {
        let clock = f64::from(step) * SMP_TIMEOUT_SECONDS * 4.0;
        central.tick_smp(clock);
        peripheral.tick_smp(clock);
    }

    assert!(!session_is_failed(&central, conn_c));
    assert!(!session_is_failed(&peripheral, conn_p));
    assert_eq!(
        central.connections[&conn_c].ltk, ltk,
        "the key is still here"
    );
    assert!(central.connections[&conn_c].is_encrypted);
    assert!(
        central.connections[&conn_c]
            .pairing_session
            .as_ref()
            .unwrap()
            .is_complete()
    );
}

/// The first tick establishes the clock's baseline and consumes no time.
///
/// A session is created whenever a peer decides to pair, which on a
/// long-running scene is at second 900, not second zero. If the first tick
/// charged the session the scene's absolute time, every pairing after the
/// first 30 seconds of uptime would be dead on arrival.
#[test]
fn the_first_tick_only_baselines_the_clock() {
    let (mut central, conn, _request) = stalled_pairing(true);
    let late = 900.0;

    central.tick_smp(late);
    assert!(
        !session_is_failed(&central, conn),
        "a pairing started at simulated second {late} gets its own 30 seconds"
    );
    central.tick_smp(late + SMP_TIMEOUT_SECONDS - 1.0);
    assert!(!session_is_failed(&central, conn));
    central.tick_smp(late + SMP_TIMEOUT_SECONDS);
    assert!(
        session_timed_out(&central, conn),
        "and then it times out, 30 seconds after it started"
    );
}

/// "A new Pairing process shall only be performed when a new physical link has
/// been established" (Section 3.4) — which an ordinary Pairing Failed does not
/// require: after one of those, "any subsequent pairing procedure shall restart
/// from the Pairing Feature Exchange phase" (Section 3.5.5). The two failure
/// modes are deliberately not interchangeable, so this test pins both halves.
#[test]
fn a_timed_out_link_refuses_a_new_pairing_until_it_is_re_established() {
    let (mut central, conn, _request) = stalled_pairing(true);
    central.tick_smp(0.0);
    central.tick_smp(SMP_TIMEOUT_SECONDS);
    assert!(session_timed_out(&central, conn));

    assert!(
        central
            .start_pairing_with_config(conn, PairingConfig::default())
            .is_err(),
        "the same physical link may not start a second pairing after a timeout"
    );

    // A new physical link clears it: disconnect, reconnect, pair again.
    let peripheral_addr = Address::from_be_bytes(PERIPHERAL_ADDR);
    central.on_disconnected(conn);
    central.on_connected(0x0044, peripheral_addr);
    central.set_peer_address_type(0x0044, AddressType::Random);
    assert!(
        central
            .start_pairing_with_config(0x0044, PairingConfig::default())
            .is_ok(),
        "a new physical link may pair"
    );
}

/// A plain Pairing Failed, by contrast, does not lock the link (Section 3.5.5).
#[test]
fn a_pairing_failed_link_may_start_a_new_pairing_procedure() {
    let (mut central, conn, _request) = stalled_pairing(true);
    central
        .process_l2cap_packet(
            conn,
            &smp_frame(&[opcode::PAIRING_FAILED, error_code::CONFIRM_VALUE_FAILED]),
        )
        .expect("the peer's rejection is accepted");
    assert!(session_is_failed(&central, conn));
    assert!(
        !session_timed_out(&central, conn),
        "rejected is not the same as timed out"
    );
    assert!(
        central
            .start_pairing_with_config(conn, PairingConfig::default())
            .is_ok(),
        "Section 3.5.5: a subsequent procedure restarts from the Feature Exchange"
    );
}

// ===========================================================================
//  Section 3.5.5 — nothing is processed after a failure
// ===========================================================================

/// A responder session, ready to be fed hand-built PDUs.
fn responder_session() -> PairingSession {
    PairingSession::new(
        Role::Responder,
        PairingConfig::default(),
        Address::from_be_bytes(PERIPHERAL_ADDR),
        AddressType::Random,
        Address::from_be_bytes(CENTRAL_ADDR),
        AddressType::Random,
    )
}

/// An initiator session that has already sent its Pairing Request.
fn started_initiator_session() -> PairingSession {
    let mut session = PairingSession::new(
        Role::Initiator,
        PairingConfig::default(),
        Address::from_be_bytes(CENTRAL_ADDR),
        AddressType::Random,
        Address::from_be_bytes(PERIPHERAL_ADDR),
        AddressType::Random,
    );
    let _ = session.start();
    session
}

/// A Pairing Request PDU with every field settable, so a test can put exactly
/// one of them outside the range its table defines.
fn pairing_request(io_capability: u8, oob: u8, auth_req: u8, key_size: u8) -> Vec<u8> {
    vec![
        opcode::PAIRING_REQUEST,
        io_capability,
        oob,
        auth_req,
        key_size,
        0x03,
        0x03,
    ]
}

/// Once a session has failed, later PDUs are dropped — *silently*.
///
/// The silence is the part worth a test. Answering each late PDU with another
/// Pairing Failed would look like defensive coding and would break two rules at
/// once: Section 3.4's "No further SMP commands shall be sent over the L2CAP
/// Security Manager Channel", and Section 3.5.5's "no further communication for
/// the current pairing procedure is to occur". Two simble peers doing it would
/// also trade Pairing Failed PDUs at each other indefinitely, since each one is
/// itself an SMP command the other would answer.
#[test]
fn a_failed_session_drops_later_pdus_instead_of_answering_them() {
    let mut session = responder_session();
    // Fail it with a role violation — a Pairing Response aimed at a responder,
    // which is nobody's job to answer. Deliberately *not* one of the parameter
    // guards below: this test is about what happens after a failure, and it
    // should not go green or red because one of those changed.
    let reply = session
        .handle_pdu(
            SmpPairingPacket::new_response(io_capability::NO_INPUT_NO_OUTPUT, 0x09, 0x03, 0x03)
                .as_bytes(),
        )
        .expect("a misdirected PDU is refused, not an error");
    assert_eq!(
        reply,
        Some(vec![
            opcode::PAIRING_FAILED,
            error_code::COMMAND_NOT_SUPPORTED
        ]),
        "the first bad PDU does get a Pairing Failed"
    );
    assert!(session.is_failed());

    // Everything after it does not — including a perfectly valid Pairing
    // Request, which without the guard would walk the state machine straight
    // back into Phase 1 on a session that has already given up.
    for pdu in [
        pairing_request(io_capability::NO_INPUT_NO_OUTPUT, 0x00, 0x09, 16),
        vec![opcode::PAIRING_CONFIRM; 17],
        vec![opcode::PAIRING_RANDOM; 17],
        vec![opcode::PAIRING_PUBLIC_KEY; 65],
        vec![opcode::PAIRING_DHKEY_CHECK; 17],
        vec![opcode::PAIRING_FAILED, error_code::UNSPECIFIED_REASON],
        vec![0x99],
    ] {
        let code = pdu[0];
        assert_eq!(
            session.handle_pdu(&pdu).expect("no error"),
            None,
            "a failed session must answer nothing to {code:#04X}"
        );
        assert!(
            session.poll_pending().is_none(),
            "and queue nothing after {code:#04X}"
        );
    }
    assert!(session.is_failed());
    assert!(!session.is_complete());
    assert!(session.ltk().is_none());
}

// ===========================================================================
//  Section 3.5.1 / Table 3.7 — per-opcode Invalid Parameters
// ===========================================================================
//
// "The Invalid Parameters error code indicates that the command length is
// invalid or that a parameter is outside of the specified range."
//   — Bluetooth Core Spec Vol 3, Part H, Table 3.7, reason 0x0A
//
// Two checks per opcode, then: an exact length, and fields inside the ranges
// Tables 3.4 (IO capability), 3.5 (OOB data flag), 3.6 (Bonding_Flags) and
// Section 2.3.4 (7 to 16 octet keys) define.

const PAIRING_FAILED_INVALID_PARAMETERS: [u8; 2] =
    [opcode::PAIRING_FAILED, error_code::INVALID_PARAMETERS];

/// Every out-of-range field in a Pairing Request, one at a time.
///
/// Each row leaves the other five fields valid, so a row can only fail because
/// of the field it names — delete any single guard in
/// `parse_pairing_parameters` and exactly one row goes red.
#[test]
fn pairing_request_fields_outside_their_specified_range_are_refused() {
    let cases: &[(&str, Vec<u8>)] = &[
        // Table 3.4: 0x05 to 0xFF are reserved for future use.
        ("IO capability 0x05", pairing_request(0x05, 0x00, 0x09, 16)),
        ("IO capability 0xFF", pairing_request(0xFF, 0x00, 0x09, 16)),
        // Table 3.5: 0x02 to 0xFF are reserved for future use.
        ("OOB data flag 0x02", pairing_request(0x03, 0x02, 0x09, 16)),
        // Section 2.3.4: "in the range 7 to 16 octets".
        ("key size 0", pairing_request(0x03, 0x00, 0x09, 0)),
        ("key size 6", pairing_request(0x03, 0x00, 0x09, 6)),
        ("key size 17", pairing_request(0x03, 0x00, 0x09, 17)),
        ("key size 255", pairing_request(0x03, 0x00, 0x09, 255)),
        // Table 3.6: Bonding_Flags 0b10 and 0b11 are reserved, so AuthReq bit
        // 1 must be clear.
        ("Bonding_Flags 0b10", pairing_request(0x03, 0x00, 0x0A, 16)),
        // Figure 3.3: AuthReq bits 6 and 7 are reserved for future use.
        ("AuthReq bit 6", pairing_request(0x03, 0x00, 0x49, 16)),
        ("AuthReq bit 7", pairing_request(0x03, 0x00, 0x89, 16)),
    ];

    for (name, pdu) in cases {
        let mut session = responder_session();
        assert_eq!(
            session.handle_pdu(pdu).expect("refused, not an error"),
            Some(PAIRING_FAILED_INVALID_PARAMETERS.to_vec()),
            "{name} must be answered with Pairing Failed / Invalid Parameters"
        );
        assert!(session.is_failed(), "{name} must fail the pairing");
        assert!(session.ltk().is_none(), "{name} must derive no key");
    }

    // And the control: the same PDU with every field in range is accepted.
    let mut session = responder_session();
    let reply = session
        .handle_pdu(&pairing_request(
            io_capability::KEYBOARD_DISPLAY,
            0x01,
            0x2D,
            7,
        ))
        .expect("no error")
        .expect("a valid Pairing Request gets a Pairing Response");
    assert_eq!(
        reply[0],
        opcode::PAIRING_RESPONSE,
        "the range checks must not reject legal values: KeyboardDisplay, OOB \
         present, every defined AuthReq bit set, and the minimum 7-octet key"
    );
    assert!(!session.is_failed());
}

/// A Pairing Request that is valid for seven octets and then carries an eighth.
///
/// `SmpPairingPacket::parse` is a zero-copy *prefix* parse and returns the
/// trailing bytes rather than rejecting them, so this one only fails on an
/// explicit length check. It matters more than a stray byte usually would:
/// `Preq` and `Pres` are fed verbatim into the confirm value, so the two peers
/// have to agree on exactly which seven octets those are.
#[test]
fn an_over_long_pairing_request_is_refused_despite_a_valid_prefix() {
    let mut valid = pairing_request(io_capability::NO_INPUT_NO_OUTPUT, 0x00, 0x09, 16);
    valid.push(0xAA);
    let mut session = responder_session();
    assert_eq!(
        session.handle_pdu(&valid).expect("refused, not an error"),
        Some(PAIRING_FAILED_INVALID_PARAMETERS.to_vec()),
    );
    assert!(session.is_failed());
}

/// Every fixed-length SMP PDU the state machine accepts, one octet short and
/// one octet long.
///
/// Reason 0x0A's first clause is "the command length is invalid", and there is
/// one length check per opcode arm to get wrong. A peer can send any of these
/// at any time, so each is checked before the PDU's contents are looked at,
/// which is why a single table can cover them all regardless of phase.
#[test]
fn every_fixed_length_smp_pdu_is_refused_at_the_wrong_length() {
    // (opcode, exact length in octets, role that will even look at it)
    let cases: &[(u8, usize, Role)] = &[
        (opcode::PAIRING_REQUEST, 7, Role::Responder),
        (opcode::PAIRING_RESPONSE, 7, Role::Initiator),
        (opcode::PAIRING_CONFIRM, 17, Role::Initiator),
        (opcode::PAIRING_RANDOM, 17, Role::Initiator),
        (opcode::PAIRING_FAILED, 2, Role::Initiator),
        (opcode::ENCRYPTION_INFO, 17, Role::Initiator),
        (opcode::MASTER_IDENTIFICATION, 11, Role::Initiator),
        (opcode::IDENTITY_INFO, 17, Role::Initiator),
        (opcode::IDENTITY_ADDR_INFO, 8, Role::Initiator),
        (opcode::SIGNING_INFO, 17, Role::Initiator),
        (opcode::PAIRING_PUBLIC_KEY, 65, Role::Initiator),
        (opcode::PAIRING_DHKEY_CHECK, 17, Role::Initiator),
    ];

    for &(code, exact, role) in cases {
        for length in [exact - 1, exact + 1] {
            let mut session = match role {
                Role::Responder => responder_session(),
                Role::Initiator => started_initiator_session(),
            };
            let mut pdu = vec![0x00; length];
            pdu[0] = code;
            assert_eq!(
                session.handle_pdu(&pdu).expect("refused, not an error"),
                Some(PAIRING_FAILED_INVALID_PARAMETERS.to_vec()),
                "{code:#04X} at {length} octets (it is {exact}) must be refused \
                 with Invalid Parameters"
            );
            assert!(session.is_failed());
            assert!(session.ltk().is_none());
        }
    }
}

/// Section 3.6.5 defines exactly two AddrType values: 0x00 for a public device
/// address and 0x01 for a static random one.
///
/// This is the field a bond record is keyed by, so a nonsense type would file
/// the peer's identity under an address type that means nothing — and it is
/// stored straight into `PairingKeys::address_type`.
#[test]
fn an_identity_address_with_an_undefined_type_is_refused() {
    for addr_type in [0x02u8, 0x7F, 0xFF] {
        let mut session = started_initiator_session();
        let mut pdu = vec![opcode::IDENTITY_ADDR_INFO, addr_type];
        pdu.extend_from_slice(&PERIPHERAL_ADDR);
        assert_eq!(
            session.handle_pdu(&pdu).expect("refused, not an error"),
            Some(PAIRING_FAILED_INVALID_PARAMETERS.to_vec()),
            "AddrType {addr_type:#04X} is not one of the two the spec defines"
        );
    }
}

/// Section 3.3: "If a packet is received with a Code that is reserved for
/// future use it shall be ignored."
///
/// Ignored, specifically — not failed. Failing would mean any future revision
/// of the spec that defines one more command code could break this stack by
/// existing. The two *defined* commands this port does not implement are a
/// different case, and get the reason code that says so.
#[test]
fn reserved_command_codes_are_ignored_and_unimplemented_ones_are_refused() {
    for reserved in [0x0Fu8, 0x10, 0x80, 0xFF] {
        let mut session = started_initiator_session();
        assert_eq!(
            session.handle_pdu(&[reserved]).expect("no error"),
            None,
            "code {reserved:#04X} is reserved for future use and shall be ignored"
        );
        assert!(
            !session.is_failed(),
            "an ignored PDU must not fail the pairing ({reserved:#04X})"
        );
    }

    // Security Request (0x0B) and Keypress Notification (0x0E) are defined
    // commands. Section 3.3: "If pairing is supported then all commands shall
    // be supported" — this port does not support these, and says so with
    // reason 0x07 rather than pretending they were never sent.
    for defined in [opcode::SECURITY_REQUEST, 0x0E] {
        let mut session = started_initiator_session();
        assert_eq!(
            session.handle_pdu(&[defined]).expect("no error"),
            Some(vec![
                opcode::PAIRING_FAILED,
                error_code::COMMAND_NOT_SUPPORTED
            ]),
            "code {defined:#04X} is defined but unimplemented here"
        );
    }
}

/// A well-formed Pairing Random that arrives before the public keys have been
/// exchanged used to reach `dh_key.expect(...)` on the Secure Connections
/// responder path and panic the process from one remote packet.
///
/// Section 2.3.5.6.1: "After the public keys have been exchanged, the device
/// can then start computing the Diffie-Hellman Key" — so before that there is
/// no DHKey to finish the random exchange with, and the answer is a Pairing
/// Failed, not a crash.
#[test]
fn a_pairing_random_before_the_public_key_exchange_is_refused_not_a_panic() {
    for role in [Role::Initiator, Role::Responder] {
        let mut session = PairingSession::new(
            role,
            PairingConfig::default(),
            Address::from_be_bytes(PERIPHERAL_ADDR),
            AddressType::Random,
            Address::from_be_bytes(CENTRAL_ADDR),
            AddressType::Random,
        );
        let mut pdu = vec![0x00; 17];
        pdu[0] = opcode::PAIRING_RANDOM;
        let reply = session
            .handle_pdu(&pdu)
            .expect("out-of-order is refused, not an error")
            .expect("and it is answered");
        assert_eq!(reply[0], opcode::PAIRING_FAILED, "{role:?}");
        assert!(session.is_failed());
        assert!(session.ltk().is_none());
    }
}

/// Section 3.5.5: "During LE Secure Connections pairing, this command should be
/// sent if the remote device's public key is invalid... The Reason field should
/// be set to 'DHKey Check Failed'."
///
/// The distinction is not pedantry. Invalid Parameters says the PDU was
/// malformed; a public key that is 65 well-formed octets naming a point that is
/// not on P-256 is the CVE-2018-5383 attack, and the spec names a reason code
/// for it so a peer's logs can tell the two apart.
#[test]
fn a_public_key_off_the_curve_is_refused_as_a_dhkey_check_failure() {
    let mut session = responder_session();
    // 65 octets, right length, and a point that satisfies no curve equation.
    let mut pdu = vec![0xAA; 65];
    pdu[0] = opcode::PAIRING_PUBLIC_KEY;
    assert_eq!(
        session.handle_pdu(&pdu).expect("refused, not an error"),
        Some(vec![opcode::PAIRING_FAILED, error_code::DHKEY_CHECK_FAILED]),
    );
    assert!(session.is_failed());
    assert!(session.ltk().is_none());
}
