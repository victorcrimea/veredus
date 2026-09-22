// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_game_start() {
    let msg = StartSettings {
        init_attributes: vec![0xDE, 0xAD],
    };
    let bytes = msg.to_bytes();
    let decoded = StartSettings::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
