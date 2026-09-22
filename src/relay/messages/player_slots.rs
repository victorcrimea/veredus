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
#[path = "../../../tests/unit/relay/messages/player_slots.rs"]
mod tests;
