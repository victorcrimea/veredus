// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

pub mod bundle;
pub mod journal;
pub mod resume;
pub mod writer;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

use crate::relay::messages::PlayerCommand;

// Where a match stands as far as a later process is concerned. `Running`
// found at startup means the process that hosted it died without stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Running,
    Stopped,
    Finished,
    Abandoned,
}

// Who played what, as a resumed match needs it to decide who may take a slot
// back. Plain sorted lists, so two snapshots of the same table compare equal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotsSnapshot {
    pub players: Vec<SavedPlayer>,
    pub observers: Vec<String>,
    pub controller: Option<SavedIdentity>,
    pub banned_names: Vec<String>,
    pub resigned: Vec<i32>,
    // UUIDs of the players the match stopped waiting for on purpose.
    pub forfeited: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedPlayer {
    pub player_id: i32,
    pub uuid: String,
    pub name: String,
    // Empty in standalone mode, where nobody has one.
    pub lobby_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedIdentity {
    pub name: String,
    pub lobby_name: String,
}

// What the game thread hands the writer. Copies only, so the writer never
// shares state with the game (A5). Timestamps are read on the game thread,
// because the writer is not one of the threads allowed a clock (A7).
#[derive(Debug)]
pub enum SaveItem {
    Started {
        now: DateTime<Utc>,
        settings: Vec<u8>,
        ai_settings: Option<Vec<u8>>,
        ai_players: Vec<i32>,
    },
    Resumed {
        now: DateTime<Utc>,
        turn: u32,
    },
    Turn {
        turn: u32,
        length: u16,
        commands: Vec<PlayerCommand>,
    },
    Hash {
        turn: u32,
        hash: Vec<u8>,
    },
    Slots(SlotsSnapshot),
    Checkpoint {
        turn: u32,
        state: Arc<Vec<u8>>,
    },
    ClientState {
        first: u32,
        last: u32,
        state: Arc<Vec<u8>>,
    },
    AiState {
        first: u32,
        last: u32,
        state: Arc<Vec<u8>>,
    },
    Status {
        now: DateTime<Utc>,
        status: Status,
    },
    // Time to fsync the journal. Paced by the game thread, since the writer
    // may not read a clock (A7), and a match sends items far more often than
    // the writer could ever sit idle long enough to notice the time itself.
    Sync,
}

// How a game is saved, handed to the pool per game. `lobby_account` is the
// account node that hosts it, empty in standalone mode.
#[derive(Debug, Clone)]
pub struct SaveSetup {
    pub root: PathBuf,
    pub flush_interval: Duration,
    pub keep_finished: bool,
    pub lobby_account: String,
}
