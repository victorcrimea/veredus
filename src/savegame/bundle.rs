// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::io::Write;
use std::path::Path;
use std::path::PathBuf;

use blake2::Blake2b128;
use blake2::Digest;
use serde::Deserialize;
use serde::Serialize;
use sysinfo::Pid;
use sysinfo::ProcessRefreshKind;
use sysinfo::ProcessesToUpdate;
use sysinfo::System;

use crate::lobby::link::LobbyMap;
use crate::relay::messages::EnabledMod;
use crate::savegame::SavedIdentity;
use crate::savegame::Status;

// Bumped whenever a file below changes shape, so an older bundle is left
// alone instead of being misread.
pub const FORMAT_VERSION: u32 = 1;

pub const MANIFEST: &str = "manifest.json";
pub const SETTINGS: &str = "settings.json";
pub const SETTINGS_AI: &str = "settings_ai.json";
pub const SLOTS: &str = "slots.json";
pub const JOURNAL: &str = "journal.bin";
pub const STATE: &str = "state.bin";
pub const STATE_META: &str = "state.json";
pub const AI_STATE: &str = "ai_state.bin";
pub const AI_STATE_META: &str = "ai_state.json";
pub const LOCK: &str = "lock";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Standalone,
    Lobby,
    Personal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModRecord {
    pub name: String,
    pub version: String,
}

impl ModRecord {
    pub fn list(mods: &[EnabledMod]) -> Vec<ModRecord> {
        mods.iter()
            .map(|m| ModRecord {
                name: m.name.clone(),
                version: m.version.clone(),
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format_version: u32,
    pub game_id: String,
    pub created_unix_ms: i64,
    pub engine_version: String,
    // In load order, since that is what a client is compared against.
    pub mods: Vec<ModRecord>,
    pub mode: Mode,
    pub lobby_account: String,
    pub lobby_host_name: String,
    pub controller: Option<SavedIdentity>,
    pub status: Status,
    pub resume_attempts: u32,
    pub turn_length_ms: u16,
    // The player ids the AI host plays; empty without hosted AI.
    pub ai_players: Vec<i32>,
    // Defaulted so a bundle written before it was kept still loads.
    #[serde(default)]
    pub lobby_map: Option<LobbyMap>,
}

// The stored state and the turn it is at. The checksum ties the two files
// together: they are replaced one after the other, and a crash in between
// must not pair a new state with an old turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateMeta {
    pub turn: u32,
    pub checksum: String,
}

impl StateMeta {
    pub fn of(turn: u32, state: &[u8]) -> Self {
        StateMeta {
            turn,
            checksum: hex::encode(Blake2b128::digest(state)),
        }
    }

    pub fn matches(&self, state: &[u8]) -> bool {
        hex::encode(Blake2b128::digest(state)) == self.checksum
    }
}

// Written aside, flushed and renamed over the old file, so a reader sees the
// old contents or the new ones and never half of either.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp_name = path.as_os_str().to_owned();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);
    let written = std::fs::File::create(&tmp).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    let result = written.and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

pub fn write_json<T: Serialize>(path: &Path, value: &T) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(std::io::Error::other)?;
    write_atomic(path, &bytes)
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> std::io::Result<T> {
    let bytes = std::fs::read(path)?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct LockOwner {
    pid: u32,
    // A pid alone is not an identity: a container restarts its server as
    // pid 1 every time, so a stale lock would name a live process.
    start_time: u64,
}

// Held by whichever process hosts the match, so two processes started on
// one save directory never resume the same match twice.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    // None when a live process holds it. A lock left by a process that is
    // gone is taken over.
    pub fn acquire(dir: &Path) -> std::io::Result<Option<Lock>> {
        let path = dir.join(LOCK);
        let own = LockOwner {
            pid: std::process::id(),
            start_time: start_time(std::process::id()).unwrap_or(0),
        };
        let body = serde_json::to_vec(&own).map_err(std::io::Error::other)?;
        // Two attempts: the second follows removing a stale lock, and losing
        // that race to another process means it is now held.
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    file.write_all(&body)?;
                    file.sync_all()?;
                    return Ok(Some(Lock { path }));
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if held_by_live_process(&path) {
                        return Ok(None);
                    }
                    tracing::info!(path = %path.display(), "save: taking over a stale lock");
                    std::fs::remove_file(&path)?;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// An unreadable lock counts as held: guessing wrong the other way could run
// one match in two processes.
fn held_by_live_process(path: &Path) -> bool {
    let Ok(owner) = read_json::<LockOwner>(path) else {
        return true;
    };
    match start_time(owner.pid) {
        Some(start) => start == owner.start_time,
        None => false,
    }
}

fn start_time(pid: u32) -> Option<u64> {
    let pid = Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing(),
    );
    system.process(pid).map(|p| p.start_time())
}

#[cfg(test)]
#[path = "../../tests/unit/savegame/bundle.rs"]
mod tests;
