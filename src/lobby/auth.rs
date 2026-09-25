// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Handles the jabber:iq:lobbyauth <auth><token> IQ (PROTOCOL.md Sec. 17.2 /
// 9.1).

use tokio_xmpp::Client;
use tokio_xmpp::minidom::Element;
use tokio_xmpp::parsers::iq::Iq;
use tokio_xmpp::parsers::jid::Jid;

use crate::lobby::link::LobbyAuthToken;
use crate::lobby::link::LobbyToGame;

pub const NS_LOBBYAUTH: &str = "jabber:iq:lobbyauth";

// 1. reply with an empty IQ result; 2. forward (from.node, token) to the
// game thread, which is what binds the XMPP identity to the session UUID
// (Sec. 9.1). `from.node` is the JID's localpart, as vouched for by the XMPP
// server, so it is trusted without further checking here.
pub async fn handle(
    client: &mut Client,
    from: Jid,
    id: String,
    payload: &Element,
    auth_tx: Option<&std::sync::mpsc::Sender<LobbyToGame>>,
) {
    if let Some(token) = payload.get_child("token", NS_LOBBYAUTH).map(|t| t.text())
        && let Some(username) = from.node().map(|n| n.to_string())
        && let Some(tx) = auth_tx
    {
        crate::metrics::LOBBY_AUTH_TOTAL.inc();
        let _ = tx.send(LobbyToGame::Auth(LobbyAuthToken { username, token }));
    }

    let result = Iq::empty_result(from, id);
    let _ = client.send_stanza(result.into()).await;
}
