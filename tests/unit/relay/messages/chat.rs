// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_chat_with_receivers() {
    let msg = Chat {
        sender_guid: Guid("sender1".to_string()),
        message: "Hello".to_string(),
        receivers: vec![Guid("recv1".to_string()), Guid("recv2".to_string())],
    };
    let bytes = msg.to_bytes();
    let decoded = Chat::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn roundtrip_chat_no_receivers() {
    let msg = Chat {
        sender_guid: Guid("sender1".to_string()),
        message: "Hello".to_string(),
        receivers: vec![],
    };
    let bytes = msg.to_bytes();
    let decoded = Chat::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
