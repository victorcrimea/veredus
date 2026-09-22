// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn make_host(guid: &str, name: &str, player_id: i8, status: u8) -> Host {
    Host {
        guid: Guid(guid.to_string()),
        name: name.to_string(),
        player_id,
        status,
    }
}

#[test]
fn roundtrip_host() {
    let host = make_host("abc123", "TestPlayer", 1, 2);
    let bytes = host.to_bytes();
    let (decoded, bytes_consumed) = Host::from_bytes(&bytes).unwrap();

    assert_eq!(decoded, host);
    assert_eq!(bytes_consumed, bytes.len());
}

#[test]
fn roundtrip_host_empty_name() {
    let host = make_host("abc123", "", 1, 2);
    let bytes = host.to_bytes();
    let (decoded, bytes_consumed) = Host::from_bytes(&bytes).unwrap();

    assert_eq!(decoded, host);
    assert_eq!(bytes_consumed, bytes.len());
}

#[test]
fn roundtrip_host_negative_player_id() {
    let host = make_host("abc123", "TestPlayer", -1, 0);
    let bytes = host.to_bytes();
    let (decoded, bytes_consumed) = Host::from_bytes(&bytes).unwrap();

    assert_eq!(decoded, host);
    assert_eq!(bytes_consumed, bytes.len());
}
