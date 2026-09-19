// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, PartialEq)]
pub struct LoadedGame {
    pub current_turn: u32,
}

impl LoadedGame {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.current_turn.to_be_bytes());

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, String> {
        let pos = 0;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for current_turn".into());
        }
        let current_turn = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);

        Ok(Self { current_turn })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_loaded_game() {
        let msg = LoadedGame { current_turn: 42 };
        let bytes = msg.to_bytes();
        let decoded = LoadedGame::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
