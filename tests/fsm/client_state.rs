// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Without a sidecar a match is saved with a state pulled from a playing
// client now and then, and resumed from it.

use std::sync::Arc;

use veredus::enet::PeerID;
use veredus::relay::messages::Guid;
use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::PlayerPause;
use veredus::relay::messages::TurnSealed;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Idle;
use veredus::relay::server_fsm::Input;
use veredus::relay::server_fsm::Server;
use veredus::relay::turn::INITIAL_READY_TURN;
use veredus::savegame::SavedIdentity;
use veredus::savegame::SavedPlayer;
use veredus::savegame::SlotsSnapshot;
use veredus::savegame::resume::ResumeData;
use veredus::savegame::resume::SavedTurn;
use veredus::sidecar::BaseState;

use crate::harness::Harness;
use crate::harness::chats_to;
use crate::harness::disconnects;
use crate::harness::turn_sealed;

const SETTINGS: &[u8] = br#"{"settings":{"PlayerData":[{},{}],"VictoryConditions":["conquest"]}}"#;
const WITH_AI: &[u8] =
    br#"{"settings":{"PlayerData":[{},{"AI":"petra"}],"VictoryConditions":["conquest"]}}"#;
const ALICE: PeerID = PeerID(1);
const BOB: PeerID = PeerID(2);
const INTERVAL: u32 = 2;

fn pulling() -> Config {
    Config {
        saving: true,
        client_state_interval_turns: INTERVAL,
        ..Config::default()
    }
}

fn start(config: Config, settings: &[u8]) -> Harness {
    let mut h = Harness::with_config(config);
    h.start_match(&[("alice", Some(1)), ("bob", Some(2))], settings);
    h
}

fn release_through(h: &mut Harness, last: u32) {
    for turn in (INITIAL_READY_TURN + 1)..=last {
        h.input(turn_sealed(ALICE, turn));
        h.input(turn_sealed(BOB, turn));
    }
}

fn requests(h: &Harness) -> usize {
    h.log()
        .iter()
        .filter(|e| {
            matches!(
                e,
                Effect::Send {
                    msg: WireMessage::GamestateRequest(_),
                    ..
                }
            )
        })
        .count()
}

fn saved_states(effects: &[Effect]) -> Vec<u32> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::SaveState { turn, .. } => Some(*turn),
            _ => None,
        })
        .collect()
}

#[test]
fn a_client_state_is_pulled_every_interval() {
    let mut h = start(pulling(), SETTINGS);
    release_through(&mut h, INITIAL_READY_TURN + INTERVAL - 1);
    assert_eq!(requests(&h), 0, "not due yet");
    release_through(&mut h, INITIAL_READY_TURN + INTERVAL);
    assert_eq!(requests(&h), 1);
    let effects = h.serve_snapshot_at(INITIAL_READY_TURN + INTERVAL);
    assert_eq!(saved_states(&effects), vec![INITIAL_READY_TURN + INTERVAL]);
}

#[test]
fn no_pull_with_ai_players_or_a_sidecar() {
    let mut h = start(pulling(), WITH_AI);
    release_through(&mut h, INITIAL_READY_TURN + 3 * INTERVAL);
    assert_eq!(requests(&h), 0);

    let mut h = start(
        Config {
            sidecar_dumps: true,
            checkpoint_interval_turns: 0,
            ..pulling()
        },
        SETTINGS,
    );
    release_through(&mut h, INITIAL_READY_TURN + 3 * INTERVAL);
    assert_eq!(requests(&h), 0);
}

#[test]
fn no_pull_while_someone_is_paused() {
    let mut h = start(pulling(), SETTINGS);
    h.input(Input::Received {
        peer: ALICE,
        msg: WireMessage::PlayerPause(PlayerPause {
            guid: Guid(String::new()),
            pause: true,
        }),
    });
    release_through(&mut h, INITIAL_READY_TURN + 3 * INTERVAL);
    assert_eq!(requests(&h), 0);
}

#[test]
fn a_restart_is_promised_only_once_a_state_is_saved() {
    let mut h = start(pulling(), SETTINGS);
    release_through(&mut h, INITIAL_READY_TURN + INTERVAL);
    let before = start(pulling(), SETTINGS).stop();
    assert!(
        chats_to(&before, ALICE)
            .iter()
            .all(|c| c.starts_with("Server shutdown:"))
    );
    h.serve_snapshot_at(INITIAL_READY_TURN + INTERVAL);
    let after = h.stop();
    assert!(
        chats_to(&after, ALICE)
            .iter()
            .any(|c| c.starts_with("Server is restarting."))
    );
}

fn resumed_from_seed() -> Harness {
    let turns = (4..=10)
        .map(|turn| SavedTurn {
            turn,
            length: 200,
            commands: Vec::new(),
        })
        .collect();
    let data = ResumeData {
        settings: SETTINGS.to_vec(),
        turn_length_ms: 200,
        turns,
        hashes: Vec::new(),
        slots: SlotsSnapshot {
            players: vec![SavedPlayer {
                player_id: 1,
                uuid: "saved-alice".to_string(),
                name: "alice".to_string(),
                lobby_name: String::new(),
            }],
            controller: Some(SavedIdentity {
                name: "alice".to_string(),
                lobby_name: String::new(),
            }),
            ..SlotsSnapshot::default()
        },
        base: Some(BaseState {
            turn: 6,
            state: Arc::new(vec![1, 2, 3]),
        }),
        ai: None,
        lobby_map: None,
    };
    let server = Server::<Idle>::new(pulling()).resume(data);
    let (h, effects) = Harness::with_server(AnyServer::from(server));
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Checkpoint { .. })),
        "no sidecar, no restore run"
    );
    h
}

#[test]
fn a_resume_from_a_client_state_replays_to_the_stopped_turn() {
    let mut h = resumed_from_seed();
    h.admit(ALICE, "alice");
    assert!(
        h.sent_to(ALICE)
            .iter()
            .any(|m| matches!(m, WireMessage::Join(_))),
        "served the saved client state at once"
    );
    h.input(Input::Received {
        peer: ALICE,
        msg: WireMessage::LoadedGame(LoadedGame { current_turn: 6 }),
    });
    let seals: Vec<u32> = h
        .sent_to(ALICE)
        .iter()
        .filter_map(|m| match m {
            WireMessage::TurnSealed(TurnSealed { turn, .. }) => Some(*turn),
            _ => None,
        })
        .collect();
    assert_eq!(seals, vec![7, 8, 9, 10]);
    assert!(
        h.sent_to(ALICE)
            .contains(&WireMessage::LoadedGame(LoadedGame { current_turn: 10 }))
    );
}

#[test]
fn a_loaded_turn_outside_the_saved_range_is_refused() {
    let mut h = resumed_from_seed();
    h.admit(ALICE, "alice");
    let effects = h.input(Input::Received {
        peer: ALICE,
        msg: WireMessage::LoadedGame(LoadedGame { current_turn: 4 }),
    });
    assert_eq!(
        disconnects(&effects),
        vec![(ALICE, DisconnectReason::Kicked)]
    );
}
