// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::guid::Guid;
use crate::relay::fault::ParseError;
use crate::utils::read_wide_string;
use crate::utils::write_wide_string;

#[derive(Debug, Clone, PartialEq)]
pub struct Host {
    pub guid: Guid,
    pub name: String,
    pub player_id: i8,
    pub status: u8,
}

impl Host {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let guid_string = self.guid.to_string();
        let guid_bytes = guid_string.as_bytes();
        bytes.extend_from_slice(&(guid_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(guid_bytes);

        let name_bytes = write_wide_string(&self.name);
        bytes.extend_from_slice(&name_bytes);

        bytes.push(self.player_id as u8);
        bytes.push(self.status);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<(Self, usize), ParseError> {
        let mut pos = 0;

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
        pos += guid_len;

        let (name, mut pos) = read_wide_string(buffer, pos)?;

        if buffer.len() < pos + 1 {
            return Err(ParseError::Truncated { field: "player_id" });
        }
        let player_id = buffer[pos] as i8;
        pos += 1;

        if buffer.len() < pos + 1 {
            return Err(ParseError::Truncated { field: "status" });
        }
        let status = buffer[pos];
        pos += 1;

        Ok((
            Self {
                guid,
                name,
                player_id,
                status,
            },
            pos,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_host(guid: &str, name: &str, player_id: i8, status: u8) -> Host {
        Host {
            guid: Guid(guid.to_string()),
            name: name.to_string(),
            player_id,
            status,
        }
    }

    #[test]
    fn roundtrip_host() {
        let host = make_host("abc123", "TestPlayer", 1, 2);
        let bytes = host.to_bytes();
        let (decoded, bytes_consumed) = Host::from_bytes(&bytes).unwrap();

        assert_eq!(decoded, host);
        assert_eq!(bytes_consumed, bytes.len());
    }

    #[test]
    fn roundtrip_host_empty_name() {
        let host = make_host("abc123", "", 1, 2);
        let bytes = host.to_bytes();
        let (decoded, bytes_consumed) = Host::from_bytes(&bytes).unwrap();

        assert_eq!(decoded, host);
        assert_eq!(bytes_consumed, bytes.len());
    }

    #[test]
    fn roundtrip_host_negative_player_id() {
        let host = make_host("abc123", "TestPlayer", -1, 0);
        let bytes = host.to_bytes();
        let (decoded, bytes_consumed) = Host::from_bytes(&bytes).unwrap();

        assert_eq!(decoded, host);
        assert_eq!(bytes_consumed, bytes.len());
    }
}
