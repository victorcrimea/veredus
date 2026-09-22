// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_assign_player() {
    let msg = MapPlayerIdToSlot {
        player_id: 2,
        guid: Guid("abc123".to_string()),
    };
    let bytes = msg.to_bytes();
    let decoded = MapPlayerIdToSlot::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
