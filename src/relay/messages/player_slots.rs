// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::host::Host;
use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerSlots {
    pub hosts: Vec<Host>,
}

impl PlayerSlots {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        for host in &self.hosts {
            bytes.extend(host.to_bytes());
        }

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        let mut hosts = Vec::new();
        while pos < buffer.len() {
            let (host_item, bytes_read) = Host::from_bytes(&buffer[pos..])?;
            hosts.push(host_item);
            pos += bytes_read;
        }

        Ok(Self { hosts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::messages::guid::Guid;

    fn make_host(guid: &str, name: &str, player_id: i8, status: u8) -> Host {
        Host {
            guid: Guid(guid.to_string()),
            name: name.to_string(),
            player_id,
            status,
        }
    }

    #[test]
    fn roundtrip_single_host() {
        let pa = PlayerSlots {
            hosts: vec![make_host("abc123", "TestPlayer", 1, 2)],
        };
        let bytes = pa.to_bytes();
        let decoded = PlayerSlots::from_bytes(&bytes).unwrap();

        assert_eq!(decoded, pa);
    }

    #[test]
    fn roundtrip_multiple_hosts() {
        let pa = PlayerSlots {
            hosts: vec![
                make_host("aaa", "Alice", 1, 0),
                make_host("bbb", "Bob", 2, 1),
                make_host("ccc", "Charlie", 3, 2),
            ],
        };
        let bytes = pa.to_bytes();
        let decoded = PlayerSlots::from_bytes(&bytes).unwrap();

        assert_eq!(decoded, pa);
    }

    #[test]
    fn roundtrip_empty_hosts() {
        let pa = PlayerSlots { hosts: vec![] };
        let bytes = pa.to_bytes();
        let decoded = PlayerSlots::from_bytes(&bytes).unwrap();

        assert_eq!(decoded, pa);
    }
}
