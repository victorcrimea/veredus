// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use blake2::Blake2b128;
use blake2::Digest;

use crate::relay::messages::PlayerCommand;

const KIND_TURN: u8 = 1;
const KIND_HASH: u8 = 2;
const KIND_RESUMED: u8 = 3;
const KIND_STOPPED: u8 = 4;

const LEN_BYTES: usize = 4;
const CHECKSUM_BYTES: usize = 16;

// One event of the match, in the order it happened. A turn is journaled when
// it is released, which is when its commands are complete; its hash follows
// once the players agree on it.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    Turn {
        turn: u32,
        length: u16,
        commands: Vec<PlayerCommand>,
    },
    Hash {
        turn: u32,
        hash: Vec<u8>,
    },
    Resumed {
        turn: u32,
        unix_ms: i64,
    },
    Stopped {
        turn: u32,
        unix_ms: i64,
    },
}

impl Record {
    // The whole framed record, ready to append: a crash can leave only the
    // last one half written, and the checksum is how a reader tells.
    pub fn to_bytes(&self) -> Vec<u8> {
        let body = self.body();
        let mut out = Vec::with_capacity(LEN_BYTES + body.len() + CHECKSUM_BYTES);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out.extend_from_slice(&checksum(&body));
        out
    }

    fn body(&self) -> Vec<u8> {
        let mut body = Vec::new();
        match self {
            Record::Turn {
                turn,
                length,
                commands,
            } => {
                body.push(KIND_TURN);
                body.extend_from_slice(&turn.to_le_bytes());
                body.extend_from_slice(&length.to_le_bytes());
                // A turn never holds more commands than the per-turn cap,
                // which is far below this.
                body.extend_from_slice(
                    &(commands.len().min(u16::MAX as usize) as u16).to_le_bytes(),
                );
                for command in commands.iter().take(u16::MAX as usize) {
                    let bytes = command.to_bytes();
                    body.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
                    body.extend_from_slice(&bytes);
                }
            }
            Record::Hash { turn, hash } => {
                body.push(KIND_HASH);
                body.extend_from_slice(&turn.to_le_bytes());
                let len = hash.len().min(u8::MAX as usize);
                body.push(len as u8);
                body.extend_from_slice(&hash[..len]);
            }
            Record::Resumed { turn, unix_ms } => {
                body.push(KIND_RESUMED);
                body.extend_from_slice(&turn.to_le_bytes());
                body.extend_from_slice(&unix_ms.to_le_bytes());
            }
            Record::Stopped { turn, unix_ms } => {
                body.push(KIND_STOPPED);
                body.extend_from_slice(&turn.to_le_bytes());
                body.extend_from_slice(&unix_ms.to_le_bytes());
            }
        }
        body
    }

    // None for anything that is not exactly a record this code writes: the
    // body already passed its checksum, so that can only be a different
    // format, and the reader treats it like a torn tail.
    fn from_body(body: &[u8]) -> Option<Record> {
        let mut reader = Reader { buf: body, pos: 0 };
        let kind = reader.u8()?;
        let record = match kind {
            KIND_TURN => {
                let turn = reader.u32()?;
                let length = reader.u16()?;
                let count = reader.u16()?;
                let mut commands = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    let len = reader.u32()? as usize;
                    let bytes = reader.take(len)?;
                    commands.push(PlayerCommand::from_bytes(bytes).ok()?);
                }
                Record::Turn {
                    turn,
                    length,
                    commands,
                }
            }
            KIND_HASH => {
                let turn = reader.u32()?;
                let len = reader.u8()? as usize;
                let hash = reader.take(len)?.to_vec();
                Record::Hash { turn, hash }
            }
            KIND_RESUMED => Record::Resumed {
                turn: reader.u32()?,
                unix_ms: reader.i64()?,
            },
            KIND_STOPPED => Record::Stopped {
                turn: reader.u32()?,
                unix_ms: reader.i64()?,
            },
            _ => return None,
        };
        (reader.pos == body.len()).then_some(record)
    }
}

// Every whole record from the start of `bytes`, and how many bytes they
// take. Reading stops at the first record that is short or fails its
// checksum: that is the torn tail of a crash, and nothing after it can be
// trusted to line up.
pub fn read(bytes: &[u8]) -> (Vec<Record>, usize) {
    let mut records = Vec::new();
    let mut pos = 0;
    while let Some((record, used)) = read_one(&bytes[pos..]) {
        records.push(record);
        pos += used;
    }
    (records, pos)
}

fn read_one(bytes: &[u8]) -> Option<(Record, usize)> {
    let len = u32::from_le_bytes(bytes.get(..LEN_BYTES)?.try_into().ok()?) as usize;
    let end = LEN_BYTES.checked_add(len)?;
    let body = bytes.get(LEN_BYTES..end)?;
    let sum = bytes.get(end..end.checked_add(CHECKSUM_BYTES)?)?;
    if sum != checksum(body).as_slice() {
        return None;
    }
    let record = Record::from_body(body)?;
    Some((record, end + CHECKSUM_BYTES))
}

fn checksum(body: &[u8]) -> [u8; CHECKSUM_BYTES] {
    Blake2b128::digest(body).into()
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let out = self.buf.get(self.pos..end)?;
        self.pos = end;
        Some(out)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }

    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
}

#[cfg(test)]
#[path = "../../tests/unit/savegame/journal.rs"]
mod tests;
