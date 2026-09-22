// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// TEST_PLAN.md section 6: the chunked GAMESTATE sub-protocol. Transfers is
// driven directly, because filling and draining a 32-packet window through
// the FSM would bury the arithmetic under scripted chunks.

use std::sync::Arc;

use rusty_enet::PeerID;

use veredus::relay::fault::PeerFault;
use veredus::relay::gamestate_transfer::CHUNK_SIZE;
use veredus::relay::gamestate_transfer::KIND_RUNNING_GAME;
use veredus::relay::gamestate_transfer::MAX_TRANSFER;
use veredus::relay::gamestate_transfer::Purpose;
use veredus::relay::gamestate_transfer::Transfers;
use veredus::relay::gamestate_transfer::WINDOW;
use veredus::relay::messages::GamestateRequest;
use veredus::relay::messages::WireMessage;
use veredus::relay::server_fsm::Config;
use veredus::relay::server_fsm::Input;

use crate::harness::Harness;

// Too wide a window overruns the reliable queue of a client that is also
// playing; too narrow, or an ack that frees nothing, and a join stalls
// halfway with nothing left in flight to ever be acked.
#[test]
fn send_window_is_32_and_acks_release_more() {
    let peer = PeerID(1);
    let id = 7;
    let total_chunks = WINDOW as usize + 8;
    let mut tx = Transfers::default();

    let (length, chunks) = tx
        .begin_send(peer, id, Arc::new(vec![0u8; CHUNK_SIZE * total_chunks]))
        .expect("a transfer inside the cap must start");
    assert_eq!(length as usize, CHUNK_SIZE * total_chunks);
    assert_eq!(
        chunks.len(),
        WINDOW as usize,
        "the first burst fills the window"
    );

    assert!(
        tx.on_ack(peer, id, 0).is_empty(),
        "an empty ack frees nothing"
    );
    assert!(
        tx.on_ack(peer, id, WINDOW + 1).is_empty(),
        "an ack for more than is in flight is dropped"
    );

    assert_eq!(tx.on_ack(peer, id, 1).len(), 1, "one ack, one more chunk");
    let rest = tx.on_ack(peer, id, WINDOW);
    assert_eq!(
        rest.len(),
        total_chunks - WINDOW as usize - 1,
        "a full ack sends whatever is left"
    );
    assert!(
        tx.on_ack(peer, id, rest.len() as u32).is_empty(),
        "the final ack completes the transfer"
    );
    assert!(
        tx.on_ack(peer, id, 1).is_empty(),
        "a completed transfer is forgotten"
    );
}

// A zero length leaves a client waiting on chunks that never come, and a
// declared length past the cap is a peer asking the relay to buffer
// whatever it likes.
#[test]
fn oversize_and_empty_transfers_are_refused() {
    let peer = PeerID(1);
    let mut tx = Transfers::default();

    assert!(tx.begin_send(peer, 1, Arc::new(Vec::new())).is_none());
    assert!(
        tx.begin_send(peer, 2, Arc::new(vec![0u8; MAX_TRANSFER as usize + 1]))
            .is_none()
    );

    tx.expect(peer, 3, Purpose::Savegame);
    for length in [0, MAX_TRANSFER + 1] {
        assert!(
            matches!(
                tx.on_response(peer, 3, length),
                Err(PeerFault::TransferOverrun)
            ),
            "a declared length of {length} must be refused"
        );
    }

    // Through the FSM: with no snapshot cached the request is simply not
    // answered, because answering with length 0 would strand the client.
    let mut h = Harness::with_config(Config {
        observer_delay_turns: 0,
        ..Config::default()
    });
    let players = h.start_match(&[("Alice", Some(1)), ("Bob", Some(2))], &[]);
    let effects = h.input(Input::Received {
        peer: players[1].0,
        msg: WireMessage::GamestateRequest(GamestateRequest {
            request_type: KIND_RUNNING_GAME,
            request_id: 1,
        }),
    });
    assert!(
        effects.is_empty(),
        "a request with nothing to serve gets no answer, got {effects:?}"
    );
}
