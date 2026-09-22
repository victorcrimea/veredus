// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::enabled_mod::EnabledMod;
use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct Syn {
    pub magic: u32,
    pub protocol_version: u32,
    pub engine_version: String,
    pub enabled_mods: Vec<EnabledMod>,
}

impl Syn {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.magic.to_be_bytes());
        bytes.extend_from_slice(&self.protocol_version.to_be_bytes());

        let engine_bytes = self.engine_version.as_bytes();
        bytes.extend_from_slice(&(engine_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(engine_bytes);

        // Write mods data (array, NO count prefix)
        for mod_item in &self.enabled_mods {
            bytes.extend(mod_item.to_bytes());
        }

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated { field: "magic" });
        }
        let magic = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "protocol version",
            });
        }
        let protocol_version = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "engine version length",
            });
        }
        let engine_version_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + engine_version_len {
            return Err(ParseError::Truncated {
                field: "engine version data",
            });
        }
        let engine_version = String::from_utf8(buffer[pos..pos + engine_version_len].to_vec())
            .map_err(|_| ParseError::BadUtf8 {
                field: "engine version",
            })?;
        pos += engine_version_len;

        let mut enabled_mods = Vec::new();
        while pos < buffer.len() {
            let (mod_item, bytes_read) = EnabledMod::from_bytes(&buffer[pos..])?;
            enabled_mods.push(mod_item);
            pos += bytes_read;
        }

        Ok(Self {
            magic,
            protocol_version,
            engine_version,
            enabled_mods,
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/syn.rs"]
mod tests;
