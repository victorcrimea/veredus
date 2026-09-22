// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_loaded_game() {
    let msg = LoadedGame { current_turn: 42 };
    let bytes = msg.to_bytes();
    let decoded = LoadedGame::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
