// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// The FSM and the game thread must reach the lobby without depending on
// tokio-xmpp, so this half of the channel pair carries only plain data.

use crate::relay::messages::EnabledMod;
use crate::relay::script_value::ScriptValue;

// The XMPP account task forwards a lobbyauth IQ here. `UnboundedSender::send`
// on the other end is synchronous and never blocks, so the game thread gets
// no runtime (A1).
pub struct LobbyAuthToken {
    pub username: String,
    pub token: String,
}

// The map fields of Sec. 17.3's register attributes, derived from the
// controller's GAME_SETTINGS (Sec. 5) once one has arrived. None until then:
// a hostme game is listed before any settings exist.
#[derive(Debug, Clone, PartialEq)]
pub struct LobbyMap {
    pub map_name: String,
    pub nice_map_name: String,
    pub map_type: String,
    pub map_size: String,
    pub victory_conditions: String,
    pub max_players: u32,
}

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
            .map(|vc| {
                let len = vc.array_len().unwrap_or(0);
                (0..len)
                    .filter_map(|i| vc.array_get(i).and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_default();
        let max_players = settings
            .and_then(|s| s.get("PlayerData"))
            .and_then(|pd| pd.array_len())
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
    pub auth_rx: std::sync::mpsc::Receiver<LobbyAuthToken>,
    pub events_tx: tokio::sync::mpsc::UnboundedSender<GameToLobby>,
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
}
