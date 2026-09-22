// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct EnabledMod {
    pub name: String,
    pub version: String,
}

impl EnabledMod {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let name_bytes = self.name.as_bytes();
        bytes.extend_from_slice(&(name_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(name_bytes);

        let version_bytes = self.version.as_bytes();
        bytes.extend_from_slice(&(version_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(version_bytes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<(Self, usize), ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "name length",
            });
        }
        let name_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + name_len {
            return Err(ParseError::Truncated { field: "name data" });
        }
        let name = String::from_utf8(buffer[pos..pos + name_len].to_vec())
            .map_err(|_| ParseError::BadUtf8 { field: "name" })?;
        pos += name_len;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "version length",
            });
        }
        let version_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + version_len {
            return Err(ParseError::Truncated {
                field: "version data",
            });
        }
        let version = String::from_utf8(buffer[pos..pos + version_len].to_vec())
            .map_err(|_| ParseError::BadUtf8 { field: "version" })?;
        pos += version_len;

        Ok((Self { name, version }, pos))
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/enabled_mod.rs"]
mod tests;
