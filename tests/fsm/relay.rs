// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 6: the relays that rewrite or filter what a client
// claims before anyone else sees it.

use std::collections::HashSet;

use veredus::enet::PeerID;
use veredus::relay::messages::Chat;
use veredus::relay::messages::Flare;
use veredus::relay::messages::Guid;
use veredus::relay::messages::Joined;
use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::PlayerPause;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;
use veredus::relay::turn::INITIAL_READY_TURN;

use crate::harness::Harness;
use crate::harness::recipients_of;

fn live_config() -> Config {
    Config {
        observer_delay_turns: 0,
        ..Config::default()
    }
}

fn chat(peer: PeerID, sender_guid: Guid, receivers: Vec<Guid>) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::Chat(Chat {
            sender_guid,
            message: "hello".to_string(),
            receivers,
        }),
    }
}

fn relayed_chats(effects: &[Effect]) -> Vec<Chat> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                msg: WireMessage::Chat(c),
                ..
            } => Some(c.clone()),
            _ => None,
        })
        .collect()
}

fn command(peer: PeerID, player: i32) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::PlayerCommand(PlayerCommand {
            client: 1,
            player,
            turn: INITIAL_READY_TURN + 1,
            data: vec![0xAA],
        }),
    }
}

// A client picks its own sender_guid, so without the rewrite anyone could
// speak as anyone else, and a non-empty receiver list on the relayed copy
// would tell every recipient who else was addressed.
#[test]
fn chat_sender_guid_is_rewritten_and_receivers_emptied() {
    let mut h = Harness::new();
    let alice = PeerID(1);
    let alice_guid = h.admit(alice, "Alice");
    let bob = PeerID(2);
    let bob_guid = h.admit(bob, "Bob");

    let effects = h.input(chat(bob, alice_guid, Vec::new()));

    assert_eq!(
        recipients_of(&effects, |m| matches!(m, WireMessage::Chat(_))),
        [alice, bob].into_iter().collect(),
        "an untargeted chat reaches every setup session, the sender included"
    );
    for c in relayed_chats(&effects) {
        assert_eq!(
            c.sender_guid, bob_guid,
            "the claimed sender must be replaced"
        );
        assert!(
            c.receivers.is_empty(),
            "the relayed copy must carry no receivers"
        );
    }
}

// Targeted chat is a private channel: a leak to anyone not listed, the
// sender included, is a message read by someone it was not for.
#[test]
fn targeted_chat_reaches_only_listed_receivers() {
    let mut h = Harness::new();
    let alice = PeerID(1);
    let alice_guid = h.admit(alice, "Alice");
    let bob = PeerID(2);
    let bob_guid = h.admit(bob, "Bob");
    let carol = PeerID(3);
    h.admit(carol, "Carol");

    let effects = h.input(chat(alice, alice_guid, vec![bob_guid]));

    assert_eq!(
        recipients_of(&effects, |m| matches!(m, WireMessage::Chat(_))),
        [bob].into_iter().collect()
    );
}

// Without cheats a command is only relayed for the sender's own slot. An
// observer sits in the slot table as -1, so a command claiming player -1 is
// the case a naive "slot == player" comparison would let through.
#[test]
fn command_for_another_players_slot_is_dropped() {
    let mut h = Harness::with_config(live_config());
    let players = h.start_match(
        &[("Alice", Some(1)), ("Bob", Some(2)), ("Carol", None)],
        &[],
    );
    let (alice, bob, carol) = (players[0].0, players[1].0, players[2].0);

    let effects = h.input(command(bob, 1));
    assert!(
        effects.is_empty(),
        "a command for someone else's slot must be dropped silently, got {effects:?}"
    );

    let effects = h.input(command(carol, -1));
    assert!(
        effects.is_empty(),
        "an observer must never match player -1, got {effects:?}"
    );

    // The echo to the sender is what makes a client execute its own command.
    let effects = h.input(command(bob, 2));
    assert_eq!(
        recipients_of(&effects, |m| matches!(m, WireMessage::PlayerCommand(_))),
        [alice, bob, carol].into_iter().collect()
    );
}

// Same spoofing guard as chat: a flare shows the sender's name on the map.
#[test]
fn flare_guid_is_rewritten() {
    let mut h = Harness::with_config(live_config());
    let players = h.start_match(&[("Alice", Some(1)), ("Bob", Some(2))], &[]);
    let (alice_guid, bob) = (players[0].1.clone(), players[1].0);
    let bob_guid = players[1].1.clone();

    let effects = h.input(Input::Received {
        peer: bob,
        msg: WireMessage::Flare(Flare {
            guid: alice_guid,
            position_x: "1".to_string(),
            position_y: "2".to_string(),
            position_z: "3".to_string(),
        }),
    });

    let flares: Vec<Flare> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                msg: WireMessage::Flare(f),
                ..
            } => Some(f.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        flares.len(),
        2,
        "the flare reaches both players: {effects:?}"
    );
    for f in flares {
        assert_eq!(f.guid, bob_guid, "the claimed flare owner must be replaced");
    }
}

// A joiner missed every PLAYER_PAUSE sent before it arrived. Without the
// replay it would run the match while everyone else sits paused, and desync
// at the next hash.
#[test]
fn joined_replays_the_current_paused_set() {
    let mut h = Harness::with_config(live_config());
    let players = h.start_match(&[("Alice", Some(1)), ("Bob", Some(2))], &[]);
    let (alice, alice_guid) = players[0].clone();

    h.input(Input::Received {
        peer: alice,
        msg: WireMessage::PlayerPause(PlayerPause {
            guid: alice_guid.clone(),
            pause: true,
        }),
    });

    let carol = PeerID(3);
    let carol_guid = h.admit(carol, "Carol");
    h.serve_snapshot();
    h.input(Input::Received {
        peer: carol,
        msg: WireMessage::LoadedGame(LoadedGame {
            current_turn: INITIAL_READY_TURN,
        }),
    });
    let effects = h.input(Input::Received {
        peer: carol,
        msg: WireMessage::Joined(Joined { guid: carol_guid }),
    });

    let pauses_to_carol: HashSet<Guid> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                peer,
                msg: WireMessage::PlayerPause(pp),
            } if *peer == carol && pp.pause => Some(pp.guid.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        pauses_to_carol,
        [alice_guid].into_iter().collect(),
        "the joiner must be told about the pause already in place"
    );
}
