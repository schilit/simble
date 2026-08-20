// Copyright 2026 The Android Open Source Project
// SPDX-License-Identifier: Apache-2.0

//! Port of Bumble's smp_test.py test suite.
//!
//! Validates SMP pairing requests, responses, IO capabilities, and failure PDUs.

use simble::smp::{SmpPairingFailed, SmpPairingPacket, io_capability, opcode};
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
