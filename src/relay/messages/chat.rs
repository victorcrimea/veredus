// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;
use crate::utils::read_wide_string;
use crate::utils::write_wide_string;

use super::guid::Guid;

#[derive(Debug, PartialEq)]
pub struct Chat {
    pub sender_guid: Guid,
    pub message: String,
    pub receivers: Vec<Guid>,
}

impl Chat {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let sender_guid_string = self.sender_guid.to_string();
        let sender_bytes = sender_guid_string.as_bytes();
        bytes.extend_from_slice(&(sender_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(sender_bytes);

        let message_bytes = write_wide_string(&self.message);
        bytes.extend_from_slice(&message_bytes);

        for receiver in &self.receivers {
            let receiver_string = receiver.to_string();
            let receiver_bytes = receiver_string.as_bytes();
            bytes.extend_from_slice(&(receiver_bytes.len() as u32).to_be_bytes());
            bytes.extend_from_slice(receiver_bytes);
        }

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "sender_guid length",
            });
        }
        let sender_guid_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + sender_guid_len {
            return Err(ParseError::Truncated {
                field: "sender_guid data",
            });
        }
        let sender_guid = Guid(
            String::from_utf8(buffer[pos..pos + sender_guid_len].to_vec()).map_err(|_| {
                ParseError::BadUtf8 {
                    field: "sender_guid",
                }
            })?,
        );
        pos += sender_guid_len;

        let (message, mut pos) = read_wide_string(buffer, pos)?;

        let mut receivers = Vec::new();
        while pos < buffer.len() {
            if buffer.len() < pos + 4 {
                return Err(ParseError::Truncated {
                    field: "receiver guid length",
                });
            }
            let receiver_guid_len = u32::from_be_bytes([
                buffer[pos],
                buffer[pos + 1],
                buffer[pos + 2],
                buffer[pos + 3],
            ]) as usize;
            pos += 4;

            if buffer.len() < pos + receiver_guid_len {
                return Err(ParseError::Truncated {
                    field: "receiver guid data",
                });
            }
            receivers.push(Guid(
                String::from_utf8(buffer[pos..pos + receiver_guid_len].to_vec()).map_err(|_| {
                    ParseError::BadUtf8 {
                        field: "receiver guid",
                    }
                })?,
            ));
            pos += receiver_guid_len;
        }

        Ok(Self {
            sender_guid,
            message,
            receivers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_chat_with_receivers() {
        let msg = Chat {
            sender_guid: Guid("sender1".to_string()),
            message: "Hello".to_string(),
            receivers: vec![Guid("recv1".to_string()), Guid("recv2".to_string())],
        };
        let bytes = msg.to_bytes();
        let decoded = Chat::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn roundtrip_chat_no_receivers() {
        let msg = Chat {
            sender_guid: Guid("sender1".to_string()),
            message: "Hello".to_string(),
            receivers: vec![],
        };
        let bytes = msg.to_bytes();
        let decoded = Chat::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
