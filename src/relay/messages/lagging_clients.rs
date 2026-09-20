// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::performance_entry::PerformanceEntry;
use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct LaggingClients {
    pub clients: Vec<PerformanceEntry>,
}

impl LaggingClients {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        for client in &self.clients {
            bytes.extend(client.to_bytes());
        }

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;
        let mut clients = Vec::new();

        while pos < buffer.len() {
            let (client, new_pos) = PerformanceEntry::from_bytes(buffer, pos)?;
            clients.push(client);
            pos = new_pos;
        }

        Ok(Self { clients })
    }
}

#[cfg(test)]
mod tests {
    use super::super::guid::Guid;
    use super::*;

    #[test]
    fn roundtrip_client_performance() {
        let msg = LaggingClients {
            clients: vec![
                PerformanceEntry {
                    guid: Guid("abc".to_string()),
                    mean_rtt: 50,
                },
                PerformanceEntry {
                    guid: Guid("def".to_string()),
                    mean_rtt: 100,
                },
            ],
        };
        let bytes = msg.to_bytes();
        let decoded = LaggingClients::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_client_performance_empty() {
        let msg = LaggingClients { clients: vec![] };
        let bytes = msg.to_bytes();
        let decoded = LaggingClients::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
