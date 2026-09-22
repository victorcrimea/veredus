// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_join_sync_start() {
    let msg = Join {
        init_attributes: vec![0xBE, 0xEF],
    };
    let bytes = msg.to_bytes();
    let decoded = Join::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
