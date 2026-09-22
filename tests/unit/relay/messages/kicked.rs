// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_kicked() {
    let msg = Kicked {
        name: "BadPlayer".to_string(),
        ban: true,
    };
    let bytes = msg.to_bytes();
    let decoded = Kicked::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn kicked_fixed_bytes() {
    // Test with fixed byte layout that matches upstream CStrW format
    // "Bad" = 00 42 00 61 00 64 00 00 (UTF-16 BE + null terminator)
    // then 01 for ban
    let mut buf = Vec::new();
    buf.extend_from_slice(&[0x00, b'B', 0x00, b'a', 0x00, b'd', 0x00, 0x00]); // "Bad" in UTF-16 BE + null terminator
    buf.push(0x01); // ban = true

    let decoded = Kicked::from_bytes(&buf).unwrap();
    assert_eq!(decoded.name, "Bad");
    assert_eq!(decoded.ban, true);

    // Test encoding back to bytes
    let msg = Kicked {
        name: "Bad".to_string(),
        ban: true,
    };
    let encoded = msg.to_bytes();
    assert_eq!(encoded, buf);
}
