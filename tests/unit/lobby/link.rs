// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;
use crate::relay::script_value::ScriptValue;

fn settings_root(victory: ScriptValue, players: ScriptValue) -> ScriptValue {
    ScriptValue::Object(vec![(
        "initAttribs".to_string(),
        ScriptValue::Object(vec![
            (
                "map".to_string(),
                ScriptValue::String("maps/skirmishes/alpine_valleys_2p".to_string()),
            ),
            (
                "mapType".to_string(),
                ScriptValue::String("skirmish".to_string()),
            ),
            (
                "settings".to_string(),
                ScriptValue::Object(vec![
                    ("VictoryConditions".to_string(), victory),
                    ("PlayerData".to_string(), players),
                ]),
            ),
        ]),
    )])
}

fn player_data_with(n: usize, length: u32) -> ScriptValue {
    ScriptValue::Array {
        length,
        props: (0..n).map(|i| (i.to_string(), ScriptValue::Null)).collect(),
    }
}

#[test]
fn spoofed_victory_length_does_not_decide_work_and_truncates_at_cap() {
    // A hostile array can claim u32::MAX length while carrying real props;
    // the listing must finish by walking only the decoded props, capped.
    let mut props = vec![("0".to_string(), ScriptValue::String("conquest".to_string()))];
    for i in 1..70 {
        props.push((i.to_string(), ScriptValue::String("x".to_string())));
    }
    let victory = ScriptValue::Array {
        length: u32::MAX,
        props,
    };
    let root = settings_root(victory, player_data_with(2, 2));
    let map = LobbyMap::from_settings(&root).expect("map is selected");
    let mut expected = vec!["conquest".to_string()];
    expected.extend((1..64).map(|_| "x".to_string()));
    assert_eq!(map.victory_conditions, expected.join(","));
}

#[test]
fn player_data_length_is_ignored_and_clamped_to_playable_range() {
    // The lobby count must come from the decoded props, never the wire
    // length, and must stay inside the range the listing can advertise.
    let root = settings_root(
        ScriptValue::Array {
            length: 0,
            props: vec![],
        },
        player_data_with(3, u32::MAX),
    );
    assert_eq!(
        LobbyMap::from_settings(&root)
            .expect("map is selected")
            .max_players,
        3
    );

    let root = settings_root(
        ScriptValue::Array {
            length: 0,
            props: vec![],
        },
        player_data_with(100, 100),
    );
    assert_eq!(
        LobbyMap::from_settings(&root)
            .expect("map is selected")
            .max_players,
        64
    );

    let root = settings_root(
        ScriptValue::Array {
            length: 0,
            props: vec![],
        },
        player_data_with(1, u32::MAX),
    );
    assert_eq!(
        LobbyMap::from_settings(&root)
            .expect("map is selected")
            .max_players,
        2
    );

    let root = settings_root(
        ScriptValue::Array {
            length: 0,
            props: vec![],
        },
        ScriptValue::Null,
    );
    assert_eq!(
        LobbyMap::from_settings(&root)
            .expect("map is selected")
            .max_players,
        0
    );
}
