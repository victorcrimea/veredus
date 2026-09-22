// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Handles the jabber:iq:connectiondata get IQ (PROTOCOL.md Sec. 17.4).

use std::collections::HashMap;

use tokio_xmpp::Client;
use tokio_xmpp::minidom::Element;
use tokio_xmpp::parsers::iq::Iq;
use tokio_xmpp::parsers::jid::Jid;

use crate::relay::password;

pub const NS_CONNECTIONDATA: &str = "jabber:iq:connectiondata";

// Sec. 19.1: the lobby password failure threshold before a username is banned
// from further connection-data replies.
const MAX_FAILURES: u32 = 3;

// What the account task knows about the game it currently hosts. `H`, the
// stored password hash, is computed once by main and handed in whole; the
// account task never sees the raw password.
pub struct Assignment {
    pub port: u16,
    pub password_hash: String,
}

fn set_child_text(parent: &mut Element, name: &str, text: &str) {
    parent.append_child(
        Element::builder(name, NS_CONNECTIONDATA)
            .append(text)
            .build(),
    );
}

fn error_reply(error: &str) -> Element {
    let mut root = Element::bare("connectiondata", NS_CONNECTIONDATA);
    set_child_text(&mut root, "error", error);
    root
}

fn response_reply(ip: &str, port: u16) -> Element {
    let mut root = Element::bare("connectiondata", NS_CONNECTIONDATA);
    set_child_text(&mut root, "ip", ip);
    set_child_text(&mut root, "port", &port.to_string());
    root
}

// Children are omitted when empty (Sec. 17.4), so a missing password or
// clientsalt is just the empty string, not a parse failure.
fn parse_request(payload: &Element) -> (String, String) {
    let password = payload
        .get_child("password", NS_CONNECTIONDATA)
        .map(|e| e.text())
        .unwrap_or_default();
    let clientsalt = payload
        .get_child("clientsalt", NS_CONNECTIONDATA)
        .map(|e| e.text())
        .unwrap_or_default();
    (password, clientsalt)
}

async fn reply(client: &mut Client, to: Jid, id: String, payload: Element) {
    let result = Iq::Result {
        from: None,
        to: Some(to),
        id,
        payload: Some(payload),
    };
    let _ = client.send_stanza(result.into()).await;
}

fn connection_data_outcome(outcome: &str) {
    crate::metrics::LOBBY_CONNECTION_DATA_TOTAL
        .with_label_values(&[outcome])
        .inc();
}

// Checks run in PROTOCOL.md Sec. 17.4 order. `failures` is the per-assignment
// counter the account task owns: it is cleared whenever a game ends, because
// the ban is scoped to one assignment, not to the account's whole lifetime.
pub async fn handle(
    client: &mut Client,
    from: Jid,
    id: String,
    payload: &Element,
    assigned: Option<(&Assignment, &mut HashMap<String, u32>)>,
    public_ip: &str,
) {
    let Some((assignment, failures)) = assigned else {
        connection_data_outcome("no_game");
        reply(client, from, id, error_reply("not_server")).await;
        return;
    };

    let username = from.node().map(|n| n.to_string()).unwrap_or_default();
    if failures.get(&username).copied().unwrap_or(0) >= MAX_FAILURES {
        connection_data_outcome("banned");
        reply(client, from, id, error_reply("banned")).await;
        return;
    }

    let (client_password, client_salt) = parse_request(payload);

    let stored_hash = assignment.password_hash.clone();
    let hashed =
        tokio::task::spawn_blocking(move || password::hash(&stored_hash, client_salt.as_bytes()))
            .await;
    // An empty fallback would match a client that sent no password, so a
    // failed hash must refuse. The fault is ours, so it costs the client no
    // strike towards the ban.
    let expected = match hashed {
        Ok(expected) => expected,
        Err(error) => {
            tracing::error!(%error, "connection-data password hash failed");
            connection_data_outcome("error");
            reply(client, from, id, error_reply("invalid_password")).await;
            return;
        }
    };

    if expected != client_password {
        *failures.entry(username).or_insert(0) += 1;
        connection_data_outcome("wrong_password");
        reply(client, from, id, error_reply("invalid_password")).await;
        return;
    }

    failures.remove(&username);
    connection_data_outcome("ok");
    reply(client, from, id, response_reply(public_ip, assignment.port)).await;
}
