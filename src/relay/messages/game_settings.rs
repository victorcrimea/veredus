// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, PartialEq)]
pub struct GameSettings {
    pub data: Vec<u8>,
}

impl GameSettings {
    pub fn to_bytes(&self) -> Vec<u8> {
        self.data.clone()
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, String> {
        Ok(Self {
            data: buffer.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_game_setup() {
        let msg = GameSettings {
            data: vec![1, 2, 3, 4],
        };
        let bytes = msg.to_bytes();
        let decoded = GameSettings::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
