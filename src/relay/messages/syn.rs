// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::enabled_mod::EnabledMod;

#[derive(Debug, PartialEq)]
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

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, String> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for magic".into());
        }
        let magic = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for protocol version".into());
        }
        let protocol_version = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for engine version length".into());
        }
        let engine_version_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + engine_version_len {
            return Err("Buffer too short for engine version data".into());
        }
        let engine_version = String::from_utf8(buffer[pos..pos + engine_version_len].to_vec())
            .map_err(|e| format!("Invalid UTF-8 in engine version: {}", e))?;
        pos += engine_version_len;

        let mut enabled_mods = Vec::new();
        while pos < buffer.len() {
            let (mod_item, bytes_read) = EnabledMod::from_bytes(&buffer[pos..])
                .map_err(|e| format!("Failed to read mod: {}", e))?;
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
mod tests {
    use super::*;

    #[test]
    fn roundtrip_server_handshake() {
        let msg = Syn {
            magic: 0x12345678,
            protocol_version: 1,
            engine_version: "0.28.0".to_string(),
            enabled_mods: vec![EnabledMod {
                name: "public".to_string(),
                version: "0.28.0".to_string(),
            }],
        };
        let bytes = msg.to_bytes();
        let decoded = Syn::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_server_handshake_empty_mods() {
        let msg = Syn {
            magic: 0x12345678,
            protocol_version: 1,
            engine_version: "0.28.0".to_string(),
            enabled_mods: vec![],
        };
        let bytes = msg.to_bytes();
        let decoded = Syn::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
