// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::super::guid::Guid;
use super::*;

#[test]
fn roundtrip_client_performance() {
    let msg = LaggingClients {
        clients: vec![
            PerformanceEntry {
                guid: Guid("abc".to_string()),
                mean_rtt: 50,
            },
            PerformanceEntry {
                guid: Guid("def".to_string()),
                mean_rtt: 100,
            },
        ],
    };
    let bytes = msg.to_bytes();
    let decoded = LaggingClients::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}

#[test]
fn roundtrip_client_performance_empty() {
    let msg = LaggingClients { clients: vec![] };
    let bytes = msg.to_bytes();
    let decoded = LaggingClients::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
