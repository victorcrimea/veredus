// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct StateHash {
    pub turn: u32,
    pub hash: [u8; 16],
}

impl StateHash {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.turn.to_be_bytes());

        let hash_bytes = &self.hash;
        bytes.extend_from_slice(&(hash_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(hash_bytes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated { field: "turn" });
        }
        let turn = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "hash length",
            });
        }
        let hash_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if hash_len != 16 {
            return Err(ParseError::WrongSize {
                field: "hash",
                should_be: 16,
                declared: hash_len,
            });
        }

        let hash: [u8; 16] = if buffer.len() < pos + hash_len {
            return Err(ParseError::Truncated { field: "hash data" });
        } else {
            buffer[pos..pos + 16].try_into().unwrap()
        };

        Ok(Self { turn, hash })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/state_hash.rs"]
mod tests;
