// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_enabled_mod() {
    let msg = EnabledMod {
        name: "public".to_string(),
        version: "0.28.0".to_string(),
    };
    let bytes = msg.to_bytes();
    let (decoded, bytes_consumed) = EnabledMod::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
    assert_eq!(bytes_consumed, bytes.len());
}
