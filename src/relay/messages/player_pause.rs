// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::guid::Guid;

#[derive(Debug, PartialEq)]
pub struct PlayerPause {
    pub guid: Guid,
    pub pause: bool,
}

impl PlayerPause {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let guid_string = self.guid.to_string();
        let guid_bytes = guid_string.as_bytes();
        bytes.extend_from_slice(&(guid_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(guid_bytes);

        bytes.push(self.pause as u8);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, String> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for guid length".into());
        }
        let guid_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + guid_len {
            return Err("Buffer too short for guid data".into());
        }
        let guid = Guid(
            String::from_utf8(buffer[pos..pos + guid_len].to_vec())
                .map_err(|e| format!("Invalid UTF-8 in guid: {}", e))?,
        );
        pos += guid_len;

        if buffer.len() < pos + 1 {
            return Err("Buffer too short for pause".into());
        }
        let pause = buffer[pos] != 0;

        Ok(Self { guid, pause })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_client_paused() {
        let msg = PlayerPause {
            guid: Guid("abc123".to_string()),
            pause: true,
        };
        let bytes = msg.to_bytes();
        let decoded = PlayerPause::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
