// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;
use crate::utils::read_wide_string;
use crate::utils::write_wide_string;

#[derive(Debug, PartialEq)]
pub struct WrongHashPlayers {
    pub turn: u32,
    pub hash_expected: Vec<u8>,
    pub player_names: Vec<String>,
}

impl WrongHashPlayers {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.turn.to_be_bytes());

        let hash_bytes = &self.hash_expected;
        bytes.extend_from_slice(&(hash_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(hash_bytes);

        for name in &self.player_names {
            bytes.extend(write_wide_string(name));
        }

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
                field: "hash_expected length",
            });
        }
        let hash_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + hash_len {
            return Err(ParseError::Truncated {
                field: "hash_expected data",
            });
        }
        let hash_expected = buffer[pos..pos + hash_len].to_vec();
        pos += hash_len;

        let mut player_names = Vec::new();
        while pos < buffer.len() {
            let (name, new_pos) = read_wide_string(buffer, pos)?;
            player_names.push(name);
            pos = new_pos;
        }

        Ok(Self {
            turn,
            hash_expected,
            player_names,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_sync_error() {
        let msg = WrongHashPlayers {
            turn: 10,
            hash_expected: vec![0xAA],
            player_names: vec!["Alice".to_string(), "Bob".to_string()],
        };
        let bytes = msg.to_bytes();
        let decoded = WrongHashPlayers::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_sync_error_empty_names() {
        let msg = WrongHashPlayers {
            turn: 10,
            hash_expected: vec![0xAA],
            player_names: vec![],
        };
        let bytes = msg.to_bytes();
        let decoded = WrongHashPlayers::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
