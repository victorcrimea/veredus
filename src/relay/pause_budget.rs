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
    // The match is held for players who are away: gone, still syncing back
    // in, or silent too long. Their absence is charged to the same quota a
    // manual pause spends, so leaving is never a cheaper pause.
    AutoPauseStarted { uuids: Vec<Guid> },
    AutoPauseEnded,
    AbsentExpired { uuid: Guid },
    AbsentStatus { uuid: Guid, remaining: TimeDelta },
}

pub struct PauseBudget {
    pausing: HashSet<Guid>,
    // Only a player who has actually paused gets an entry, so this stays
    // bounded by the pausers rather than by every session the match saw.
    remaining: HashMap<Guid, TimeDelta>,
    budget: TimeDelta,
    // What a pause costs however soon it is lifted. The drain alone charges
    // only the time a pause was held, so toggling at packet rate would cost
    // nothing while every pause still puts a chat line on everyone's screen.
    min_charge: TimeDelta,
    // When each current pause began, as the newest tick time known here.
    paused_at: HashMap<Guid, DateTime<Utc>>,
    last_check: Option<DateTime<Utc>>,
    last_status: Option<DateTime<Utc>>,
    coordinated: bool,
    auto_paused: bool,
}

impl PauseBudget {
    pub fn new(budget: TimeDelta, min_charge: TimeDelta) -> Self {
        Self {
            pausing: HashSet::new(),
            remaining: HashMap::new(),
            budget,
            min_charge,
            paused_at: HashMap::new(),
            last_check: None,
            last_status: None,
            coordinated: false,
            auto_paused: false,
        }
    }

    // Returns true when the set actually changed, which is what decides
    // whether the message is worth relaying.
    pub fn set_pausing(&mut self, uuid: &Guid, pausing: bool) -> bool {
        if pausing {
            let inserted = self.pausing.insert(uuid.clone());
            if inserted && let Some(now) = self.last_check {
                self.paused_at.insert(uuid.clone(), now);
            }
            inserted
        } else {
            let removed = self.pausing.remove(uuid);
            if removed {
                self.charge_shortfall(uuid);
            }
            removed
        }
    }

    // Charges what the drain has not yet taken of the minimum, so a pause
    // held longer than the minimum costs exactly its length.
    fn charge_shortfall(&mut self, uuid: &Guid) {
        let (Some(start), Some(now)) = (self.paused_at.remove(uuid), self.last_check) else {
            return;
        };
        let held = now.signed_duration_since(start).max(TimeDelta::zero());
        let shortfall = self.min_charge - held;
        if shortfall > TimeDelta::zero() {
            let left = self.remaining.entry(uuid.clone()).or_insert(self.budget);
            *left = (*left - shortfall).max(TimeDelta::zero());
        }
    }

    // A departing client stops the drain without any broadcast, so a client
    // that pauses and then leaves never gets an unpause on the wire. Its
    // quota is left behind, because the same player may reclaim the slot.
    pub fn clear_pausing(&mut self, uuid: &Guid) {
        // Charged like an unpause, or leaving would be the free way to toggle.
        if self.pausing.remove(uuid) {
            self.charge_shortfall(uuid);
        }
    }

    pub fn auto_paused(&self) -> bool {
        self.auto_paused
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

    // `players` is the count of connected players, which the slot table owns,
    // and `absent` the players the match should wait for, which only the
    // caller can tell. Everything else is this module's own state, so a
    // caller can drive a whole match by choosing the sequence of `now` it
    // passes in.
    pub fn check(
        &mut self,
        now: DateTime<Utc>,
        players: usize,
        absent: &[Guid],
    ) -> Vec<BudgetEvent> {
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
            self.paused_at.remove(&uuid);
            events.push(BudgetEvent::Expired { uuid });
        }

        // Charged even during a coordinated pause: that freeze is an
        // agreement among the players present, and the absent one is not
        // party to it.
        let mut waiting = Vec::new();
        for uuid in absent {
            let left = self.remaining.entry(uuid.clone()).or_insert(self.budget);
            if *left <= TimeDelta::zero() {
                continue;
            }
            *left = (*left - elapsed).max(TimeDelta::zero());
            if *left <= TimeDelta::zero() {
                events.push(BudgetEvent::AbsentExpired { uuid: uuid.clone() });
            } else {
                waiting.push(uuid.clone());
            }
        }

        if !waiting.is_empty() && !self.auto_paused {
            self.auto_paused = true;
            events.push(BudgetEvent::AutoPauseStarted {
                uuids: waiting.clone(),
            });
        } else if waiting.is_empty() && self.auto_paused {
            self.auto_paused = false;
            events.push(BudgetEvent::AutoPauseEnded);
        }

        let status_due = (!coordinated || !waiting.is_empty()) && self.status_due(now);
        // Nothing is being spent during a coordinated pause, so a countdown
        // would repeat the same number every interval for as long as it lasts.
        if !coordinated && status_due {
            for uuid in &self.pausing {
                events.push(BudgetEvent::Status {
                    uuid: uuid.clone(),
                    remaining: self.remaining(uuid),
                });
            }
        }
        if status_due {
            for uuid in waiting {
                let remaining = self.remaining(&uuid);
                events.push(BudgetEvent::AbsentStatus { uuid, remaining });
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

#[cfg(test)]
#[path = "../../tests/unit/relay/pause_budget.rs"]
mod tests;
