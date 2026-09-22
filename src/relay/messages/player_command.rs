// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

#[derive(Debug, Clone, PartialEq)]
pub struct PlayerCommand {
    pub client: u32,
    pub player: i32,
    pub turn: u32,
    pub data: Vec<u8>,
}

impl PlayerCommand {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();

        bytes.extend_from_slice(&self.client.to_le_bytes());
        bytes.extend_from_slice(&self.player.to_le_bytes());
        bytes.extend_from_slice(&self.turn.to_le_bytes());

        let data_bytes = &self.data;
        bytes.extend_from_slice(data_bytes);

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        let mut pos = 0;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated { field: "client" });
        }
        let client = u32::from_le_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated { field: "player" });
        }
        let player = i32::from_le_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        if buffer.len() < pos + 4 {
            return Err(ParseError::Truncated { field: "turn" });
        }
        let turn = u32::from_le_bytes([
            buffer[pos],
            buffer[pos + 1],
            buffer[pos + 2],
            buffer[pos + 3],
        ]);
        pos += 4;

        let data = buffer[pos..].to_vec();

        Ok(Self {
            client,
            player,
            turn,
            data,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_simulation() {
        let msg = PlayerCommand {
            client: 1,
            player: -1,
            turn: 42,
            data: vec![0x03, 0x01],
        };
        let bytes = msg.to_bytes();
        let decoded = PlayerCommand::from_bytes(&bytes).unwrap();
        assert_eq!(decoded, msg);
    }
}

impl PlayerCommand {
    /// Extract the "type" property from a SpiderMonkey-serialized command
    /// object. Returns None if parsing fails or the object doesn't have a
    /// string "type" property.
    pub fn extract_command_type(data: &[u8]) -> Option<String> {
        if data.first()? != &SCRIPT_TYPE_OBJECT {
            return None;
        }
        let mut pos = 1;

        let num_props = read_u32(data, &mut pos)?;

        for _ in 0..num_props {
            // Each property: ScriptString(name) + ScriptVal(value)
            let (name, _) = read_script_string(data, &mut pos)?;
            if name == "type" {
                if data.get(pos)? == &SCRIPT_TYPE_STRING {
                    pos += 1;
                    let (val, _) = read_script_string(data, &mut pos)?;
                    return Some(val);
                } else {
                    return None;
                }
            } else {
                pos = skip_script_val(data, pos, 0)?;
            }
        }

        None
    }
}

pub(crate) const SCRIPT_TYPE_VOID: u8 = 0x00;
pub(crate) const SCRIPT_TYPE_NULL: u8 = 0x01;
pub(crate) const SCRIPT_TYPE_ARRAY: u8 = 0x02;
pub(crate) const SCRIPT_TYPE_OBJECT: u8 = 0x03;
pub(crate) const SCRIPT_TYPE_STRING: u8 = 0x04;
pub(crate) const SCRIPT_TYPE_INT: u8 = 0x05;
pub(crate) const SCRIPT_TYPE_DOUBLE: u8 = 0x06;
pub(crate) const SCRIPT_TYPE_BOOLEAN: u8 = 0x07;
pub(crate) const SCRIPT_TYPE_BACKREF: u8 = 0x08;
const SCRIPT_TYPE_TYPED_ARRAY: u8 = 0x09;
const SCRIPT_TYPE_ARRAY_BUFFER: u8 = 0x0a;
const SCRIPT_TYPE_OBJECT_PROTOTYPE: u8 = 0x0b;
const SCRIPT_TYPE_OBJECT_NUMBER: u8 = 0x0c;
const SCRIPT_TYPE_OBJECT_STRING: u8 = 0x0d;
const SCRIPT_TYPE_OBJECT_BOOLEAN: u8 = 0x0e;
const SCRIPT_TYPE_OBJECT_MAP: u8 = 0x0f;
const SCRIPT_TYPE_OBJECT_SET: u8 = 0x10;

pub(crate) fn read_u32(data: &[u8], pos: &mut usize) -> Option<u32> {
    if *pos + 4 > data.len() {
        return None;
    }
    let val = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Some(val)
}

/// Parse a ScriptString: u8 isLatin1 + u32 LE length + raw bytes
pub(crate) fn read_script_string(data: &[u8], pos: &mut usize) -> Option<(String, usize)> {
    let is_latin1 = *data.get(*pos)?;
    *pos += 1;

    let len = read_u32(data, pos)? as usize;

    if is_latin1 != 0 {
        if *pos + len > data.len() {
            return None;
        }
        let s: String = data[*pos..*pos + len].iter().map(|&b| b as char).collect();
        *pos += len;
        Some((s, *pos))
    } else {
        let byte_len = len * 2;
        if *pos + byte_len > data.len() {
            return None;
        }
        let utf16: Vec<u16> = (0..len)
            .map(|i| u16::from_le_bytes([data[*pos + i * 2], data[*pos + i * 2 + 1]]))
            .collect();
        let s = String::from_utf16_lossy(&utf16);
        *pos += byte_len;
        Some((s, *pos))
    }
}

// Commands come from any player and this runs on the game thread. A stack
// overflow aborts the whole process, which catch_unwind cannot stop, so a
// deeply nested packet must be refused before it recurses that far. Real
// commands nest only a few levels.
const MAX_SKIP_DEPTH: usize = 64;

/// Skip over a ScriptVal without fully parsing it.
pub(crate) fn skip_script_val(data: &[u8], mut pos: usize, depth: usize) -> Option<usize> {
    if depth > MAX_SKIP_DEPTH {
        return None;
    }
    let tag = *data.get(pos)?;
    pos += 1;

    match tag {
        SCRIPT_TYPE_VOID | SCRIPT_TYPE_NULL => Some(pos),
        SCRIPT_TYPE_BOOLEAN | SCRIPT_TYPE_OBJECT_BOOLEAN => {
            if pos + 1 > data.len() {
                return None;
            }
            Some(pos + 1)
        }
        SCRIPT_TYPE_INT | SCRIPT_TYPE_BACKREF => {
            if pos + 4 > data.len() {
                return None;
            }
            Some(pos + 4)
        }
        SCRIPT_TYPE_DOUBLE | SCRIPT_TYPE_OBJECT_NUMBER => {
            if pos + 8 > data.len() {
                return None;
            }
            Some(pos + 8)
        }
        SCRIPT_TYPE_STRING | SCRIPT_TYPE_OBJECT_STRING => {
            let (_, new_pos) = read_script_string(data, &mut pos)?;
            Some(new_pos)
        }
        SCRIPT_TYPE_OBJECT => {
            let num_props = read_u32(data, &mut pos)?;
            for _ in 0..num_props {
                let (_, _) = read_script_string(data, &mut pos)?;
                pos = skip_script_val(data, pos, depth + 1)?;
            }
            Some(pos)
        }
        SCRIPT_TYPE_OBJECT_PROTOTYPE => {
            // ScriptString(proto_name) + same as OBJECT (u32 num_props + props)
            let (_, _) = read_script_string(data, &mut pos)?;
            let num_props = read_u32(data, &mut pos)?;
            for _ in 0..num_props {
                let (_, _) = read_script_string(data, &mut pos)?;
                pos = skip_script_val(data, pos, depth + 1)?;
            }
            Some(pos)
        }
        SCRIPT_TYPE_ARRAY => {
            // Format: u32 array_length (JS .length, informational) +
            //         u32 num_props + [num_props * (ScriptString + ScriptVal)]
            // Array indices are stored as named string properties ("0", "1", etc.)
            let _array_length = read_u32(data, &mut pos)?;
            let num_props = read_u32(data, &mut pos)?;
            for _ in 0..num_props {
                let (_, _) = read_script_string(data, &mut pos)?;
                pos = skip_script_val(data, pos, depth + 1)?;
            }
            Some(pos)
        }
        SCRIPT_TYPE_TYPED_ARRAY => {
            // u8 array_type + u32 byte_offset + u32 length + ScriptVal(buffer)
            if pos + 1 + 4 + 4 > data.len() {
                return None;
            }
            pos += 1 + 4 + 4;
            pos = skip_script_val(data, pos, depth + 1)?;
            Some(pos)
        }
        SCRIPT_TYPE_ARRAY_BUFFER => {
            // u32 length + raw bytes[length]
            let length = read_u32(data, &mut pos)? as usize;
            if pos + length > data.len() {
                return None;
            }
            Some(pos + length)
        }
        SCRIPT_TYPE_OBJECT_MAP => {
            // u32 size + (ScriptVal key + ScriptVal value) * size
            let size = read_u32(data, &mut pos)?;
            for _ in 0..size {
                pos = skip_script_val(data, pos, depth + 1)?; // key
                pos = skip_script_val(data, pos, depth + 1)?; // value
            }
            Some(pos)
        }
        SCRIPT_TYPE_OBJECT_SET => {
            // u32 size + ScriptVal * size
            let size = read_u32(data, &mut pos)?;
            for _ in 0..size {
                pos = skip_script_val(data, pos, depth + 1)?;
            }
            Some(pos)
        }
        _ => None,
    }
}
