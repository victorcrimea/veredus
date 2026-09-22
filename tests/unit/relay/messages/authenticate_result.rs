// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_authenticate_result() {
    let msg = AuthenticateResult {
        code: AuthenticateResultCode::Ok,
        host_id: 1,
        is_controller: true,
        message: "Welcome".to_string(),
    };
    let bytes = msg.to_bytes();
    let decoded = AuthenticateResult::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
