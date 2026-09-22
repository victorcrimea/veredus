// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct TurnSealed {
    pub turn: u32,
    pub turn_length: u16,
}

impl TurnSealed {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.turn.to_be_bytes());
        bytes.extend_from_slice(&self.turn_length.to_be_bytes());

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

        if buffer.len() < pos + 2 {
            return Err(ParseError::Truncated {
                field: "turn_length",
            });
        }
        let turn_length = u16::from_be_bytes([buffer[pos], buffer[pos + 1]]);

        Ok(Self { turn, turn_length })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/turn_sealed.rs"]
mod tests;
