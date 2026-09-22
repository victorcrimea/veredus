// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn roundtrip_game_saved_start() {
    let msg = StartSavegameSettings {
        init_attributes: r#"{"mapType":"random"}"#.to_string(),
    };
    let bytes = msg.to_bytes();
    let decoded = StartSavegameSettings::from_bytes(&bytes).unwrap();
    assert_eq!(decoded, msg);
}
