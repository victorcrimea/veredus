// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// The save effects a running match emits for its bundle, and the operator's
// stop that keeps it for a restart.

use veredus::enet::PeerID;
use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::StateHash;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;
use veredus::relay::turn::INITIAL_READY_TURN;
use veredus::savegame::Status;

use crate::harness::Harness;
use crate::harness::chats_to;
use crate::harness::resign_data;
use crate::harness::turn_sealed;

const SETTINGS: &[u8] = br#"{"settings":{"PlayerData":[{},{}],"VictoryConditions":["conquest"]}}"#;

fn saving() -> Config {
    Config {
        saving: true,
        sidecar_dumps: true,
        ..Config::default()
    }
}

fn two_players(config: Config) -> Harness {
    let mut h = Harness::with_config(config);
    h.start_match(&[("alice", Some(1)), ("bob", Some(2))], SETTINGS);
    h
}

fn is_save(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::SaveStarted { .. }
            | Effect::SaveTurn { .. }
            | Effect::SaveHash { .. }
            | Effect::SaveSlots(_)
            | Effect::SaveStatus(_)
    )
}

fn command(peer: PeerID, player: i32, turn: u32, data: Vec<u8>) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::PlayerCommand(PlayerCommand {
            client: peer.0 as u32,
            player,
            turn,
            data,
        }),
    }
}

fn hash(peer: PeerID, turn: u32, fill: u8) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::StateHash(StateHash {
            turn,
            hash: [fill; 16],
        }),
    }
}

#[test]
fn match_start_saves_settings_and_slots() {
    let h = two_players(saving());
    let started: Vec<&Effect> = h
        .log()
        .iter()
        .filter(|e| matches!(e, Effect::SaveStarted { .. }))
        .collect();
    assert_eq!(
        started,
        vec![&Effect::SaveStarted {
            settings: SETTINGS.to_vec(),
            ai_settings: None,
            ai_players: Vec::new(),
            lobby_map: None,
        }]
    );
    let slots = h
        .log()
        .iter()
        .find_map(|e| match e {
            Effect::SaveSlots(s) => Some(s.clone()),
            _ => None,
        })
        .expect("slots saved at start");
    let names: Vec<(i32, &str)> = slots
        .players
        .iter()
        .map(|p| (p.player_id, p.name.as_str()))
        .collect();
    assert_eq!(names, vec![(1, "alice"), (2, "bob")]);
    assert_eq!(slots.controller.map(|c| c.name), Some("alice".to_string()));
}

#[test]
fn nothing_is_saved_before_the_match_starts() {
    let mut h = Harness::with_config(saving());
    h.admit(PeerID(1), "alice");
    assert!(!h.log().iter().any(is_save));
}

#[test]
fn a_released_turn_is_saved_with_its_commands() {
    let mut h = two_players(saving());
    let turn = INITIAL_READY_TURN + 1;
    h.input(command(PeerID(1), 1, turn, vec![9, 9]));
    h.input(turn_sealed(PeerID(1), turn));
    let effects = h.input(turn_sealed(PeerID(2), turn));
    let saved: Vec<&Effect> = effects
        .iter()
        .filter(|e| matches!(e, Effect::SaveTurn { .. }))
        .collect();
    assert_eq!(
        saved,
        vec![&Effect::SaveTurn {
            turn,
            length: 200,
            commands: vec![PlayerCommand {
                client: 1,
                player: 1,
                turn,
                data: vec![9, 9],
            }],
        }]
    );
}

#[test]
fn an_agreed_hash_is_saved_once() {
    let mut h = two_players(saving());
    let first = h.input(hash(PeerID(1), 1, 7));
    assert!(!first.iter().any(|e| matches!(e, Effect::SaveHash { .. })));
    let second = h.input(hash(PeerID(2), 1, 7));
    let saved: Vec<&Effect> = second
        .iter()
        .filter(|e| matches!(e, Effect::SaveHash { .. }))
        .collect();
    assert_eq!(
        saved,
        vec![&Effect::SaveHash {
            turn: 1,
            hash: vec![7; 16],
        }]
    );
}

#[test]
fn slots_are_saved_only_when_they_change() {
    let mut h = two_players(saving());
    let before = h.log().len();
    h.advance(chrono::TimeDelta::seconds(2));
    assert!(
        !h.log()[before..]
            .iter()
            .any(|e| matches!(e, Effect::SaveSlots(_)))
    );

    let effects = h.input(command(PeerID(2), 2, INITIAL_READY_TURN + 1, resign_data()));
    let slots = effects
        .iter()
        .find_map(|e| match e {
            Effect::SaveSlots(s) => Some(s.clone()),
            _ => None,
        })
        .expect("a resign changes the saved slots");
    assert_eq!(slots.resigned, vec![2]);
}

#[test]
fn nothing_is_saved_with_saving_off() {
    let mut h = two_players(Config::default());
    let turn = INITIAL_READY_TURN + 1;
    h.input(turn_sealed(PeerID(1), turn));
    h.input(turn_sealed(PeerID(2), turn));
    h.input(hash(PeerID(1), 1, 7));
    h.input(hash(PeerID(2), 1, 7));
    assert!(!h.log().iter().any(is_save));
    assert!(!h.server().saved_on_stop());
}

#[test]
fn stop_in_game_saves_and_promises_a_restart() {
    let h = two_players(saving());
    assert!(h.server().saved_on_stop());
    let effects = h.stop();
    assert!(effects.contains(&Effect::SaveStatus(Status::Stopped)));
    let chats = chats_to(&effects, PeerID(1));
    assert!(
        chats.iter().any(|c| c.starts_with("Server is restarting.")),
        "{chats:?}"
    );
}

#[test]
fn stop_without_a_sidecar_saves_but_promises_nothing() {
    let h = two_players(Config {
        sidecar_dumps: false,
        ..saving()
    });
    let effects = h.stop();
    assert!(effects.contains(&Effect::SaveStatus(Status::Stopped)));
    let chats = chats_to(&effects, PeerID(1));
    assert!(
        chats.iter().all(|c| c.starts_with("Server shutdown:")),
        "{chats:?}"
    );
}

#[test]
fn stop_in_setup_is_a_plain_shutdown() {
    let mut h = Harness::with_config(saving());
    h.admit(PeerID(1), "alice");
    assert!(!h.server().saved_on_stop());
    let effects = h.stop();
    assert!(!effects.iter().any(is_save));
    let chats = chats_to(&effects, PeerID(1));
    assert!(
        chats.iter().all(|c| c.starts_with("Server shutdown:")),
        "{chats:?}"
    );
}
