// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_sync_error() {
    let msg = WrongHashPlayers {
        turn: 10,
        hash_expected: vec![0xAA],
        player_names: vec!["Alice".to_string(), "Bob".to_string()],
    };
    let bytes = msg.to_bytes();
    let decoded = WrongHashPlayers::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn roundtrip_sync_error_empty_names() {
    let msg = WrongHashPlayers {
        turn: 10,
        hash_expected: vec![0xAA],
        player_names: vec![],
    };
    let bytes = msg.to_bytes();
    let decoded = WrongHashPlayers::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
