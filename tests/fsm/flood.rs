// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Per-peer flood limits on chat, flares, commands and pause.

use std::net::Ipv4Addr;

use chrono::TimeDelta;

use veredus::enet::PeerID;
use veredus::relay::messages::Authenticate;
use veredus::relay::messages::Chat;
use veredus::relay::messages::Flare;
use veredus::relay::messages::Guid;
use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::PlayerCommand;
use veredus::relay::messages::PlayerPause;
use veredus::relay::messages::PreGameStatus;
use veredus::relay::messages::StartSettings;
use veredus::relay::messages::SynAck;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;

use crate::harness::Harness;
use crate::harness::READY;
use crate::harness::chats_to;
use crate::harness::disconnects;
use crate::harness::expect_send;
use crate::harness::recipients_of;

const TWO_PLAYERS: &[u8] = br#"{"settings":{"PlayerData":[{},{}]}}"#;
const ALICE: PeerID = PeerID(1);
const BOB: PeerID = PeerID(2);

// Kicks are off unless a test is about them, so a drop can be asserted on
// its own without the kick arriving first.
fn quiet() -> Config {
    Config {
        flood_kick_multiple: 0,
        ..Config::default()
    }
}

// Alice plays 1 and Bob plays 2. The closing tick is what a live game does
// every 10 ms, and the pause budget learns the time only from in-game ticks.
fn two_player_match(config: Config) -> (Harness, Guid, Guid) {
    let mut h = Harness::with_config(config);
    let players = h.start_match(&[("Alice", Some(1)), ("Bob", Some(2))], TWO_PLAYERS);
    h.tick_at(h.now);
    (h, players[0].1.clone(), players[1].1.clone())
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

fn flare(h: &mut Harness, peer: PeerID) -> Vec<Effect> {
    h.input(Input::Received {
        peer,
        msg: WireMessage::Flare(Flare {
            guid: Guid(String::new()),
            position_x: "1".to_string(),
            position_y: "2".to_string(),
            position_z: "3".to_string(),
        }),
    })
}

fn command(h: &mut Harness, peer: PeerID, player: i32, turn: u32, bytes: usize) -> Vec<Effect> {
    h.input(Input::Received {
        peer,
        msg: WireMessage::PlayerCommand(PlayerCommand {
            client: 1,
            player,
            turn,
            data: vec![0; bytes],
        }),
    })
}

fn pause(h: &mut Harness, peer: PeerID, guid: &Guid, pause: bool) -> Vec<Effect> {
    h.input(Input::Received {
        peer,
        msg: WireMessage::PlayerPause(PlayerPause {
            guid: guid.clone(),
            pause,
        }),
    })
}

fn next_turn(h: &Harness) -> u32 {
    h.server().snapshot().ready_turn + 1
}

fn chats(effects: &[Effect]) -> usize {
    recipients_of(effects, |m| matches!(m, WireMessage::Chat(_))).len()
}

fn flares(effects: &[Effect]) -> usize {
    recipients_of(effects, |m| matches!(m, WireMessage::Flare(_))).len()
}

fn commands(effects: &[Effect]) -> usize {
    recipients_of(effects, |m| matches!(m, WireMessage::PlayerCommand(_))).len()
}

fn kicked(effects: &[Effect], peer: PeerID) -> bool {
    disconnects(effects).contains(&(peer, DisconnectReason::Kicked))
}

// The budget is not readable from outside, but every accepted pause tells
// everyone how much of it is left.
fn budget_announced(effects: &[Effect]) -> String {
    chats_to(effects, BOB)
        .into_iter()
        .find(|c| c.contains("pause budget left"))
        .expect("expected the pause announcement")
}

#[test]
fn chat_within_burst_reaches_everyone() {
    let (mut h, _, _) = two_player_match(Config {
        chat_per_sec: 1,
        chat_burst: 3,
        ..quiet()
    });
    for _ in 0..3 {
        assert_eq!(chats(&chat(&mut h, ALICE, "hi")), 2);
    }
}

#[test]
fn chat_past_burst_is_dropped_until_refilled() {
    let (mut h, _, _) = two_player_match(Config {
        chat_per_sec: 1,
        chat_burst: 2,
        ..quiet()
    });
    chat(&mut h, ALICE, "a");
    chat(&mut h, ALICE, "b");
    let dropped = chat(&mut h, ALICE, "c");
    assert_eq!(chats(&dropped), 0);
    assert!(disconnects(&dropped).is_empty());

    h.advance(TimeDelta::seconds(1));
    assert_eq!(chats(&chat(&mut h, ALICE, "d")), 2);
}

#[test]
fn chat_limit_is_per_peer() {
    let (mut h, _, _) = two_player_match(Config {
        chat_per_sec: 1,
        chat_burst: 1,
        ..quiet()
    });
    chat(&mut h, ALICE, "a");
    assert_eq!(chats(&chat(&mut h, ALICE, "b")), 0);
    assert_eq!(chats(&chat(&mut h, BOB, "c")), 2);
}

#[test]
fn chat_flood_is_kicked() {
    let (mut h, _, _) = two_player_match(Config {
        chat_per_sec: 1,
        chat_burst: 2,
        flood_kick_multiple: 2,
        ..Config::default()
    });
    chat(&mut h, ALICE, "a");
    chat(&mut h, ALICE, "b");
    assert!(!kicked(&chat(&mut h, ALICE, "c"), ALICE));
    assert!(kicked(&chat(&mut h, ALICE, "d"), ALICE));
}

#[test]
fn chat_rate_zero_never_limits() {
    let (mut h, _, _) = two_player_match(Config {
        chat_per_sec: 0,
        chat_burst: 0,
        flood_kick_multiple: 1,
        ..Config::default()
    });
    for _ in 0..100 {
        let effects = chat(&mut h, ALICE, "hi");
        assert_eq!(chats(&effects), 2);
        assert!(disconnects(&effects).is_empty());
    }
}

#[test]
fn chat_longer_than_the_cap_is_dropped() {
    let (mut h, _, _) = two_player_match(Config {
        chat_max_chars: 5,
        ..quiet()
    });
    assert_eq!(chats(&chat(&mut h, ALICE, "12345")), 2);
    assert_eq!(chats(&chat(&mut h, ALICE, "123456")), 0);
    // Counted in characters, not in bytes.
    assert_eq!(
        chats(&chat(&mut h, ALICE, "\u{e9}\u{e9}\u{e9}\u{e9}\u{e9}")),
        2
    );
}

#[test]
fn chat_length_cap_zero_allows_any_length() {
    let (mut h, _, _) = two_player_match(Config {
        chat_max_chars: 0,
        ..quiet()
    });
    assert_eq!(chats(&chat(&mut h, ALICE, &"x".repeat(10_000))), 2);
}

#[test]
fn flare_past_burst_is_dropped_until_refilled() {
    let (mut h, _, _) = two_player_match(Config {
        flare_per_sec: 1,
        flare_burst: 2,
        ..quiet()
    });
    assert_eq!(flares(&flare(&mut h, ALICE)), 2);
    assert_eq!(flares(&flare(&mut h, ALICE)), 2);
    assert_eq!(flares(&flare(&mut h, ALICE)), 0);
    h.advance(TimeDelta::seconds(1));
    assert_eq!(flares(&flare(&mut h, ALICE)), 2);
}

#[test]
fn flare_flood_is_kicked() {
    let (mut h, _, _) = two_player_match(Config {
        flare_per_sec: 1,
        flare_burst: 1,
        flood_kick_multiple: 1,
        ..Config::default()
    });
    assert!(!kicked(&flare(&mut h, ALICE), ALICE));
    assert!(kicked(&flare(&mut h, ALICE), ALICE));
}

#[test]
fn commands_past_the_per_turn_count_are_dropped() {
    let (mut h, _, _) = two_player_match(Config {
        commands_per_turn: 2,
        ..quiet()
    });
    let turn = next_turn(&h);
    assert_eq!(commands(&command(&mut h, ALICE, 1, turn, 10)), 2);
    assert_eq!(commands(&command(&mut h, ALICE, 1, turn, 10)), 2);
    assert_eq!(commands(&command(&mut h, ALICE, 1, turn, 10)), 0);
    // The next turn has a quota of its own, and so does the other player.
    assert_eq!(commands(&command(&mut h, ALICE, 1, turn + 1, 10)), 2);
    assert_eq!(commands(&command(&mut h, BOB, 2, turn, 10)), 2);
}

#[test]
fn commands_past_the_per_turn_bytes_are_dropped() {
    let (mut h, _, _) = two_player_match(Config {
        commands_per_turn: 0,
        command_bytes_per_turn: 100,
        ..quiet()
    });
    let turn = next_turn(&h);
    assert_eq!(commands(&command(&mut h, ALICE, 1, turn, 60)), 2);
    assert_eq!(commands(&command(&mut h, ALICE, 1, turn, 40)), 2);
    assert_eq!(commands(&command(&mut h, ALICE, 1, turn, 1)), 0);
}

#[test]
fn command_flood_is_kicked() {
    let (mut h, _, _) = two_player_match(Config {
        commands_per_turn: 1,
        flood_kick_multiple: 2,
        ..Config::default()
    });
    let turn = next_turn(&h);
    assert!(!kicked(&command(&mut h, ALICE, 1, turn, 1), ALICE));
    assert!(!kicked(&command(&mut h, ALICE, 1, turn, 1), ALICE));
    assert!(kicked(&command(&mut h, ALICE, 1, turn, 1), ALICE));
}

#[test]
fn command_caps_zero_never_limit() {
    let (mut h, _, _) = two_player_match(Config {
        commands_per_turn: 0,
        command_bytes_per_turn: 0,
        flood_kick_multiple: 1,
        ..Config::default()
    });
    let turn = next_turn(&h);
    for _ in 0..100 {
        assert_eq!(commands(&command(&mut h, ALICE, 1, turn, 1000)), 2);
    }
}

// Drives the real hosted-AI start: Alice alone against an AI in slot 2, the
// start held until the AI host dials in over loopback under the name the
// server asked it to spawn with.
#[test]
fn ai_host_commands_are_not_capped() {
    let mut h = Harness::with_config(Config {
        hosted_ai: true,
        commands_per_turn: 1,
        flood_kick_multiple: 1,
        ..Config::default()
    });
    let alice = h.admit(ALICE, "Alice");
    h.map_player_id_to_slot(ALICE, 1, &alice);
    h.input(Input::Received {
        peer: ALICE,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: alice,
            status: READY,
        }),
    });
    let held = h.input(Input::Received {
        peer: ALICE,
        msg: WireMessage::StartSettings(StartSettings {
            init_attributes: br#"{"settings":{"PlayerData":[{},{"AI":"petra"}]}}"#.to_vec(),
        }),
    });
    let name = held
        .iter()
        .find_map(|e| match e {
            Effect::SpawnAiHost { name } => Some(name.clone()),
            _ => None,
        })
        .expect("expected the AI host to be spawned");

    let ai = PeerID(9);
    let connected = h.input(Input::Connected {
        peer: ai,
        addr: Ipv4Addr::LOCALHOST,
    });
    let syn = expect_send(&connected, ai, |m| match m {
        WireMessage::Syn(s) => Some(s.clone()),
        _ => None,
    })
    .expect("expected a Syn to the AI host");
    h.input(Input::Received {
        peer: ai,
        msg: WireMessage::SynAck(SynAck {
            magic_response: syn.magic,
            protocol_version: syn.protocol_version,
            engine_version: syn.engine_version,
            enabled_mods: syn.enabled_mods,
        }),
    });
    h.input(Input::Received {
        peer: ai,
        msg: WireMessage::Authenticate(Authenticate {
            name,
            password: String::new(),
            controller_secret: String::new(),
        }),
    });
    for peer in [ALICE, ai] {
        h.input(Input::Received {
            peer,
            msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
        });
    }
    assert!(matches!(h.server(), AnyServer::InGame(_)));

    let turn = next_turn(&h);
    for _ in 0..10 {
        let effects = command(&mut h, ai, 2, turn, 10);
        assert_eq!(commands(&effects), 2);
        assert!(disconnects(&effects).is_empty());
    }
    // The same cap still holds for the human.
    assert_eq!(commands(&command(&mut h, ALICE, 1, turn, 10)), 2);
    assert!(kicked(&command(&mut h, ALICE, 1, turn, 10), ALICE));
}

#[test]
fn repeated_pause_is_not_relayed() {
    let (mut h, alice, _) = two_player_match(quiet());
    let first = pause(&mut h, ALICE, &alice, true);
    assert_eq!(
        recipients_of(&first, |m| matches!(m, WireMessage::PlayerPause(_))).len(),
        1
    );
    assert_eq!(chats(&first), 2);
    assert!(pause(&mut h, ALICE, &alice, true).is_empty());
}

#[test]
fn repeated_unpause_is_not_relayed() {
    let (mut h, alice, _) = two_player_match(quiet());
    assert!(pause(&mut h, ALICE, &alice, false).is_empty());
    pause(&mut h, ALICE, &alice, true);
    let lifted = pause(&mut h, ALICE, &alice, false);
    assert_eq!(
        recipients_of(&lifted, |m| matches!(m, WireMessage::PlayerPause(_))).len(),
        1
    );
    assert!(pause(&mut h, ALICE, &alice, false).is_empty());
}

#[test]
fn short_pause_costs_the_minimum_charge() {
    let (mut h, alice, _) = two_player_match(Config {
        pause_budget: TimeDelta::seconds(180),
        pause_min_charge: TimeDelta::seconds(5),
        ..quiet()
    });
    let first = pause(&mut h, ALICE, &alice, true);
    assert_eq!(
        budget_announced(&first),
        "Alice paused. 180s of pause budget left."
    );
    h.advance(TimeDelta::seconds(1));
    pause(&mut h, ALICE, &alice, false);
    let second = pause(&mut h, ALICE, &alice, true);
    assert_eq!(
        budget_announced(&second),
        "Alice paused. 175s of pause budget left."
    );
}

#[test]
fn long_pause_costs_only_its_length() {
    let (mut h, alice, _) = two_player_match(Config {
        pause_budget: TimeDelta::seconds(180),
        pause_min_charge: TimeDelta::seconds(5),
        ..quiet()
    });
    pause(&mut h, ALICE, &alice, true);
    h.advance(TimeDelta::seconds(8));
    pause(&mut h, ALICE, &alice, false);
    let second = pause(&mut h, ALICE, &alice, true);
    assert_eq!(
        budget_announced(&second),
        "Alice paused. 172s of pause budget left."
    );
}

#[test]
fn pause_toggling_drains_the_budget_until_refused() {
    let (mut h, alice, _) = two_player_match(Config {
        pause_budget: TimeDelta::seconds(12),
        pause_min_charge: TimeDelta::seconds(5),
        ..quiet()
    });
    for _ in 0..2 {
        pause(&mut h, ALICE, &alice, true);
        pause(&mut h, ALICE, &alice, false);
    }
    let third = pause(&mut h, ALICE, &alice, true);
    assert_eq!(
        budget_announced(&third),
        "Alice paused. 2s of pause budget left."
    );
    pause(&mut h, ALICE, &alice, false);

    // Out of budget: Alice is told to lift her own overlay, Bob hears nothing.
    let refused = pause(&mut h, ALICE, &alice, true);
    assert_eq!(chats_to(&refused, ALICE), ["You are out of pause budget."]);
    assert_eq!(
        expect_send(&refused, ALICE, |m| match m {
            WireMessage::PlayerPause(p) => Some(p.pause),
            _ => None,
        }),
        Some(false)
    );
    assert!(!recipients_of(&refused, |_| true).contains(&BOB));
}
