// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::guid::Guid;

#[derive(Debug, PartialEq)]
pub struct PerformanceEntry {
    pub guid: Guid,
    pub mean_rtt: u32,
}

impl PerformanceEntry {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let guid_string = self.guid.to_string();
        let guid_bytes = guid_string.as_bytes();
        bytes.extend_from_slice(&(guid_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(guid_bytes);

        bytes.extend_from_slice(&self.mean_rtt.to_be_bytes());

        bytes
    }

    pub fn from_bytes(buffer: &[u8], pos: usize) -> Result<(Self, usize), String> {
        let mut pos = pos;

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

        if buffer.len() < pos + 4 {
            return Err("Buffer too short for mean_rtt".into());
        }
        let mean_rtt = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        Ok((Self { guid, mean_rtt }, pos))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_performance_entry() {
        let msg = PerformanceEntry {
            guid: Guid("abc123".to_string()),
            mean_rtt: 50,
        };
        let bytes = msg.to_bytes();
        let (decoded, pos) = PerformanceEntry::from_bytes(&bytes, 0).unwrap();
        assert_eq!(decoded, msg);
        assert_eq!(pos, bytes.len());
    }
}
