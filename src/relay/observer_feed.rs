// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

use rusty_enet::PeerID;

use crate::relay::messages::Flare;
use crate::relay::messages::Join;
use crate::relay::turn::INITIAL_READY_TURN;

// Observers watch the match this many turns behind the players, so what they
// see cannot be relayed back to a player in time to matter. At the stock
// 200 ms turn that is one minute.
pub const DEFAULT_DELAY_TURNS: u32 = 300;

// The feed keeps no copy of the turn stream: every command and turn length is
// already retained in the match log for join replay, so releasing a turn to
// the delayed observers is a read of the log at a later moment. What it does
// hold is the traffic the log does not keep (flares) and joiners that have to
// wait for the feed to reach the state they were handed.
pub struct ObserverFeed {
    delay: u32,
    // The last turn sealed to delayed observers. Only ever moves forward,
    // because a client cannot take a turn back once it has been sealed.
    head: u32,
    // Keyed by the turn the flare belongs to, so it reaches the delayed
    // observers alongside that turn rather than ahead of it.
    flares: BTreeMap<u32, Vec<Flare>>,
    joins: HashMap<PeerID, HeldJoin>,
}

struct HeldJoin {
    // The highest turn the snapshot's source had been sealed when it was
    // taken. The snapshot's own turn is inside its compressed payload, which
    // stays opaque, but it cannot be past what its source was allowed to run.
    bound: u32,
    join: Join,
    // Held back with the JOIN, because a joiner that could download it
    // earlier would see state from a turn the feed has not reached.
    snapshot: Arc<Vec<u8>>,
}

impl ObserverFeed {
    pub fn new(delay: u32) -> Self {
        ObserverFeed {
            delay,
            head: INITIAL_READY_TURN,
            flares: BTreeMap::new(),
            joins: HashMap::new(),
        }
    }

    pub fn head(&self) -> u32 {
        self.head
    }

    // Returns the turns that became due. Once no player is left there is
    // nothing to hide, so the feed drains to the live turn and follows it
    // from then on, which is what lets observers see the match end.
    pub fn advance(&mut self, live_ready_turn: u32, draining: bool) -> RangeInclusive<u32> {
        let target = if draining {
            live_ready_turn
        } else {
            live_ready_turn.saturating_sub(self.delay)
        };
        let from = self.head + 1;
        self.head = self.head.max(target);
        from..=self.head
    }

    pub fn queue_flare(&mut self, turn: u32, flare: Flare) {
        self.flares.entry(turn).or_default().push(flare);
    }

    pub fn due_flares(&mut self) -> Vec<Flare> {
        let later = self.flares.split_off(&(self.head + 1));
        std::mem::replace(&mut self.flares, later)
            .into_values()
            .flatten()
            .collect()
    }

    pub fn hold_join(&mut self, joiner: PeerID, bound: u32, join: Join, snapshot: Arc<Vec<u8>>) {
        self.joins.insert(
            joiner,
            HeldJoin {
                bound,
                join,
                snapshot,
            },
        );
    }

    // A released join leaves the feed together with its snapshot, which the
    // caller grants to the joiner as it sends the JOIN.
    pub fn due_joins(&mut self) -> Vec<(PeerID, Join, Arc<Vec<u8>>)> {
        let head = self.head;
        let due: Vec<PeerID> = self
            .joins
            .iter()
            .filter(|(_, held)| held.bound <= head)
            .map(|(peer, _)| *peer)
            .collect();
        due.into_iter()
            .filter_map(|peer| {
                let held = self.joins.remove(&peer)?;
                Some((peer, held.join, held.snapshot))
            })
            .collect()
    }

    // Called once the joiner has loaded, and on departure.
    pub fn forget(&mut self, peer: PeerID) {
        self.joins.remove(&peer);
    }
}
