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
}

// The result of a completed comparison. Names live on the sessions, so the
// caller resolves the peers rather than this module reaching for them.
pub struct HashMismatch {
    pub turn: u32,
    pub reference: Vec<u8>,
    pub mismatched: Vec<PeerID>,
}

impl Default for TurnManager {
    fn default() -> Self {
        TurnManager {
            ready_turn: INITIAL_READY_TURN,
            clients: HashMap::new(),
            pending: HashMap::new(),
            out_of_sync: HashSet::new(),
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
        self.out_of_sync.contains(&peer)
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
    ) {
        self.clients.insert(
            peer,
            ClientTurn {
                client_id,
                ready_turn,
                simulated_turn,
                observer,
            },
        );
    }

    pub fn forget(&mut self, peer: PeerID) {
        self.clients.remove(&peer);
        self.out_of_sync.remove(&peer);
        for reported in self.pending.values_mut() {
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
        // With nobody registered there is nothing to wait for, but also nobody
        // to send a seal to, so hold rather than run the turn counter away.
        if self.clients.is_empty() {
            return false;
        }
        self.clients
            .values()
            .filter(|c| self.blocks(c, observer_lag_limit))
            .all(|c| c.ready_turn > self.ready_turn)
    }

    // There is deliberately no timeout: a stalled player blocks the match
    // until it disconnects.
    fn blocks(&self, client: &ClientTurn, observer_lag_limit: Option<u32>) -> bool {
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
    ) -> Result<Option<HashMismatch>, PeerFault> {
        let client = self.clients.get_mut(&peer).ok_or(PeerFault::NoSession)?;
        let want = client.simulated_turn + 1;
        if turn != want {
            return Err(PeerFault::StateHashOutOfSequence { got: turn, want });
        }
        client.simulated_turn = turn;
        self.pending.entry(turn).or_default().insert(peer, hash);
        Ok(self.compare(turn))
    }

    // A departure can complete a comparison that was waiting on the departed
    // client, so this is also worth running after `forget`.
    pub fn compare(&mut self, turn: u32) -> Option<HashMismatch> {
        if !self.out_of_sync.is_empty() {
            return None;
        }
        let reported = self.pending.get(&turn)?;
        if !self.clients.keys().all(|p| reported.contains_key(p)) {
            return None;
        }

        // The reference is whatever the lowest client id reported: the server
        // runs no simulation, so it can only compare, never adjudicate.
        let reference_peer = *reported
            .keys()
            .min_by_key(|p| self.clients.get(p).map(|c| c.client_id).unwrap_or(u16::MAX))?;
        let reference = reported.get(&reference_peer)?.clone();

        let mismatched: Vec<PeerID> = reported
            .iter()
            .filter(|(_, hash)| **hash != reference)
            .map(|(peer, _)| *peer)
            .collect();

        self.pending.remove(&turn);

        if mismatched.is_empty() {
            return None;
        }
        self.out_of_sync.extend(mismatched.iter().copied());
        Some(HashMismatch {
            turn,
            reference,
            mismatched,
        })
    }

    // Comparison resumes only once every out-of-sync client has gone.
    pub fn recheck_pending(&mut self) -> Vec<HashMismatch> {
        if !self.out_of_sync.is_empty() {
            return Vec::new();
        }
        let mut turns: Vec<u32> = self.pending.keys().copied().collect();
        turns.sort_unstable();
        turns.into_iter().filter_map(|t| self.compare(t)).collect()
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
