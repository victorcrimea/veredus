// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use crate::lobby::link::LobbyMap;
use crate::relay::messages::PlayerCommand;
use crate::relay::server_fsm::Config;
use crate::relay::server_fsm::SIMULATION_VERSION;
use crate::relay::turn::INITIAL_READY_TURN;
use crate::savegame::SlotsSnapshot;
use crate::savegame::Status;
use crate::savegame::bundle;
use crate::savegame::bundle::FORMAT_VERSION;
use crate::savegame::bundle::Lock;
use crate::savegame::bundle::Manifest;
use crate::savegame::bundle::ModRecord;
use crate::savegame::bundle::Mode;
use crate::savegame::bundle::StateMeta;
use crate::savegame::journal;
use crate::savegame::journal::Record;
use crate::savegame::writer::Existing;
use crate::sidecar::BaseState;

// One released turn as the journal kept it.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedTurn {
    pub turn: u32,
    pub length: u16,
    pub commands: Vec<PlayerCommand>,
}

// Everything the FSM needs to rebuild a saved match. Plain data: reading it
// is IO, and that happens here, before the game thread exists (A2).
#[derive(Debug, Clone)]
pub struct ResumeData {
    // The stock clients' copy, exactly as JOIN carries it.
    pub settings: Vec<u8>,
    pub turn_length_ms: u16,
    // In release order. The last one is the turn the match stopped at.
    pub turns: Vec<SavedTurn>,
    pub hashes: Vec<(u32, Vec<u8>)>,
    pub slots: SlotsSnapshot,
    // The newest state, from a sidecar checkpoint or a client, when one was
    // stored and still matches its turn. A sidecar replays from it; without
    // one it is what returning clients load.
    pub base: Option<BaseState>,
    // What brings the AI host back, for a match with hosted AI players.
    pub ai: Option<AiResume>,
    // What the lobby listing showed, for a lobby match to be listed again.
    pub lobby_map: Option<LobbyMap>,
}

// The AI host's side of a saved match: its own copy of the settings, the
// player ids it plays and the newest state pulled from it.
#[derive(Debug, Clone)]
pub struct AiResume {
    pub settings: Vec<u8>,
    pub players: Vec<i32>,
    pub state: BaseState,
}

impl ResumeData {
    // The turn the match stopped at: the last one released, or the one a
    // fresh match starts at when it stopped before releasing any.
    pub fn last_turn(&self) -> u32 {
        self.turns
            .last()
            .map_or(INITIAL_READY_TURN, |t| t.turn)
            .max(INITIAL_READY_TURN)
    }
}

// A bundle chosen for resuming, with its lock held and its manifest already
// counting this attempt.
#[derive(Debug)]
pub struct Resumable {
    pub game_id: String,
    pub data: ResumeData,
    pub existing: Existing,
}

// What the running server must match for a saved match to be resumed on it.
#[derive(Debug, Clone)]
pub struct Expect {
    pub mode: Mode,
    pub engine_version: String,
    pub mods: Vec<ModRecord>,
    // Without a sidecar nothing can rebuild the state.
    pub sidecar: bool,
    pub max_attempts: u32,
}

impl Expect {
    // What a game hosted with `config` runs.
    pub fn new(mode: Mode, config: &Config, max_attempts: u32) -> Self {
        Expect {
            mode,
            engine_version: SIMULATION_VERSION.to_string(),
            mods: ModRecord::list(&config.enabled_mods),
            sidecar: config.sidecar_dumps,
            max_attempts,
        }
    }
}

// The newest bundle under `root` that can be resumed here, locked and with
// this attempt counted. Only one game can hold the standalone port, so the
// rest are named in the log and left untouched for a later start.
pub fn pick(root: &Path, expect: &Expect) -> Option<Resumable> {
    let mut chosen = None;
    for dir in scan(root) {
        if chosen.is_some() {
            tracing::warn!(dir = %dir.display(), "save: another saved match is waiting, it is left for a later start");
            continue;
        }
        match load(&dir, expect) {
            Ok(resumable) => chosen = Some(resumable),
            Err(Skip::Locked) => {
                tracing::info!(dir = %dir.display(), "save: saved match is held by another process")
            }
            Err(skip) => {
                tracing::warn!(dir = %dir.display(), %skip, "save: saved match not resumed")
            }
        }
    }
    chosen
}

// Every bundle under `root` that can be resumed here, newest first, each
// locked and with this attempt counted. A lobby server has an account pool
// rather than one port, so it takes them all.
pub fn pick_all(root: &Path, expect: &Expect) -> Vec<Resumable> {
    let mut chosen = Vec::new();
    for dir in scan(root) {
        match load(&dir, expect) {
            Ok(resumable) => chosen.push(resumable),
            Err(Skip::Locked) => {
                tracing::info!(dir = %dir.display(), "save: saved match is held by another process")
            }
            Err(skip) => {
                tracing::warn!(dir = %dir.display(), %skip, "save: saved match not resumed")
            }
        }
    }
    chosen
}

#[derive(Debug, thiserror::Error)]
pub enum Skip {
    #[error("not resumable: {0:?}")]
    NotResumable(Status),
    #[error("another process holds it")]
    Locked,
    #[error("incompatible: {0}")]
    Incompatible(String),
    #[error("no sidecar to rebuild its state, and no saved state to serve")]
    NoSidecar,
    #[error("given up after {0} resume attempts, marked abandoned")]
    Abandoned(u32),
    #[error("unreadable: {0}")]
    Unreadable(String),
}

// Bundles a later process could pick up, the most recently written first.
// Unreadable ones are left for `load` to report.
pub fn scan(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut found: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|dir| dir.join(bundle::MANIFEST).is_file())
        .filter(|dir| {
            bundle::read_json::<Manifest>(&dir.join(bundle::MANIFEST)).map_or(true, |m| {
                matches!(m.status, Status::Running | Status::Stopped)
            })
        })
        .map(|dir| {
            // A file time orders bundles and is never compared with the
            // clock, so it is not a clock read (A7).
            let written = std::fs::metadata(dir.join(bundle::JOURNAL))
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            (written, dir)
        })
        .collect();
    found.sort_by_key(|(written, _)| std::cmp::Reverse(*written));
    found.into_iter().map(|(_, dir)| dir).collect()
}

// Takes the bundle's lock, checks it can be resumed here and counts the
// attempt. Every refusal before the attempt is counted leaves the bundle as
// it was, so a later process with the right setup can still resume it.
pub fn load(dir: &Path, expect: &Expect) -> Result<Resumable, Skip> {
    let manifest_path = dir.join(bundle::MANIFEST);
    let manifest: Manifest =
        bundle::read_json(&manifest_path).map_err(|e| Skip::Unreadable(e.to_string()))?;
    if !matches!(manifest.status, Status::Running | Status::Stopped) {
        return Err(Skip::NotResumable(manifest.status));
    }
    let lock = Lock::acquire(dir)
        .map_err(|e| Skip::Unreadable(e.to_string()))?
        .ok_or(Skip::Locked)?;
    check_compatible(&manifest, expect)?;
    let base = read_state(dir);
    if !expect.sidecar && base.is_none() {
        return Err(Skip::NoSidecar);
    }
    let ai = if manifest.ai_players.is_empty() {
        None
    } else {
        // The AI host is a sidecar process, whatever state is saved.
        if !expect.sidecar {
            return Err(Skip::NoSidecar);
        }
        Some(read_ai(dir, &manifest.ai_players)?)
    };

    let settings =
        std::fs::read(dir.join(bundle::SETTINGS)).map_err(|e| Skip::Unreadable(e.to_string()))?;
    let (turns, hashes) = read_journal(&dir.join(bundle::JOURNAL))?;
    let slots: SlotsSnapshot = match bundle::read_json(&dir.join(bundle::SLOTS)) {
        Ok(slots) => slots,
        Err(error) => {
            tracing::warn!(%error, dir = %dir.display(), "save: no readable slot table, nobody gets a slot back");
            SlotsSnapshot::default()
        }
    };

    let mut manifest = manifest;
    manifest.resume_attempts += 1;
    if manifest.resume_attempts > expect.max_attempts {
        manifest.status = Status::Abandoned;
        let attempts = manifest.resume_attempts - 1;
        bundle::write_json(&manifest_path, &manifest)
            .map_err(|e| Skip::Unreadable(e.to_string()))?;
        return Err(Skip::Abandoned(attempts));
    }
    bundle::write_json(&manifest_path, &manifest).map_err(|e| Skip::Unreadable(e.to_string()))?;

    let data = ResumeData {
        settings,
        turn_length_ms: manifest.turn_length_ms,
        turns,
        hashes,
        slots,
        base,
        ai,
        lobby_map: manifest.lobby_map.clone(),
    };
    Ok(Resumable {
        game_id: manifest.game_id.clone(),
        data,
        existing: Existing {
            dir: dir.to_path_buf(),
            manifest,
            lock,
        },
    })
}

fn check_compatible(manifest: &Manifest, expect: &Expect) -> Result<(), Skip> {
    if manifest.format_version != FORMAT_VERSION {
        return Err(Skip::Incompatible(format!(
            "format version {}",
            manifest.format_version
        )));
    }
    if manifest.mode != expect.mode {
        return Err(Skip::Incompatible(format!(
            "saved in {:?} mode",
            manifest.mode
        )));
    }
    if manifest.engine_version != expect.engine_version {
        return Err(Skip::Incompatible(format!(
            "engine version {}",
            manifest.engine_version
        )));
    }
    if manifest.mods != expect.mods {
        return Err(Skip::Incompatible("a different mod list".to_string()));
    }
    Ok(())
}

// The released turns and the agreed hashes.
type Journal = (Vec<SavedTurn>, Vec<(u32, Vec<u8>)>);

// The torn tail of a crash is cut off here, so the resumed match appends
// right after the last whole record.
fn read_journal(path: &Path) -> Result<Journal, Skip> {
    let bytes = std::fs::read(path).map_err(|e| Skip::Unreadable(e.to_string()))?;
    let (records, used) = journal::read(&bytes);
    if used < bytes.len() {
        tracing::warn!(
            path = %path.display(),
            dropped = bytes.len() - used,
            "save: journal has a torn tail, cutting it off"
        );
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(|e| Skip::Unreadable(e.to_string()))?;
        file.set_len(used as u64)
            .and_then(|()| file.sync_all())
            .map_err(|e| Skip::Unreadable(e.to_string()))?;
    }
    let mut turns = Vec::new();
    let mut hashes = Vec::new();
    for record in records {
        match record {
            Record::Turn {
                turn,
                length,
                commands,
            } => turns.push(SavedTurn {
                turn,
                length,
                commands,
            }),
            Record::Hash { turn, hash } => hashes.push((turn, hash)),
            Record::Resumed { .. } | Record::Stopped { .. } => {}
        }
    }
    Ok((turns, hashes))
}

// A state whose turn record does not match it is dropped rather than
// trusted: a sidecar then replays from turn 0, which is slower but right,
// and without one the match is not resumed.
fn read_state(dir: &Path) -> Option<BaseState> {
    let state = std::fs::read(dir.join(bundle::STATE)).ok()?;
    let meta: StateMeta = bundle::read_json(&dir.join(bundle::STATE_META)).ok()?;
    if !meta.matches(&state) {
        tracing::warn!(dir = %dir.display(), "save: saved state does not match its turn, not used");
        return None;
    }
    Some(BaseState {
        turn: meta.turn,
        state: Arc::new(state),
    })
}

// Without its own state the AI host cannot be brought back, and its
// players would stand idle for the rest of the match, so the bundle is left
// alone rather than resumed without them.
fn read_ai(dir: &Path, players: &[i32]) -> Result<AiResume, Skip> {
    let settings = std::fs::read(dir.join(bundle::SETTINGS_AI))
        .map_err(|_| Skip::Incompatible("no AI host settings".to_string()))?;
    let missing = || Skip::Incompatible("no AI host state".to_string());
    let state = std::fs::read(dir.join(bundle::AI_STATE)).map_err(|_| missing())?;
    let meta: StateMeta =
        bundle::read_json(&dir.join(bundle::AI_STATE_META)).map_err(|_| missing())?;
    if !meta.matches(&state) {
        return Err(Skip::Incompatible(
            "the AI host state does not match its turn".to_string(),
        ));
    }
    Ok(AiResume {
        settings,
        players: players.to_vec(),
        state: BaseState {
            turn: meta.turn,
            state: Arc::new(state),
        },
    })
}

#[cfg(test)]
#[path = "../../tests/unit/savegame/resume.rs"]
mod tests;
