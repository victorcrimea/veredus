// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::relay::messages::guid::Guid;

fn make_host(guid: &str, name: &str, player_id: i8, status: u8) -> Host {
    Host {
        guid: Guid(guid.to_string()),
        name: name.to_string(),
        player_id,
        status,
    }
}

#[test]
fn roundtrip_single_host() {
    let pa = PlayerSlots {
        hosts: vec![make_host("abc123", "TestPlayer", 1, 2)],
    };
    let bytes = pa.to_bytes();
    let decoded = PlayerSlots::from_bytes(&bytes).unwrap();

    assert_eq!(decoded, pa);
}

#[test]
fn roundtrip_multiple_hosts() {
    let pa = PlayerSlots {
        hosts: vec![
            make_host("aaa", "Alice", 1, 0),
            make_host("bbb", "Bob", 2, 1),
            make_host("ccc", "Charlie", 3, 2),
        ],
    };
    let bytes = pa.to_bytes();
    let decoded = PlayerSlots::from_bytes(&bytes).unwrap();

    assert_eq!(decoded, pa);
}

#[test]
fn roundtrip_empty_hosts() {
    let pa = PlayerSlots { hosts: vec![] };
    let bytes = pa.to_bytes();
    let decoded = PlayerSlots::from_bytes(&bytes).unwrap();

    assert_eq!(decoded, pa);
}
