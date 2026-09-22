// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_rejoined() {
    let msg = Joined {
        guid: Guid("abc123".to_string()),
    };
    let bytes = msg.to_bytes();
    let decoded = Joined::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
