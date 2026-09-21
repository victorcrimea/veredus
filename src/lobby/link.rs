// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// The FSM and the game thread must reach the lobby without depending on
// tokio-xmpp, so this half of the channel pair carries only plain data.

// The XMPP account task forwards a lobbyauth IQ here. `UnboundedSender::send`
// on the other end is synchronous and never blocks, so the game thread gets
// no runtime (A1).
pub struct LobbyAuthToken {
    pub username: String,
    pub token: String,
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
    },
    Started {
        nbp: u32,
        players: String,
    },
}
