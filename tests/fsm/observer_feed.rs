// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// F22: observers watch the match a fixed number of turns behind the
// players, served from the match log rather than from the live stream.

use veredus::enet::PeerID;
use veredus::relay::messages::Flare;
use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;
use veredus::relay::turn::INITIAL_READY_TURN;

use crate::harness::Harness;
use crate::harness::turn_sealed;

const DELAY: u32 = 3;

// Alice and Bob play, Olga watches on the delayed feed.
fn delayed_match() -> (Harness, PeerID, PeerID, PeerID) {
    let mut h = Harness::with_config(Config {
        observer_delay_turns: DELAY,
        ..Config::default()
    });
    let players = h.start_match(&[("Alice", Some(1)), ("Bob", Some(2)), ("Olga", None)], &[]);
    (h, players[0].0, players[1].0, players[2].0)
}

// Both players seal `turn`, releasing it live. The observer never seals.
fn release(h: &mut Harness, alice: PeerID, bob: PeerID, turn: u32) -> Vec<Effect> {
    let mut effects = h.input(turn_sealed(alice, turn));
    effects.extend(h.input(turn_sealed(bob, turn)));
    effects
}

fn seals_to(effects: &[Effect], peer: PeerID) -> Vec<u32> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                peer: p,
                msg: WireMessage::TurnSealed(t),
            } if *p == peer => Some(t.turn),
            _ => None,
        })
        .collect()
}

fn flare_to(effects: &[Effect], peer: PeerID) -> bool {
    effects
        .iter()
        .any(|e| matches!(e, Effect::Send { peer: p, msg: WireMessage::Flare(_) } if *p == peer))
}

// An observer who sees the live stream can relay it to a player in time to
// matter; one who gets a turn's commands after its seal, or not at all,
// desyncs. And an observer that blocked release would let any spectator
// stall the match.
#[test]
fn delayed_observer_sees_turns_and_commands_delay_behind_live() {
    let (mut h, alice, bob, olga) = delayed_match();
    let command_turn = INITIAL_READY_TURN + 2;
    let command = PlayerCommand {
        client: 1,
        player: 1,
        turn: command_turn,
        data: vec![0xAA],
    };
    let effects = h.input(Input::Received {
        peer: alice,
        msg: WireMessage::PlayerCommand(command.clone()),
    });
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::Send { peer, .. } if *peer == olga)),
        "a command is not relayed live to the delayed observer"
    );

    let first = INITIAL_READY_TURN + 1;
    for turn in first..first + 6 {
        let effects = release(&mut h, alice, bob, turn);
        assert_eq!(
            seals_to(&effects, alice),
            vec![turn],
            "the observer never seals, and must not hold the players back"
        );
        let due: Vec<u32> = if turn >= first + DELAY {
            vec![turn - DELAY]
        } else {
            Vec::new()
        };
        assert_eq!(
            seals_to(&effects, olga),
            due,
            "with live at {turn} the observer is sealed exactly {DELAY} turns behind"
        );
    }

    let stream: Vec<WireMessage> = h
        .sent_to(olga)
        .into_iter()
        .filter(|m| {
            matches!(
                m,
                WireMessage::TurnSealed(_) | WireMessage::PlayerCommand(_)
            )
        })
        .collect();
    let at = stream
        .iter()
        .position(|m| *m == WireMessage::PlayerCommand(command.clone()))
        .expect("the observer is sent the command with its turn");
    assert!(
        matches!(&stream[at + 1], WireMessage::TurnSealed(t) if t.turn == command_turn),
        "the command must arrive right before its own turn's seal, got {stream:?}"
    );
}

// Once no player is left there is nothing to hide, and an observer still
// three turns behind would never see the match end.
#[test]
fn feed_drains_to_live_once_no_player_remains() {
    let (mut h, alice, bob, olga) = delayed_match();
    let first = INITIAL_READY_TURN + 1;
    let last = first + 5;
    for turn in first..=last {
        release(&mut h, alice, bob, turn);
    }

    let effects = h.enet_confirms_disconnect(alice);
    assert!(
        seals_to(&effects, olga).is_empty(),
        "Bob is still playing, so the delay holds"
    );

    let effects = h.enet_confirms_disconnect(bob);
    assert_eq!(
        seals_to(&effects, olga),
        ((last - DELAY + 1)..=last).collect::<Vec<_>>(),
        "the last player leaving drains every outstanding turn at once"
    );
}

// A flare marks where a player is looking now. Sent live to a delayed
// observer, it shows where things are going to be.
#[test]
fn flare_reaches_delayed_observer_with_its_turn() {
    let (mut h, alice, bob, olga) = delayed_match();
    let first = INITIAL_READY_TURN + 1;
    release(&mut h, alice, bob, first);
    let flare_turn = first;

    let effects = h.input(Input::Received {
        peer: alice,
        msg: WireMessage::Flare(Flare {
            guid: Default::default(),
            position_x: "1".to_string(),
            position_y: "2".to_string(),
            position_z: "3".to_string(),
        }),
    });
    assert!(flare_to(&effects, bob), "players see a flare at once");
    assert!(!flare_to(&effects, olga), "the observer does not, yet");

    for turn in (first + 1)..(flare_turn + DELAY) {
        let effects = release(&mut h, alice, bob, turn);
        assert!(
            !flare_to(&effects, olga),
            "live at {turn}: the feed has not reached turn {flare_turn}"
        );
    }
    let effects = release(&mut h, alice, bob, flare_turn + DELAY);
    assert_eq!(seals_to(&effects, olga), vec![flare_turn]);
    assert!(
        flare_to(&effects, olga),
        "the flare arrives with the turn it was raised at"
    );
}
