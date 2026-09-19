// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, PartialEq)]
pub struct GamestateResponse {
    pub request_id: u32,
    pub length: u32,
}

impl GamestateResponse {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.request_id.to_be_bytes());
        bytes.extend_from_slice(&self.length.to_be_bytes());

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, String> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for request_id".into());
        }
        let request_id = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for length".into());
        }
        let length = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);

        Ok(Self { request_id, length })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_file_transfer_response() {
        let msg = GamestateResponse {
            request_id: 7,
            length: 1024,
        };
        let bytes = msg.to_bytes();
        let decoded = GamestateResponse::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
