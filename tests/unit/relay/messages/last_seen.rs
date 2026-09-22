// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_client_timeout() {
    let msg = LastSeen {
        guid: Guid("abc123".to_string()),
        last_received_time: 5000,
    };
    let bytes = msg.to_bytes();
    let decoded = LastSeen::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
