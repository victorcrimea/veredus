// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// F18: the relay holds the match with its own PLAYER_PAUSE while a player
// is away, and lets go once nobody is left worth waiting for.

use std::collections::HashSet;

use chrono::TimeDelta;
use rusty_enet::PeerID;

use veredus::relay::messages::Guid;
use veredus::relay::messages::Kicked;
use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::WireMessage;
use veredus::relay::monitor::PeerStats;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;
use veredus::relay::turn::INITIAL_READY_TURN;

use crate::harness::Harness;
use crate::harness::resign_data;

fn stats(peer: PeerID, silent: TimeDelta) -> PeerStats {
    PeerStats {
        peer,
        mean_rtt: TimeDelta::milliseconds(50),
        since_last_received: silent,
    }
}

// Every PLAYER_PAUSE in `effects` as (recipient, guid, pause).
fn pauses(effects: &[Effect]) -> Vec<(PeerID, Guid, bool)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                peer,
                msg: WireMessage::PlayerPause(pp),
            } => Some((*peer, pp.guid.clone(), pp.pause)),
            _ => None,
        })
        .collect()
}

const CONQUEST_3: &[u8] =
    br#"{"settings":{"PlayerData":[{},{},{}],"VictoryConditions":["conquest"]}}"#;

// Holding under a player's own GUID would let that player's client lift it,
// and a hold nobody lifts once the player is back freezes the match for
// good.
#[test]
fn silent_player_holds_the_match_and_return_lifts_it() {
    let mut h = Harness::new();
    let players = h.start_match(&[("Alice", Some(1)), ("Bob", Some(2))], &[]);
    let (alice, alice_guid) = players[0].clone();
    let (bob, bob_guid) = players[1].clone();
    let t0 = h.now;
    let quiet = TimeDelta::zero();

    let effects = h.tick_with_stats(t0, vec![stats(alice, quiet), stats(bob, quiet)]);
    assert!(pauses(&effects).is_empty(), "nobody is away yet");

    let effects = h.tick_with_stats(
        t0 + TimeDelta::seconds(1),
        vec![stats(alice, quiet), stats(bob, TimeDelta::seconds(11))],
    );
    let held = pauses(&effects);
    assert_eq!(
        held.iter().map(|(p, _, _)| *p).collect::<HashSet<_>>(),
        [alice, bob].into_iter().collect(),
        "the hold reaches every in-game client, got {effects:?}"
    );
    let relay_guid = held[0].1.clone();
    for (_, guid, pause) in &held {
        assert!(*pause);
        assert_eq!(*guid, relay_guid, "one hold, one GUID");
    }
    assert!(
        relay_guid != alice_guid && relay_guid != bob_guid,
        "the hold must be under the relay's own GUID, not a player's"
    );

    let effects = h.tick_with_stats(
        t0 + TimeDelta::seconds(2),
        vec![stats(alice, quiet), stats(bob, quiet)],
    );
    let lifted = pauses(&effects);
    assert!(!lifted.is_empty(), "Bob is back, so the hold is lifted");
    for (_, guid, pause) in lifted {
        assert!(!pause);
        assert_eq!(guid, relay_guid, "the lift must name the GUID that held");
    }
}

// Waiting on a player who resigned or was kicked holds the match for
// someone who is never coming back, and burns everyone's evening until the
// quota runs out.
#[test]
fn resigned_or_kicked_players_are_not_waited_for() {
    let three_players = || {
        let mut h = Harness::new();
        let players = h.start_match(
            &[("Alice", Some(1)), ("Bob", Some(2)), ("Carol", Some(3))],
            CONQUEST_3,
        );
        let peers: Vec<PeerID> = players.iter().map(|(p, _)| *p).collect();
        let t0 = h.now;
        h.tick_at(t0);
        (h, peers)
    };

    // The control: a plain departure is waited for, so the two cases below
    // are quiet for the right reason.
    let (mut h, peers) = three_players();
    h.enet_confirms_disconnect(peers[2]);
    let effects = h.advance(TimeDelta::seconds(1));
    assert!(
        pauses(&effects).iter().any(|(_, _, pause)| *pause),
        "a player who just left is waited for, got {effects:?}"
    );

    let (mut h, peers) = three_players();
    h.input(Input::Received {
        peer: peers[1],
        msg: WireMessage::PlayerCommand(PlayerCommand {
            client: 2,
            player: 2,
            turn: INITIAL_READY_TURN + 1,
            data: resign_data(),
        }),
    });
    assert!(
        matches!(h.server(), AnyServer::InGame(_)),
        "two players remain, so the match goes on"
    );
    h.enet_confirms_disconnect(peers[1]);
    let effects = h.advance(TimeDelta::seconds(1));
    assert!(
        pauses(&effects).is_empty(),
        "a resigned player is not waited for, got {effects:?}"
    );

    let (mut h, peers) = three_players();
    h.input(Input::Received {
        peer: peers[0],
        msg: WireMessage::Kicked(Kicked {
            name: "Carol".to_string(),
            ban: false,
        }),
    });
    h.enet_confirms_disconnect(peers[2]);
    let effects = h.advance(TimeDelta::seconds(1));
    assert!(
        pauses(&effects).is_empty(),
        "a kicked player is not waited for, got {effects:?}"
    );
}
