// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_file_transfer_response() {
    let msg = GamestateResponse {
        request_id: 7,
        length: 1024,
    };
    let bytes = msg.to_bytes();
    let decoded = GamestateResponse::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
