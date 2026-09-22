// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_file_transfer_data() {
    let msg = GamestateChunk {
        request_id: 5,
        data: vec![10, 20, 30],
    };
    let bytes = msg.to_bytes();
    let decoded = GamestateChunk::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
