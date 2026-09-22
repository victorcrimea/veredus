// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct StartSavegameSettings {
    pub init_attributes: String,
}

impl StartSavegameSettings {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let attr_bytes = self.init_attributes.as_bytes();
        bytes.extend_from_slice(&(attr_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(attr_bytes);

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
        let init_attributes =
            String::from_utf8(buffer[pos..pos + attr_len].to_vec()).map_err(|_| {
                ParseError::BadUtf8 {
                    field: "init_attributes",
                }
            })?;

        Ok(Self { init_attributes })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/start_savegame_settings.rs"]
mod tests;
