// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Validates SMP pairing requests, responses, IO capabilities, failure PDUs,
//! LTK<->link-key conversion, identity address resolution, debug mode, and
//! the `PairingSession` state machine end to end (LE Legacy and LE Secure
//! Connections) driven through a real `VirtualDevice`.

use simble::VirtualDevice;
use simble::crypto::smp_crypto::rev;
use simble::device::MemoryBondStore;
use simble::smp::{
    IdentityAddressPreference, KeyStore, PairingConfig, PairingSession, Role,
    SMP_DEBUG_KEY_PUBLIC_X, SMP_DEBUG_KEY_PUBLIC_Y, SmpPairingFailed, SmpPairingPacket,
    io_capability, opcode, resolve_identity_address,
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
