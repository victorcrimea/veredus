// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_sync_check() {
    let msg = StateHash {
        turn: 10,
        hash: vec![0xAA, 0xBB, 0xCC],
    };
    let bytes = msg.to_bytes();
    let decoded = StateHash::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
