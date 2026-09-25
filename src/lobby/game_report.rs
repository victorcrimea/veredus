// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Personal mode's rated-game report. The sidecar engine builds the report
// itself and prints it next to a replay's result; the relay only decides
// whether to send it and says which player it is sent for. The checks here
// follow what the rating bot accepts. Plain data only: the game thread calls
// this, and it must not need tokio-xmpp.

use serde_json::Value;

use crate::lobby::link::GameReport;

// The rating bot rates two-player games only.
const RATED_PLAYERS: usize = 2;

// A player state the rating bot refuses a whole report over.
const UNFINISHED: &str = "active";

// Whether a START_SETTINGS asks for a rated match: rating switched on, and
// a match the bot would rate. The settings list the players without gaia.
pub fn is_rated(init_attributes: &[u8]) -> bool {
    let Ok(attribs) = serde_json::from_slice::<Value>(init_attributes) else {
        return false;
    };
    let settings = attribs.get("settings");
    let rating = settings
        .and_then(|s| s.get("RatingEnabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let players = settings
        .and_then(|s| s.get("PlayerData"))
        .and_then(Value::as_array)
        .map_or(0, Vec::len);
    rating && players == RATED_PLAYERS
}

// The engine's report, as the sidecar printed it (a JSON object of string
// attributes, without playerID), sent for `player_id`. None when it cannot
// be read or the bot would refuse it: the bot drops a report in which any
// player is still unfinished, which is what an operator's stop leaves.
pub fn for_player(report_json: &str, player_id: i32) -> Option<GameReport> {
    let object = serde_json::from_str::<Value>(report_json).ok()?;
    let object = object.as_object()?;
    let mut attrs = Vec::with_capacity(object.len() + 1);
    attrs.push(("playerID".to_string(), player_id.to_string()));
    for (key, value) in object {
        attrs.push((key.clone(), value.as_str()?.to_string()));
    }
    // One entry per player, each followed by a comma.
    let states = object.get("playerStates")?.as_str()?;
    if states.split(',').any(|state| state == UNFINISHED) {
        return None;
    }
    Some(GameReport { attrs })
}
