// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::collections::HashSet;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;

use crate::relay::messages::Guid;

// A quota longer than any match anyone would play is how an operator turns
// the policy off without a second switch to keep in step with this one.
pub const DEFAULT_BUDGET: TimeDelta = TimeDelta::seconds(180);
pub const STATUS_INTERVAL: TimeDelta = TimeDelta::seconds(10);

// What a pass found worth telling the clients about. The module decides,
// the caller does the talking: sending is not this module's business.
pub enum BudgetEvent {
    Expired { uuid: Guid },
    Status { uuid: Guid, remaining: TimeDelta },
    CoordinatedStarted,
    CoordinatedEnded,
}

pub struct PauseBudget {
    pausing: HashSet<Guid>,
    // Only a player who has actually paused gets an entry, so this stays
    // bounded by the pausers rather than by every session the match saw.
    remaining: HashMap<Guid, TimeDelta>,
    budget: TimeDelta,
    last_check: Option<DateTime<Utc>>,
    last_status: Option<DateTime<Utc>>,
    coordinated: bool,
}

impl PauseBudget {
    pub fn new(budget: TimeDelta) -> Self {
        Self {
            pausing: HashSet::new(),
            remaining: HashMap::new(),
            budget,
            last_check: None,
            last_status: None,
            coordinated: false,
        }
    }

    // Returns true when the set actually changed, which is what decides
    // whether the message is worth relaying.
    pub fn set_pausing(&mut self, uuid: &Guid, pausing: bool) -> bool {
        if pausing {
            self.pausing.insert(uuid.clone())
        } else {
            self.pausing.remove(uuid)
        }
    }

    // A departing client stops the drain without any broadcast, so a client
    // that pauses and then leaves never gets an unpause on the wire. Its
    // quota is left behind, because the same player may reclaim the slot.
    pub fn clear_pausing(&mut self, uuid: &Guid) {
        self.pausing.remove(uuid);
    }

    pub fn pausing(&self) -> impl Iterator<Item = &Guid> {
        self.pausing.iter()
    }

    // A peek, so asking what is left does not hand out an entry to a player
    // who has never paused.
    pub fn remaining(&self, uuid: &Guid) -> TimeDelta {
        self.remaining.get(uuid).copied().unwrap_or(self.budget)
    }

    // A reconnecting client authenticates under a fresh UUID, so without this
    // a player empties the quota, comes back into the same slot and starts
    // over on a full one.
    pub fn inherit(&mut self, old: &Guid, new: &Guid) {
        if let Some(left) = self.remaining.remove(old) {
            self.remaining.insert(new.clone(), left);
        }
    }

    // `players` is the count of connected players, which the slot table owns.
    // Everything else is this module's own state, so a caller can drive a
    // whole match by choosing the sequence of `now` it passes in.
    pub fn check(&mut self, now: DateTime<Utc>, players: usize) -> Vec<BudgetEvent> {
        let mut events = Vec::new();

        // A wall clock can step backwards, so a negative delta re-anchors and
        // charges nothing rather than handing back time nobody spent.
        let elapsed = match self.last_check {
            Some(last) => now.signed_duration_since(last).max(TimeDelta::zero()),
            None => TimeDelta::zero(),
        };
        self.last_check = Some(now);

        // With one human, "everyone is paused" is vacuously true, which would
        // let a lone player in a hosted-AI match freeze their own quota.
        let coordinated = players >= 2 && self.pausing.len() == players;
        if coordinated != self.coordinated {
            self.coordinated = coordinated;
            events.push(if coordinated {
                BudgetEvent::CoordinatedStarted
            } else {
                BudgetEvent::CoordinatedEnded
            });
        }

        if !coordinated {
            let budget = self.budget;
            for uuid in &self.pausing {
                let left = self.remaining.entry(uuid.clone()).or_insert(budget);
                *left = (*left - elapsed).max(TimeDelta::zero());
            }
        }

        let spent: Vec<Guid> = self
            .pausing
            .iter()
            .filter(|uuid| self.remaining(uuid) <= TimeDelta::zero())
            .cloned()
            .collect();
        for uuid in spent {
            self.pausing.remove(&uuid);
            events.push(BudgetEvent::Expired { uuid });
        }

        // Nothing is being spent during a coordinated pause, so a countdown
        // would repeat the same number every interval for as long as it lasts.
        if !coordinated && self.status_due(now) {
            for uuid in &self.pausing {
                events.push(BudgetEvent::Status {
                    uuid: uuid.clone(),
                    remaining: self.remaining(uuid),
                });
            }
        }

        events
    }

    // One anchor for everyone, so two players pausing at once are reported
    // together instead of drifting into a stream of alternating lines.
    fn status_due(&mut self, now: DateTime<Utc>) -> bool {
        match self.last_status {
            Some(last) => {
                let since = now.signed_duration_since(last);
                if since >= STATUS_INTERVAL || since < TimeDelta::zero() {
                    self.last_status = Some(now);
                    true
                } else {
                    false
                }
            }
            None => {
                self.last_status = Some(now);
                false
            }
        }
    }
}
