// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::guid::Guid;
use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct LastSeen {
    pub guid: Guid,
    pub last_received_time: u32,
}

impl LastSeen {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let guid_string = self.guid.to_string();
        let guid_bytes = guid_string.as_bytes();
        bytes.extend_from_slice(&(guid_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(guid_bytes);

        bytes.extend_from_slice(&self.last_received_time.to_be_bytes());

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "guid length",
            });
        }
        let guid_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + guid_len {
            return Err(ParseError::Truncated { field: "guid data" });
        }
        let guid = Guid(
            String::from_utf8(buffer[pos..pos + guid_len].to_vec())
                .map_err(|_| ParseError::BadUtf8 { field: "guid" })?,
        );
        pos += guid_len;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "last_received_time",
            });
        }
        let last_received_time = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);

        Ok(Self {
            guid,
            last_received_time,
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/last_seen.rs"]
mod tests;
