// Copyright 2026 Bill Schilit
// SPDX-License-Identifier: Apache-2.0

//! AT command tokenizer tests, ported from Bumble's `at_test.py`.

use simble::classic::at::{self, AtParameter};

#[test]
fn test_tokenize_parameters() {
    assert_eq!(
        at::tokenize_parameters(b"1, 2, 3").unwrap(),
        vec![
            b"1".to_vec(),
            b",".to_vec(),
            b"2".to_vec(),
            b",".to_vec(),
            b"3".to_vec()
        ]
    );
    assert_eq!(
        at::tokenize_parameters(b"\"1, 2, 3\"").unwrap(),
        vec![b"1, 2, 3".to_vec()]
    );
    assert_eq!(
        at::tokenize_parameters(b"(1, \"2, 3\")").unwrap(),
        vec![
            b"(".to_vec(),
            b"1".to_vec(),
            b",".to_vec(),
            b"2, 3".to_vec(),
            b")".to_vec(),
        ]
    );
}

#[test]
fn test_parse_parameters() {
    assert_eq!(
        at::parse_parameters(b"1, 2, 3").unwrap(),
        vec![
            AtParameter::Value(b"1".to_vec()),
            AtParameter::Value(b"2".to_vec()),
            AtParameter::Value(b"3".to_vec()),
        ]
    );
    assert_eq!(
        at::parse_parameters(b"1,, 3").unwrap(),
        vec![
            AtParameter::Value(b"1".to_vec()),
            AtParameter::Value(Vec::new()),
            AtParameter::Value(b"3".to_vec()),
        ]
    );
    assert_eq!(
        at::parse_parameters(b"\"1, 2, 3\"").unwrap(),
        vec![AtParameter::Value(b"1, 2, 3".to_vec())]
    );
    assert_eq!(
        at::parse_parameters(b"1, (2, (3))").unwrap(),
        vec![
            AtParameter::Value(b"1".to_vec()),
            AtParameter::List(vec![
                AtParameter::Value(b"2".to_vec()),
                AtParameter::List(vec![AtParameter::Value(b"3".to_vec())]),
            ]),
        ]
    );
    assert_eq!(
        at::parse_parameters(b"1, (2, \"3, 4\"), 5").unwrap(),
        vec![
            AtParameter::Value(b"1".to_vec()),
            AtParameter::List(vec![
                AtParameter::Value(b"2".to_vec()),
                AtParameter::Value(b"3, 4".to_vec()),
            ]),
            AtParameter::Value(b"5".to_vec()),
        ]
    );
}
