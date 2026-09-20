// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::guid::Guid;
use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct Ack {
    pub use_protocol_version: u32,
    pub flags: u32,
    pub guid: Guid,
}

impl Ack {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.use_protocol_version.to_be_bytes());
        bytes.extend_from_slice(&self.flags.to_be_bytes());

        let guid_string = self.guid.to_string();
        let guid_string_bytes = guid_string.as_bytes();
        bytes.extend_from_slice(&(guid_string_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(guid_string_bytes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "use_protocol_version",
            });
        }
        let use_protocol_version = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated { field: "flags" });
        }
        let flags = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

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
        let guid_str = String::from_utf8(buffer[pos..pos + guid_len].to_vec())
            .map_err(|_| ParseError::BadUtf8 { field: "guid" })?;

        tracing::trace!(guid = %guid_str, "deserialized Ack guid");

        let guid = Guid(guid_str);

        Ok(Self {
            use_protocol_version,
            flags,
            guid,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_server_handshake_response() {
        let msg = Ack {
            use_protocol_version: 1,
            flags: 0,
            guid: Guid("abc123".to_string()),
        };
        let bytes = msg.to_bytes();
        let decoded = Ack::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
