//! Bencode integration tests
//!
//! Tests for the bencode crate covering:
//! - Parsing valid bencode
//! - Handling invalid bencode
//! - Round-trip serialization/deserialization
//! - Edge cases and error conditions

use bencode::{decode, encode, BencodeValue};

/// Test helper to create a bencode byte string
fn bstr(s: &str) -> Vec<u8> {
    format!("{}:{}", s.len(), s).into_bytes()
}

/// Test helper to create a bencode integer
fn bint(n: i64) -> Vec<u8> {
    format!("i{}e", n).into_bytes()
}

/// Test helper to create a bencode list start
fn blist() -> Vec<u8> {
    b"l".to_vec()
}

/// Test helper to create a bencode dict start
fn bdict() -> Vec<u8> {
    b"d".to_vec()
}

/// Test helper to end a bencode list or dict
fn bend() -> Vec<u8> {
    b"e".to_vec()
}

#[test]
fn test_decode_byte_string() {
    let input = b"5:hello";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::Bytes(bytes) => {
            assert_eq!(bytes, b"hello");
        }
        _ => panic!("Expected Bytes, got {:?}", result),
    }
}

#[test]
fn test_decode_empty_byte_string() {
    let input = b"0:";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::Bytes(bytes) => {
            assert!(bytes.is_empty());
        }
        _ => panic!("Expected Bytes, got {:?}", result),
    }
}

#[test]
fn test_decode_integer() {
    let test_cases = vec![
        (b"i0e" as &[u8], 0i64),
        (b"i42e", 42),
        (b"i-42e", -42),
        (b"i1234567890e", 1234567890),
        (b"i-1e", -1),
    ];

    for (input, expected) in test_cases {
        let result = decode(input).unwrap();
        match result {
            BencodeValue::Int(n) => assert_eq!(n, expected),
            _ => panic!("Expected Int for input {:?}", input),
        }
    }
}

#[test]
fn test_decode_integer_negative_zero() {
    // Note: -0 is technically valid bencode but should parse as 0
    let input = b"i-0e";
    let result = decode(input);
    // Most implementations reject -0
    assert!(result.is_err() || matches!(result.unwrap(), BencodeValue::Int(0)));
}

#[test]
fn test_decode_integer_leading_zeros() {
    // Leading zeros are typically invalid
    let input = b"i042e";
    let result = decode(input);
    // Most strict implementations reject this
    assert!(result.is_err() || matches!(result.unwrap(), BencodeValue::Int(42)));
}

#[test]
fn test_decode_list() {
    let input = b"li1ei2ei3ee";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::List(list) => {
            assert_eq!(list.len(), 3);
            assert!(matches!(list[0], BencodeValue::Int(1)));
            assert!(matches!(list[1], BencodeValue::Int(2)));
            assert!(matches!(list[2], BencodeValue::Int(3)));
        }
        _ => panic!("Expected List, got {:?}", result),
    }
}

#[test]
fn test_decode_empty_list() {
    let input = b"le";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::List(list) => {
            assert!(list.is_empty());
        }
        _ => panic!("Expected List, got {:?}", result),
    }
}

#[test]
fn test_decode_nested_list() {
    let input = b"lli1ei2eel3:abce";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::List(list) => {
            assert_eq!(list.len(), 2);
            // First element is [1, 2]
            match &list[0] {
                BencodeValue::List(inner) => {
                    assert_eq!(inner.len(), 2);
                }
                _ => panic!("Expected nested list"),
            }
            // Second element is "abc"
            match &list[1] {
                BencodeValue::Bytes(bytes) => {
                    assert_eq!(bytes, b"abc");
                }
                _ => panic!("Expected bytes"),
            }
        }
        _ => panic!("Expected List, got {:?}", result),
    }
}

#[test]
fn test_decode_dictionary() {
    let input = b"d4:name5:hello3:agei25ee";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::Dict(dict) => {
            assert_eq!(dict.len(), 2);

            let name = dict.get(b"name".as_slice()).expect("Missing 'name' key");
            match name {
                BencodeValue::Bytes(bytes) => {
                    assert_eq!(bytes, b"hello");
                }
                _ => panic!("Expected Bytes for 'name'"),
            }

            let age = dict.get(b"age".as_slice()).expect("Missing 'age' key");
            match age {
                BencodeValue::Int(n) => {
                    assert_eq!(*n, 25);
                }
                _ => panic!("Expected Int for 'age'"),
            }
        }
        _ => panic!("Expected Dict, got {:?}", result),
    }
}

#[test]
fn test_decode_empty_dictionary() {
    let input = b"de";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::Dict(dict) => {
            assert!(dict.is_empty());
        }
        _ => panic!("Expected Dict, got {:?}", result),
    }
}

#[test]
fn test_decode_dictionary_sorted_keys() {
    // Bencode requires dictionary keys to be sorted lexicographically
    // This test verifies we can parse correctly sorted dicts
    let input = b"d1:ai1e1:bi2e1:ci3ee";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::Dict(dict) => {
            assert_eq!(dict.len(), 3);
            // Keys should be in order: a, b, c
            let keys: Vec<_> = dict.keys().collect();
            assert_eq!(keys[0], b"a".as_slice());
            assert_eq!(keys[1], b"b".as_slice());
            assert_eq!(keys[2], b"c".as_slice());
        }
        _ => panic!("Expected Dict, got {:?}", result),
    }
}

#[test]
fn test_decode_torrent_info_structure() {
    // Example of a minimal torrent info dictionary
    let input =
        b"d6:lengthi1024e4:name8:test.txt12:piece lengthi262144e6:pieces20:12345678901234567890e";
    let result = decode(input).unwrap();

    match result {
        BencodeValue::Dict(dict) => {
            assert!(dict.contains_key(b"length".as_slice()));
            assert!(dict.contains_key(b"name".as_slice()));
            assert!(dict.contains_key(b"piece length".as_slice()));
            assert!(dict.contains_key(b"pieces".as_slice()));
        }
        _ => panic!("Expected Dict, got {:?}", result),
    }
}

#[test]
fn test_decode_error_unexpected_end() {
    let inputs = vec![
        b"5:hell" as &[u8], // Truncated string
        b"i42" as &[u8],    // Unterminated integer
        b"l" as &[u8],      // Unterminated list
        b"d" as &[u8],      // Unterminated dict
        b"li1e" as &[u8],   // Unterminated list
    ];

    for input in inputs {
        let result = decode(input);
        assert!(result.is_err(), "Expected error for input {:?}", input);
    }
}

#[test]
fn test_decode_error_invalid_format() {
    let inputs = vec![
        b"x" as &[u8],      // Invalid start byte
        b"5" as &[u8],      // Missing colon in string
        b":hello" as &[u8], // Missing length in string
        b"i" as &[u8],      // Empty integer
        b"ie" as &[u8],     // Empty integer value
    ];

    for input in inputs {
        let result = decode(input);
        assert!(result.is_err(), "Expected error for input {:?}", input);
    }
}

#[test]
fn test_encode_byte_string() {
    let value = BencodeValue::Bytes(b"hello".to_vec());
    let encoded = encode(&value);
    assert_eq!(encoded, b"5:hello");
}

#[test]
fn test_encode_empty_byte_string() {
    let value = BencodeValue::Bytes(b"".to_vec());
    let encoded = encode(&value);
    assert_eq!(encoded, b"0:");
}

#[test]
fn test_encode_integer() {
    let test_cases = vec![
        (BencodeValue::Int(0), b"i0e" as &[u8]),
        (BencodeValue::Int(42), b"i42e"),
        (BencodeValue::Int(-42), b"i-42e"),
    ];

    for (value, expected) in test_cases {
        let encoded = encode(&value);
        assert_eq!(encoded, expected);
    }
}

#[test]
fn test_encode_list() {
    let value = BencodeValue::List(vec![
        BencodeValue::Int(1),
        BencodeValue::Int(2),
        BencodeValue::Int(3),
    ]);
    let encoded = encode(&value);
    assert_eq!(encoded, b"li1ei2ei3ee");
}

#[test]
fn test_encode_empty_list() {
    let value = BencodeValue::List(vec![]);
    let encoded = encode(&value);
    assert_eq!(encoded, b"le");
}

#[test]
fn test_encode_dictionary() {
    use std::collections::BTreeMap;

    let mut dict = BTreeMap::new();
    dict.insert(b"age".to_vec(), BencodeValue::Int(25));
    dict.insert(b"name".to_vec(), BencodeValue::Bytes(b"hello".to_vec()));

    let value = BencodeValue::Dict(dict);
    let encoded = encode(&value);

    // Keys must be sorted: age comes before name
    assert_eq!(encoded, b"d3:agei25e4:name5:helloe");
}

#[test]
fn test_encode_empty_dictionary() {
    use std::collections::BTreeMap;

    let dict: BTreeMap<Vec<u8>, BencodeValue> = BTreeMap::new();
    let value = BencodeValue::Dict(dict);
    let encoded = encode(&value);
    assert_eq!(encoded, b"de");
}

#[test]
fn test_roundtrip() {
    let test_values = vec![
        BencodeValue::Bytes(b"Hello, World!".to_vec()),
        BencodeValue::Int(42),
        BencodeValue::Int(-999),
        BencodeValue::List(vec![
            BencodeValue::Int(1),
            BencodeValue::Bytes(b"test".to_vec()),
        ]),
    ];

    for original in test_values {
        let encoded = encode(&original);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(original, decoded);
    }
}

#[test]
fn test_large_byte_string() {
    let large_data = vec![b'x'; 10000];
    let value = BencodeValue::Bytes(large_data.clone());
    let encoded = encode(&value);

    // Verify the length prefix is correct
    let prefix = format!("{}:", large_data.len());
    assert!(encoded.starts_with(prefix.as_bytes()));

    // Verify roundtrip
    let decoded = decode(&encoded).unwrap();
    match decoded {
        BencodeValue::Bytes(bytes) => assert_eq!(bytes, large_data),
        _ => panic!("Expected Bytes"),
    }
}

#[test]
fn test_deeply_nested_structure() {
    // Create a deeply nested list: [[[[[]]]]]
    let mut value = BencodeValue::List(vec![]);
    for _ in 0..10 {
        value = BencodeValue::List(vec![value]);
    }

    let encoded = encode(&value);
    let decoded = decode(&encoded).unwrap();
    assert_eq!(value, decoded);
}
