// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 5.1.

use veredus::enet::PeerID;
use veredus::relay::messages::Kicked;
use veredus::relay::messages::SynAck;
use veredus::relay::messages::WireMessage;
use veredus::relay::password;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Input;

use crate::harness::Harness;
use crate::harness::disconnects;

// T1.1: on_authenticate runs loading, lobby name, password, reserved prefix,
// duplicate name, ban, capacity, in that order, and the order is
// wire-observable. Reordering the guards, or hoisting the ban check up to
// "fail fast", changes which reason a client's error dialog shows, and
// nothing else would catch it.

#[test]
fn wrong_password_beats_banned_name() {
    let hash = "server-secret";
    let mut h = Harness::with_config(Config {
        server_password_hash: hash.to_string(),
        ..Config::default()
    });

    let controller = PeerID(1);
    h.connect(controller);
    h.syn_ack(controller);
    let controller_password = password::hash(hash, b"Controller");
    let result = h.authenticate_with_password(controller, "Controller", &controller_password);
    assert!(result.is_controller);

    let target = PeerID(2);
    h.connect(target);
    h.syn_ack(target);
    let target_password = password::hash(hash, b"Eve");
    h.authenticate_with_password(target, "Eve", &target_password);

    // The controller bans "Eve" by name, but the session is only soft
    // disconnected: it is not removed until ENet confirms it (4.2).
    let kick_effects = h.input(Input::Received {
        peer: controller,
        msg: WireMessage::Kicked(Kicked {
            name: "Eve".to_string(),
            ban: true,
        }),
    });
    assert_eq!(
        disconnects(&kick_effects),
        vec![(target, DisconnectReason::Banned)]
    );

    // Free the name by removing the old session outright, so the next check
    // that would trip is the ban, not "name already in use".
    h.enet_confirms_disconnect(target);

    // A newcomer claims the banned name with the wrong password. If the
    // password check still runs first, the reason is Refused, not Banned.
    let newcomer = PeerID(3);
    h.connect(newcomer);
    h.syn_ack(newcomer);
    let effects = h.send_authenticate(newcomer, "Eve", "");
    assert_eq!(
        disconnects(&effects),
        vec![(newcomer, DisconnectReason::Refused)]
    );
}

#[test]
fn name_in_use_beats_banned_name() {
    let mut h = Harness::new();

    let controller = PeerID(1);
    h.admit(controller, "Controller");

    let target = PeerID(2);
    h.admit(target, "Eve");

    // Ban "Eve" without ever confirming the disconnect: the session, and so
    // the name, is still in use (4.2).
    let kick_effects = h.input(Input::Received {
        peer: controller,
        msg: WireMessage::Kicked(Kicked {
            name: "Eve".to_string(),
            ban: true,
        }),
    });
    assert_eq!(
        disconnects(&kick_effects),
        vec![(target, DisconnectReason::Banned)]
    );

    // A newcomer claims the same, still-occupied, banned name. If the
    // duplicate-name check still runs before the ban check, the reason is
    // NameInUse, not Banned.
    let newcomer = PeerID(3);
    h.connect(newcomer);
    h.syn_ack(newcomer);
    let effects = h.send_authenticate(newcomer, "Eve", "");
    assert_eq!(
        disconnects(&effects),
        vec![(newcomer, DisconnectReason::NameInUse)]
    );
}

// T1.2: a session that already holds a UUID is past the handshake. A second
// SYN_ACK must not reissue one, or the slot, the controller record and the
// paused set (all keyed off the old UUID) would be stranded.
#[test]
fn second_syn_ack_is_refused() {
    let mut h = Harness::new();
    let peer = PeerID(1);

    let syn = h.connect(peer);
    let _first_guid = h.syn_ack(peer);

    let second = WireMessage::SynAck(SynAck {
        magic_response: syn.magic,
        protocol_version: syn.protocol_version,
        engine_version: syn.engine_version.clone(),
        enabled_mods: syn.enabled_mods.clone(),
    });
    let effects = h.input(Input::Received { peer, msg: second });

    // WrongPhase maps to no disconnect reason, so the message is dropped and
    // the peer stays connected: no new Ack, no Disconnect, nothing at all.
    assert!(
        effects.is_empty(),
        "a second SynAck must be silently dropped, got {effects:?}"
    );
}

// T1.3: auth::reserved is checked against the sanitized name, and
// sanitization trims first, so leading whitespace must not smuggle the
// reserved prefix past the check. A change to that ordering would let a
// client wear the same display prefix as the relay's own chat row, and two
// identically named rows would reach a client that resolves chat senders out
// of the slot list.
#[test]
fn reserved_prefix_is_refused_as_name_in_use() {
    let mut h = Harness::new();
    let peer = PeerID(1);

    h.connect(peer);
    h.syn_ack(peer);
    let effects = h.send_authenticate(peer, "   ----x", "");

    assert_eq!(
        disconnects(&effects),
        vec![(peer, DisconnectReason::NameInUse)]
    );
}
