// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_clients_loading() {
    let msg = PlayersLoading {
        clients: vec![Guid("abc".to_string()), Guid("def".to_string())],
    };
    let bytes = msg.to_bytes();
    let decoded = PlayersLoading::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn roundtrip_clients_loading_empty() {
    let msg = PlayersLoading { clients: vec![] };
    let bytes = msg.to_bytes();
    let decoded = PlayersLoading::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
