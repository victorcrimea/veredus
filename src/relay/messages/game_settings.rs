// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct GameSettings {
    pub data: Vec<u8>,
}

impl GameSettings {
    pub fn to_bytes(&self) -> Vec<u8> {
        self.data.clone()
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        Ok(Self {
            data: buffer.to_vec(),
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/game_settings.rs"]
mod tests;
