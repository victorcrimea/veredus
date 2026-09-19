// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::fs;
use std::path::Path;
use std::path::PathBuf;
use veredus::relay::messages::WireMessage;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/real_messages")
}

// Fixture files are named "<WireMessage variant name>_<3-digit index>_<8-char
// hash>.hex" (see WireMessage::name() in src/relay/messages/mod.rs); strip the
// trailing index/hash segments to recover the variant name to check against.
fn expected_name_from_filename(stem: &str) -> String {
    let mut parts: Vec<&str> = stem.split('_').collect();
    parts.truncate(parts.len().saturating_sub(2));
    parts.join("_")
}

fn load_fixtures() -> Vec<(String, Vec<u8>)> {
    let mut fixtures = Vec::new();
    for entry in fs::read_dir(fixtures_dir()).expect("failed to read fixtures dir") {
        let entry = entry.expect("failed to read fixture dir entry");
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("hex") {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("fixture filename is not valid UTF-8")
            .to_string();
        let contents = fs::read_to_string(&path).expect("failed to read fixture file");
        let bytes = hex::decode(contents.trim())
            .unwrap_or_else(|e| panic!("failed to hex-decode {}: {}", path.display(), e));
        fixtures.push((stem, bytes));
    }
    fixtures.sort_by(|a, b| a.0.cmp(&b.0));
    fixtures
}

#[test]
fn real_message_fixtures_parse_and_roundtrip() {
    let fixtures = load_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no fixtures found in {:?}",
        fixtures_dir()
    );

    let mut failures = Vec::new();
    for (stem, bytes) in &fixtures {
        let expected_name = expected_name_from_filename(stem);

        let parsed = match WireMessage::from_bytes(bytes) {
            Ok(parsed) => parsed,
            Err(e) => {
                failures.push(format!("{stem}: from_bytes failed: {e}"));
                continue;
            }
        };

        if parsed.name() != expected_name {
            failures.push(format!(
                "{stem}: parsed as {}, expected {}",
                parsed.name(),
                expected_name
            ));
            continue;
        }

        let roundtripped = parsed.to_bytes();
        if &roundtripped != bytes {
            failures.push(format!(
                "{stem}: roundtrip mismatch, original {} bytes, roundtripped {} bytes",
                bytes.len(),
                roundtripped.len()
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} fixtures failed:\n{}",
        failures.len(),
        fixtures.len(),
        failures.join("\n")
    );
}

#[test]
fn all_wire_variants_have_a_fixture() {
    // These three are internal-only and never appear on the wire (see
    // WireMessage::id() in src/relay/messages/mod.rs), so they can never have a
    // captured fixture.
    const NEVER_SERIALIZED: &[&str] = &["ConnectComplete", "ConnectionLost", "Invalid"];

    let fixtures = load_fixtures();
    let covered: std::collections::HashSet<String> = fixtures
        .iter()
        .map(|(stem, _)| expected_name_from_filename(stem))
        .collect();

    let missing: Vec<&str> = WireMessage::ALL_NAMES
        .iter()
        .copied()
        .filter(|name| !NEVER_SERIALIZED.contains(name) && !covered.contains(*name))
        .collect();

    assert!(
        missing.is_empty(),
        "wire message types with no real-world fixture: {:?}",
        missing
    );
}
