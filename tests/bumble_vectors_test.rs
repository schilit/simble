// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Spec test vectors adapted from Google's Bumble test suite
//! (<https://github.com/google/bumble>, `tests/`).
//!
//! These exist because Simble's own tests exercise both ends of an exchange
//! against each other: if an encoding is wrong in a self-consistent way, both
//! sides agree and the test still passes. A vector lifted from the
//! specification — and cross-checked against a second, independent
//! implementation's expectations — cannot be satisfied by a self-consistent
//! mistake.
//!
//! Each test cites the Bumble test it was adapted from.

use simble::crypto::P256Keypair;
use simble::crypto::smp_crypto::rev;

/// Parses a spec-style big-endian hex string (spaces ignored) into bytes.
fn be(hex: &str) -> [u8; 32] {
    let cleaned: String = hex.chars().filter(|c| !c.is_whitespace()).collect();
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&cleaned[i * 2..i * 2 + 2], 16).expect("valid hex");
    }
    out
}

/// LE Secure Connections P-256 ECDH, against the worked example in the Core
/// Spec (Vol 3, Part H, Section 2.3.5.6.1) — adapted from Bumble's
/// `smp_test.py::test_ecc`.
///
/// Simble's own ECDH tests only check that two freshly generated keypairs
/// agree with each other, which a consistently byte-swapped implementation
/// would also pass. This pins the actual DHKey the specification says two
/// known private keys must produce, so a wrong byte order cannot hide.
#[test]
fn test_ecdh_dhkey_matches_the_spec_vector() {
    const PRIVATE_A: &str =
        "3f49f6d4 a3c55f38 74c9b3e3 d2103f50 4aff607b eb40b799 5899b8a6 cd3c1abd";
    const PUBLIC_A_X: &str =
        "20b003d2 f297be2c 5e2c83a7 e9f9a5b9 eff49111 acf4fddb cc030148 0e359de6";
    const PUBLIC_A_Y: &str =
        "dc809c49 652aeb6d 63329abf 5a52155c 766345c2 8fed3024 741c8ed0 1589d28b";
    const PRIVATE_B: &str =
        "55188b3d 32f6bb9a 900afcfb eed4e72a 59cb9ac2 f19d7cfb 6b4fdd49 f47fc5fd";
    const PUBLIC_B_X: &str =
        "1ea1f0f0 1faf1d96 09592284 f19e4c00 47b58afd 8615a69f 559077b2 2faaa190";
    const PUBLIC_B_Y: &str =
        "4c55f33e 429dad37 7356703a 9ab85160 472d1130 e28e3676 5f89aff9 15b1214a";
    const DHKEY: &str =
        "ec0234a3 57c8ad05 341010a6 0a397d9b 99796b13 b4f866f1 868d34f3 73bfa698";

    // Simble stores public keys in SMP wire order (little-endian), which is
    // the reverse of how the spec prints them.
    let key_a = P256Keypair::from_private_be(&be(PRIVATE_A)).expect("valid private scalar");
    assert_eq!(key_a.x_le, rev(&be(PUBLIC_A_X)), "public key A.x");
    assert_eq!(key_a.y_le, rev(&be(PUBLIC_A_Y)), "public key A.y");

    let key_b = P256Keypair::from_private_be(&be(PRIVATE_B)).expect("valid private scalar");
    assert_eq!(key_b.x_le, rev(&be(PUBLIC_B_X)), "public key B.x");
    assert_eq!(key_b.y_le, rev(&be(PUBLIC_B_Y)), "public key B.y");

    // Both sides must land on the spec's DHKey, not merely on each other.
    let expected = rev(&be(DHKEY));
    let a_with_b = key_a
        .shared_x_le(&rev(&be(PUBLIC_B_X)), &rev(&be(PUBLIC_B_Y)))
        .expect("B's public key is on the curve");
    assert_eq!(a_with_b, expected, "A's view of the shared secret");

    let b_with_a = key_b
        .shared_x_le(&rev(&be(PUBLIC_A_X)), &rev(&be(PUBLIC_A_Y)))
        .expect("A's public key is on the curve");
    assert_eq!(b_with_a, expected, "B's view of the shared secret");
}

/// The debug keypair the spec publishes for reproducible traces (Vol 3,
/// Part H, 2.3.5.6.1) is the same keypair as vector A above — a detail worth
/// pinning, since Simble's SMP debug mode depends on it deriving correctly.
/// Adapted from Bumble's `smp_test.py::test_smp_debug_mode`.
#[test]
fn test_debug_keypair_is_the_spec_vector_a_keypair() {
    const PRIVATE_A: &str =
        "3f49f6d4 a3c55f38 74c9b3e3 d2103f50 4aff607b eb40b799 5899b8a6 cd3c1abd";

    let derived = P256Keypair::from_private_be(&be(PRIVATE_A)).expect("valid private scalar");
    assert_eq!(
        derived.x_le,
        rev(&simble::smp::SMP_DEBUG_KEY_PUBLIC_X),
        "the debug public key X is vector A's"
    );
    assert_eq!(
        derived.y_le,
        rev(&simble::smp::SMP_DEBUG_KEY_PUBLIC_Y),
        "the debug public key Y is vector A's"
    );
}

/// A public key that is not a point on P-256 must be rejected rather than
/// used. Mandatory since CVE-2018-5383 (the "invalid curve" attack), and the
/// behaviour Android relies on — it rejects pairing with a peer that accepts
/// garbage points.
#[test]
fn test_invalid_curve_points_are_rejected() {
    let key = P256Keypair::from_private_be(&be(
        "3f49f6d4 a3c55f38 74c9b3e3 d2103f50 4aff607b eb40b799 5899b8a6 cd3c1abd",
    ))
    .expect("valid private scalar");

    // Random coordinates are not on the curve with overwhelming probability.
    assert!(
        key.shared_x_le(&[0x42; 32], &[0x24; 32]).is_none(),
        "a random point must not produce a shared secret"
    );
    // The point at infinity / all-zero coordinates is likewise not valid.
    assert!(
        key.shared_x_le(&[0x00; 32], &[0x00; 32]).is_none(),
        "the zero point must not produce a shared secret"
    );
}

// --- Advertising data ------------------------------------------------------
//
// Adapted from Bumble's `core_test.py::test_ad_data`, which round-trips
// advertising data through a parser and checks that repeated structures of
// the same AD type are all preserved. Simble has an AD *builder* but no
// parser, so nothing independently verified what it emits — every
// advertising bug found so far was in this builder. These tests walk the
// emitted bytes as length-type-value structures and check them against the
// layout the spec requires (Core Spec Vol 3, Part C, Section 11).

use simble::gap::AdvertisingData;

/// AD type values, hardcoded from the spec's Assigned Numbers rather than
/// imported from Simble — an independent check should not agree with the
/// implementation by construction.
mod ad_type {
    pub const FLAGS: u8 = 0x01;
    pub const COMPLETE_16BIT_UUIDS: u8 = 0x03;
    pub const COMPLETE_LOCAL_NAME: u8 = 0x09;
    pub const SERVICE_DATA_16BIT: u8 = 0x16;
    pub const MANUFACTURER_SPECIFIC_DATA: u8 = 0xFF;
}

/// One decoded advertising structure: its AD type and its value bytes.
#[derive(Debug, PartialEq, Eq)]
struct AdStructure {
    ad_type: u8,
    value: Vec<u8>,
}

/// Walks an advertising payload as length-prefixed structures, the way a
/// scanner does. Returns `Err` if a length overruns the buffer or a
/// zero-length structure appears mid-payload — both make the rest of the
/// advertisement unparseable, which is exactly the failure a builder bug
/// produces.
fn parse_ad_structures(data: &[u8]) -> Result<Vec<AdStructure>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let length = data[i] as usize;
        if length == 0 {
            return Err(format!("zero-length AD structure at offset {i}"));
        }
        if i + 1 + length > data.len() {
            return Err(format!(
                "AD structure at offset {i} claims {length} bytes, only {} remain",
                data.len() - i - 1
            ));
        }
        out.push(AdStructure {
            ad_type: data[i + 1],
            value: data[i + 2..i + 1 + length].to_vec(),
        });
        i += 1 + length;
    }
    Ok(out)
}

/// Every field the builder supports must emit a well-formed structure, with
/// 16-bit values little-endian on the wire.
#[test]
fn test_advertising_builder_emits_parseable_structures() {
    let ad = AdvertisingData::new()
        .with_flags(0x06)
        .with_name("simble")
        .with_service_uuid_16(0x180D)
        .with_service_data_16(0xFE2C, &[0x00, 0x11, 0x22])
        .with_manufacturer_data(0x00E0, &[0xAB]);

    let structures = parse_ad_structures(&ad.to_bytes()).expect("builder emits parseable data");

    assert_eq!(
        structures,
        vec![
            AdStructure { ad_type: ad_type::FLAGS, value: vec![0x06] },
            AdStructure {
                ad_type: ad_type::COMPLETE_LOCAL_NAME,
                value: b"simble".to_vec(),
            },
            // 16-bit UUIDs go out little-endian.
            AdStructure {
                ad_type: ad_type::COMPLETE_16BIT_UUIDS,
                value: vec![0x0D, 0x18],
            },
            // Service data is the UUID (little-endian) followed by the data.
            AdStructure {
                ad_type: ad_type::SERVICE_DATA_16BIT,
                value: vec![0x2C, 0xFE, 0x00, 0x11, 0x22],
            },
            // Manufacturer data is the company ID (little-endian) then data.
            AdStructure {
                ad_type: ad_type::MANUFACTURER_SPECIFIC_DATA,
                value: vec![0xE0, 0x00, 0xAB],
            },
        ]
    );
}

/// Repeated structures of the same AD type must all survive — Bumble's
/// `test_ad_data` asserts the same for two TX Power Level structures. A
/// beacon carrying more than one service-data structure is ordinary.
#[test]
fn test_repeated_service_data_structures_all_survive() {
    let ad = AdvertisingData::new()
        .with_service_data_16(0xFE2C, &[0x01])
        .with_service_data_16(0xFEAA, &[0x02, 0x03]);

    let structures = parse_ad_structures(&ad.to_bytes()).expect("parseable");
    let service_data: Vec<&AdStructure> = structures
        .iter()
        .filter(|s| s.ad_type == ad_type::SERVICE_DATA_16BIT)
        .collect();

    assert_eq!(service_data.len(), 2, "both structures must be present");
    assert_eq!(service_data[0].value, vec![0x2C, 0xFE, 0x01]);
    assert_eq!(service_data[1].value, vec![0xAA, 0xFE, 0x02, 0x03]);
}

/// An empty name must be omitted entirely rather than emitted as a
/// zero-length structure. A stub name field makes everything after it
/// unparseable and pushes the payload over the 31-byte limit, which is how a
/// beacon ends up silently never transmitting.
#[test]
fn test_empty_name_emits_no_structure() {
    let ad = AdvertisingData::new().with_flags(0x06).with_name("");
    let bytes = ad.to_bytes();

    let structures = parse_ad_structures(&bytes).expect("no zero-length structure");
    assert!(
        !structures
            .iter()
            .any(|s| s.ad_type == ad_type::COMPLETE_LOCAL_NAME),
        "an empty name must not appear on the air: {structures:?}"
    );
}

// --- RFCOMM ----------------------------------------------------------------

use simble::classic::rfcomm::RfcommFrame;

/// A real RFCOMM frame, byte for byte, from Bumble's
/// `rfcomm_test.py::test_frames`: `03 3f 01 1c`.
///
/// Decoded against TS 07.10 / the RFCOMM spec: address `0x03` is DLCI 0 (the
/// multiplexer control channel) with the E/A and C/R bits set, control `0x3F`
/// is SABM with the poll/final bit, length `0x01` encodes a zero-byte payload
/// in the one-octet form, and `0x1C` is the header FCS. Simble must agree
/// with an independent implementation on every one of those fields.
#[test]
fn test_rfcomm_sabm_frame_vector() {
    let data = [0x03u8, 0x3F, 0x01, 0x1C];
    let frame = RfcommFrame::parse(&data).expect("Bumble's SABM frame must parse");

    assert_eq!(frame.dlci, 0, "DLCI 0 is the multiplexer control channel");
    assert_eq!(frame.c_r, 1, "command/response bit set");
    assert_eq!(frame.p_f, 1, "SABM carries the poll/final bit");
    assert!(frame.information.is_empty(), "SABM carries no payload");

    // Re-encoding must reproduce the original bytes, FCS included — a
    // round-trip that starts from another stack's bytes cannot be satisfied
    // by a self-consistent encoding mistake.
    assert_eq!(
        frame.to_bytes(),
        data.to_vec(),
        "re-encoding must reproduce the wire bytes"
    );
}

// --- Encrypted Advertising Data ---------------------------------------------
//
// From the Core Specification Supplement (CSS) Part A, Section 2.3 — the SIG's
// own worked example for Encrypted Advertising Data, and the only external
// reference this code has ever been checked against.
//
// `src/gap/ead.rs` was previously tested ONLY by encrypting and decrypting
// against itself, which is exactly the failure this file exists to catch: a
// self-consistent mistake in the nonce order, the AAD octet or the key
// ordering round-trips perfectly and proves nothing. These are somebody
// else's bytes.
//
// BYTE ORDER, which is the whole difficulty of using this vector. The
// Supplement prints the IV and the Randomizer most-significant-octet first,
// the way it prints any integer, but the CCM nonce is assembled from them in
// over-the-air order — so both must be reversed before being handed to this
// API, while the session key, being a byte array rather than an integer, is
// used exactly as printed. Feeding all three verbatim produces a completely
// different ciphertext.
//
// That was established empirically: of the eight combinations of reversing
// key / IV / randomizer, exactly one reproduces the Supplement's ciphertext
// and MIC, and it is the one this convention predicts. Neither Bumble nor
// Zephyr implements EAD, so there was no second implementation to ask.

/// The Supplement's own key material, in the order it prints them.
const CSS_SESSION_KEY: &str = "57A9DA12D12E6E131E20612AD10A6A19";
const CSS_IV_PRINTED: &str = "46E77AB1EF007A9E";
const CSS_RANDOMIZER_PRINTED: &str = "DECA57E118";
/// Two AD structures: `0F 09 "Short Mini-Bus"` then `03 19 0A 8C` (Appearance).
/// That it decodes to something meaningful is itself a check on transcription.
const CSS_PLAINTEXT: &str = "0F095368 6F727420 4D696E69 2D427573 03190A8C";
const CSS_CIPHERTEXT: &str = "74E4DCAF DC51C728 2810C221 7F0E4CEF 4343181F";
const CSS_MIC: &str = "BA0069CC";

fn css_key() -> simble::gap::ead::KeyMaterial {
    simble::gap::ead::KeyMaterial {
        session_key: hex16(CSS_SESSION_KEY),
        iv: rev8(hex8(CSS_IV_PRINTED)),
    }
}

/// CSS Part A, Section 2.3 — encrypt direction.
#[test]
fn ead_css_vector_encrypts_to_the_spec_ciphertext() {
    use simble::gap::ead::encrypt_ad;

    let randomizer = rev5(hex5(CSS_RANDOMIZER_PRINTED));
    let plaintext = hex(CSS_PLAINTEXT);
    let out = encrypt_ad(&css_key(), &randomizer, &plaintext);

    // Length || 0x31 || Randomizer || Ciphertext || MIC
    assert_eq!(out[0] as usize, out.len() - 1, "AD length octet");
    assert_eq!(out[1], 0x31, "Encrypted Data AD type");
    assert_eq!(&out[2..7], &randomizer[..], "randomizer carried verbatim");
    assert_eq!(
        &out[7..7 + plaintext.len()],
        &hex(CSS_CIPHERTEXT)[..],
        "ciphertext must match the Supplement's worked example",
    );
    assert_eq!(
        &out[7 + plaintext.len()..],
        &hex(CSS_MIC)[..],
        "MIC must match the Supplement's worked example",
    );
}

/// The same vector backwards: the Supplement's ciphertext must decrypt to the
/// Supplement's plaintext. Reaches the decrypt path, which the encrypt test
/// alone does not.
#[test]
fn ead_css_vector_decrypts_the_spec_ciphertext() {
    use simble::gap::ead::decrypt_ad;

    let mut value = rev5(hex5(CSS_RANDOMIZER_PRINTED)).to_vec();
    value.extend_from_slice(&hex(CSS_CIPHERTEXT));
    value.extend_from_slice(&hex(CSS_MIC));

    let plaintext = decrypt_ad(&css_key(), &value).expect("spec vector must authenticate");
    assert_eq!(plaintext, hex(CSS_PLAINTEXT));
    // It really is a Complete Local Name — the human check on the transcription.
    assert_eq!(&plaintext[2..16], b"Short Mini-Bus");
}

/// A wrong MIC must not authenticate. Without this, a decrypt that ignored the
/// tag entirely would pass every test above.
#[test]
fn ead_rejects_a_tampered_mic() {
    use simble::gap::ead::decrypt_ad;

    let mut value = rev5(hex5(CSS_RANDOMIZER_PRINTED)).to_vec();
    value.extend_from_slice(&hex(CSS_CIPHERTEXT));
    value.extend_from_slice(&hex(CSS_MIC));
    *value.last_mut().unwrap() ^= 0x01;

    assert!(decrypt_ad(&css_key(), &value).is_none());
}

fn hex(s: &str) -> Vec<u8> {
    let clean: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    (0..clean.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&clean[i..i + 2], 16).unwrap())
        .collect()
}
fn hex16(s: &str) -> [u8; 16] {
    hex(s).try_into().unwrap()
}
fn hex8(s: &str) -> [u8; 8] {
    hex(s).try_into().unwrap()
}
fn hex5(s: &str) -> [u8; 5] {
    hex(s).try_into().unwrap()
}
fn rev8(mut b: [u8; 8]) -> [u8; 8] {
    b.reverse();
    b
}
fn rev5(mut b: [u8; 5]) -> [u8; 5] {
    b.reverse();
    b
}
