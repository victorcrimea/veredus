// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Builders for the IQs a stock client sends the rating bot, shaped as its
// XmppClient builds them.

use tokio_xmpp::minidom::Element;
use tokio_xmpp::minidom::rxml::Namespace as RxmlNamespace;
use tokio_xmpp::minidom::rxml::NcName;

use crate::lobby::link::GameReport;

pub const NS_GAMEREPORT: &str = "jabber:iq:gamereport";
pub const NS_BOARDLIST: &str = "jabber:iq:boardlist";

// The client asks for this once it has logged in, for the leaderboard.
pub fn get_leaderboard() -> Element {
    Element::builder("query", NS_BOARDLIST)
        .append(
            Element::builder("command", NS_BOARDLIST)
                .append("getleaderboard")
                .build(),
        )
        .build()
}

pub fn report(report: &GameReport) -> Element {
    let mut game = Element::bare("game", NS_GAMEREPORT);
    for (key, value) in &report.attrs {
        // Unit and structure class names come from the game's data, so one
        // that is no valid attribute name is dropped rather than trusted.
        match NcName::try_from(key.as_str()) {
            Ok(name) => {
                game.set_attr(RxmlNamespace::NONE, name, value.as_str());
            }
            Err(_) => tracing::warn!(%key, "game report attribute is not a valid name, dropped"),
        }
    }
    Element::builder("report", NS_GAMEREPORT)
        .append(game)
        .build()
}
