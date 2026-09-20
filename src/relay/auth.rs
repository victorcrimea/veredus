// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::messages::EnabledMod;

pub const MAX_NAME_LEN: usize = 32;
pub const ANONYMOUS: &str = "Anonymous";

// The separator a deduplicated name is cut at. Unlike the rest of
// sanitization this spelling is constrained: lobby authentication recovers the
// XMPP username by cutting here, so any other separator breaks lobby mode.
const SUFFIX_MARKER: &str = " (";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LateObserverPolicy {
    #[default]
    Everyone,
    Buddies,
    Deny,
}

// None of this reaches the wire: the client never inspects the shape of a
// name, so these rules only reproduce what the stock server displays.
pub fn sanitize(raw: &str) -> String {
    let replaced: String = raw
        .chars()
        .map(|c| match c {
            '[' => '{',
            ']' => '}',
            other => other,
        })
        .take(MAX_NAME_LEN)
        .collect();

    let trimmed = replaced.trim();
    if trimmed.is_empty() {
        ANONYMOUS.to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn suffix_stripped(name: &str) -> &str {
    match name.find(SUFFIX_MARKER) {
        Some(at) => &name[..at],
        None => name,
    }
}

// Counts up from 2 until nothing else displays the candidate.
pub fn deduplicate(candidate: &str, taken: impl Fn(&str) -> bool) -> String {
    if !taken(candidate) {
        return candidate.to_string();
    }
    let mut counter = 2u32;
    loop {
        let next = format!("{candidate}{SUFFIX_MARKER}{counter})");
        if !taken(&next) {
            return next;
        }
        counter += 1;
    }
}

// Positional and exact. The client never checks any of this itself, so the
// server is the only place an incompatible peer is caught, and the client
// reads the stored handshake to explain which component differed.
pub fn compatible(
    server_version: &str,
    server_mods: &[EnabledMod],
    client_version: &str,
    client_mods: &[EnabledMod],
) -> bool {
    if server_version != client_version || server_mods.len() != client_mods.len() {
        return false;
    }
    server_mods.iter().zip(client_mods).all(|(server, client)| {
        format!("{}-{}", server.name, server.version)
            == format!("{}-{}", client.name, client.version)
    })
}
