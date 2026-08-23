// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! CSIP crypto primitive and service tests.

use simble::crypto::smp_crypto::rev;
use simble::gap::{AdvertisingData, resolvable_set_identifier};
use simble::gatt::GattDatabase;
use simble::profiles::csip::{CoordinatedSetIdentificationService, k1, rsi, rsi_matches, s1, sih};

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

/// The CSIS Appendix A sample data for `sih`, in the spec's big-endian
/// notation: SIRK `457d7d0921a1fd22cecd8c86dd72cccd`, prand `0x69f563`,
/// hash `0x1948da`. Every RSI test below is anchored to these three values.
const SAMPLE_SIRK_BE: [u8; 16] = [
    0x45, 0x7d, 0x7d, 0x09, 0x21, 0xa1, 0xfd, 0x22, 0xce, 0xcd, 0x8c, 0x86, 0xdd, 0x72, 0xcc, 0xcd,
];
const SAMPLE_PRAND_LE: [u8; 3] = [0x63, 0xf5, 0x69]; // rev of 69f563
const SAMPLE_HASH_LE: [u8; 3] = [0xda, 0x48, 0x19]; // rev of 1948da

#[test]
fn test_sih_hash() {
    let sirk = rev(&SAMPLE_SIRK_BE);
    let hash = sih(&sirk, &SAMPLE_PRAND_LE);
    assert_eq!(hash, SAMPLE_HASH_LE);
}

/// CSIS Section 4.9: `resolvableSetIdentifier = hash || prand`, and the least
/// significant octet of the RSI is the least significant octet of the *hash*.
/// Advertising data goes out least significant octet first, so the six octets
/// on the air are hash then prand — not the other way round, which is how this
/// read more naturally and how it was first written. Zephyr's
/// `bt_csip_set_member_generate_rsi` agrees: `memcpy(rsi, hash, 3)` then
/// `memcpy(rsi + 3, prand, 3)`.
///
/// Nothing our own scanner does can catch this: a reversed RSI resolves
/// perfectly against our own resolver. Only the spec's octets can.
#[test]
fn test_the_rsi_puts_the_hash_before_the_prand_on_the_wire() {
    let sirk = rev(&SAMPLE_SIRK_BE);
    let identifier = rsi(&sirk, &SAMPLE_PRAND_LE);
    assert_eq!(
        identifier,
        [0xda, 0x48, 0x19, 0x63, 0xf5, 0x69],
        "hash 1948da then prand 69f563, each least significant octet first"
    );
    assert_eq!(&identifier[..3], &SAMPLE_HASH_LE, "hash occupies rsi[0..3]");
    assert_eq!(
        &identifier[3..],
        &SAMPLE_PRAND_LE,
        "prand occupies rsi[3..6]"
    );
}

/// CSIS Section 4.8: the two most significant bits of prand are 0 then 1.
/// prand is little-endian here, so its most significant octet is index 2 — the
/// last one on the wire.
#[test]
fn test_the_prand_msbs_are_forced_to_0b01() {
    let sirk = [0x11u8; 16];
    for raw in [0x00u8, 0x3F, 0x80, 0xC0, 0xFF] {
        let identifier = rsi(&sirk, &[0xAA, 0xBB, raw]);
        assert_eq!(
            identifier[5] & 0b1100_0000,
            0b0100_0000,
            "raw msb {raw:#04X} became {:#04X}",
            identifier[5]
        );
        assert_eq!(
            identifier[5] & 0b0011_1111,
            raw & 0b0011_1111,
            "the other six bits are the caller's"
        );
    }
}

/// The prand is masked *before* the hash is taken, so the advertised hash
/// always matches the advertised prand. Taking it after would put a hash of
/// the unmasked prand next to the masked one, and no coordinator could ever
/// resolve the pair.
#[test]
fn test_the_hash_is_taken_over_the_masked_prand() {
    let sirk = [0x11u8; 16];
    let identifier = rsi(&sirk, &[0xAA, 0xBB, 0xFF]);
    let advertised_prand: [u8; 3] = identifier[3..].try_into().unwrap();
    assert_eq!(sih(&sirk, &advertised_prand), identifier[..3]);
    assert!(rsi_matches(&sirk, &identifier), "and so it self-resolves");
}

#[test]
fn test_a_wrong_sirk_yields_a_different_rsi_and_does_not_resolve() {
    // The perturbation that matters: a coordinator holding somebody else's
    // SIRK must not recognise this member. One bit of SIRK difference is
    // enough — AES is not a checksum.
    let sirk = rev(&SAMPLE_SIRK_BE);
    let mut nearly = sirk;
    nearly[0] ^= 0x01;

    let mine = rsi(&sirk, &SAMPLE_PRAND_LE);
    let theirs = rsi(&nearly, &SAMPLE_PRAND_LE);

    assert_eq!(&mine[3..], &theirs[3..], "same prand");
    assert_ne!(&mine[..3], &theirs[..3], "different hash");
    assert!(rsi_matches(&sirk, &mine));
    assert!(
        !rsi_matches(&nearly, &mine),
        "one flipped SIRK bit is enough"
    );
    assert!(!rsi_matches(&[0xFF; 16], &mine));
}

#[test]
fn test_a_fresh_prand_gives_the_same_member_a_different_identifier() {
    // An RSI that never changed would be a stable tracker, which is the whole
    // reason prand exists. Same SIRK, new prand, new six octets — and both
    // still resolve for the coordinator.
    let sirk = rev(&SAMPLE_SIRK_BE);
    let first = rsi(&sirk, &SAMPLE_PRAND_LE);
    let second = rsi(&sirk, &[0x11, 0x22, 0x33]);
    assert_ne!(first, second);
    assert!(rsi_matches(&sirk, &first));
    assert!(rsi_matches(&sirk, &second));
}

#[test]
fn test_only_six_octets_are_a_resolvable_set_identifier() {
    let sirk = rev(&SAMPLE_SIRK_BE);
    let identifier = rsi(&sirk, &SAMPLE_PRAND_LE);
    assert!(!rsi_matches(&sirk, &identifier[..5]), "too short");
    assert!(!rsi_matches(&sirk, &[]), "empty");
    let mut too_long = identifier.to_vec();
    too_long.push(0x00);
    assert!(!rsi_matches(&sirk, &too_long), "too long");
}

/// End to end, outside the crate: a set member builds its advertisement, and a
/// coordinator holding the SIRK picks it out of the raw advertising octets.
/// This is what `sih` never had — a caller that is not a test of `sih`.
#[test]
fn test_a_coordinator_resolves_a_member_from_its_raw_advertising_data() {
    let sirk = rev(&SAMPLE_SIRK_BE);
    let payload = AdvertisingData::new()
        .with_name("Earbud L")
        .with_service_uuid_16(0x1846)
        .with_resolvable_set_identifier(&rsi(&sirk, &SAMPLE_PRAND_LE))
        .to_bytes();

    // AD type 0x2E, six octets, at the position the builder put it.
    assert!(payload.windows(2).any(|w| w == [0x07, 0x2E]));

    let advertised = resolvable_set_identifier(&payload).expect("the member advertises an RSI");
    assert_eq!(advertised, [0xda, 0x48, 0x19, 0x63, 0xf5, 0x69]);
    assert!(rsi_matches(&sirk, advertised), "this is my other earbud");
    assert!(
        !rsi_matches(&[0x00; 16], advertised),
        "and not somebody else's"
    );
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
