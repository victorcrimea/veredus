// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_server_handshake() {
    let msg = Syn {
        magic: 0x12345678,
        protocol_version: 1,
        engine_version: "0.28.0".to_string(),
        enabled_mods: vec![EnabledMod {
            name: "public".to_string(),
            version: "0.28.0".to_string(),
        }],
    };
    let bytes = msg.to_bytes();
    let decoded = Syn::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn roundtrip_server_handshake_empty_mods() {
    let msg = Syn {
        magic: 0x12345678,
        protocol_version: 1,
        engine_version: "0.28.0".to_string(),
        enabled_mods: vec![],
    };
    let bytes = msg.to_bytes();
    let decoded = Syn::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
