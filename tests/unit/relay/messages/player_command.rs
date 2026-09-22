// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_simulation() {
    let msg = PlayerCommand {
        client: 1,
        player: -1,
        turn: 42,
        data: vec![0x03, 0x01],
    };
    let bytes = msg.to_bytes();
    let decoded = PlayerCommand::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
