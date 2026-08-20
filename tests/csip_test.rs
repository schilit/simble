// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Ported tests from Bumble's csip_test.py validating CSIP crypto primitives and service.

use simble::crypto::smp_crypto::rev;
use simble::gatt::GattDatabase;
use simble::profiles::csip::{CoordinatedSetIdentificationService, k1, s1, sih};

#[test]
fn test_s1_salt_generation() {
    let mut m = b"SIRKenc".to_vec();
    m.reverse();
    let salt = s1(&m);
    let expected = rev(&[
        0x69, 0x01, 0x98, 0x3f, 0x18, 0x14, 0x9e, 0x82, 0x3c, 0x7d, 0x13, 0x3a, 0x7d, 0x77, 0x45,
        0x72,
    ]);
    assert_eq!(salt, expected);
}

#[test]
fn test_k1_derivation() {
    let k = rev(&[
        0x67, 0x6e, 0x1b, 0x9b, 0xd4, 0x48, 0x69, 0x6f, 0x06, 0x1e, 0xc6, 0x22, 0x3c, 0xe5, 0xce,
        0xd9,
    ]);
    let mut sirk_enc = b"SIRKenc".to_vec();
    sirk_enc.reverse();
    let salt = s1(&sirk_enc);

    let mut p = b"csis".to_vec();
    p.reverse();
    let derived = k1(&k, &salt, &p);
    let expected = rev(&[
        0x52, 0x77, 0x45, 0x3c, 0xc0, 0x94, 0xd9, 0x82, 0xb0, 0xe8, 0xee, 0x53, 0x2f, 0x2d, 0x1f,
        0x8b,
    ]);
    assert_eq!(derived, expected);
}

#[test]
fn test_sih_hash() {
    let sirk = rev(&[
        0x45, 0x7d, 0x7d, 0x09, 0x21, 0xa1, 0xfd, 0x22, 0xce, 0xcd, 0x8c, 0x86, 0xdd, 0x72, 0xcc,
        0xcd,
    ]);
    let prand = [0x63, 0xf5, 0x69]; // rev of 69f563
    let hash = sih(&sirk, &prand);
    let expected = [0xda, 0x48, 0x19]; // rev of 1948da
    assert_eq!(hash, expected);
}

#[test]
fn test_csip_service_registration() {
    let mut db = GattDatabase::new();
    let sirk = [0xAA; 16];
    let csip = CoordinatedSetIdentificationService::register(&mut db, sirk, 2, 1);

    // Read SIRK
    let sirk_read = db.read(csip.sirk_value_handle, 0).expect("read sirk");
    assert_eq!(sirk_read.len(), 17);
    assert_eq!(sirk_read[0], 0x01); // Plaintext
    assert_eq!(&sirk_read[1..17], &sirk);

    // Read Member Size
    let size = db.read(csip.size_value_handle, 0).unwrap();
    assert_eq!(size, &[0x02]);

    // Read Member Rank
    let rank = db.read(csip.rank_value_handle, 0).unwrap();
    assert_eq!(rank, &[0x01]);
}
