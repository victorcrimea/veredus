// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// A join sourced from a live client must be re-sourced when
// that client leaves, overruns, answers with a bad length or goes quiet.
// Otherwise the joiner waits in syncing forever, because the protocol has no
// failure notice and no timeout of its own.

use chrono::TimeDelta;
use rusty_enet::PeerID;

use veredus::relay::gamestate_transfer::KIND_RUNNING_GAME;
use veredus::relay::messages::GamestateChunk;
use veredus::relay::messages::GamestateResponse;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::DisconnectReason;
use veredus::relay::server_fsm::Effect;
use veredus::relay::server_fsm::Input;

use crate::harness::Harness;
use crate::harness::disconnects;

const STALL: TimeDelta = TimeDelta::seconds(30);

// Every GAMESTATE_REQUEST for running-game state in `effects`, as
// (source, request_id).
fn snapshot_requests(effects: &[Effect]) -> Vec<(PeerID, u32)> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Send {
                peer,
                msg: WireMessage::GamestateRequest(r),
            } if r.request_type == KIND_RUNNING_GAME => Some((*peer, r.request_id)),
            _ => None,
        })
        .collect()
}

struct Setup {
    h: Harness,
    players: [PeerID; 2],
    joiner: PeerID,
    source: PeerID,
    request_id: u32,
}

// Two live players in a running match and a third client admitted as a
// joiner. Which player is asked first depends on HashMap order, so the test
// reads it back instead of assuming one.
fn joiner_waiting_on_a_client() -> Setup {
    let mut h = Harness::with_config(Config {
        observer_delay_turns: 0,
        join_source_stall: Some(STALL),
        ..Config::default()
    });
    let started = h.start_match(&[("Alice", Some(1)), ("Bob", Some(2))], &[]);
    let players = [started[0].0, started[1].0];

    let joiner = PeerID(3);
    let before = h.log().len();
    h.admit(joiner, "Carol");
    let requests = snapshot_requests(&h.log()[before..]);
    assert_eq!(
        requests.len(),
        1,
        "admitting a joiner asks exactly one client for a snapshot, got {requests:?}"
    );
    let (source, request_id) = requests[0];
    assert!(players.contains(&source), "the source must be a player");

    Setup {
        h,
        players,
        joiner,
        source,
        request_id,
    }
}

fn other(players: [PeerID; 2], peer: PeerID) -> PeerID {
    if players[0] == peer {
        players[1]
    } else {
        players[0]
    }
}

fn assert_resourced_to(effects: &[Effect], expected: PeerID, what: &str) {
    let requests = snapshot_requests(effects);
    assert_eq!(
        requests.len(),
        1,
        "{what}: expected one new snapshot request, got {requests:?}"
    );
    assert_eq!(
        requests[0].0, expected,
        "{what}: the snapshot must be asked of the remaining player"
    );
}

#[test]
fn source_leaving_resources_the_join() {
    let Setup {
        mut h,
        players,
        joiner,
        source,
        ..
    } = joiner_waiting_on_a_client();

    let effects = h.enet_confirms_disconnect(source);
    assert_resourced_to(&effects, other(players, source), "source left");
    assert!(
        disconnects(&effects).iter().all(|(p, _)| *p != joiner),
        "the joiner must not be dropped while another source is left"
    );
}

#[test]
fn silent_source_is_abandoned_after_the_stall_limit() {
    let Setup {
        mut h,
        players,
        joiner,
        source,
        ..
    } = joiner_waiting_on_a_client();

    // The first tick only starts the clock; just short of the limit nothing
    // may happen yet.
    h.advance(TimeDelta::seconds(1));
    let early = h.advance(STALL - TimeDelta::seconds(1));
    assert!(
        snapshot_requests(&early).is_empty(),
        "a source inside the stall limit must be left alone"
    );

    let second = other(players, source);
    let effects = h.advance(TimeDelta::seconds(1));
    assert_resourced_to(&effects, second, "source silent");

    // The first source is still in-game but already failed, so once the
    // second one stalls too no client is left, and with no sidecar the
    // joiner is told the match cannot be joined.
    h.advance(TimeDelta::seconds(1));
    let effects = h.advance(STALL);
    assert!(
        snapshot_requests(&effects).is_empty(),
        "a source that already failed must not be asked again, got {:?}",
        snapshot_requests(&effects)
    );
    assert_eq!(
        disconnects(&effects),
        vec![(joiner, DisconnectReason::MatchInProgress)],
        "with every source tried the joiner is disconnected"
    );
}

#[test]
fn overrunning_source_resources_the_join() {
    let Setup {
        mut h,
        players,
        source,
        request_id,
        ..
    } = joiner_waiting_on_a_client();

    h.input(Input::Received {
        peer: source,
        msg: WireMessage::GamestateResponse(GamestateResponse {
            request_id,
            length: 4,
        }),
    });
    let effects = h.input(Input::Received {
        peer: source,
        msg: WireMessage::GamestateChunk(GamestateChunk {
            request_id,
            data: vec![0u8; 8],
        }),
    });
    assert_resourced_to(&effects, other(players, source), "source overran");
}

#[test]
fn bad_response_length_resources_after_the_stall_limit() {
    let Setup {
        mut h,
        players,
        source,
        request_id,
        ..
    } = joiner_waiting_on_a_client();

    // A zero length is refused but leaves nothing that could ever complete,
    // so only the stall limit can get the joiner out.
    h.input(Input::Received {
        peer: source,
        msg: WireMessage::GamestateResponse(GamestateResponse {
            request_id,
            length: 0,
        }),
    });
    h.advance(TimeDelta::seconds(1));
    let effects = h.advance(STALL);
    assert_resourced_to(&effects, other(players, source), "bad response length");
}
