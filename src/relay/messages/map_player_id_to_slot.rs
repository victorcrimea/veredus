// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::guid::Guid;
use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct MapPlayerIdToSlot {
    pub player_id: i8,
    pub guid: Guid,
}

impl MapPlayerIdToSlot {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.push(self.player_id as u8);

        let guid_string = self.guid.to_string();
        let guid_bytes = guid_string.as_bytes();
        bytes.extend_from_slice(&(guid_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(guid_bytes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 1 {
            return Err(ParseError::Truncated { field: "player_id" });
        }
        let player_id = buffer[pos] as i8;
        pos += 1;

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

        Ok(Self { player_id, guid })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_assign_player() {
        let msg = MapPlayerIdToSlot {
            player_id: 2,
            guid: Guid("abc123".to_string()),
        };
        let bytes = msg.to_bytes();
        let decoded = MapPlayerIdToSlot::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
