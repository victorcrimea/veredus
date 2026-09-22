// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_end_command_batch() {
    let msg = TurnSealed {
        turn: 100,
        turn_length: 500,
    };
    let bytes = msg.to_bytes();
    let decoded = TurnSealed::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
