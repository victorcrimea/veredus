// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, PartialEq)]
pub struct GamestateChunk {
    pub request_id: u32,
    pub data: Vec<u8>,
}

impl GamestateChunk {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.request_id.to_be_bytes());
        bytes.extend_from_slice(&(self.data.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&self.data);

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
            return Err("Buffer too short for data length".into());
        }
        let data_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + data_len {
            return Err("Buffer too short for data".into());
        }
        let data = buffer[pos..pos + data_len].to_vec();

        Ok(Self { request_id, data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_file_transfer_data() {
        let msg = GamestateChunk {
            request_id: 5,
            data: vec![10, 20, 30],
        };
        let bytes = msg.to_bytes();
        let decoded = GamestateChunk::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
