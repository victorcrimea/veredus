// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_file_transfer_ack() {
    let msg = GamestateChunkAck {
        request_id: 7,
        num_packets: 3,
    };
    let bytes = msg.to_bytes();
    let decoded = GamestateChunkAck::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
