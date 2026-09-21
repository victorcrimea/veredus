// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 5.3.

use rusty_enet::PeerID;

use veredus::relay::messages::GameSettings;
use veredus::relay::messages::MapPlayerIdToSlot;
use veredus::relay::messages::PreGameStatus;
use veredus::relay::messages::StartSettings;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::AnyServer;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;

use crate::harness::Harness;
use crate::harness::recipients_of;

const READY: u8 = 1;

fn start_settings() -> WireMessage {
    WireMessage::StartSettings(StartSettings {
        init_attributes: Vec::new(),
    })
}

// T1.7: on_start_settings has a comment saying clients observe three things
// in exactly this order: stale unauthenticated sessions dropped, PLAYER_SLOTS
// broadcast, START_SETTINGS relayed. A client that received START_SETTINGS
// before the final slot list would start the match with a stale roster.
#[test]
fn start_emits_disconnects_then_slots_then_start_settings() {
    let mut h = Harness::new();

    let controller = PeerID(1);
    let controller_guid = h.admit(controller, "Alice");
    h.map_player_id_to_slot(controller, 1, &controller_guid);
    h.input(Input::Received {
        peer: controller,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: controller_guid,
            status: READY,
        }),
    });

    // Connected but never authenticated, so it is stale when the match
    // starts.
    let stale = PeerID(2);
    h.connect(stale);

    let effects = h.input(Input::Received {
        peer: controller,
        msg: start_settings(),
    });

    let disconnect_pos = effects
        .iter()
        .position(|e| {
            matches!(
                e,
                Effect::Disconnect { peer, reason }
                    if *peer == stale && *reason == DisconnectReason::ServerLoading
            )
        })
        .expect("the stale session should be dropped with ServerLoading");
    let slots_pos = effects
        .iter()
        .position(|e| {
            matches!(
                e,
                Effect::Send {
                    msg: WireMessage::PlayerSlots(_),
                    ..
                }
            )
        })
        .expect("expected a PLAYER_SLOTS broadcast");
    let start_pos = effects
        .iter()
        .position(|e| {
            matches!(
                e,
                Effect::Send {
                    msg: WireMessage::StartSettings(_),
                    ..
                }
            )
        })
        .expect("expected a relayed START_SETTINGS");

    assert!(
        disconnect_pos < slots_pos,
        "the stale session must be dropped before the slot list goes out: {effects:?}"
    );
    assert!(
        slots_pos < start_pos,
        "the slot list must reach clients before START_SETTINGS: {effects:?}"
    );

    let slots_recipients = recipients_of(&effects, |m| matches!(m, WireMessage::PlayerSlots(_)));
    assert_eq!(slots_recipients, [controller].into_iter().collect());
    let start_recipients = recipients_of(&effects, |m| matches!(m, WireMessage::StartSettings(_)));
    assert_eq!(start_recipients, [controller].into_iter().collect());
}

// T1.8: rejecting an unready start outright is a deliberate deviation from
// the stock server, which relays the start anyway and then cannot accept the
// LOADED_GAMEs that follow, stranding every client on the loading screen.
// Slots::all_ready also requires observers to be ready, since it filters
// only on `connected`, not on slot assignment; relaxing that reintroduces
// the same stranded-loading-screen bug for an observer who never readies up.
#[test]
fn start_is_refused_until_every_connected_player_is_ready() {
    let mut h = Harness::new();

    let a = PeerID(1);
    let a_guid = h.admit(a, "Alice");
    h.map_player_id_to_slot(a, 1, &a_guid);

    let b = PeerID(2);
    let b_guid = h.admit_as_player(b, "Bob", 2, a);

    let effects = h.input(Input::Received {
        peer: a,
        msg: start_settings(),
    });
    assert!(
        effects.is_empty(),
        "start must be refused, not relayed, while a connected player is not ready"
    );
    assert!(matches!(h.server(), AnyServer::Setup(_)));

    h.input(Input::Received {
        peer: a,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: a_guid,
            status: READY,
        }),
    });
    h.input(Input::Received {
        peer: b,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: b_guid,
            status: READY,
        }),
    });

    // An observer holding no slot still counts.
    let carol = PeerID(3);
    let carol_guid = h.admit(carol, "Carol");

    let effects = h.input(Input::Received {
        peer: a,
        msg: start_settings(),
    });
    assert!(
        effects.is_empty(),
        "an unready observer should still block start, even though it holds no slot"
    );
    assert!(matches!(h.server(), AnyServer::Setup(_)));

    h.input(Input::Received {
        peer: carol,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid: carol_guid,
            status: READY,
        }),
    });

    let effects = h.input(Input::Received {
        peer: a,
        msg: start_settings(),
    });
    assert!(
        !effects.is_empty(),
        "start should succeed once every connected session is ready"
    );
    assert!(matches!(h.server(), AnyServer::Loading(_)));
}

// T1.9: require_controller returns NotController, which maps to no
// disconnect: a message from anyone but the controller is silently ignored.
// Turning that into a disconnect would let any client get any other client
// dropped by sending one message.
#[test]
fn non_controller_setup_messages_are_silently_ignored() {
    let mut h = Harness::new();

    let controller = PeerID(1);
    let controller_guid = h.admit(controller, "Alice");

    let bystander = PeerID(2);
    h.admit(bystander, "Bob");

    let messages = vec![
        start_settings(),
        WireMessage::GameSettings(GameSettings {
            data: vec![1, 2, 3],
        }),
        WireMessage::MapPlayerIdToSlot(MapPlayerIdToSlot {
            player_id: 1,
            guid: controller_guid,
        }),
        WireMessage::ResetPregameStatus,
    ];

    for msg in messages {
        let name = msg.name();
        let effects = h.input(Input::Received {
            peer: bystander,
            msg,
        });
        assert!(
            effects.is_empty(),
            "{name} from a non-controller must be silently ignored, got {effects:?}"
        );
        assert!(
            matches!(h.server(), AnyServer::Setup(_)),
            "{name} from a non-controller must not change the server's phase"
        );
    }
}
