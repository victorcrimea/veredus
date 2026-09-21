// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Builders for the jabber:iq:gamelist IQs sent to the game bot (PROTOCOL.md
// Sec. 17.3).

use std::collections::HashMap;

use tokio_xmpp::minidom::Element;
use tokio_xmpp::minidom::rxml::Namespace as RxmlNamespace;
use tokio_xmpp::minidom::rxml::NcName;

pub const NS_GAMELIST: &str = "jabber:iq:gamelist";

// The server tracks no real map settings (script-value decoding is
// unimplemented, Sec. 20.2), so every listing advertises the same skirmish
// map. F27 is where this becomes the controller's actual GAME_SETTINGS.
const FIXED_MAP_NAME: &str = "maps/skirmishes/alpine_valleys_2p";
const FIXED_NICE_MAP_NAME: &str = "Alpine Valleys (2)";
const FIXED_MAP_SIZE: &str = "0";
const FIXED_MAP_TYPE: &str = "skirmish";
const FIXED_VICTORY_CONDITIONS: &str = "conquest";
const FIXED_MAX_PLAYERS: &str = "2";

fn set_attr(element: &mut Element, name: &str, value: &str) {
    let name = NcName::try_from(name).expect("attribute name is a valid NCName");
    element.set_attr(RxmlNamespace::NONE, name, value);
}

pub fn register(attrs: &HashMap<String, String>) -> Element {
    let mut game = Element::bare("game", NS_GAMELIST);
    for (key, value) in attrs {
        set_attr(&mut game, key, value);
    }

    Element::builder("query", NS_GAMELIST)
        .append(
            Element::builder("command", NS_GAMELIST)
                .append("register")
                .build(),
        )
        .append(game)
        .build()
}

pub fn changestate(nbp: u32, players: &str) -> Element {
    let mut game = Element::bare("game", NS_GAMELIST);
    set_attr(&mut game, "nbp", &nbp.to_string());
    set_attr(&mut game, "players", players);

    Element::builder("query", NS_GAMELIST)
        .append(
            Element::builder("command", NS_GAMELIST)
                .append("changestate")
                .build(),
        )
        .append(game)
        .build()
}

pub fn unregister() -> Element {
    Element::builder("query", NS_GAMELIST)
        .append(
            Element::builder("command", NS_GAMELIST)
                .append("unregister")
                .build(),
        )
        .append(Element::bare("game", NS_GAMELIST))
        .build()
}

// The shape the stock lobby client expects for the `mods` attribute. Real
// per-mod versions would come from the controller's handshake, which the
// relay does not track per game (Sec. 20.2), so every listing advertises the
// single mod the server itself was configured with.
fn mods_json(engine_version: &str) -> String {
    let mods = serde_json::json!([{
        "mod": "public",
        "name": "0ad",
        "version": engine_version,
        "ignoreInCompatibilityChecks": false,
    }]);
    mods.to_string()
}

pub struct RegisterAttrs<'a> {
    pub server_name: &'a str,
    pub engine_version: &'a str,
    pub host_username: &'a str,
    pub host_jid: &'a str,
    pub nbp: u32,
    pub players: &'a str,
    pub has_password: bool,
}

pub fn register_attrs(attrs: RegisterAttrs<'_>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    out.insert("name".to_string(), attrs.server_name.to_string());
    out.insert("hostUsername".to_string(), attrs.host_username.to_string());
    // Joining clients salt the game password with this exact full JID, and the
    // game bot stores it verbatim rather than filling it in.
    out.insert("hostJID".to_string(), attrs.host_jid.to_string());
    out.insert("nbp".to_string(), attrs.nbp.to_string());
    out.insert("maxnbp".to_string(), FIXED_MAX_PLAYERS.to_string());
    out.insert("players".to_string(), attrs.players.to_string());
    out.insert(
        "hasPassword".to_string(),
        if attrs.has_password { "true" } else { "" }.to_string(),
    );
    out.insert("mods".to_string(), mods_json(attrs.engine_version));
    out.insert("mapName".to_string(), FIXED_MAP_NAME.to_string());
    out.insert("niceMapName".to_string(), FIXED_NICE_MAP_NAME.to_string());
    out.insert("mapSize".to_string(), FIXED_MAP_SIZE.to_string());
    out.insert("mapType".to_string(), FIXED_MAP_TYPE.to_string());
    out.insert(
        "victoryConditions".to_string(),
        FIXED_VICTORY_CONDITIONS.to_string(),
    );
    out
}
