// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn to_bytes_structure() {
    let msg = Authenticate {
        name: "AB".to_string(),
        password: "pw".to_string(),
        controller_secret: "s".to_string(),
    };
    let bytes = msg.to_bytes();
    // name: "\0A\0B" + \0\0
    // password: 4-byte len (2) + "pw"
    // secret: 4-byte len (1) + "s"
    assert_eq!(bytes.len(), 4 + 2 + 4 + 2 + 4 + 1);
    assert_eq!(&bytes[0..6], b"\0A\0B\0\0");
    assert_eq!(&bytes[6..10], [0x00, 0x00, 0x00, 0x02]);
    assert_eq!(&bytes[10..12], b"pw");
    assert_eq!(&bytes[12..16], [0x00, 0x00, 0x00, 0x01]);
    assert_eq!(&bytes[16..17], b"s");
}

#[test]
fn from_bytes_with_wide_string_name() {
    // from_bytes expects name as wide string (UTF-16 BE, null-terminated),
    // then password and secret as length-prefixed UTF-8.
    let mut buf = Vec::new();
    // Wide string "Hi" = 0x00 0x48 0x00 0x69 0x00 0x00
    buf.extend_from_slice(&[0x00, b'H', 0x00, b'i', 0x00, 0x00]);
    // password: len=2 + "pw"
    buf.extend_from_slice(&2u32.to_be_bytes());
    buf.extend_from_slice(b"pw");
    // secret: len=1 + "s"
    buf.extend_from_slice(&1u32.to_be_bytes());
    buf.extend_from_slice(b"s");

    let decoded = Authenticate::from_bytes(&buf).unwrap();
    assert_eq!(decoded.name, "Hi");
    assert_eq!(decoded.password, "pw");
    assert_eq!(decoded.controller_secret, "s");
}
