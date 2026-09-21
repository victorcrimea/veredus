// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 5.6.

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use rusty_enet::PeerID;

use veredus::relay::messages::Guid;
use veredus::relay::messages::LoadedGame;
use veredus::relay::messages::PlayerPause;
use veredus::relay::messages::PreGameStatus;
use veredus::relay::messages::StartSettings;
use veredus::relay::messages::WireMessage;
use veredus::relay::monitor::PeerStats;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;

use crate::harness::Harness;
use crate::harness::recipients_of;

const READY: u8 = 1;

fn ready(peer: PeerID, guid: Guid) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::PreGameStatus(PreGameStatus {
            guid,
            status: READY,
        }),
    }
}

fn loaded(peer: PeerID) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::LoadedGame(LoadedGame { current_turn: 0 }),
    }
}

fn pause(peer: PeerID, guid: Guid, pause: bool) -> Input {
    Input::Received {
        peer,
        msg: WireMessage::PlayerPause(PlayerPause { guid, pause }),
    }
}

fn chat_to(effects: &[Effect], peer: PeerID) -> Option<String> {
    effects.iter().find_map(|e| match e {
        Effect::Send {
            peer: p,
            msg: WireMessage::Chat(c),
        } if *p == peer => Some(c.message.clone()),
        _ => None,
    })
}

fn player_pause_to(effects: &[Effect], peer: PeerID) -> Option<PlayerPause> {
    effects.iter().find_map(|e| match e {
        Effect::Send {
            peer: p,
            msg: WireMessage::PlayerPause(pp),
        } if *p == peer => Some(pp.clone()),
        _ => None,
    })
}

// Admits two players into slots 1 and 2, marks both ready, starts the match
// and finishes loading, so the returned harness is InGame with both players
// able to speak.
fn two_player_match(h: &mut Harness) -> (PeerID, Guid, PeerID, Guid) {
    let a = PeerID(1);
    let a_guid = h.admit(a, "Alice");
    h.map_player_id_to_slot(a, 1, &a_guid);
    let b = PeerID(2);
    let b_guid = h.admit_as_player(b, "Bob", 2, a);

    h.input(ready(a, a_guid.clone()));
    h.input(ready(b, b_guid.clone()));
    h.input(Input::Received {
        peer: a,
        msg: WireMessage::StartSettings(StartSettings {
            init_attributes: Vec::new(),
        }),
    });
    h.input(loaded(a));
    h.input(loaded(b));

    (a, a_guid, b, b_guid)
}

// T1.16: both refusals have the same required shape, because the client has
// already pinned its own pause overlay up the moment it asked. Refusing is
// not enough: dropping the self-unpause would leave the refused client
// frozen on its own screen while the match keeps running without it, and
// nothing else in the system would notice.
#[test]
fn pause_is_refused_for_observers_and_for_an_empty_budget() {
    // An observer holds no player, so its pause would freeze everyone
    // else's screen for nothing.
    {
        let mut h = Harness::new();
        let a = PeerID(1);
        let a_guid = h.admit(a, "Alice");
        h.map_player_id_to_slot(a, 1, &a_guid);
        let b = PeerID(2);
        let b_guid = h.admit(b, "Bob"); // no slot: an observer.

        h.input(ready(a, a_guid));
        h.input(ready(b, b_guid.clone()));
        h.input(Input::Received {
            peer: a,
            msg: WireMessage::StartSettings(StartSettings {
                init_attributes: Vec::new(),
            }),
        });
        h.input(loaded(a));
        h.input(loaded(b));

        let effects = h.input(pause(b, b_guid.clone(), true));

        assert_eq!(
            recipients_of(&effects, |_| true),
            [b].into_iter().collect(),
            "an observer's refused pause must reach only the observer, got {effects:?}"
        );
        assert_eq!(
            chat_to(&effects, b).as_deref(),
            Some("Only players can pause the game.")
        );
        assert_eq!(
            player_pause_to(&effects, b),
            Some(PlayerPause {
                guid: b_guid,
                pause: false
            })
        );
    }

    // A player with no budget left is refused too, and just as loudly told
    // to lift its own overlay.
    {
        let mut h = Harness::with_config(Config {
            pause_budget: TimeDelta::zero(),
            ..Config::default()
        });
        let (a, a_guid, _b, _b_guid) = two_player_match(&mut h);

        let effects = h.input(pause(a, a_guid.clone(), true));

        assert_eq!(
            recipients_of(&effects, |_| true),
            [a].into_iter().collect(),
            "a refused pause must reach only the player who asked, got {effects:?}"
        );
        assert_eq!(
            chat_to(&effects, a).as_deref(),
            Some("You are out of pause budget.")
        );
        assert_eq!(
            player_pause_to(&effects, a),
            Some(PlayerPause {
                guid: a_guid,
                pause: false
            })
        );
    }
}

fn tick(now: DateTime<Utc>) -> Input {
    Input::Tick {
        now,
        stats: Vec::new(),
    }
}

// T1.17: PauseBudget::check drains while exactly one side is paused, freezes
// while every connected player is paused at once, resumes the moment that
// stops being true, and expires exactly once. The players >= 2 guard against
// a lone player vacuously satisfying "everyone is paused" gets its own case,
// because it is exactly what a simplifying refactor would reintroduce.
#[test]
fn budget_drains_freezes_when_coordinated_and_expires_once() {
    let mut h = Harness::with_config(Config {
        pause_budget: TimeDelta::seconds(50),
        ..Config::default()
    });
    let (a, a_guid, b, b_guid) = two_player_match(&mut h);
    let epoch = h.now;

    h.input(tick(epoch)); // anchor last_check and last_status.
    h.input(pause(a, a_guid.clone(), true));

    // Alone, Alice's quota drains.
    h.input(tick(epoch + TimeDelta::seconds(10)));

    // Bob joins the pause: everyone is now paused, so draining freezes.
    h.input(pause(b, b_guid.clone(), true));
    let effects = h.input(tick(epoch + TimeDelta::seconds(15)));
    assert_eq!(
        chat_to(&effects, a).as_deref(),
        Some("Everyone is paused, so nobody's pause budget is draining.")
    );

    // Bob resumes: draining picks back up from where it left off.
    h.input(pause(b, b_guid, false));
    let effects = h.input(tick(epoch + TimeDelta::seconds(20)));
    assert_eq!(
        chat_to(&effects, a).as_deref(),
        Some("Pause budgets are draining again.")
    );

    // Alice has spent 10s + 5s = 15s of her 50s; 35 more exhausts her.
    let effects = h.input(tick(epoch + TimeDelta::seconds(55)));
    assert_eq!(
        recipients_of(&effects, |m| matches!(m, WireMessage::PlayerPause(_))),
        [a, b].into_iter().collect(),
        "the forced unpause must reach the pauser too, got {effects:?}"
    );
    assert_eq!(
        player_pause_to(&effects, a),
        Some(PlayerPause {
            guid: a_guid.clone(),
            pause: false
        })
    );
    assert_eq!(
        chat_to(&effects, a).as_deref(),
        Some("Alice is out of pause budget.")
    );

    // The expiry must not repeat on a later tick.
    let effects = h.input(tick(epoch + TimeDelta::seconds(65)));
    assert!(
        player_pause_to(&effects, a).is_none() && chat_to(&effects, a).is_none(),
        "an already-expired pause must not fire twice, got {effects:?}"
    );

    // A lone player pausing must not vacuously satisfy "everyone is paused"
    // and freeze its own quota forever.
    let mut solo = Harness::with_config(Config {
        pause_budget: TimeDelta::seconds(50),
        ..Config::default()
    });
    let alice = PeerID(1);
    let alice_guid = solo.admit(alice, "Alice");
    solo.map_player_id_to_slot(alice, 1, &alice_guid);
    solo.input(ready(alice, alice_guid.clone()));
    solo.input(Input::Received {
        peer: alice,
        msg: WireMessage::StartSettings(StartSettings {
            init_attributes: Vec::new(),
        }),
    });
    solo.input(loaded(alice));
    let solo_epoch = solo.now;

    solo.input(tick(solo_epoch));
    solo.input(pause(alice, alice_guid.clone(), true));
    let effects = solo.input(tick(solo_epoch + TimeDelta::seconds(50)));
    assert_eq!(
        player_pause_to(&effects, alice),
        Some(PlayerPause {
            guid: alice_guid,
            pause: false
        }),
        "a single connected player pausing must still drain its own quota, got {effects:?}"
    );
}

// T1.18: three independent cadences all key off `now.signed_duration_since`
// treating a negative delta as "run now and re-anchor": Monitor::due,
// PauseBudget::check and PauseBudget::status_due. Replacing that with an
// unsigned subtraction, or dropping the `< TimeDelta::zero()` arm, makes the
// server go quiet for however far the wall clock stepped back, which is
// close to undiagnosable in production.
#[test]
fn backward_clock_steps_re_anchor_rather_than_stall() {
    // Monitor::due: a silent peer is reported once due, not reported again
    // immediately after, and reported again right after a backward step.
    {
        let mut h = Harness::new();
        let (a, _a_guid, b, _b_guid) = two_player_match(&mut h);
        let epoch = h.now;
        let stale = vec![PeerStats {
            peer: a,
            mean_rtt: TimeDelta::zero(),
            since_last_received: TimeDelta::milliseconds(3000),
        }];

        let effects = h.input(Input::Tick {
            now: epoch,
            stats: stale.clone(),
        });
        assert_eq!(
            recipients_of(&effects, |m| matches!(m, WireMessage::LastSeen(_))),
            [b].into_iter().collect(),
            "the first pass is always due"
        );

        let effects = h.input(Input::Tick {
            now: epoch,
            stats: stale.clone(),
        });
        assert!(
            recipients_of(&effects, |m| matches!(m, WireMessage::LastSeen(_))).is_empty(),
            "a pass under a second later must not run again yet, got {effects:?}"
        );

        let effects = h.input(Input::Tick {
            now: epoch - TimeDelta::seconds(5),
            stats: stale,
        });
        assert_eq!(
            recipients_of(&effects, |m| matches!(m, WireMessage::LastSeen(_))),
            [b].into_iter().collect(),
            "a backward clock step must run the pass immediately, not stall"
        );
    }

    // PauseBudget::check: a backward step must charge nothing, not refund.
    {
        let mut h = Harness::with_config(Config {
            pause_budget: TimeDelta::seconds(20),
            ..Config::default()
        });
        let (a, a_guid, _b, _b_guid) = two_player_match(&mut h);
        let epoch = h.now;

        h.input(tick(epoch));
        h.input(pause(a, a_guid.clone(), true));
        h.input(tick(epoch + TimeDelta::seconds(15))); // 5s of budget left.

        // Step backward. If this refunded time instead of charging zero,
        // the next 5 forward seconds would not be enough to exhaust it.
        h.input(tick(epoch + TimeDelta::seconds(10)));
        let effects = h.input(tick(epoch + TimeDelta::seconds(15)));
        assert_eq!(
            player_pause_to(&effects, a),
            Some(PlayerPause {
                guid: a_guid,
                pause: false
            }),
            "exactly 5 more seconds should exhaust the budget the backward step must not have refunded, got {effects:?}"
        );
    }

    // PauseBudget::status_due: a backward step must fire the status line
    // immediately, without waiting out the normal interval.
    {
        let mut h = Harness::with_config(Config {
            pause_budget: TimeDelta::seconds(100),
            ..Config::default()
        });
        let (a, a_guid, _b, _b_guid) = two_player_match(&mut h);
        let epoch = h.now;

        // The first ever call anchors last_status without firing it.
        h.input(tick(epoch));
        h.input(pause(a, a_guid.clone(), true));

        // 3s later is nowhere near the 10s interval, so this does not fire
        // either, and importantly does not move the anchor forward: a call
        // that returns false leaves last_status where it was.
        let effects = h.input(tick(epoch + TimeDelta::seconds(3)));
        assert!(
            chat_to(&effects, a).is_none(),
            "3s since the last status line is nowhere near the 10s interval, got {effects:?}"
        );

        // Stepping earlier than the anchor itself (not just earlier than the
        // previous tick) must fire immediately rather than waiting for 10
        // forward seconds that may never come.
        let effects = h.input(tick(epoch - TimeDelta::seconds(1)));
        assert_eq!(
            chat_to(&effects, a).as_deref(),
            Some("Alice is paused. 97s of pause budget left."),
            "a backward step must fire the status line immediately, got {effects:?}"
        );
    }
}
