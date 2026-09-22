// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::authenticate_result_code::AuthenticateResultCode;
use crate::relay::fault::ParseError;
use crate::utils::read_wide_string;
use crate::utils::write_wide_string;

#[derive(Debug, Clone, PartialEq)]
pub struct AuthenticateResult {
    pub code: AuthenticateResultCode,
    pub host_id: u16,
    pub is_controller: bool,
    pub message: String,
}

impl AuthenticateResult {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&(self.code as u32).to_be_bytes());
        bytes.extend_from_slice(&self.host_id.to_be_bytes());
        bytes.push(self.is_controller as u8);

        let message_bytes = write_wide_string(&self.message);
        bytes.extend_from_slice(&message_bytes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated { field: "code" });
        }
        let code = u32::from_be_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 2 {
            return Err(ParseError::Truncated { field: "host_id" });
        }
        let host_id = u16::from_be_bytes([buffer[pos], buffer[pos + 1]]);
        pos += 2;

        if buffer.len() < pos + 1 {
            return Err(ParseError::Truncated {
                field: "is_controller",
            });
        }
        let is_controller = buffer[pos] != 0;
        pos += 1;

        let (message, _pos) = read_wide_string(buffer, pos)?;

        let code = match code {
            0 => AuthenticateResultCode::Ok,
            1 => AuthenticateResultCode::OkSavedGame,
            2 => AuthenticateResultCode::OkRejoining,
            3.. => AuthenticateResultCode::PasswordInvalid,
        };

        Ok(Self {
            code,
            host_id,
            is_controller,
            message,
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/unit/relay/messages/authenticate_result.rs"]
mod tests;
