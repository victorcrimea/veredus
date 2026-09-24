// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::sync::mpsc;
use std::time::Duration;

use chrono::DateTime;

use crate::savegame::SaveItem;
use crate::savegame::SaveSetup;
use crate::savegame::SavedPlayer;
use crate::savegame::writer;
use crate::savegame::writer::BundleMeta;

use super::*;

const GAME_ID: &str = "gid_0199a000-0000-7000-8000-000000000000";

fn setup() -> SaveSetup {
    SaveSetup {
        root: std::env::temp_dir().join(format!(
            "veredus-resume-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        )),
        flush_interval: Duration::from_millis(5),
        keep_finished: false,
        lobby_account: String::new(),
    }
}

fn mods() -> Vec<ModRecord> {
    vec![ModRecord {
        name: "0ad".to_string(),
        version: "0.28.0".to_string(),
    }]
}

fn expect() -> Expect {
    Expect {
        mode: Mode::Standalone,
        engine_version: "0.28.0".to_string(),
        mods: mods(),
        sidecar: true,
        max_attempts: 3,
    }
}

fn write(setup: &SaveSetup, existing: Option<Existing>, items: Vec<SaveItem>) {
    let (tx, rx) = mpsc::channel();
    for item in items {
        tx.send(item).unwrap();
    }
    drop(tx);
    let meta = BundleMeta {
        mode: Mode::Standalone,
        lobby_account: String::new(),
        lobby_host_name: String::new(),
        engine_version: "0.28.0".to_string(),
        mods: mods(),
        turn_length_ms: 200,
    };
    writer::run(setup.clone(), meta, GAME_ID.to_string(), existing, rx);
}

fn stopped_match(setup: &SaveSetup, ai_players: Vec<i32>) -> PathBuf {
    let slots = SlotsSnapshot {
        players: vec![SavedPlayer {
            player_id: 1,
            uuid: "u".to_string(),
            name: "alice".to_string(),
            lobby_name: String::new(),
        }],
        ..SlotsSnapshot::default()
    };
    write(
        setup,
        None,
        vec![
            SaveItem::Started {
                now: DateTime::UNIX_EPOCH,
                settings: b"{\"settings\":{}}".to_vec(),
                ai_settings: None,
                ai_players,
            },
            SaveItem::Slots(slots),
            SaveItem::Turn {
                turn: 4,
                length: 200,
                commands: Vec::new(),
            },
            SaveItem::Hash {
                turn: 1,
                hash: vec![1; 16],
            },
            SaveItem::Checkpoint {
                turn: 4,
                state: Arc::new(vec![4, 4]),
            },
            SaveItem::Turn {
                turn: 5,
                length: 250,
                commands: Vec::new(),
            },
            SaveItem::Status {
                now: DateTime::UNIX_EPOCH,
                status: Status::Stopped,
            },
        ],
    );
    setup.root.join(GAME_ID)
}

fn manifest(dir: &Path) -> Manifest {
    bundle::read_json(&dir.join(bundle::MANIFEST)).unwrap()
}

#[test]
fn a_stopped_match_loads_whole() {
    let setup = setup();
    let dir = stopped_match(&setup, Vec::new());
    let resumable = load(&dir, &expect()).expect("resumable");
    assert_eq!(resumable.game_id, GAME_ID);
    let data = &resumable.data;
    assert_eq!(data.last_turn(), 5);
    assert_eq!(data.turns[1].length, 250);
    assert_eq!(data.hashes, vec![(1, vec![1; 16])]);
    assert_eq!(data.slots.players[0].name, "alice");
    assert_eq!(data.base.as_ref().map(|b| b.turn), Some(4));
    assert_eq!(manifest(&dir).resume_attempts, 1);
    assert!(matches!(load(&dir, &expect()), Err(Skip::Locked)));
    drop(resumable);
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn an_incompatible_match_is_left_untouched() {
    let setup = setup();
    let dir = stopped_match(&setup, Vec::new());
    let mut other_mods = expect();
    other_mods.mods.push(ModRecord {
        name: "extra".to_string(),
        version: "1".to_string(),
    });
    assert!(matches!(
        load(&dir, &other_mods),
        Err(Skip::Incompatible(_))
    ));
    let mut no_sidecar = expect();
    no_sidecar.sidecar = false;
    std::fs::remove_file(dir.join(bundle::STATE)).unwrap();
    assert!(matches!(load(&dir, &no_sidecar), Err(Skip::NoSidecar)));
    let mut lobby = expect();
    lobby.mode = Mode::Lobby;
    assert!(matches!(load(&dir, &lobby), Err(Skip::Incompatible(_))));
    let after = manifest(&dir);
    assert_eq!(after.resume_attempts, 0);
    assert_eq!(after.status, Status::Stopped);
    assert!(!dir.join(bundle::LOCK).exists());
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn a_match_with_hosted_ai_is_not_resumed_yet() {
    let setup = setup();
    let dir = stopped_match(&setup, vec![2]);
    assert!(matches!(load(&dir, &expect()), Err(Skip::Incompatible(_))));
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn too_many_attempts_abandon_it() {
    let setup = setup();
    let dir = stopped_match(&setup, Vec::new());
    let mut once = expect();
    once.max_attempts = 1;
    drop(load(&dir, &once).expect("first attempt"));
    assert!(matches!(load(&dir, &once), Err(Skip::Abandoned(1))));
    assert_eq!(manifest(&dir).status, Status::Abandoned);
    assert!(scan(&setup.root).is_empty());
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn a_torn_journal_tail_is_cut_off() {
    let setup = setup();
    let dir = stopped_match(&setup, Vec::new());
    let path = dir.join(bundle::JOURNAL);
    let whole = std::fs::read(&path).unwrap().len();
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    std::io::Write::write_all(&mut file, &[9, 0, 0, 0, 1, 2]).unwrap();
    drop(file);
    let resumable = load(&dir, &expect()).expect("resumable");
    assert_eq!(resumable.data.last_turn(), 5);
    assert_eq!(std::fs::read(&path).unwrap().len(), whole);
    drop(resumable);
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn a_state_that_does_not_match_its_turn_is_dropped() {
    let setup = setup();
    let dir = stopped_match(&setup, Vec::new());
    std::fs::write(dir.join(bundle::STATE), [7, 7, 7]).unwrap();
    let resumable = load(&dir, &expect()).expect("resumable");
    assert!(resumable.data.base.is_none());
    drop(resumable);
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn a_resumed_match_carries_on_in_the_same_journal() {
    let setup = setup();
    let dir = stopped_match(&setup, Vec::new());
    let resumable = load(&dir, &expect()).expect("resumable");
    write(
        &setup,
        Some(resumable.existing),
        vec![
            SaveItem::Resumed {
                now: DateTime::UNIX_EPOCH,
                turn: 5,
            },
            SaveItem::Turn {
                turn: 6,
                length: 200,
                commands: Vec::new(),
            },
        ],
    );
    let after = manifest(&dir);
    assert_eq!(after.status, Status::Running);
    assert_eq!(after.resume_attempts, 1);
    let again = load(&dir, &expect()).expect("still resumable after a crash");
    assert_eq!(again.data.last_turn(), 6);
    drop(again);
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn scan_finds_running_and_stopped_only() {
    let setup = setup();
    let dir = stopped_match(&setup, Vec::new());
    assert_eq!(scan(&setup.root), vec![dir.clone()]);
    let mut finished = manifest(&dir);
    finished.status = Status::Finished;
    bundle::write_json(&dir.join(bundle::MANIFEST), &finished).unwrap();
    assert!(scan(&setup.root).is_empty());
    assert!(scan(&setup.root.join("missing")).is_empty());
    std::fs::remove_dir_all(&setup.root).unwrap();
}

#[test]
fn without_a_sidecar_a_checkpoint_or_client_state_is_enough() {
    let setup = setup();
    let dir = stopped_match(&setup, Vec::new());
    let mut no_sidecar = expect();
    no_sidecar.sidecar = false;
    let resumable = load(&dir, &no_sidecar).expect("resumable from the checkpoint");
    assert!(resumable.data.seed.is_none());
    assert_eq!(resumable.data.base.as_ref().map(|b| b.turn), Some(4));
    drop(resumable);
    std::fs::remove_file(dir.join(bundle::STATE)).unwrap();
    assert!(matches!(load(&dir, &no_sidecar), Err(Skip::NoSidecar)));

    write(
        &setup,
        None,
        vec![
            SaveItem::Started {
                now: DateTime::UNIX_EPOCH,
                settings: b"{}".to_vec(),
                ai_settings: None,
                ai_players: Vec::new(),
            },
            SaveItem::ClientState {
                first: 3,
                last: 5,
                state: Arc::new(vec![8, 8]),
            },
        ],
    );
    let resumable = load(&dir, &no_sidecar).expect("resumable from the client state");
    let seed = resumable.data.seed.clone().expect("seed");
    assert_eq!(seed.turns, 3..=5);
    assert_eq!(*seed.state, vec![8, 8]);
    drop(resumable);

    std::fs::write(dir.join(bundle::CLIENT_STATE), [1]).unwrap();
    assert!(matches!(load(&dir, &no_sidecar), Err(Skip::NoSidecar)));
    std::fs::remove_dir_all(&setup.root).unwrap();
}
