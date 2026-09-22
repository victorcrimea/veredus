// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct Join {
    pub init_attributes: Vec<u8>,
}

impl Join {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&(self.init_attributes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&self.init_attributes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "init_attributes length",
            });
        }
        let attr_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + attr_len {
            return Err(ParseError::Truncated {
                field: "init_attributes data",
            });
        }
        let init_attributes = buffer[pos..pos + attr_len].to_vec();

        Ok(Self { init_attributes })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/join.rs"]
mod tests;
