// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn flooding_disconnects_as_kicked() {
    assert_eq!(PeerFault::Flooding.reason(), Some(DisconnectReason::Kicked));
}
