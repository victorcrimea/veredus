// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;

pub fn hex_dump(data: &[u8]) -> String {
    const BYTES_PER_LINE: usize = 8;

    let mut out = String::new();

    for (i, chunk) in data.chunks(BYTES_PER_LINE).enumerate() {
        // Offset
        out.push_str(&format!("{:08x}  ", i * BYTES_PER_LINE));

        // Hex section
        for j in 0..BYTES_PER_LINE {
            if j < chunk.len() {
                out.push_str(&format!("{:02x} ", chunk[j]));
            } else {
                out.push_str("   ");
            }

            if j == 7 {
                out.push(' ');
            }
        }

        out.push_str(" |");

        // ASCII section
        for &b in chunk {
            let c = if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '.'
            };
            out.push(c);
        }

        out.push_str("|\n");
    }

    out
}
pub fn read_wide_string(buffer: &[u8], start_pos: usize) -> Result<(String, usize), ParseError> {
    let mut pos = start_pos;
    let mut units = Vec::new();

    loop {
        if buffer.len() < pos + 2 {
            return Err(ParseError::Truncated {
                field: "wide string",
            });
        }

        let unit = u16::from_be_bytes([buffer[pos], buffer[pos + 1]]);
        pos += 2;

        if unit == 0 {
            break; // null terminator found
        }

        units.push(unit);
    }

    // Decoded as pairs rather than unit by unit, so a character outside the
    // BMP survives the relay's decode and re-encode of chat and names. Only a
    // lone surrogate, which no text can be rebuilt from, is replaced.
    let out = char::decode_utf16(units)
        .map(|r| r.unwrap_or('\u{FFFD}'))
        .collect();

    Ok((out, pos))
}
pub fn write_wide_string(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);

    // big-endian
    for unit in s.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }

    // null terminator
    out.push(0);
    out.push(0);

    out
}
