// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use crate::relay::messages::PlayerCommand;
use crate::savegame::journal;

use super::*;

fn setup(keep_finished: bool) -> SaveSetup {
    SaveSetup {
        root: std::env::temp_dir().join(format!(
            "veredus-writer-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        )),
        flush_interval: Duration::from_millis(5),
        keep_finished,
        lobby_account: String::new(),
    }
}

fn meta() -> BundleMeta {
    BundleMeta {
        mode: Mode::Standalone,
        lobby_account: String::new(),
        lobby_host_name: String::new(),
        engine_version: "0.28.0".to_string(),
        mods: vec![ModRecord {
            name: "0ad".to_string(),
            version: "0.28.0".to_string(),
        }],
        turn_length_ms: 200,
    }
}

fn started() -> SaveItem {
    SaveItem::Started {
        now: DateTime::UNIX_EPOCH,
        settings: b"{}".to_vec(),
        ai_settings: None,
        ai_players: Vec::new(),
    }
}

fn run_items(setup: &SaveSetup, items: Vec<SaveItem>) {
    let (tx, rx) = mpsc::channel();
    for item in items {
        tx.send(item).unwrap();
    }
    drop(tx);
    run(setup.clone(), meta(), "gid_test".to_string(), None, rx);
}

fn turn(n: u32) -> SaveItem {
    SaveItem::Turn {
        turn: n,
        length: 200,
        commands: vec![PlayerCommand {
            client: 1,
            player: 1,
            turn: n,
            data: vec![1],
        }],
    }
}

#[test]
fn a_stopped_match_keeps_its_bundle() {
    let setup = setup(false);
    run_items(
        &setup,
        vec![
            started(),
            turn(4),
            SaveItem::Hash {
                turn: 1,
                hash: vec![3; 16],
            },
            SaveItem::Checkpoint {
                turn: 4,
                state: Arc::new(vec![5, 5]),
            },
            SaveItem::Status {
                now: DateTime::UNIX_EPOCH,
                status: Status::Stopped,
            },
        ],
    );
    let dir = setup.root.join("gid_test");
    let manifest: Manifest = bundle::read_json(&dir.join(bundle::MANIFEST)).unwrap();
    assert_eq!(manifest.status, Status::Stopped);
    assert_eq!(manifest.game_id, "gid_test");
    assert_eq!(std::fs::read(dir.join(bundle::SETTINGS)).unwrap(), b"{}");
    let state = std::fs::read(dir.join(bundle::STATE)).unwrap();
    let state_meta: StateMeta = bundle::read_json(&dir.join(bundle::STATE_META)).unwrap();
    assert_eq!(state_meta.turn, 4);
    assert!(state_meta.matches(&state));
    let bytes = std::fs::read(dir.join(bundle::JOURNAL)).unwrap();
    let (records, used) = journal::read(&bytes);
    assert_eq!(used, bytes.len());
    assert_eq!(records.len(), 3);
    assert!(matches!(records[2], Record::Stopped { turn: 4, .. }));
    assert!(!dir.join(bundle::LOCK).exists());
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn a_finished_match_is_deleted_unless_kept() {
    for keep in [false, true] {
        let setup = setup(keep);
        run_items(
            &setup,
            vec![
                started(),
                turn(4),
                SaveItem::Status {
                    now: DateTime::UNIX_EPOCH,
                    status: Status::Finished,
                },
            ],
        );
        assert_eq!(setup.root.join("gid_test").exists(), keep);
        let _ = std::fs::remove_dir_all(&setup.root);
    }
}

#[test]
fn a_game_that_never_started_leaves_nothing() {
    let setup = setup(false);
    run_items(&setup, vec![turn(4)]);
    assert!(!setup.root.exists());
}

#[test]
fn a_crash_leaves_the_bundle_running() {
    let setup = setup(false);
    run_items(&setup, vec![started(), turn(4)]);
    let dir = setup.root.join("gid_test");
    let manifest: Manifest = bundle::read_json(&dir.join(bundle::MANIFEST)).unwrap();
    assert_eq!(manifest.status, Status::Running);
    std::fs::remove_dir_all(&setup.root).unwrap();
}
