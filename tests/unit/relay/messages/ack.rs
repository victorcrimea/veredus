// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_server_handshake_response() {
    let msg = Ack {
        use_protocol_version: 1,
        flags: 0,
        guid: Guid("abc123".to_string()),
    };
    let bytes = msg.to_bytes();
    let decoded = Ack::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
