// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 5.5.

use rusty_enet::PeerID;

use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::PreGameStatus;
use veredus::relay::messages::StartSettings;
use veredus::relay::messages::TurnSealed;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::Input;
use veredus::relay::turn::INITIAL_READY_TURN;

use crate::harness::Harness;

const READY: u8 = 1;
// Config::default's turn_length_ms, which is what every TurnSealed on the
// wire carries for the life of one match (FrozenSettings never changes it).
const TURN_LENGTH_MS: u16 = 200;

fn turn_sealed(peer: PeerID, turn: u32) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::TurnSealed(TurnSealed {
            turn,
            turn_length: TURN_LENGTH_MS,
        }),
    }
}

// T1.15: Server<InGame>::on_loaded_game is the most delicate stream in the
// tree. Its failure mode is a client-side assert or a desync, since the
// client asserts each TURN_SEALED it receives is exactly its ready turn plus
// one. What breaks it is the `upper` expression, the `turn <= ready_turn`
// guard, or the inclusive range bound, and each gives a different wrong
// stream, none of them obvious from reading the diff.
#[test]
fn join_replay_is_contiguous_from_the_snapshot_turn() {
    let mut h = Harness::with_config(Config {
        observer_delay_turns: 0,
        ..Config::default()
    });

    let alice = PeerID(1);
    let alice_guid = h.admit(alice, "Alice");
    h.map_player_id_to_slot(alice, 1, &alice_guid);
    let bob = PeerID(2);
    let bob_guid = h.admit_as_player(bob, "Bob", 2, alice);

    h.input(Input::Received {
        peer: alice,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: alice_guid,
            status: READY,
        }),
    });
    h.input(Input::Received {
        peer: bob,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: bob_guid,
            status: READY,
        }),
    });
    h.input(Input::Received {
        peer: alice,
        msg: WireMessage::StartSettings(StartSettings {
            init_attributes: Vec::new(),
        }),
    });
    h.input(Input::Received {
        peer: alice,
        msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
    });
    h.input(Input::Received {
        peer: bob,
        msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
    });

    let ready_turn = INITIAL_READY_TURN + 3;
    for turn in (INITIAL_READY_TURN + 1)..=ready_turn {
        h.input(turn_sealed(alice, turn));
        h.input(turn_sealed(bob, turn));
    }

    // Alice has already sealed commands two turns beyond the ready turn
    // (the COMMAND_DELAY window); the replay must not withhold them just
    // because they are ahead of what has been released.
    let beyond = ready_turn + 2;
    h.input(Input::Received {
        peer: alice,
        msg: WireMessage::PlayerCommand(PlayerCommand {
            client: 1,
            player: 1,
            turn: beyond,
            data: vec![0xAA],
        }),
    });

    let carol = PeerID(3);
    h.admit(carol, "Carol");
    h.serve_snapshot();

    let snapshot_turn = INITIAL_READY_TURN;
    h.input(Input::Received {
        peer: carol,
        msg: WireMessage::LoadedGame(LoadedGame {
            current_turn: snapshot_turn,
        }),
    });

    let stream: Vec<WireMessage> = h
        .sent_to(carol)
        .into_iter()
        .filter(|m| {
            matches!(
                m,
                WireMessage::TurnSealed(_)
                    | WireMessage::PlayerCommand(_)
                    | WireMessage::LoadedGame(_)
            )
        })
        .collect();

    let expected = vec![
        WireMessage::TurnSealed(TurnSealed {
            turn: snapshot_turn + 1,
            turn_length: TURN_LENGTH_MS,
        }),
        WireMessage::TurnSealed(TurnSealed {
            turn: snapshot_turn + 2,
            turn_length: TURN_LENGTH_MS,
        }),
        WireMessage::TurnSealed(TurnSealed {
            turn: ready_turn,
            turn_length: TURN_LENGTH_MS,
        }),
        WireMessage::PlayerCommand(PlayerCommand {
            client: 1,
            player: 1,
            turn: beyond,
            data: vec![0xAA],
        }),
        WireMessage::LoadedGame(LoadedGame {
            current_turn: ready_turn,
        }),
    ];

    assert_eq!(
        stream, expected,
        "the joiner should see seals contiguous from snapshot_turn+1 through ready_turn, \
         the already-recorded command beyond it, and a final LOADED_GAME at ready_turn"
    );
}
