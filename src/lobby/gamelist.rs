// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Builders for the jabber:iq:gamelist IQs sent to the game bot (PROTOCOL.md
// Sec. 17.3).

use std::collections::HashMap;

use tokio_xmpp::minidom::Element;
use tokio_xmpp::minidom::rxml::Namespace as RxmlNamespace;
use tokio_xmpp::minidom::rxml::NcName;

use crate::lobby::link::LobbyMap;
use crate::relay::messages::EnabledMod;
use crate::sidecar::mod_pathname;

pub const NS_GAMELIST: &str = "jabber:iq:gamelist";

// A hostme game is listed as soon as an account is assigned, before the
// controller has sent any GAME_SETTINGS (Sec. 5), so there is a moment with
// nothing real to advertise. These placeholders fill that moment; the first
// GAME_SETTINGS from the controller replaces them (see LobbyMap).
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

// The shape the stock lobby client expects for the `mods` attribute. Built
// from Config::enabled_mods, the list admission enforces against every
// client's SYN reply, so this is provably what everyone in the game runs.
fn mods_json(mods: &[EnabledMod]) -> String {
    let mods: Vec<_> = mods
        .iter()
        .map(|m| match mod_pathname(&m.name) {
            Some(pathname) => serde_json::json!({
                "mod": pathname,
                "name": m.name,
                "version": m.version,
                "ignoreInCompatibilityChecks": false,
            }),
            None => serde_json::json!({
                "name": m.name,
                "version": m.version,
                "ignoreInCompatibilityChecks": false,
            }),
        })
        .collect();
    serde_json::Value::Array(mods).to_string()
}

pub struct RegisterAttrs<'a> {
    pub server_name: &'a str,
    pub mods: &'a [EnabledMod],
    pub host_username: &'a str,
    pub host_jid: &'a str,
    pub nbp: u32,
    pub players: &'a str,
    pub has_password: bool,
    pub map: Option<&'a LobbyMap>,
}

pub fn register_attrs(attrs: RegisterAttrs<'_>) -> HashMap<String, String> {
    let mut out = HashMap::new();
    out.insert("name".to_string(), attrs.server_name.to_string());
    out.insert("hostUsername".to_string(), attrs.host_username.to_string());
    // Joining clients salt the game password with this exact full JID, and the
    // game bot stores it verbatim rather than filling it in.
    out.insert("hostJID".to_string(), attrs.host_jid.to_string());
    out.insert("nbp".to_string(), attrs.nbp.to_string());
    out.insert("players".to_string(), attrs.players.to_string());
    out.insert(
        "hasPassword".to_string(),
        if attrs.has_password { "true" } else { "" }.to_string(),
    );
    out.insert("mods".to_string(), mods_json(attrs.mods));
    match attrs.map {
        Some(map) => {
            out.insert("mapName".to_string(), map.map_name.clone());
            out.insert("niceMapName".to_string(), map.nice_map_name.clone());
            out.insert("mapSize".to_string(), map.map_size.clone());
            out.insert("mapType".to_string(), map.map_type.clone());
            out.insert(
                "victoryConditions".to_string(),
                map.victory_conditions.clone(),
            );
            out.insert("maxnbp".to_string(), map.max_players.to_string());
        }
        None => {
            out.insert("mapName".to_string(), FIXED_MAP_NAME.to_string());
            out.insert("niceMapName".to_string(), FIXED_NICE_MAP_NAME.to_string());
            out.insert("mapSize".to_string(), FIXED_MAP_SIZE.to_string());
            out.insert("mapType".to_string(), FIXED_MAP_TYPE.to_string());
            out.insert(
                "victoryConditions".to_string(),
                FIXED_VICTORY_CONDITIONS.to_string(),
            );
            out.insert("maxnbp".to_string(), FIXED_MAX_PLAYERS.to_string());
        }
    }
    out
}
