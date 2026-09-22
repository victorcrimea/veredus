// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn strips_control_characters() {
    assert_eq!(sanitize("x\ry\u{0A}z\u{1B}w"), "xyzw");
}

#[test]
fn strips_bidi_override() {
    assert_eq!(sanitize("a\u{202E}b\u{0A}c"), "abc");
}

#[test]
fn lone_format_character_falls_back_to_anonymous() {
    assert_eq!(sanitize("\u{200D}"), ANONYMOUS);
}

#[test]
fn ordinary_name_is_unaffected() {
    assert_eq!(sanitize("normal_name"), "normal_name");
}

#[test]
fn stripping_happens_before_truncation() {
    // Forty control characters ahead of two real ones: if truncation ran
    // first, MAX_NAME_LEN would be spent entirely on characters that then
    // get stripped, leaving nothing.
    let padded = format!("{}OK", "\u{0}".repeat(40));
    assert_eq!(sanitize(&padded), "OK");
}

#[test]
fn bracket_remap_and_stripping_combine() {
    assert_eq!(sanitize("[a\u{202E}b]"), "{ab}");
}
