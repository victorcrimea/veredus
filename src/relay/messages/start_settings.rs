// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

#[derive(Debug, PartialEq)]
pub struct StartSettings {
    pub init_attributes: Vec<u8>,
}

impl StartSettings {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&(self.init_attributes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&self.init_attributes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, String> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for init_attributes length".into());
        }
        let attr_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + attr_len {
            return Err("Buffer too short for init_attributes data".into());
        }
        let init_attributes = buffer[pos..pos + attr_len].to_vec();

        Ok(Self { init_attributes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_game_start() {
        let msg = StartSettings {
            init_attributes: vec![0xDE, 0xAD],
        };
        let bytes = msg.to_bytes();
        let decoded = StartSettings::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
