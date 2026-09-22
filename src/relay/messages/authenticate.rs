// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;
use crate::utils::{read_wide_string, write_wide_string};

#[derive(Debug, Clone, PartialEq)]
pub struct Authenticate {
    pub name: String,
    pub password: String,
    pub controller_secret: String,
}

impl Authenticate {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&write_wide_string(&self.name));

        let password_bytes = self.password.as_bytes();
        bytes.extend_from_slice(&(password_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(password_bytes);

        let secret_bytes = self.controller_secret.as_bytes();
        bytes.extend_from_slice(&(secret_bytes.len() as u32).to_be_bytes());
        bytes.extend_from_slice(secret_bytes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let pos = 0;

        let (name, mut pos) = read_wide_string(buffer, pos)?;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "password length",
            });
        }
        let password_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + password_len {
            return Err(ParseError::Truncated {
                field: "password data",
            });
        }
        let password = String::from_utf8(buffer[pos..pos + password_len].to_vec())
            .map_err(|_| ParseError::BadUtf8 { field: "password" })?;
        pos += password_len;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated {
                field: "controller_secret length",
            });
        }
        let secret_len = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]) as usize;
        pos += 4;

        if buffer.len() < pos + secret_len {
            return Err(ParseError::Truncated {
                field: "controller_secret data",
            });
        }
        let controller_secret =
            String::from_utf8(buffer[pos..pos + secret_len].to_vec()).map_err(|_| {
                ParseError::BadUtf8 {
                    field: "controller_secret",
                }
            })?;

        Ok(Self {
            name,
            password,
            controller_secret,
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/authenticate.rs"]
mod tests;
