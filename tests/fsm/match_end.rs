// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// F9 without a sidecar: the resign rule and the post-game phase it leads to.
// The checkpoint path that also ends a match needs pyrogenesis and is out of
// scope here.

use chrono::TimeDelta;
use rusty_enet::PeerID;

use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;
use veredus::relay::turn::INITIAL_READY_TURN;

use crate::harness::Harness;
use crate::harness::chats_to;
use crate::harness::disconnects;
use crate::harness::recipients_of;
use crate::harness::resign_data;
use crate::harness::turn_sealed;

fn settings(players: usize, victory: &str) -> Vec<u8> {
    let player_data = vec!["{}"; players].join(",");
    format!(r#"{{"settings":{{"PlayerData":[{player_data}],"VictoryConditions":[{victory}]}}}}"#)
        .into_bytes()
}

fn resign(peer: PeerID, player: i32) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::PlayerCommand(PlayerCommand {
            client: 1,
            player,
            turn: INITIAL_READY_TURN + 1,
            data: resign_data(),
        }),
    }
}

fn match_ended(effects: &[Effect]) -> Vec<Option<u32>> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::MatchEnded { checkpoint } => Some(*checkpoint),
            _ => None,
        })
        .collect()
}

fn game_over(effects: &[Effect]) -> bool {
    effects.iter().any(|e| matches!(e, Effect::GameOver))
}

fn two_player_post_game(config: Config) -> (Harness, PeerID, PeerID) {
    let mut h = Harness::with_config(config);
    let players = h.start_match(
        &[("Alice", Some(1)), ("Bob", Some(2))],
        &settings(2, r#""conquest""#),
    );
    let (alice, bob) = (players[0].0, players[1].0);
    h.input(resign(bob, 2));
    assert!(matches!(h.server(), AnyServer::PostGame(_)));
    (h, alice, bob)
}

// Ending early withdraws a live match from the lobby and stops admitting
// its players back; never ending leaves a decided match listed and holding
// its accounts until the last client quits. A resign claiming someone
// else's slot must count for nothing, or any player could end the match for
// the other side.
#[test]
fn resign_leaving_one_player_enters_post_game() {
    // An endless match has no winner to declare.
    {
        let mut h = Harness::new();
        let players = h.start_match(&[("Alice", Some(1)), ("Bob", Some(2))], &settings(2, ""));
        h.input(resign(players[1].0, 2));
        assert!(
            matches!(h.server(), AnyServer::InGame(_)),
            "a resign must not end a match with no victory condition"
        );
    }

    let mut h = Harness::new();
    let players = h.start_match(
        &[("Alice", Some(1)), ("Bob", Some(2)), ("Carol", Some(3))],
        &settings(3, r#""conquest""#),
    );
    let (alice, bob, carol) = (players[0].0, players[1].0, players[2].0);

    let effects = h.input(resign(bob, 1));
    assert!(effects.is_empty(), "a resign for another slot is dropped");
    assert!(matches!(h.server(), AnyServer::InGame(_)));

    let effects = h.input(resign(bob, 2));
    assert!(
        match_ended(&effects).is_empty(),
        "two players are still in it"
    );
    assert!(matches!(h.server(), AnyServer::InGame(_)));

    let effects = h.input(resign(carol, 3));
    assert_eq!(
        match_ended(&effects),
        vec![None],
        "the second resign decides the match, once and without a checkpoint"
    );
    assert!(matches!(h.server(), AnyServer::PostGame(_)));
    for peer in [alice, bob, carol] {
        assert!(
            chats_to(&effects, peer)
                .iter()
                .any(|line| line.starts_with("The match is over.")),
            "{peer:?} should be told the match is over"
        );
    }
}

// Admitting a joiner into a decided match costs a snapshot for a game that
// is about to close. Stopping turn release would freeze the victory screen
// for the clients still watching. And a post-game that never closes holds
// the lobby account forever.
#[test]
fn post_game_refuses_joiners_and_closes_on_linger_or_empty() {
    let (mut h, alice, bob) = two_player_post_game(Config {
        post_game_linger: TimeDelta::seconds(60),
        ..Config::default()
    });

    let dave = PeerID(9);
    h.connect(dave);
    h.syn_ack(dave);
    let effects = h.send_authenticate(dave, "Dave", "");
    assert_eq!(
        disconnects(&effects),
        vec![(dave, DisconnectReason::MatchInProgress)]
    );
    h.enet_confirms_disconnect(dave);

    h.input(turn_sealed(alice, INITIAL_READY_TURN + 1));
    let effects = h.input(turn_sealed(bob, INITIAL_READY_TURN + 1));
    assert_eq!(
        recipients_of(&effects, |m| matches!(m, WireMessage::TurnSealed(_))),
        [alice, bob].into_iter().collect(),
        "turns keep releasing after the match is decided"
    );

    let t0 = h.now;
    assert!(
        !game_over(&h.tick_at(t0)),
        "the first tick only anchors the linger"
    );
    // A backward step re-anchors, so the linger now runs from t0 - 10 s.
    assert!(!game_over(&h.tick_at(t0 - TimeDelta::seconds(10))));
    assert!(!game_over(&h.tick_at(t0 + TimeDelta::seconds(49))));
    assert!(
        game_over(&h.tick_at(t0 + TimeDelta::seconds(50))),
        "the linger ends 60 s after the re-anchored start"
    );

    // With nobody left there is nothing to linger for.
    let (mut h, alice, bob) = two_player_post_game(Config::default());
    let t0 = h.now;
    assert!(!game_over(&h.tick_at(t0)));
    h.enet_confirms_disconnect(alice);
    h.enet_confirms_disconnect(bob);
    assert!(
        game_over(&h.tick_at(t0 + TimeDelta::seconds(1))),
        "an empty post-game closes on the next tick"
    );
}
