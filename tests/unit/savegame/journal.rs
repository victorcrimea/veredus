// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn command(turn: u32, data: &[u8]) -> PlayerCommand {
    PlayerCommand {
        client: 2,
        player: 1,
        turn,
        data: data.to_vec(),
    }
}

fn sample() -> Vec<Record> {
    vec![
        Record::Turn {
            turn: 4,
            length: 200,
            commands: vec![command(4, b"abc"), command(4, b"")],
        },
        Record::Turn {
            turn: 5,
            length: 500,
            commands: Vec::new(),
        },
        Record::Hash {
            turn: 1,
            hash: vec![7; 16],
        },
        Record::Stopped {
            turn: 5,
            unix_ms: 1_700_000_000_000,
        },
        Record::Resumed {
            turn: 5,
            unix_ms: -1,
        },
    ]
}

fn encode(records: &[Record]) -> Vec<u8> {
    records.iter().flat_map(Record::to_bytes).collect()
}

#[test]
fn every_kind_roundtrips() {
    let bytes = encode(&sample());
    let (records, used) = read(&bytes);
    assert_eq!(records, sample());
    assert_eq!(used, bytes.len());
}

#[test]
fn empty_journal_reads_nothing() {
    assert_eq!(read(&[]), (Vec::new(), 0));
}

#[test]
fn torn_tail_is_cut_at_the_last_whole_record() {
    let records = sample();
    let whole = encode(&records[..2]);
    let mut bytes = whole.clone();
    let last = records[2].to_bytes();
    bytes.extend_from_slice(&last[..last.len() - 1]);
    let (read_back, used) = read(&bytes);
    assert_eq!(read_back, records[..2].to_vec());
    assert_eq!(used, whole.len());
}

#[test]
fn bad_checksum_stops_reading_there() {
    let records = sample();
    let first = records[0].to_bytes();
    let mut bytes = first.clone();
    let mut second = records[1].to_bytes();
    second[5] ^= 0xFF;
    bytes.extend_from_slice(&second);
    bytes.extend_from_slice(&records[2].to_bytes());
    let (read_back, used) = read(&bytes);
    assert_eq!(read_back, vec![records[0].clone()]);
    assert_eq!(used, first.len());
}

#[test]
fn huge_declared_length_is_a_torn_tail() {
    let mut bytes = encode(&sample()[..1]);
    let used = bytes.len();
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(&[1, 2, 3]);
    assert_eq!(read(&bytes).1, used);
}
