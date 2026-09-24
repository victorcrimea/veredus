// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "veredus-bundle-{name}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn manifest_roundtrips() {
    let dir = temp_dir("manifest");
    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        game_id: "gid_x".to_string(),
        created_unix_ms: 5,
        engine_version: "0.28.0".to_string(),
        mods: vec![ModRecord {
            name: "0ad".to_string(),
            version: "0.28.0".to_string(),
        }],
        mode: Mode::Standalone,
        lobby_account: String::new(),
        lobby_host_name: String::new(),
        controller: Some(SavedIdentity {
            name: "alice".to_string(),
            lobby_name: String::new(),
        }),
        status: Status::Stopped,
        resume_attempts: 2,
        turn_length_ms: 200,
        ai_players: vec![2],
        lobby_map: None,
    };
    let path = dir.join(MANIFEST);
    write_json(&path, &manifest).unwrap();
    let back: Manifest = read_json(&path).unwrap();
    assert_eq!(back, manifest);
    assert!(!dir.join("manifest.json.tmp").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn state_meta_checks_its_state() {
    let meta = StateMeta::of(40, b"state");
    assert!(meta.matches(b"state"));
    assert!(!meta.matches(b"other"));
}

#[test]
fn a_live_lock_is_refused_and_released_on_drop() {
    let dir = temp_dir("live");
    let lock = Lock::acquire(&dir).unwrap().expect("first acquire");
    assert!(Lock::acquire(&dir).unwrap().is_none());
    drop(lock);
    assert!(!dir.join(LOCK).exists());
    assert!(Lock::acquire(&dir).unwrap().is_some());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_stale_lock_is_taken_over() {
    let dir = temp_dir("stale");
    // Our own pid with a start time it never had names a process that is gone.
    let stale = LockOwner {
        pid: std::process::id(),
        start_time: 1,
    };
    std::fs::write(dir.join(LOCK), serde_json::to_vec(&stale).unwrap()).unwrap();
    assert!(Lock::acquire(&dir).unwrap().is_some());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_unreadable_lock_counts_as_held() {
    let dir = temp_dir("garbage");
    std::fs::write(dir.join(LOCK), b"not json").unwrap();
    assert!(Lock::acquire(&dir).unwrap().is_none());
    std::fs::remove_dir_all(&dir).unwrap();
}
