// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, PartialEq)]
pub struct GamestateChunkAck {
    pub request_id: u32,
    pub num_packets: u32,
}

impl GamestateChunkAck {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.request_id.to_be_bytes());
        bytes.extend_from_slice(&self.num_packets.to_be_bytes());

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
            return Err("Buffer too short for num_packets".into());
        }
        let num_packets = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);

        Ok(Self {
            request_id,
            num_packets,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_file_transfer_ack() {
        let msg = GamestateChunkAck {
            request_id: 7,
            num_packets: 3,
        };
        let bytes = msg.to_bytes();
        let decoded = GamestateChunkAck::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
