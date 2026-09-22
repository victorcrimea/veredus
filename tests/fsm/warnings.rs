// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 6: the once-a-second LAST_SEEN / LAGGING_CLIENTS
// pass.

use chrono::TimeDelta;
use rusty_enet::PeerID;

use veredus::relay::messages::WireMessage;
use veredus::relay::monitor::PeerStats;

use crate::harness::Harness;
use crate::harness::recipients_of;

fn stats(peer: PeerID, rtt_ms: i64, silent_ms: i64) -> PeerStats {
    PeerStats {
        peer,
        mean_rtt: TimeDelta::milliseconds(rtt_ms),
        since_last_received: TimeDelta::milliseconds(silent_ms),
    }
}

// More than one warning a second per bad peer floods every client's chat
// box; telling a peer about itself is noise the client shows as someone else
// lagging; and in setup the lobby screen is where a player decides whether to
// start with a bad connection at all, so setup sessions must get it too.
#[test]
fn warnings_are_rate_limited_never_self_addressed_and_reach_setup() {
    let mut h = Harness::new();
    let alice = PeerID(1);
    h.admit(alice, "Alice");
    let bob = PeerID(2);
    let bob_guid = h.admit(bob, "Bob");
    let carol = PeerID(3);
    let carol_guid = h.admit(carol, "Carol");
    let epoch = h.now;

    let silent_bob = || {
        vec![
            stats(alice, 50, 0),
            stats(bob, 50, 3000),
            stats(carol, 50, 0),
        ]
    };
    let is_last_seen_for_bob =
        |m: &WireMessage| matches!(m, WireMessage::LastSeen(ls) if ls.guid == bob_guid);

    let effects = h.tick_with_stats(epoch, silent_bob());
    assert_eq!(
        recipients_of(&effects, is_last_seen_for_bob),
        [alice, carol].into_iter().collect(),
        "the silent peer is reported to everyone but itself, got {effects:?}"
    );

    let effects = h.tick_with_stats(epoch + TimeDelta::milliseconds(500), silent_bob());
    assert!(
        effects.is_empty(),
        "a second pass inside the same second must stay quiet, got {effects:?}"
    );

    let effects = h.tick_with_stats(epoch + TimeDelta::seconds(1), silent_bob());
    assert_eq!(
        recipients_of(&effects, is_last_seen_for_bob),
        [alice, carol].into_iter().collect(),
        "a full second later the warning is due again"
    );

    let effects = h.tick_with_stats(
        epoch + TimeDelta::seconds(2),
        vec![stats(alice, 50, 0), stats(bob, 50, 0), stats(carol, 500, 0)],
    );
    assert_eq!(
        recipients_of(&effects, |m| matches!(
            m,
            WireMessage::LaggingClients(lc)
                if lc.clients.len() == 1 && lc.clients[0].guid == carol_guid
        )),
        [alice, bob].into_iter().collect(),
        "a lagging peer is reported to everyone but itself, got {effects:?}"
    );
    assert!(
        recipients_of(&effects, |m| matches!(m, WireMessage::LastSeen(_))).is_empty(),
        "Bob is back, so nobody is reported silent"
    );
}
