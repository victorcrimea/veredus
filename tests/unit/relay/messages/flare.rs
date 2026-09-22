// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_flare() {
    let msg = Flare {
        guid: Guid("abc123".to_string()),
        position_x: "1.5".to_string(),
        position_y: "2.5".to_string(),
        position_z: "3.5".to_string(),
    };
    let bytes = msg.to_bytes();
    let decoded = Flare::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
