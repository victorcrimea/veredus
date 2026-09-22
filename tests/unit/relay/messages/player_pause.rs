// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_client_paused() {
    let msg = PlayerPause {
        guid: Guid("abc123".to_string()),
        pause: true,
    };
    let bytes = msg.to_bytes();
    let decoded = PlayerPause::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
