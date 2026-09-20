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
    let mut out = String::new();

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

        let ch = if (0xD800..=0xDFFF).contains(&unit) {
            '\u{FFFD}'
        } else {
            char::from_u32(unit as u32).unwrap_or('\u{FFFD}')
        };

        out.push(ch);
    }

    Ok((out, pos))
}
pub fn write_wide_string(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2 + 2);

    for ch in s.chars() {
        // characters above U+FFFF become replacement character
        let unit: u16 = if (ch as u32) <= 0xFFFF {
            ch as u16
        } else {
            0xFFFD
        };

        // big-endian
        out.push((unit >> 8) as u8);
        out.push((unit & 0xFF) as u8);
    }

    // null terminator
    out.push(0);
    out.push(0);

    out
}
