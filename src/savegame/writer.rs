// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;

use chrono::DateTime;
use chrono::Utc;

use crate::savegame::SaveItem;
use crate::savegame::SaveSetup;
use crate::savegame::SlotsSnapshot;
use crate::savegame::Status;
use crate::savegame::bundle;
use crate::savegame::bundle::ClientStateMeta;
use crate::savegame::bundle::FORMAT_VERSION;
use crate::savegame::bundle::Lock;
use crate::savegame::bundle::Manifest;
use crate::savegame::bundle::ModRecord;
use crate::savegame::bundle::Mode;
use crate::savegame::bundle::StateMeta;
use crate::savegame::journal::Record;

// What a fresh bundle's manifest is made of, besides what the match itself
// reports.
#[derive(Debug, Clone)]
pub struct BundleMeta {
    pub mode: Mode,
    pub lobby_account: String,
    pub lobby_host_name: String,
    pub engine_version: String,
    pub mods: Vec<ModRecord>,
    pub turn_length_ms: u16,
}

// A bundle that already exists, for a resumed match to carry on writing to.
// Its lock was taken when it was chosen for resuming.
#[derive(Debug)]
pub struct Existing {
    pub dir: PathBuf,
    pub manifest: Manifest,
    pub lock: Lock,
}

struct Writer {
    dir: PathBuf,
    game_id: String,
    meta: BundleMeta,
    keep_finished: bool,
    manifest: Option<Manifest>,
    journal: Option<File>,
    lock: Option<Lock>,
    // Unsynced journal bytes, fsynced on the next flush.
    dirty: bool,
    // After one IO error the bundle is incomplete, so nothing more is
    // written to it; the game itself carries on.
    failed: bool,
    last_turn: u32,
}

// The body of a game's writer thread. It returns once the game thread drops
// its sender, after the last flush and status change, which is what lets
// the pool join it and know the bundle is final.
pub fn run(
    setup: SaveSetup,
    meta: BundleMeta,
    game_id: String,
    existing: Option<Existing>,
    rx: Receiver<SaveItem>,
) {
    let mut writer = Writer {
        dir: setup.root.join(&game_id),
        game_id,
        meta,
        keep_finished: setup.keep_finished,
        manifest: None,
        journal: None,
        lock: None,
        dirty: false,
        failed: false,
        last_turn: 0,
    };
    if let Some(existing) = existing {
        writer.reopen(existing);
    }
    loop {
        match rx.recv_timeout(setup.flush_interval) {
            Ok(item) => writer.apply(item),
            Err(RecvTimeoutError::Timeout) => writer.flush(),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    writer.finish();
}

impl Writer {
    fn fail(&mut self, context: &str, error: std::io::Error) {
        if !self.failed {
            tracing::error!(%error, dir = %self.dir.display(), "save: {context}, this match is no longer saved");
        }
        self.failed = true;
        self.journal = None;
    }

    fn reopen(&mut self, existing: Existing) {
        self.dir = existing.dir;
        self.lock = Some(existing.lock);
        let journal = std::fs::OpenOptions::new()
            .append(true)
            .open(self.dir.join(bundle::JOURNAL));
        match journal {
            Ok(file) => self.journal = Some(file),
            Err(error) => return self.fail("cannot open the journal", error),
        }
        let mut manifest = existing.manifest;
        manifest.status = Status::Running;
        self.manifest = Some(manifest);
        self.write_manifest();
    }

    fn apply(&mut self, item: SaveItem) {
        if self.failed {
            return;
        }
        match item {
            SaveItem::Started {
                now,
                settings,
                ai_settings,
                ai_players,
            } => self.start(now, &settings, ai_settings.as_deref(), ai_players),
            SaveItem::Resumed { now, turn } => {
                self.last_turn = turn;
                self.append(&Record::Resumed {
                    turn,
                    unix_ms: now.timestamp_millis(),
                });
            }
            SaveItem::Turn {
                turn,
                length,
                commands,
            } => {
                self.last_turn = turn;
                self.append(&Record::Turn {
                    turn,
                    length,
                    commands,
                });
            }
            SaveItem::Hash { turn, hash } => self.append(&Record::Hash { turn, hash }),
            SaveItem::Slots(slots) => self.write_slots(slots),
            SaveItem::Checkpoint { turn, state } => self.write_state(turn, &state),
            SaveItem::ClientState { first, last, state } => {
                self.write_client_state(first, last, &state)
            }
            SaveItem::Status { now, status } => self.set_status(now, status),
        }
    }

    fn start(
        &mut self,
        now: DateTime<Utc>,
        settings: &[u8],
        ai_settings: Option<&[u8]>,
        ai_players: Vec<i32>,
    ) {
        if self.manifest.is_some() {
            return;
        }
        if let Err(error) = std::fs::create_dir_all(&self.dir) {
            return self.fail("cannot create the save directory", error);
        }
        match Lock::acquire(&self.dir) {
            Ok(Some(lock)) => self.lock = Some(lock),
            Ok(None) => {
                let error = std::io::Error::other("another process holds its lock");
                return self.fail("cannot lock the bundle", error);
            }
            Err(error) => return self.fail("cannot lock the bundle", error),
        }
        if let Err(error) = bundle::write_atomic(&self.dir.join(bundle::SETTINGS), settings) {
            return self.fail("cannot write the settings", error);
        }
        if let Some(ai) = ai_settings
            && let Err(error) = bundle::write_atomic(&self.dir.join(bundle::SETTINGS_AI), ai)
        {
            return self.fail("cannot write the AI settings", error);
        }
        let journal = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(bundle::JOURNAL));
        match journal {
            Ok(file) => self.journal = Some(file),
            Err(error) => return self.fail("cannot create the journal", error),
        }
        self.manifest = Some(Manifest {
            format_version: FORMAT_VERSION,
            game_id: self.game_id.clone(),
            created_unix_ms: now.timestamp_millis(),
            engine_version: self.meta.engine_version.clone(),
            mods: self.meta.mods.clone(),
            mode: self.meta.mode,
            lobby_account: self.meta.lobby_account.clone(),
            lobby_host_name: self.meta.lobby_host_name.clone(),
            controller: None,
            status: Status::Running,
            resume_attempts: 0,
            turn_length_ms: self.meta.turn_length_ms,
            ai_players,
        });
        self.write_manifest();
        tracing::info!(dir = %self.dir.display(), "save: match bundle started");
    }

    fn append(&mut self, record: &Record) {
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        if let Err(error) = journal.write_all(&record.to_bytes()) {
            return self.fail("cannot append to the journal", error);
        }
        self.dirty = true;
    }

    fn flush(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(journal) = self.journal.as_mut() else {
            return;
        };
        if let Err(error) = journal.sync_data() {
            return self.fail("cannot sync the journal", error);
        }
        self.dirty = false;
    }

    fn write_manifest(&mut self) {
        let Some(manifest) = self.manifest.as_ref() else {
            return;
        };
        if let Err(error) = bundle::write_json(&self.dir.join(bundle::MANIFEST), manifest) {
            self.fail("cannot write the manifest", error);
        }
    }

    fn write_slots(&mut self, slots: SlotsSnapshot) {
        let Some(manifest) = self.manifest.as_mut() else {
            return;
        };
        let controller_changed = manifest.controller != slots.controller;
        manifest.controller = slots.controller.clone();
        if let Err(error) = bundle::write_json(&self.dir.join(bundle::SLOTS), &slots) {
            return self.fail("cannot write the slots", error);
        }
        if controller_changed {
            self.write_manifest();
        }
    }

    // The state first, then the turn it is at: the meta's checksum is what
    // tells a reader whether the pair belongs together.
    fn write_state(&mut self, turn: u32, state: &[u8]) {
        if self.manifest.is_none() {
            return;
        }
        if let Err(error) = bundle::write_atomic(&self.dir.join(bundle::STATE), state) {
            return self.fail("cannot write the checkpoint state", error);
        }
        let meta = StateMeta::of(turn, state);
        if let Err(error) = bundle::write_json(&self.dir.join(bundle::STATE_META), &meta) {
            self.fail("cannot write the checkpoint turn", error);
        }
    }

    fn write_client_state(&mut self, first: u32, last: u32, state: &[u8]) {
        if self.manifest.is_none() {
            return;
        }
        if let Err(error) = bundle::write_atomic(&self.dir.join(bundle::CLIENT_STATE), state) {
            return self.fail("cannot write the client state", error);
        }
        let meta = ClientStateMeta::of(first, last, state);
        if let Err(error) = bundle::write_json(&self.dir.join(bundle::CLIENT_STATE_META), &meta) {
            self.fail("cannot write the client state turns", error);
        }
    }

    fn set_status(&mut self, now: DateTime<Utc>, status: Status) {
        if self.manifest.is_none() {
            return;
        }
        if status == Status::Stopped {
            let turn = self.last_turn;
            self.append(&Record::Stopped {
                turn,
                unix_ms: now.timestamp_millis(),
            });
            self.flush();
        }
        if let Some(manifest) = self.manifest.as_mut() {
            manifest.status = status;
        }
        self.write_manifest();
    }

    fn finish(mut self) {
        self.flush();
        self.journal = None;
        let finished = self
            .manifest
            .as_ref()
            .is_some_and(|m| m.status == Status::Finished);
        if finished && !self.keep_finished {
            // The lock lives inside the directory, so it goes with it.
            self.lock = None;
            match std::fs::remove_dir_all(&self.dir) {
                Ok(()) => {
                    tracing::info!(dir = %self.dir.display(), "save: finished match bundle deleted")
                }
                Err(error) => {
                    tracing::warn!(%error, dir = %self.dir.display(), "save: cannot delete a finished match bundle")
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/savegame/writer.rs"]
mod tests;
