// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real_messages")
}

fn game_settings_fixtures() -> Vec<ScriptValue> {
    let mut out = Vec::new();
    for entry in fs::read_dir(fixtures_dir()).expect("read fixtures dir") {
        let path = entry.expect("read fixture entry").path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !stem.starts_with("GameSettings_") {
            continue;
        }
        let contents = fs::read_to_string(&path).expect("read fixture");
        let bytes = hex::decode(contents.trim()).expect("hex-decode fixture");
        // Strip the 3-byte message header (type + BE size); GAME_SETTINGS
        // carries a script value with no envelope of its own (Sec. 4.3).
        let body = &bytes[3..];
        let value = decode(body).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        out.push(value);
    }
    assert!(!out.is_empty(), "no GameSettings fixtures found");
    out
}

#[test]
fn every_game_settings_fixture_decodes_fully() {
    // decode() already rejects trailing bytes, so a successful call here
    // means the whole fixture was consumed.
    game_settings_fixtures();
}

#[test]
fn fixture_settings_have_expected_shape() {
    for value in game_settings_fixtures() {
        let init_attribs = value.get("initAttribs").expect("initAttribs");
        let map = init_attribs.get("map").and_then(|v| v.as_str());
        assert!(map.is_some_and(|m| !m.is_empty()), "map name present");
        let settings = init_attribs.get("settings").expect("settings");
        let player_data = settings.get("PlayerData").expect("PlayerData");
        assert!(player_data.array_len().unwrap() >= 2);
        let victory = settings
            .get("VictoryConditions")
            .expect("VictoryConditions");
        assert_eq!(
            victory.array_get(0).and_then(|v| v.as_str()),
            Some("conquest")
        );
    }
}

#[test]
fn decodes_utf16_string() {
    // "Hi" as UTF-16LE, wrapped in the STRING tag.
    let mut bytes = vec![TAG_STRING, 0, 2, 0, 0, 0];
    bytes.extend_from_slice(&[b'H', 0, b'i', 0]);
    assert_eq!(decode(&bytes), Ok(ScriptValue::String("Hi".to_string())));
}

#[test]
fn resolves_a_backref_to_a_finished_object() {
    // { a: [], b: PRIOR_OBJECT(ref of `a`) }. The object is ref 1 and the
    // array is ref 2 (Sec. 5 numbers arrays and objects before their
    // properties), so `b` names an already-finished value, not a cycle.
    fn latin1_key(byte: u8) -> Vec<u8> {
        let mut key = vec![1u8];
        key.extend_from_slice(&1u32.to_le_bytes());
        key.push(byte);
        key
    }
    let mut bytes = vec![TAG_OBJECT];
    bytes.extend_from_slice(&2u32.to_le_bytes()); // 2 props
    bytes.extend(latin1_key(b'a'));
    bytes.push(TAG_ARRAY);
    bytes.extend_from_slice(&0u32.to_le_bytes()); // arrayLength
    bytes.extend_from_slice(&0u32.to_le_bytes()); // 0 props
    bytes.extend(latin1_key(b'b'));
    bytes.push(TAG_PRIOR_OBJECT);
    bytes.extend_from_slice(&2u32.to_le_bytes());

    let decoded = decode(&bytes).unwrap();
    let expected_array = ScriptValue::Array {
        length: 0,
        props: vec![],
    };
    assert_eq!(
        decoded,
        ScriptValue::Object(vec![
            ("a".to_string(), expected_array.clone()),
            ("b".to_string(), expected_array),
        ])
    );
}

#[test]
fn rejects_self_referential_cycle() {
    // A single-prop array whose element is PRIOR_OBJECT(1), naming
    // itself while still open. Structured clone cannot produce this for
    // container tags, so it is a corrupt or hostile packet.
    let mut bytes = vec![TAG_ARRAY];
    bytes.extend_from_slice(&1u32.to_le_bytes()); // arrayLength
    bytes.extend_from_slice(&1u32.to_le_bytes()); // 1 prop
    bytes.push(1); // latin1
    bytes.extend_from_slice(&1u32.to_le_bytes());
    bytes.push(b'0');
    bytes.push(TAG_PRIOR_OBJECT);
    bytes.extend_from_slice(&1u32.to_le_bytes());
    assert_eq!(decode(&bytes), Err(ScriptValueError::BadBackref(1)));
}

#[test]
fn rejects_bad_boolean() {
    let bytes = vec![TAG_BOOLEAN, 2];
    assert_eq!(decode(&bytes), Err(ScriptValueError::BadBoolean(2)));
}

#[test]
fn rejects_unresolved_backref() {
    let mut bytes = vec![TAG_PRIOR_OBJECT];
    bytes.extend_from_slice(&5u32.to_le_bytes());
    assert_eq!(decode(&bytes), Err(ScriptValueError::BadBackref(5)));
}

#[test]
fn rejects_object_prototype() {
    let bytes = vec![TAG_OBJECT_PROTOTYPE];
    assert_eq!(
        decode(&bytes),
        Err(ScriptValueError::Unsupported(TAG_OBJECT_PROTOTYPE))
    );
}

#[test]
fn rejects_truncated_input() {
    let bytes = vec![TAG_INT, 0, 0];
    assert_eq!(decode(&bytes), Err(ScriptValueError::Truncated));
}

#[test]
fn rejects_trailing_bytes() {
    let bytes = vec![TAG_VOID, 0xff];
    assert_eq!(decode(&bytes), Err(ScriptValueError::TrailingBytes(1)));
}

#[test]
fn rejects_excess_depth() {
    // MAX_DEPTH+2 nested single-element arrays: each array's one prop is
    // itself an array, so this is a linear chain rather than a
    // fan-out, and cheap to build for the test.
    let mut bytes = Vec::new();
    for _ in 0..MAX_DEPTH + 2 {
        bytes.push(TAG_ARRAY);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(1);
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.push(b'0');
    }
    bytes.push(TAG_VOID);
    assert_eq!(decode(&bytes), Err(ScriptValueError::TooDeep));
}
