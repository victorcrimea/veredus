// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 5.2.

use chrono::TimeDelta;
use rusty_enet::PeerID;

use veredus::relay::auth;
use veredus::relay::messages::Guid;
use veredus::relay::messages::Kicked;
use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::PlayerPause;
use veredus::relay::messages::PlayerSlots;
use veredus::relay::messages::PreGameStatus;
use veredus::relay::messages::StartSettings;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;
use veredus::relay::slots::UNASSIGNED;

use crate::harness::Harness;
use crate::harness::disconnects;
use crate::harness::expect_send;

// T1.4: `admit` rejects at `sessions >= max_sessions` with the arriving
// session already inserted, which is what leaves one peer free to be told
// the server is full. An off-by-one either way is either an early rejection
// or a full server that never explains itself.
#[test]
fn full_server_keeps_a_spare_peer() {
    let mut h = Harness::with_config(Config {
        max_sessions: 3,
        ..Config::default()
    });

    let first = PeerID(1);
    h.admit(first, "Alice");

    let second = PeerID(2);
    h.admit(second, "Bob");

    let third = PeerID(3);
    h.connect(third);
    h.syn_ack(third);
    let effects = h.send_authenticate(third, "Carol", "");

    assert_eq!(
        disconnects(&effects),
        vec![(third, DisconnectReason::ServerFull)]
    );
    assert!(
        expect_send(&effects, third, |m| match m {
            WireMessage::AuthenticateResult(r) => Some(r.clone()),
            _ => None,
        })
        .is_none(),
        "a rejected peer must not also receive an AuthenticateResult"
    );
}

// T1.5: broadcast_player_slots always appends the relay's own row after the
// sorted hosts. A client faults on a chat sender it cannot find in that
// list, so the row has to be present in every PLAYER_SLOTS the server ever
// sends, not just the one right after a call site someone remembered to
// check.
#[test]
fn every_player_slots_broadcast_carries_the_server_row() {
    let mut h = Harness::new();
    let expected_name = auth::server_display_name("SERVER");

    let controller = PeerID(1);
    let controller_guid = h.admit(controller, "Alice");
    h.map_player_id_to_slot(controller, 1, &controller_guid);

    let bob = PeerID(2);
    h.admit_as_player(bob, "Bob", 2, controller);

    h.input(Input::Received {
        peer: controller,
        msg: WireMessage::Kicked(Kicked {
            name: "Bob".to_string(),
            ban: false,
        }),
    });
    h.enet_confirms_disconnect(bob);

    let slots_messages: Vec<_> = h
        .log()
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                msg: WireMessage::PlayerSlots(ps),
                ..
            } => Some(ps.clone()),
            _ => None,
        })
        .collect();

    assert!(
        !slots_messages.is_empty(),
        "the scripted session should have produced at least one PLAYER_SLOTS"
    );
    for ps in &slots_messages {
        assert!(
            ps.hosts
                .iter()
                .any(|host| host.name == expected_name && host.player_id == UNASSIGNED),
            "PLAYER_SLOTS is missing the relay's own row: {ps:?}"
        );
    }
}

const READY: u8 = 1;

// Admits two players, marks both ready, starts the match and finishes
// loading, landing the server in InGame with both players holding slots.
fn two_player_match() -> (Harness, PeerID, Guid, PeerID, Guid) {
    let mut h = Harness::new();

    let a = PeerID(1);
    let a_guid = h.admit(a, "Alice");
    h.map_player_id_to_slot(a, 1, &a_guid);

    let b = PeerID(2);
    let b_guid = h.admit_as_player(b, "Bob", 2, a);

    h.input(Input::Received {
        peer: a,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: a_guid.clone(),
            status: READY,
        }),
    });
    h.input(Input::Received {
        peer: b,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: b_guid.clone(),
            status: READY,
        }),
    });

    h.input(Input::Received {
        peer: a,
        msg: WireMessage::StartSettings(StartSettings {
            init_attributes: Vec::new(),
        }),
    });
    assert!(
        matches!(h.server(), AnyServer::Loading(_)),
        "both players are ready, so start should move the server to Loading"
    );

    h.input(Input::Received {
        peer: a,
        msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
    });
    h.input(Input::Received {
        peer: b,
        msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
    });
    assert!(
        matches!(h.server(), AnyServer::InGame(_)),
        "both players have loaded, so the server should be InGame"
    );

    (h, a, a_guid, b, b_guid)
}

fn last_player_slots(h: &Harness) -> PlayerSlots {
    h.log()
        .iter()
        .rev()
        .find_map(|e| match e {
            Effect::Send {
                msg: WireMessage::PlayerSlots(ps),
                ..
            } => Some(ps.clone()),
            _ => None,
        })
        .expect("expected at least one PLAYER_SLOTS broadcast")
}

// T1.6: Slots::recoverable matches a disconnected entry by UUID first, then
// by name, and only when the slot is free. Without the `free` closure a
// returning client would take a slot out from under someone currently
// playing in it.
#[test]
fn slot_recovery_prefers_uuid_then_name_and_never_steals() {
    // Case 1: recovery by name works, and the recovered UUID carries the
    // departed player's pause quota over (pause_budget.rs's `inherit`),
    // because a reconnecting client authenticates under a fresh UUID.
    {
        let (mut h, _a, _a_guid, b, b_guid) = two_player_match();
        h.advance(TimeDelta::zero()); // anchor PauseBudget::check's clock.

        // Bob drains 30s of his own quota while Alice keeps playing, so
        // there is a non-default amount for recovery to carry over.
        h.input(Input::Received {
            peer: b,
            msg: WireMessage::PlayerPause(PlayerPause {
                guid: b_guid.clone(),
                pause: true,
            }),
        });
        h.advance(TimeDelta::seconds(30));
        h.input(Input::Received {
            peer: b,
            msg: WireMessage::PlayerPause(PlayerPause {
                guid: b_guid.clone(),
                pause: false,
            }),
        });
        h.enet_confirms_disconnect(b);

        let carol = PeerID(3);
        let carol_guid = h.admit(carol, "Bob");

        let slots = last_player_slots(&h);
        let carol_row = slots
            .hosts
            .iter()
            .find(|host| host.guid == carol_guid)
            .expect("Carol should appear in the slot list");
        assert_eq!(
            carol_row.player_id, 2,
            "a same-named arrival should recover the departed player's slot"
        );

        // Finish Carol's join so she can speak in-game, then confirm the
        // quota she inherited is Bob's drained 150s, not a fresh 180s.
        h.input(Input::Received {
            peer: carol,
            msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
        });
        let effects = h.input(Input::Received {
            peer: carol,
            msg: WireMessage::PlayerPause(PlayerPause {
                guid: carol_guid,
                pause: true,
            }),
        });
        let chat = effects.iter().find_map(|e| match e {
            Effect::Send {
                msg: WireMessage::Chat(c),
                ..
            } => Some(c.message.clone()),
            _ => None,
        });
        assert_eq!(
            chat.as_deref(),
            Some("Bob paused. 150s of pause budget left."),
            "recovered slot should inherit Bob's drained quota; effects: {effects:?}"
        );
    }

    // Case 2: a slot number that has since been handed to someone else must
    // never be handed back out just because a departed player's name
    // matches. MAP_PLAYER_ID_TO_SLOT is a setup-only message, so this
    // reshuffle has to happen before the match starts: Bob is assigned slot
    // 2 and leaves during setup, the controller hands slot 2 to Alice
    // instead, and only then does the match start and a same-named "Bob"
    // try to join. Reassigning the slot away from Bob's leftover entry
    // (slots.rs's `assign` clears every other entry holding that slot,
    // connected or not) is what leaves nothing for a same-named arrival to
    // recover into.
    {
        let mut h = Harness::new();

        let a = PeerID(1);
        let a_guid = h.admit(a, "Alice");
        h.map_player_id_to_slot(a, 1, &a_guid);

        let b = PeerID(2);
        let b_guid = h.admit(b, "Bob");
        h.map_player_id_to_slot(a, 2, &b_guid);
        h.enet_confirms_disconnect(b);

        h.map_player_id_to_slot(a, 2, &a_guid);

        h.input(Input::Received {
            peer: a,
            msg: WireMessage::PreGameStatus(PreGameStatus {
                guid: a_guid,
                status: READY,
            }),
        });
        h.input(Input::Received {
            peer: a,
            msg: WireMessage::StartSettings(StartSettings {
                init_attributes: Vec::new(),
            }),
        });
        h.input(Input::Received {
            peer: a,
            msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
        });
        assert!(matches!(h.server(), AnyServer::InGame(_)));

        let carol = PeerID(3);
        let carol_guid = h.admit(carol, "Bob");

        let slots = last_player_slots(&h);
        let carol_row = slots
            .hosts
            .iter()
            .find(|host| host.guid == carol_guid)
            .expect("Carol should appear in the slot list");
        assert_eq!(
            carol_row.player_id, UNASSIGNED,
            "a same-named arrival must never land in a slot a connected player now holds"
        );
    }
}
