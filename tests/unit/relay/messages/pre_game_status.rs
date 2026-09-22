// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_ready() {
    let msg = PreGameStatus {
        guid: Guid("abc123".to_string()),
        status: 1,
    };
    let bytes = msg.to_bytes();
    let decoded = PreGameStatus::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
