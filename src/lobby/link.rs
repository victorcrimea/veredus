// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// The FSM and the game thread must reach the lobby without depending on
// tokio-xmpp, so this half of the channel pair carries only plain data.

use serde::Deserialize;
use serde::Serialize;

use crate::relay::messages::EnabledMod;
use crate::relay::script_value::ScriptValue;

// The XMPP account task forwards a lobbyauth IQ here. `UnboundedSender::send`
// on the other end is synchronous and never blocks, so the game thread gets
// no runtime (A1).
pub struct LobbyAuthToken {
    pub username: String,
    pub token: String,
}

pub enum LobbyToGame {
    Auth(LobbyAuthToken),
    // A lobby player was just handed the game's address, so its connection
    // is about to arrive. Personal mode uses this to tell a lobby player on a
    // trusted network from the initiator while the game is unlisted.
    JoinerExpected,
}

// The map fields of Sec. 17.3's register attributes, derived from the
// controller's GAME_SETTINGS (Sec. 5) once one has arrived. None until then:
// a hostme game is listed before any settings exist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LobbyMap {
    pub map_name: String,
    pub nice_map_name: String,
    pub map_type: String,
    pub map_size: String,
    pub victory_conditions: String,
    pub max_players: u32,
}

// The listing is built from controller-supplied settings on the game thread,
// so the work it does must be bounded by what was actually decoded, never by
// a length field inside the decoded value: a hostile array can claim any
// length while carrying almost no props, and looping up to that length would
// stall the whole game.
const MAX_VICTORY_CONDITIONS: usize = 64;
const MIN_LISTED_PLAYERS: u32 = 2;
const MAX_LISTED_PLAYERS: u32 = 64;

impl LobbyMap {
    // Mirrors what the stock client itself sends in
    // gui/gamesetup/Controllers/LobbyGameRegistration.js: `root` is the whole
    // decoded GAME_SETTINGS value (both "initial-update" and "update" carry
    // "initAttribs"). None means no map is selected yet, same as the stock
    // client withholding registration until then.
    pub fn from_settings(root: &ScriptValue) -> Option<LobbyMap> {
        let attribs = root.get("initAttribs")?;
        let map_name = attribs.get("map")?.as_str()?.to_string();
        if map_name.is_empty() {
            return None;
        }
        let map_type = attribs.get("mapType")?.as_str()?.to_string();
        let settings = attribs.get("settings");
        let nice_map_name = settings
            .and_then(|s| s.get("mapName"))
            .and_then(|v| v.as_str())
            .unwrap_or(&map_name)
            .to_string();
        let map_size = if map_type == "random" {
            settings
                .and_then(|s| s.get("Size"))
                .and_then(|v| v.as_number())
                .map(|n| (n as i64).to_string())
                .unwrap_or_else(|| "0".to_string())
        } else {
            "Default".to_string()
        };
        let victory_conditions = settings
            .and_then(|s| s.get("VictoryConditions"))
            .and_then(|vc| match vc {
                ScriptValue::Array { props, .. } => Some(
                    props
                        .iter()
                        .take(MAX_VICTORY_CONDITIONS)
                        .filter_map(|(_, v)| v.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                ),
                _ => None,
            })
            .unwrap_or_default();
        let max_players = settings
            .and_then(|s| s.get("PlayerData"))
            .and_then(|pd| match pd {
                ScriptValue::Array { props, .. } => Some(
                    u32::try_from(props.len())
                        .unwrap_or(MAX_LISTED_PLAYERS)
                        .clamp(MIN_LISTED_PLAYERS, MAX_LISTED_PLAYERS),
                ),
                _ => None,
            })
            .unwrap_or(0);
        Some(LobbyMap {
            map_name,
            nice_map_name,
            map_type,
            map_size,
            victory_conditions,
            max_players,
        })
    }
}

// The game-side half of a game's lobby channels, passed into
// `run_game_server`. Each game owns its own pair (A5).
pub struct LobbyLink {
    pub auth_rx: std::sync::mpsc::Receiver<LobbyToGame>,
    pub events_tx: tokio::sync::mpsc::UnboundedSender<GameToLobby>,
    // Personal mode only. The account's, not the game's: a rated result is
    // known only once the outcome replay is done, long after the game
    // thread has dropped `events_tx` and the account has moved on.
    pub report_tx: Option<tokio::sync::mpsc::UnboundedSender<GameReport>>,
}

// A rated match's result, as the attributes of the stock client's
// jabber:iq:gamereport stanza, sent on behalf of the player the account
// belongs to.
pub struct GameReport {
    pub attrs: Vec<(String, String)>,
}

// Game-server thread -> XMPP account task. Dropping the sending half (by the
// game thread exiting) is how the account task learns the game ended: it then
// sends `unregister` and reports `GameEnded` to main.
pub enum GameToLobby {
    Listing {
        host_username: String,
        nbp: u32,
        players: String,
        map: Option<LobbyMap>,
        mods: Vec<EnabledMod>,
    },
    Started {
        nbp: u32,
        players: String,
    },
    // The match has been decided. The game keeps running for whoever stays,
    // but it is no longer one to list.
    Ended,
    // Personal mode: the player the account belongs to has left, so the game
    // is taken off the list until they are back. The account keeps
    // answering for it, so players who know it can still come back.
    Unlisted,
}

#[cfg(test)]
#[path = "../../tests/unit/lobby/link.rs"]
mod tests;
