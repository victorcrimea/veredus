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

fn kick(controller: PeerID, name: &str, ban: bool) -> Input {
    Input::Received {
        peer: controller,
        msg: WireMessage::Kicked(Kicked {
            name: name.to_string(),
            ban,
        }),
    }
}

// TEST_PLAN.md section 6. A controller that can kick itself strands the
// match with no controller; an unknown name or a non-controller sender must
// cost nobody their connection; and a ban that keys on the name alone is
// dodged by renaming, one that keys on the address alone by reconnecting
// from elsewhere under the same name.
#[test]
fn kick_cannot_target_self_or_unknown_names_and_ban_blocks_name_and_ip() {
    let mut h = Harness::new();
    let alice = PeerID(1);
    h.admit(alice, "Alice");
    let bob = PeerID(2);
    h.admit(bob, "Bob");
    let carol = PeerID(3);
    h.admit(carol, "Carol");

    for (sender, target) in [(alice, "Alice"), (alice, "Nobody"), (bob, "Carol")] {
        let effects = h.input(kick(sender, target, true));
        assert!(
            effects.is_empty(),
            "kick of {target} from {sender:?} must do nothing, got {effects:?}"
        );
    }

    let effects = h.input(kick(alice, "Bob", true));
    assert_eq!(disconnects(&effects), vec![(bob, DisconnectReason::Banned)]);
    h.enet_confirms_disconnect(bob);

    // Same name, new address.
    let dave = PeerID(4);
    h.connect(dave);
    h.syn_ack(dave);
    let effects = h.send_authenticate(dave, "Bob", "");
    assert_eq!(
        disconnects(&effects),
        vec![(dave, DisconnectReason::Banned)]
    );

    // Same address, refused before it gets a session or a handshake.
    let effects = h.input(Input::Connected {
        peer: bob,
        addr: Harness::addr_for(bob),
    });
    assert_eq!(disconnects(&effects), vec![(bob, DisconnectReason::Banned)]);
    assert!(
        h.sent_to(bob)
            .iter()
            .filter(|m| matches!(m, WireMessage::Syn(_)))
            .count()
            == 1,
        "a banned address must not be sent a second SYN"
    );
}

// TEST_PLAN.md section 6. The controller flag reaches a client only in its
// AUTHENTICATE_RESULT, so a role left behind by a departed controller would
// strand setup forever; and promoting someone already connected would hand
// the role to a client that never learns it has it.
#[test]
fn controller_role_is_released_on_leave_and_never_promoted_in_place() {
    for release in [true, false] {
        let mut h = Harness::with_config(Config {
            release_controller_on_leave: release,
            ..Config::default()
        });
        let alice = PeerID(1);
        h.admit(alice, "Alice");
        let bob = PeerID(2);
        h.admit(bob, "Bob");

        h.enet_confirms_disconnect(alice);

        let effects = h.input(Input::Received {
            peer: bob,
            msg: WireMessage::ResetPregameStatus,
        });
        assert!(
            effects.is_empty(),
            "release={release}: Bob was connected before the role freed up, \
             so he must not act as controller, got {effects:?}"
        );

        let carol = PeerID(3);
        h.connect(carol);
        h.syn_ack(carol);
        let result = h.authenticate(carol, "Carol");
        assert_eq!(
            result.is_controller, release,
            "release={release}: the next arrival takes the role only when it was released"
        );
    }
}

// TEST_PLAN.md section 6. Lobby auth matches a renamed client back to its
// account through auth::suffix_stripped, so the separator and the counter
// are part of the contract, not cosmetics.
#[test]
fn duplicate_names_count_from_two_with_paren_suffix() {
    let mut h = Harness::with_config(Config {
        allow_duplicate_names: true,
        ..Config::default()
    });
    let guids: Vec<Guid> = (1..=3).map(|i| h.admit(PeerID(i), "Bob")).collect();

    let slots = last_player_slots(&h);
    let names: Vec<&str> = guids
        .iter()
        .map(|g| {
            slots
                .hosts
                .iter()
                .find(|host| &host.guid == g)
                .map(|host| host.name.as_str())
                .expect("every admitted Bob should be in the slot list")
        })
        .collect();
    assert_eq!(names, vec!["Bob", "Bob (2)", "Bob (3)"]);
    for name in names {
        assert_eq!(auth::suffix_stripped(name), "Bob");
    }
}

// TEST_PLAN.md section 6. A session still in the handshake has no slot and
// no name, and is exactly the one a filtered shutdown would forget, leaving
// its client waiting for an ENet timeout.
#[test]
fn shutdown_disconnects_every_session_including_unauthenticated() {
    let mut h = Harness::new();
    let admitted = PeerID(1);
    h.admit(admitted, "Alice");
    let synced = PeerID(2);
    h.connect(synced);
    h.syn_ack(synced);
    let fresh = PeerID(3);
    h.connect(fresh);

    let effects = h.shutdown();

    let mut got: Vec<(PeerID, DisconnectReason)> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::DisconnectNow { peer, reason } => Some((*peer, *reason)),
            _ => None,
        })
        .collect();
    got.sort_by_key(|(p, _)| p.0);
    assert_eq!(
        got,
        vec![
            (admitted, DisconnectReason::ServerShuttingDown),
            (synced, DisconnectReason::ServerShuttingDown),
            (fresh, DisconnectReason::ServerShuttingDown),
        ]
    );
}
