// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::guid::Guid;
use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct Flare {
    pub guid: Guid,
    pub position_x: String,
    pub position_y: String,
    pub position_z: String,
}

impl Flare {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        let guid_string = self.guid.to_string();
        let guid_bytes = guid_string.as_bytes();
        bytes.extend_from_slice(&(guid_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(guid_bytes);

        let x_bytes = self.position_x.as_bytes();
        bytes.extend_from_slice(&(x_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(x_bytes);

        let y_bytes = self.position_y.as_bytes();
        bytes.extend_from_slice(&(y_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(y_bytes);

        let z_bytes = self.position_z.as_bytes();
        bytes.extend_from_slice(&(z_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(z_bytes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
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

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "position_x length",
            });
        }
        let x_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + x_len {
            return Err(ParseError::Truncated {
                field: "position_x data",
            });
        }
        let position_x = String::from_utf8(buffer[pos..pos + x_len].to_vec()).map_err(|_| {
            ParseError::BadUtf8 {
                field: "position_x",
            }
        })?;
        pos += x_len;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "position_y length",
            });
        }
        let y_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + y_len {
            return Err(ParseError::Truncated {
                field: "position_y data",
            });
        }
        let position_y = String::from_utf8(buffer[pos..pos + y_len].to_vec()).map_err(|_| {
            ParseError::BadUtf8 {
                field: "position_y",
            }
        })?;
        pos += y_len;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "position_z length",
            });
        }
        let z_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + z_len {
            return Err(ParseError::Truncated {
                field: "position_z data",
            });
        }
        let position_z = String::from_utf8(buffer[pos..pos + z_len].to_vec()).map_err(|_| {
            ParseError::BadUtf8 {
                field: "position_z",
            }
        })?;

        Ok(Self {
            guid,
            position_x,
            position_y,
            position_z,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_flare() {
        let msg = Flare {
            guid: Guid("abc123".to_string()),
            position_x: "1.5".to_string(),
            position_y: "2.5".to_string(),
            position_z: "3.5".to_string(),
        };
        let bytes = msg.to_bytes();
        let decoded = Flare::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}
