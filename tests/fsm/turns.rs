// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 5.4.

use rusty_enet::PeerID;

use veredus::relay::messages::Guid;
use veredus::relay::messages::Kicked;
use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::PreGameStatus;
use veredus::relay::messages::StartSettings;
use veredus::relay::messages::StateHash;
use veredus::relay::messages::TurnSealed;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;
use veredus::relay::turn::INITIAL_READY_TURN;

use crate::harness::Harness;
use crate::harness::disconnects;

const READY: u8 = 1;

// A client seals this many turns ahead of the one it is about to simulate
// (server_fsm.rs's own COMMAND_DELAY, which is not exported since nothing
// outside the FSM needs it on the wire).
const COMMAND_DELAY: u32 = 4;

struct Player {
    peer: PeerID,
    guid: Guid,
}

// Admits each named entry in order (the first becomes controller, since it
// authenticates with the default empty controller secret first), assigns a
// slot to the ones that ask for one and leaves the rest as observers, marks
// everyone ready, starts the match and drives every player through
// LOADED_GAME so the server ends up InGame.
fn start_match(h: &mut Harness, spec: &[(&str, Option<i8>)]) -> Vec<Player> {
    let mut players = Vec::new();
    let controller = PeerID(100);
    for (i, (name, slot)) in spec.iter().enumerate() {
        let peer = PeerID(100 + i);
        let guid = h.admit(peer, name);
        if let Some(slot) = slot {
            h.map_player_id_to_slot(controller, *slot, &guid);
        }
        players.push(Player { peer, guid });
    }
    for p in &players {
        h.input(Input::Received {
            peer: p.peer,
            msg: WireMessage::PreGameStatus(PreGameStatus {
                guid: p.guid.clone(),
                status: READY,
            }),
        });
    }
    h.input(Input::Received {
        peer: controller,
        msg: WireMessage::StartSettings(StartSettings {
            init_attributes: Vec::new(),
        }),
    });
    assert!(matches!(h.server(), AnyServer::Loading(_)));
    for p in &players {
        h.input(Input::Received {
            peer: p.peer,
            msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
        });
    }
    assert!(matches!(h.server(), AnyServer::InGame(_)));
    players
}

fn turn_sealed(peer: PeerID, turn: u32) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::TurnSealed(TurnSealed {
            turn,
            turn_length: 200,
        }),
    }
}

fn state_hash(peer: PeerID, turn: u32, hash: Vec<u8>) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::StateHash(StateHash { turn, hash }),
    }
}

// A broadcast reaches every in-game session, so one release shows up as
// several identical effects, always consecutive in one call's output;
// dedup collapses those without hiding a genuinely repeated turn number.
fn turn_sealed_effects(effects: &[Effect]) -> Vec<u32> {
    let mut turns: Vec<u32> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                msg: WireMessage::TurnSealed(t),
                ..
            } => Some(t.turn),
            _ => None,
        })
        .collect();
    turns.dedup();
    turns
}

// A broadcast reaches every in-game session, so one mismatch event shows up
// as several identical effects; this counts distinct events, not recipients.
fn wrong_hash_players(effects: &[Effect]) -> Vec<Vec<String>> {
    let mut events: Vec<Vec<String>> = Vec::new();
    for e in effects {
        if let Effect::Send {
            msg: WireMessage::WrongHashPlayers(w),
            ..
        } = e
            && !events.contains(&w.player_names)
        {
            events.push(w.player_names.clone());
        }
    }
    events
}

// T1.10: the two counters start apart, and a joiner resuming mid-match uses
// a different formula from a fresh start. An edit to COMMAND_DELAY,
// INITIAL_READY_TURN or the `ready_turn + COMMAND_DELAY - 1` expression
// desyncs the very next client to touch it, and the server reports the
// *joiner* as the mismatched client, which sends debugging in the wrong
// direction.
#[test]
fn turn_counters_start_and_resume_at_the_right_offsets() {
    let mut h = Harness::new();
    let players = start_match(&mut h, &[("Alice", Some(1)), ("Bob", Some(2))]);
    let alice = players[0].peer;
    let bob = players[1].peer;

    // At start: register(INITIAL_READY_TURN, FIRST_SIMULATED_TURN). The
    // first accepted seal is INITIAL_READY_TURN + 1; one below or above
    // that is out of sequence.
    let effects = h.input(turn_sealed(alice, INITIAL_READY_TURN));
    assert_eq!(
        disconnects(&effects),
        vec![(alice, DisconnectReason::OutOfSequenceTurnSeal)]
    );
    let effects = h.input(turn_sealed(alice, INITIAL_READY_TURN + 2));
    assert_eq!(
        disconnects(&effects),
        vec![(alice, DisconnectReason::OutOfSequenceTurnSeal)]
    );

    // The first accepted hash is for turn 1 (FIRST_SIMULATED_TURN + 1).
    let effects = h.input(state_hash(bob, 0, vec![0xAA]));
    assert_eq!(
        disconnects(&effects),
        vec![(bob, DisconnectReason::OutOfSequenceStateHash)]
    );
    let effects = h.input(state_hash(bob, 2, vec![0xAA]));
    assert_eq!(
        disconnects(&effects),
        vec![(bob, DisconnectReason::OutOfSequenceStateHash)]
    );

    // Advance the match to a turn R > INITIAL_READY_TURN by sealing three
    // turns from both players.
    let ready_turn = INITIAL_READY_TURN + 3;
    for turn in (INITIAL_READY_TURN + 1)..=ready_turn {
        h.input(turn_sealed(alice, turn));
        h.input(turn_sealed(bob, turn));
    }
    assert!(
        !turn_sealed_effects(h.log())
            .into_iter()
            .filter(|t| *t == ready_turn)
            .collect::<Vec<_>>()
            .is_empty(),
        "the scripted seals should have released up to turn {ready_turn}"
    );

    // A joiner resuming at R registers seal R + COMMAND_DELAY and hash R + 1
    // as its first accepted values.
    let carol = PeerID(200);
    h.admit(carol, "Carol");
    h.input(Input::Received {
        peer: carol,
        msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
    });

    let expected_seal = ready_turn + COMMAND_DELAY;
    let effects = h.input(turn_sealed(carol, expected_seal - 1));
    assert_eq!(
        disconnects(&effects),
        vec![(carol, DisconnectReason::OutOfSequenceTurnSeal)]
    );
    let effects = h.input(turn_sealed(carol, expected_seal + 1));
    assert_eq!(
        disconnects(&effects),
        vec![(carol, DisconnectReason::OutOfSequenceTurnSeal)]
    );

    let expected_hash = ready_turn + 1;
    let effects = h.input(state_hash(carol, expected_hash - 1, vec![0xBB]));
    assert_eq!(
        disconnects(&effects),
        vec![(carol, DisconnectReason::OutOfSequenceStateHash)]
    );
    let effects = h.input(state_hash(carol, expected_hash + 1, vec![0xBB]));
    assert_eq!(
        disconnects(&effects),
        vec![(carol, DisconnectReason::OutOfSequenceStateHash)]
    );
}

// T1.11: TurnManager::blocks lets an observer stay out of the way entirely
// (observer_lag_limit: None), or block once it falls `limit` turns behind.
// The two failure modes this guards against are opposite and both bad: a
// match that never advances, or a turn counter that runs away with nobody
// left to send seals to.
#[test]
fn observer_does_not_block_release_until_the_lag_limit() {
    // With no limit, an observer that never seals a turn must never hold up
    // the players who do.
    {
        let mut h = Harness::new();
        let players = start_match(
            &mut h,
            &[("Alice", Some(1)), ("Bob", Some(2)), ("Carol", None)],
        );
        let alice = players[0].peer;
        let bob = players[1].peer;

        for turn in (INITIAL_READY_TURN + 1)..=(INITIAL_READY_TURN + 3) {
            h.input(turn_sealed(alice, turn));
            h.input(turn_sealed(bob, turn));
        }
        let released = turn_sealed_effects(h.log());
        for turn in (INITIAL_READY_TURN + 1)..=(INITIAL_READY_TURN + 3) {
            assert!(
                released.contains(&turn),
                "turn {turn} should have released without the never-sealing observer holding it up"
            );
        }
    }

    // With a limit of 2, the observer starts blocking once the gap reaches
    // it.
    {
        let mut h = Harness::with_config(Config {
            observer_lag_limit: Some(2),
            observer_delay_turns: 0,
            ..Config::default()
        });
        let players = start_match(
            &mut h,
            &[("Alice", Some(1)), ("Bob", Some(2)), ("Carol", None)],
        );
        let alice = players[0].peer;
        let bob = players[1].peer;

        // Turn R+1: gap is 0, does not yet block.
        h.input(turn_sealed(alice, INITIAL_READY_TURN + 1));
        let effects = h.input(turn_sealed(bob, INITIAL_READY_TURN + 1));
        assert_eq!(turn_sealed_effects(&effects), vec![INITIAL_READY_TURN + 1]);

        // Turn R+2: gap is 1, still does not block.
        h.input(turn_sealed(alice, INITIAL_READY_TURN + 2));
        let effects = h.input(turn_sealed(bob, INITIAL_READY_TURN + 2));
        assert_eq!(turn_sealed_effects(&effects), vec![INITIAL_READY_TURN + 2]);

        // Turn R+3: the gap this release would create reaches the limit, so
        // no further turn is released once the players outrun the observer
        // by 2.
        h.input(turn_sealed(alice, INITIAL_READY_TURN + 3));
        let effects = h.input(turn_sealed(bob, INITIAL_READY_TURN + 3));
        assert!(
            turn_sealed_effects(&effects).is_empty(),
            "the observer is now 2 turns behind, so release should stall, got {effects:?}"
        );
    }
}

// The empty-clients guard: with nobody registered there is nothing to wait
// for, but also nobody to send a seal to, so release must hold rather than
// run the turn counter away. Reachable once every session in an ongoing
// match has left. Without the guard this would not fail cleanly: it would
// hang, since `everyone_ahead` on an empty client set is vacuously true.
#[test]
fn release_holds_with_no_clients_registered() {
    let mut h = Harness::new();
    let players = start_match(&mut h, &[("Alice", Some(1)), ("Bob", Some(2))]);
    let alice = players[0].peer;
    let bob = players[1].peer;

    h.enet_confirms_disconnect(alice);
    let effects = h.enet_confirms_disconnect(bob);

    assert!(
        effects.is_empty(),
        "the last departure from an empty match must produce nothing, got {effects:?}"
    );
}

// T1.12: release is a `while` loop for exactly this reason. Turning it back
// into an `if` would make the match advance one turn per subsequent seal
// and crawl for the rest of the game.
#[test]
fn a_departure_can_release_several_turns_at_once() {
    let mut h = Harness::new();
    let players = start_match(&mut h, &[("Alice", Some(1)), ("Bob", Some(2))]);
    let alice = players[0].peer;
    let bob = players[1].peer;

    // Alice races ahead while Bob stalls at the initial ready turn, so
    // nothing releases yet.
    for turn in (INITIAL_READY_TURN + 1)..=(INITIAL_READY_TURN + 3) {
        let effects = h.input(turn_sealed(alice, turn));
        assert!(turn_sealed_effects(&effects).is_empty());
    }

    let effects = h.enet_confirms_disconnect(bob);
    let released = turn_sealed_effects(&effects);
    assert_eq!(
        released,
        vec![
            INITIAL_READY_TURN + 1,
            INITIAL_READY_TURN + 2,
            INITIAL_READY_TURN + 3
        ],
        "Bob's departure should release every turn Alice had already sealed, in order, in one go"
    );
}

// T1.13: the window from 4.2, asserted on purpose. Context::disconnect does
// not remove the session, so a kicked player keeps blocking release until
// ENet confirms the disconnect; "fixing" that to remove the session eagerly
// would drop it before its slot broadcast and before ENet has flushed the
// reliable queue.
#[test]
fn kicked_player_still_blocks_release_until_enet_confirms() {
    let mut h = Harness::new();
    let players = start_match(&mut h, &[("Alice", Some(1)), ("Bob", Some(2))]);
    let alice = players[0].peer;
    let bob = players[1].peer;

    let kick_effects = h.input(Input::Received {
        peer: alice,
        msg: WireMessage::Kicked(Kicked {
            name: "Bob".to_string(),
            ban: false,
        }),
    });
    assert_eq!(
        disconnects(&kick_effects),
        vec![(bob, DisconnectReason::Kicked)]
    );

    let effects = h.input(turn_sealed(alice, INITIAL_READY_TURN + 1));
    assert!(
        turn_sealed_effects(&effects).is_empty(),
        "a kicked-but-not-yet-confirmed player must still block release, got {effects:?}"
    );

    let effects = h.enet_confirms_disconnect(bob);
    assert_eq!(
        turn_sealed_effects(&effects),
        vec![INITIAL_READY_TURN + 1],
        "release should proceed as soon as ENet confirms the departure"
    );
}

// T1.14: compare() returns None while out_of_sync is non-empty, and
// recheck_pending resumes only once it is empty. Clearing out_of_sync too
// eagerly would give one WRONG_HASH_PLAYERS per turn, for every remaining
// turn in the match, for a desync that has already been reported.
#[test]
fn desync_latches_comparison_off_until_the_desynced_peer_leaves() {
    let mut h = Harness::new();
    let players = start_match(
        &mut h,
        &[("Alice", Some(1)), ("Bob", Some(2)), ("Carol", Some(3))],
    );
    let alice = players[0].peer;
    let bob = players[1].peer;
    let carol = players[2].peer;

    // Turn 1: Bob's hash disagrees with Alice's (the reference, by lowest
    // client id); Carol agrees. Reported exactly once.
    h.input(state_hash(alice, 1, vec![1]));
    h.input(state_hash(bob, 1, vec![2]));
    let effects = h.input(state_hash(carol, 1, vec![1]));
    assert_eq!(wrong_hash_players(&effects), vec![vec!["Bob".to_string()]]);

    // Turn 2: a genuine new mismatch between Alice and Carol, but the
    // outstanding desync from turn 1 must suppress it entirely.
    h.input(state_hash(alice, 2, vec![9]));
    h.input(state_hash(bob, 2, vec![3]));
    let effects = h.input(state_hash(carol, 2, vec![7]));
    assert!(
        wrong_hash_players(&effects).is_empty(),
        "an outstanding desync must suppress comparison entirely, got {effects:?}"
    );

    // Bob leaves. Comparison resumes, but only for turns reported from now
    // on: the turn 2 hashes arrived while latched and were never kept, or a
    // desynced player that plays on would grow them for the whole match.
    let effects = h.enet_confirms_disconnect(bob);
    assert!(
        wrong_hash_players(&effects).is_empty(),
        "hashes reported while latched must not be compared later, got {effects:?}"
    );
}
