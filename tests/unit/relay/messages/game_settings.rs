// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_game_setup() {
    let msg = GameSettings {
        data: vec![1, 2, 3, 4],
    };
    let bytes = msg.to_bytes();
    let decoded = GameSettings::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
