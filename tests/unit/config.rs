// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn default_config_is_valid() {
    assert!(FileConfig::default().validate().is_ok());
}

#[test]
fn chat_rate_without_burst_is_rejected() {
    let mut config = FileConfig::default();
    config.game.chat_per_sec = 1;
    config.game.chat_burst = 0;
    assert!(config.validate().is_err());
}

#[test]
fn flare_rate_without_burst_is_rejected() {
    let mut config = FileConfig::default();
    config.game.flare_per_sec = 1;
    config.game.flare_burst = 0;
    assert!(config.validate().is_err());
}

#[test]
fn disabled_rate_needs_no_burst() {
    let mut config = FileConfig::default();
    config.game.chat_per_sec = 0;
    config.game.chat_burst = 0;
    config.game.flare_per_sec = 0;
    config.game.flare_burst = 0;
    assert!(config.validate().is_ok());
}

#[test]
fn flood_limits_reach_the_game_config() {
    let mut config = FileConfig::default();
    config.game.chat_per_sec = 7;
    config.game.chat_burst = 8;
    config.game.chat_max_chars = 9;
    config.game.flare_per_sec = 10;
    config.game.flare_burst = 11;
    config.game.commands_per_turn = 12;
    config.game.command_bytes_per_turn = 13;
    config.game.pause_min_charge_secs = 14;
    config.game.flood_kick_multiple = 15;
    let game = config.game.server_config(false, 0);
    assert_eq!(game.chat_per_sec, 7);
    assert_eq!(game.chat_burst, 8);
    assert_eq!(game.chat_max_chars, 9);
    assert_eq!(game.flare_per_sec, 10);
    assert_eq!(game.flare_burst, 11);
    assert_eq!(game.commands_per_turn, 12);
    assert_eq!(game.command_bytes_per_turn, 13);
    assert_eq!(game.pause_min_charge, TimeDelta::seconds(14));
    assert_eq!(game.flood_kick_multiple, 15);
}

#[test]
fn generated_config_carries_the_flood_defaults() {
    let text = toml::to_string(&FileConfig::default()).unwrap();
    let parsed: FileConfig = toml::from_str(&text).unwrap();
    let game = parsed.game.server_config(false, 0);
    let defaults = Config::default();
    assert_eq!(game.chat_per_sec, defaults.chat_per_sec);
    assert_eq!(game.chat_burst, defaults.chat_burst);
    assert_eq!(game.chat_max_chars, defaults.chat_max_chars);
    assert_eq!(game.flare_per_sec, defaults.flare_per_sec);
    assert_eq!(game.flare_burst, defaults.flare_burst);
    assert_eq!(game.commands_per_turn, defaults.commands_per_turn);
    assert_eq!(game.command_bytes_per_turn, defaults.command_bytes_per_turn);
    assert_eq!(game.pause_min_charge, defaults.pause_min_charge);
    assert_eq!(game.flood_kick_multiple, defaults.flood_kick_multiple);
}

#[test]
fn client_state_interval_is_counted_in_turns() {
    assert_eq!(client_state_interval_turns(false, 300, 200), 1500);
    assert_eq!(client_state_interval_turns(false, 0, 200), 0);
    assert_eq!(client_state_interval_turns(true, 300, 200), 0);
    assert_eq!(client_state_interval_turns(false, 1, 5000), 1);
}
