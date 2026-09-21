// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::collections::HashSet;

use rusty_enet::PeerID;

use crate::relay::fault::PeerFault;
use crate::relay::messages::PlayerCommand;

// One below the 4-turn command delay, so turn 4 is the first release.
pub const INITIAL_READY_TURN: u32 = 3;

struct ClientTurn {
    client_id: u16,
    ready_turn: u32,
    simulated_turn: u32,
    // The controller counts as a player here even when it holds no slot, so
    // it keeps blocking release and the match cannot run away from it.
    observer: bool,
    // On the delayed observer feed. Such a client never blocks release and is
    // left out of the live hash comparison: its reports arrive a whole delay
    // late, and waiting for them would hold every player's desync report back
    // by the same amount.
    delayed: bool,
}

// Bounded by the session count: hashes are discarded as soon as a turn is
// compared, and a departing client is forgotten outright.
pub struct TurnManager {
    ready_turn: u32,
    clients: HashMap<PeerID, ClientTurn>,
    pending: HashMap<u32, HashMap<PeerID, Vec<u8>>>,
    // Comparison stops while anyone is known to be out of sync, and resumes
    // once they have all left, so one desync does not spam every turn.
    out_of_sync: HashSet<PeerID>,
    // The agreed hash of every compared turn, kept for the whole match: a
    // delayed observer reports a turn long after the players have, and a late
    // observer can start from any turn the feed has reached. Sixteen bytes a
    // turn.
    references: HashMap<u32, Vec<u8>>,
    // Delayed observers' hashes for turns whose reference is not known yet,
    // which happens only while the live comparison is suspended.
    observer_pending: HashMap<u32, HashMap<PeerID, Vec<u8>>>,
    // Tracked apart from `out_of_sync`, so that one observer going out of sync
    // neither suspends the players' comparison nor anybody else's.
    observers_out_of_sync: HashSet<PeerID>,
}

// The result of a completed comparison. Names live on the sessions, so the
// caller resolves the peers rather than this module reaching for them.
pub struct HashMismatch {
    pub turn: u32,
    pub reference: Vec<u8>,
    pub mismatched: Vec<PeerID>,
    // Some for a delayed observer, which is told on its own: everyone else is
    // a whole delay past that turn and has no use for the report.
    pub recipient: Option<PeerID>,
}

impl Default for TurnManager {
    fn default() -> Self {
        TurnManager {
            ready_turn: INITIAL_READY_TURN,
            clients: HashMap::new(),
            pending: HashMap::new(),
            out_of_sync: HashSet::new(),
            references: HashMap::new(),
            observer_pending: HashMap::new(),
            observers_out_of_sync: HashSet::new(),
        }
    }
}

impl TurnManager {
    pub fn ready_turn(&self) -> u32 {
        self.ready_turn
    }

    pub fn is_registered(&self, peer: PeerID) -> bool {
        self.clients.contains_key(&peer)
    }

    pub fn is_out_of_sync(&self, peer: PeerID) -> bool {
        self.out_of_sync.contains(&peer) || self.observers_out_of_sync.contains(&peer)
    }

    pub fn is_delayed(&self, peer: PeerID) -> bool {
        self.clients.get(&peer).is_some_and(|c| c.delayed)
    }

    pub fn delayed_peers(&self) -> Vec<PeerID> {
        self.clients
            .iter()
            .filter(|(_, c)| c.delayed)
            .map(|(p, _)| *p)
            .collect()
    }

    // The two counters start apart: a client seals four turns ahead of the one
    // it is about to simulate. At game start that is seal 4 against hash 1; a
    // joiner resuming at R owes seal R+4 against hash R+1.
    pub fn register(
        &mut self,
        peer: PeerID,
        client_id: u16,
        ready_turn: u32,
        simulated_turn: u32,
        observer: bool,
        delayed: bool,
    ) {
        self.clients.insert(
            peer,
            ClientTurn {
                client_id,
                ready_turn,
                simulated_turn,
                observer,
                delayed,
            },
        );
    }

    pub fn forget(&mut self, peer: PeerID) {
        self.clients.remove(&peer);
        self.out_of_sync.remove(&peer);
        self.observers_out_of_sync.remove(&peer);
        for reported in self.pending.values_mut() {
            reported.remove(&peer);
        }
        for reported in self.observer_pending.values_mut() {
            reported.remove(&peer);
        }
    }

    pub fn on_turn_sealed(&mut self, peer: PeerID, turn: u32) -> Result<(), PeerFault> {
        let client = self.clients.get_mut(&peer).ok_or(PeerFault::NoSession)?;
        let want = client.ready_turn + 1;
        if turn != want {
            return Err(PeerFault::TurnSealOutOfSequence { got: turn, want });
        }
        client.ready_turn = turn;
        Ok(())
    }

    // Returns every turn that became releasable. Usually one, but a departure
    // can unblock several at once.
    pub fn release(&mut self, observer_lag_limit: Option<u32>) -> Vec<u32> {
        let mut released = Vec::new();
        while self.everyone_ahead(observer_lag_limit) {
            self.ready_turn += 1;
            released.push(self.ready_turn);
        }
        released
    }

    fn everyone_ahead(&self, observer_lag_limit: Option<u32>) -> bool {
        // With nobody blocking there is nothing to wait for, and `all` over
        // the empty set would be true on every pass, running the turn counter
        // away in an endless loop. That is not only an empty match: once the
        // players and the controller have left, the observers that remain
        // block nothing. Hold instead.
        let mut blocking = self
            .clients
            .values()
            .filter(|c| self.blocks(c, observer_lag_limit))
            .peekable();
        if blocking.peek().is_none() {
            return false;
        }
        blocking.all(|c| c.ready_turn > self.ready_turn)
    }

    // There is deliberately no timeout: a stalled player blocks the match
    // until it disconnects.
    fn blocks(&self, client: &ClientTurn, observer_lag_limit: Option<u32>) -> bool {
        // The delay is already slack of its own, and an observer that pauses
        // its delayed feed must stall nobody but itself.
        if client.delayed {
            return false;
        }
        if !client.observer {
            return true;
        }
        match observer_lag_limit {
            Some(limit) => self.ready_turn.saturating_sub(client.ready_turn) >= limit,
            None => false,
        }
    }

    pub fn on_state_hash(
        &mut self,
        peer: PeerID,
        turn: u32,
        hash: Vec<u8>,
    ) -> Result<Vec<HashMismatch>, PeerFault> {
        let client = self.clients.get_mut(&peer).ok_or(PeerFault::NoSession)?;
        let want = client.simulated_turn + 1;
        if turn != want {
            return Err(PeerFault::StateHashOutOfSequence { got: turn, want });
        }
        client.simulated_turn = turn;
        if client.delayed {
            if self.references.contains_key(&turn) {
                return Ok(self
                    .compare_observer(peer, turn, &hash)
                    .into_iter()
                    .collect());
            }
            if !self.observers_out_of_sync.contains(&peer) {
                self.observer_pending
                    .entry(turn)
                    .or_default()
                    .insert(peer, hash);
            }
            return Ok(Vec::new());
        }
        self.pending.entry(turn).or_default().insert(peer, hash);
        Ok(self.compare(turn))
    }

    // A departure can complete a comparison that was waiting on the departed
    // client, so this is also worth running after `forget`.
    pub fn compare(&mut self, turn: u32) -> Vec<HashMismatch> {
        let mut found = Vec::new();
        if !self.out_of_sync.is_empty() {
            return found;
        }
        let Some(reported) = self.pending.get(&turn) else {
            return found;
        };
        if !self
            .clients
            .iter()
            .filter(|(_, c)| !c.delayed)
            .all(|(p, _)| reported.contains_key(p))
        {
            return found;
        }

        // The reference is whatever the lowest client id reported: the server
        // runs no simulation, so it can only compare, never adjudicate.
        let Some(reference) = reported
            .iter()
            .min_by_key(|(p, _)| self.clients.get(p).map(|c| c.client_id).unwrap_or(u16::MAX))
            .map(|(_, hash)| hash.clone())
        else {
            return found;
        };

        let mismatched: Vec<PeerID> = reported
            .iter()
            .filter(|(_, hash)| **hash != reference)
            .map(|(peer, _)| *peer)
            .collect();

        self.pending.remove(&turn);
        self.references.insert(turn, reference.clone());

        if !mismatched.is_empty() {
            self.out_of_sync.extend(mismatched.iter().copied());
            found.push(HashMismatch {
                turn,
                reference,
                mismatched,
                recipient: None,
            });
        }

        // Observers that got here first were waiting on this reference.
        for (peer, hash) in self.observer_pending.remove(&turn).unwrap_or_default() {
            found.extend(self.compare_observer(peer, turn, &hash));
        }
        found
    }

    // Reported once per observer; after that its hashes are not compared
    // again until it rejoins, just as the players' comparison stays quiet.
    fn compare_observer(&mut self, peer: PeerID, turn: u32, hash: &[u8]) -> Option<HashMismatch> {
        if self.observers_out_of_sync.contains(&peer) {
            return None;
        }
        let reference = self.references.get(&turn)?;
        if reference.as_slice() == hash {
            return None;
        }
        self.observers_out_of_sync.insert(peer);
        Some(HashMismatch {
            turn,
            reference: reference.clone(),
            mismatched: vec![peer],
            recipient: Some(peer),
        })
    }

    // Comparison resumes only once every out-of-sync client has gone.
    pub fn recheck_pending(&mut self) -> Vec<HashMismatch> {
        if !self.out_of_sync.is_empty() {
            return Vec::new();
        }
        let mut turns: Vec<u32> = self.pending.keys().copied().collect();
        turns.sort_unstable();
        turns.into_iter().flat_map(|t| self.compare(t)).collect()
    }
}

// Append-only and retained for the whole match, because a joiner has to be
// replayed every command and every turn length from its snapshot turn onward.
#[derive(Default)]
pub struct MatchLog {
    commands: BTreeMap<u32, Vec<PlayerCommand>>,
    turn_lengths: BTreeMap<u32, u16>,
}

impl MatchLog {
    pub fn record_command(&mut self, command: PlayerCommand) {
        self.commands.entry(command.turn).or_default().push(command);
    }

    pub fn record_turn_length(&mut self, turn: u32, length: u16) {
        self.turn_lengths.insert(turn, length);
    }

    pub fn commands_for(&self, turn: u32) -> &[PlayerCommand] {
        self.commands.get(&turn).map_or(&[], |v| v.as_slice())
    }

    pub fn turn_length(&self, turn: u32) -> Option<u16> {
        self.turn_lengths.get(&turn).copied()
    }

    pub fn last_command_turn(&self) -> Option<u32> {
        self.commands.keys().next_back().copied()
    }
}
