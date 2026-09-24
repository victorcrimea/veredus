// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// A saved match rebuilt after a restart: returning clients are held at the
// turn it stopped at until the controller restarts it.

use std::sync::Arc;

use chrono::TimeDelta;

use veredus::enet::PeerID;
use veredus::relay::messages::Chat;
use veredus::relay::messages::Guid;
use veredus::relay::messages::Joined;
use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::PlayerPause;
use veredus::relay::messages::TurnSealed;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Idle;
use veredus::relay::server_fsm::Input;
use veredus::relay::server_fsm::Server;
use veredus::savegame::SavedIdentity;
use veredus::savegame::SavedPlayer;
use veredus::savegame::SlotsSnapshot;
use veredus::savegame::Status;
use veredus::savegame::resume::ResumeData;
use veredus::savegame::resume::SavedTurn;
use veredus::sidecar::BaseState;

use crate::harness::Harness;
use crate::harness::chats_to;
use crate::harness::turn_sealed;

const SETTINGS: &[u8] = br#"{"settings":{"PlayerData":[{},{}],"VictoryConditions":["conquest"]}}"#;
const STOPPED_AT: u32 = 10;
const ALICE: PeerID = PeerID(1);
const BOB: PeerID = PeerID(2);

fn config() -> Config {
    Config {
        saving: true,
        sidecar_dumps: true,
        ..Config::default()
    }
}

fn player(id: i32, name: &str) -> SavedPlayer {
    SavedPlayer {
        player_id: id,
        uuid: format!("saved-{name}"),
        name: name.to_string(),
        lobby_name: String::new(),
    }
}

fn data(base: Option<BaseState>) -> ResumeData {
    let turns = (4..=STOPPED_AT)
        .map(|turn| SavedTurn {
            turn,
            length: 200,
            commands: if turn == 6 {
                vec![PlayerCommand {
                    client: 1,
                    player: 1,
                    turn,
                    data: vec![1],
                }]
            } else {
                Vec::new()
            },
        })
        .collect();
    ResumeData {
        settings: SETTINGS.to_vec(),
        turn_length_ms: 200,
        turns,
        hashes: vec![(1, vec![7; 16])],
        slots: SlotsSnapshot {
            players: vec![player(1, "alice"), player(2, "bob")],
            controller: Some(SavedIdentity {
                name: "alice".to_string(),
                lobby_name: String::new(),
            }),
            ..SlotsSnapshot::default()
        },
        base,
        ai: None,
    }
}

fn resumed(config: Config, base: Option<BaseState>) -> (Harness, Vec<Effect>) {
    let server = Server::<Idle>::new(config).resume(data(base));
    Harness::with_server(AnyServer::from(server))
}

fn restore_id(effects: &[Effect]) -> u32 {
    effects
        .iter()
        .find_map(|e| match e {
            Effect::Checkpoint { id, turn, request } => {
                assert_eq!(*turn, STOPPED_AT);
                assert_eq!(request.first_turn(), 0);
                assert_eq!(request.last_turn(), STOPPED_AT);
                assert_eq!(request.hashes, vec![(1, vec![7; 16])]);
                assert_eq!(request.commands.len(), 1);
                Some(*id)
            }
            _ => None,
        })
        .expect("a restore run for the stopped turn")
}

fn restored(h: &mut Harness, id: u32) -> Vec<Effect> {
    h.input(Input::Checkpointed {
        id,
        state: Some(vec![1, 2, 3]),
        players: Vec::new(),
    })
}

fn joins_sent(h: &Harness, peer: PeerID) -> usize {
    h.sent_to(peer)
        .iter()
        .filter(|m| matches!(m, WireMessage::Join(_)))
        .count()
}

// Loads the snapshot it was handed and reports back, as a stock client does.
fn load(h: &mut Harness, peer: PeerID) {
    h.input(Input::Received {
        peer,
        msg: WireMessage::LoadedGame(LoadedGame {
            current_turn: STOPPED_AT,
        }),
    });
    h.input(Input::Received {
        peer,
        msg: WireMessage::Joined(Joined {
            guid: Guid(String::new()),
        }),
    });
}

fn chat(h: &mut Harness, peer: PeerID, text: &str) -> Vec<Effect> {
    h.input(Input::Received {
        peer,
        msg: WireMessage::Chat(Chat {
            sender_guid: Guid(String::new()),
            message: text.to_string(),
            receivers: Vec::new(),
        }),
    })
}

// Resumed, rebuilt, and alice back in the game.
fn alice_back() -> Harness {
    let (mut h, effects) = resumed(config(), None);
    let id = restore_id(&effects);
    restored(&mut h, id);
    h.admit(ALICE, "alice");
    load(&mut h, ALICE);
    h
}

#[test]
fn a_joiner_waits_for_the_rebuilt_state_then_is_held_paused() {
    let (mut h, effects) = resumed(config(), None);
    let id = restore_id(&effects);
    assert!(matches!(h.server(), AnyServer::Resuming(_)));

    h.admit(ALICE, "alice");
    assert_eq!(joins_sent(&h, ALICE), 0, "no JOIN before the state exists");
    assert!(
        !h.sent_to(ALICE)
            .iter()
            .any(|m| matches!(m, WireMessage::GamestateRequest(_))),
        "nobody else can serve it"
    );

    restored(&mut h, id);
    let join = h
        .sent_to(ALICE)
        .into_iter()
        .find_map(|m| match m {
            WireMessage::Join(j) => Some(j),
            _ => None,
        })
        .expect("JOIN once the state is rebuilt");
    assert_eq!(join.init_attributes, SETTINGS);

    load(&mut h, ALICE);
    let sent = h.sent_to(ALICE);
    assert!(sent.contains(&WireMessage::LoadedGame(LoadedGame {
        current_turn: STOPPED_AT
    })));
    assert!(
        sent.iter().any(|m| matches!(
            m,
            WireMessage::PlayerPause(PlayerPause { pause: true, guid }) if guid.0 != String::new()
        )),
        "held paused under the relay's own id"
    );
}

#[test]
fn a_stored_state_at_the_stopped_turn_needs_no_run() {
    let base = BaseState {
        turn: STOPPED_AT,
        state: Arc::new(vec![9]),
    };
    let (mut h, effects) = resumed(config(), Some(base));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Checkpoint { .. }))
    );
    h.admit(ALICE, "alice");
    assert_eq!(joins_sent(&h, ALICE), 1);
}

#[test]
fn an_older_stored_state_is_the_base_of_the_run() {
    let base = BaseState {
        turn: 5,
        state: Arc::new(vec![9]),
    };
    let (_, effects) = resumed(config(), Some(base));
    let request = effects
        .iter()
        .find_map(|e| match e {
            Effect::Checkpoint { request, .. } => Some(request.clone()),
            _ => None,
        })
        .expect("a restore run");
    assert_eq!(request.first_turn(), 5);
    assert_eq!(request.last_turn(), STOPPED_AT);
    assert!(request.hashes.is_empty(), "no agreed hash after turn 5");
}

#[test]
fn only_original_names_get_a_slot_back() {
    let mut h = alice_back();
    h.admit(PeerID(3), "carol");
    let slots = h
        .sent_to(PeerID(3))
        .into_iter()
        .rev()
        .find_map(|m| match m {
            WireMessage::PlayerSlots(s) => Some(s),
            _ => None,
        })
        .expect("slot list sent to carol");
    let slot_of = |name: &str| {
        slots
            .hosts
            .iter()
            .find(|host| host.name == name)
            .map(|host| host.player_id)
    };
    assert_eq!(slot_of("alice"), Some(1));
    assert_eq!(slot_of("carol"), Some(-1));
}

#[test]
fn no_turn_is_released_until_the_controller_resumes() {
    let mut h = alice_back();
    let held = h.input(turn_sealed(ALICE, STOPPED_AT + 4));
    assert!(
        !held.iter().any(|e| matches!(
            e,
            Effect::Send {
                msg: WireMessage::TurnSealed(_),
                ..
            }
        )),
        "nothing is released while held"
    );

    let effects = chat(&mut h, ALICE, "!resume");
    assert!(matches!(h.server(), AnyServer::InGame(_)));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::Send {
            peer: ALICE,
            msg: WireMessage::TurnSealed(TurnSealed { turn, .. })
        } if *turn == STOPPED_AT + 1
    )));
    assert!(effects.iter().any(|e| matches!(
        e,
        Effect::Send {
            peer: ALICE,
            msg: WireMessage::PlayerPause(PlayerPause { pause: false, .. })
        }
    )));
    assert!(
        !effects.iter().any(|e| matches!(
            e,
            Effect::Send {
                msg: WireMessage::Chat(c),
                ..
            } if c.message == "!resume"
        )),
        "the command is not relayed"
    );
}

#[test]
fn only_the_controller_resumes_until_the_grace_is_over() {
    let mut h = alice_back();
    h.admit(BOB, "bob");
    load(&mut h, BOB);
    let refused = chat(&mut h, BOB, "!resume");
    assert!(matches!(h.server(), AnyServer::Resuming(_)));
    assert!(
        chats_to(&refused, BOB)
            .iter()
            .any(|c| c.starts_with("Only alice (the controller)")),
        "{:?}",
        chats_to(&refused, BOB)
    );

    h.advance(TimeDelta::seconds(1));
    h.advance(TimeDelta::minutes(6));
    chat(&mut h, BOB, "!resume");
    assert!(matches!(h.server(), AnyServer::InGame(_)));
}

#[test]
fn a_client_still_loading_cannot_resume() {
    let (mut h, _) = resumed(config(), None);
    h.admit(ALICE, "alice");
    let effects = chat(&mut h, ALICE, "!resume");
    assert!(chats_to(&effects, ALICE).is_empty());
    assert!(matches!(h.server(), AnyServer::Resuming(_)));
}

#[test]
fn a_failed_rebuild_abandons_the_match() {
    let (mut h, effects) = resumed(config(), None);
    let id = restore_id(&effects);
    h.admit(ALICE, "alice");
    let effects = h.input(Input::Checkpointed {
        id,
        state: None,
        players: Vec::new(),
    });
    assert!(effects.contains(&Effect::SaveStatus(Status::Abandoned)));
    assert!(effects.contains(&Effect::GameOver));
}

#[test]
fn nobody_coming_back_abandons_the_match() {
    let (mut h, effects) = resumed(config(), None);
    restored(&mut h, restore_id(&effects));
    h.advance(TimeDelta::seconds(1));
    let early = h.advance(TimeDelta::minutes(14));
    assert!(!early.contains(&Effect::GameOver));
    let late = h.advance(TimeDelta::minutes(2));
    assert!(late.contains(&Effect::SaveStatus(Status::Abandoned)));
    assert!(late.contains(&Effect::GameOver));
}

#[test]
fn a_returning_player_keeps_the_match_waiting() {
    let mut h = alice_back();
    h.advance(TimeDelta::seconds(1));
    let effects = h.advance(TimeDelta::minutes(20));
    assert!(!effects.contains(&Effect::GameOver));
}

#[test]
fn the_waiting_list_is_announced() {
    let h = alice_back();
    let chats = chats_to(h.log(), ALICE);
    assert!(
        chats.iter().any(|c| c
            == "Match restored at turn 10 after a server restart. Waiting for: bob. alice (the controller) types !resume to continue."),
        "{chats:?}"
    );
}

#[test]
fn stop_while_resuming_saves_again() {
    let h = alice_back();
    assert!(h.server().saved_on_stop());
    let effects = h.stop();
    assert!(effects.contains(&Effect::SaveStatus(Status::Stopped)));
}

#[test]
fn a_resumed_match_journals_from_where_it_stopped() {
    let mut h = alice_back();
    chat(&mut h, ALICE, "!resume");
    let effects = h.input(turn_sealed(ALICE, STOPPED_AT + 5));
    let saved: Vec<u32> = h
        .log()
        .iter()
        .chain(effects.iter())
        .filter_map(|e| match e {
            Effect::SaveTurn { turn, .. } => Some(*turn),
            _ => None,
        })
        .collect();
    assert_eq!(saved.first(), Some(&(STOPPED_AT + 1)));
}
