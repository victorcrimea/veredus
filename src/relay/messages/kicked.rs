// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct Kicked {
    pub name: String,
    pub ban: bool,
}

impl Kicked {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&crate::utils::write_wide_string(&self.name));

        bytes.push(self.ban as u8);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        let (name, new_pos) = crate::utils::read_wide_string(buffer, pos)?;
        pos = new_pos;

        if buffer.len() < pos + 1 {
            return Err(ParseError::Truncated { field: "ban" });
        }
        let ban = buffer[pos] != 0;

        Ok(Self { name, ban })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/kicked.rs"]
mod tests;
