// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! Tests validating JsonKeyStore parsing and namespace handling.

use simble::smp::{KeyStore, PairingKey, PairingKeys};

const JSON1: &str = r#"
{
    "my_namespace": {
        "14:7D:DA:4E:53:A8/P": {
            "address_type": 0,
            "irk": {
                "authenticated": false,
                "value": "e7b2543b206e4e46b44f9e51dad22bd1"
            },
            "link_key": {
                "authenticated": false,
                "value": "0745dd9691e693d9dca740f7d8dfea75"
            },
            "ltk": {
                "authenticated": false,
                "value": "d1897ee10016eb1a08e4e037fd54c683"
            }
        }
    }
}
"#;

const JSON2: &str = r#"
{
    "my_namespace1": {},
    "my_namespace2": {}
}
"#;

const JSON3: &str = r#"
{
    "my_namespace1": {},
    "__DEFAULT__": {
        "14:7D:DA:4E:53:A8/P": {
            "address_type": 0,
            "irk": {
                "authenticated": false,
                "value": "e7b2543b206e4e46b44f9e51dad22bd1"
            }
        }
    }
}
"#;

#[test]
fn test_basic_keystore_crud() {
    let ks = KeyStore::new(Some("my_namespace"));
    assert_eq!(ks.get_all().len(), 0);

    let mut keys = PairingKeys::default();
    ks.update("foo", keys.clone());

    let foo = ks.get("foo").expect("foo exists");
    assert!(foo.ltk.is_none());

    let ltk_bytes = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    keys.ltk = Some(PairingKey::new(ltk_bytes));
    ks.update("foo", keys);

    let foo2 = ks.get("foo").expect("foo exists");
    assert!(foo2.ltk.is_some());
    assert_eq!(foo2.ltk.unwrap().value, ltk_bytes);

    let json_str = ks.to_json().expect("serialize json");
    assert!(json_str.contains("my_namespace"));
    assert!(json_str.contains("foo"));
    assert!(json_str.contains("000102030405060708090a0b0c0d0e0f"));
}

#[test]
fn test_keystore_json_parsing() {
    let ks = KeyStore::from_json(JSON1, Some("my_namespace")).expect("parse json");
    let foo = ks.get("14:7D:DA:4E:53:A8/P").expect("peer exists");

    let expected_ltk = [
        0xd1, 0x89, 0x7e, 0xe1, 0x00, 0x16, 0xeb, 0x1a, 0x08, 0xe4, 0xe0, 0x37, 0xfd, 0x54, 0xc6,
        0x83,
    ];
    assert_eq!(foo.ltk.unwrap().value, expected_ltk);

    let expected_irk = [
        0xe7, 0xb2, 0x54, 0x3b, 0x20, 0x6e, 0x4e, 0x46, 0xb4, 0x4f, 0x9e, 0x51, 0xda, 0xd2, 0x2b,
        0xd1,
    ];
    assert_eq!(foo.irk.unwrap().value, expected_irk);
}

#[test]
fn test_keystore_default_namespace() {
    // 1. Load JSON1 with default namespace
    let ks1 = KeyStore::from_json(JSON1, None).expect("load json1");
    let all1 = ks1.get_all();
    assert_eq!(all1.len(), 1);
    assert_eq!(all1[0].0, "14:7D:DA:4E:53:A8/P");

    // 2. Load JSON2 and insert into __DEFAULT__
    let ks2 = KeyStore::from_json(JSON2, None).expect("load json2");
    let mut keys = PairingKeys::default();
    let ltk = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    keys.ltk = Some(PairingKey::new(ltk));
    ks2.update("foo", keys);

    let json2_out = ks2.to_json().unwrap();
    assert!(json2_out.contains("__DEFAULT__"));
    assert!(json2_out.contains("foo"));

    // 3. Load JSON3
    let ks3 = KeyStore::from_json(JSON3, None).expect("load json3");
    let all3 = ks3.get_all();
    assert_eq!(all3.len(), 1);
    assert_eq!(all3[0].0, "14:7D:DA:4E:53:A8/P");
}
