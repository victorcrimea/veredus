// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn encode(turn: u32, declared_len: u32, hash: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&turn.to_be_bytes());
    bytes.extend_from_slice(&declared_len.to_be_bytes());
    bytes.extend_from_slice(hash);
    bytes
}

#[test]
fn roundtrip_sync_check() {
    let msg = StateHash {
        turn: 10,
        hash: [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
            0xEE, 0xFF,
        ],
    };
    let bytes = msg.to_bytes();
    assert_eq!(bytes.len(), 4 + 4 + 16);
    assert_eq!(&bytes[4..8], &16u32.to_be_bytes());
    let decoded = StateHash::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn rejects_short_hash() {
    let bytes = encode(10, 3, &[0xAA, 0xBB, 0xCC]);
    assert_eq!(
        StateHash::from_bytes(&bytes),
        Err(ParseError::WrongSize {
            field: "hash",
            declared: 3,
            should_be: 16,
        })
    );
}

#[test]
fn rejects_long_hash() {
    let bytes = encode(10, 17, &[0xAA; 17]);
    assert_eq!(
        StateHash::from_bytes(&bytes),
        Err(ParseError::WrongSize {
            field: "hash",
            declared: 17,
            should_be: 16,
        })
    );
}

#[test]
fn rejects_empty_hash() {
    let bytes = encode(10, 0, &[]);
    assert_eq!(
        StateHash::from_bytes(&bytes),
        Err(ParseError::WrongSize {
            field: "hash",
            declared: 0,
            should_be: 16,
        })
    );
}

// A huge declared length must be refused on the length alone, before any
// attempt to read or allocate that many bytes.
#[test]
fn rejects_huge_declared_length_without_data() {
    let bytes = encode(10, u32::MAX, &[]);
    assert_eq!(
        StateHash::from_bytes(&bytes),
        Err(ParseError::WrongSize {
            field: "hash",
            declared: u32::MAX as usize,
            should_be: 16,
        })
    );
}

#[test]
fn rejects_truncated_hash_data() {
    let bytes = encode(10, 16, &[0xAA; 15]);
    assert_eq!(
        StateHash::from_bytes(&bytes),
        Err(ParseError::Truncated { field: "hash data" })
    );
}
