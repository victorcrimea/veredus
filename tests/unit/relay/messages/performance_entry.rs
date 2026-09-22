// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_performance_entry() {
    let msg = PerformanceEntry {
        guid: Guid("abc123".to_string()),
        mean_rtt: 50,
    };
    let bytes = msg.to_bytes();
    let (decoded, pos) = PerformanceEntry::from_bytes(&bytes, 0).unwrap();
    assert_eq!(decoded, msg);
    assert_eq!(pos, bytes.len());
}
