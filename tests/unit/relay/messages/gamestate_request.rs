// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_file_transfer_request() {
    let msg = GamestateRequest {
        request_type: -1,
        request_id: 42,
    };
    let bytes = msg.to_bytes();
    let decoded = GamestateRequest::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
