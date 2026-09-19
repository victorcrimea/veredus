// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

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

    pub fn from_bytes(buffer: &[u8]) -> Result<(Self, usize), String> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for name length".into());
        }
        let name_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + name_len {
            return Err("Buffer too short for name data".into());
        }
        let name = String::from_utf8(buffer[pos..pos + name_len].to_vec())
            .map_err(|e| format!("Invalid UTF-8 in name: {}", e))?;
        pos += name_len;

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for version length".into());
        }
        let version_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + version_len {
            return Err("Buffer too short for version data".into());
        }
        let version = String::from_utf8(buffer[pos..pos + version_len].to_vec())
            .map_err(|e| format!("Invalid UTF-8 in version: {}", e))?;
        pos += version_len;

        Ok((Self { name, version }, pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_enabled_mod() {
        let msg = EnabledMod {
            name: "public".to_string(),
            version: "0.28.0".to_string(),
        };
        let bytes = msg.to_bytes();
        let (decoded, bytes_consumed) = EnabledMod::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(bytes_consumed, bytes.len());
    }
}
