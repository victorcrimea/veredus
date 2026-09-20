// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use rusty_enet::PeerID;

use crate::relay::messages::Guid;

// Warnings are advisory only: real connection loss is ENet's peer timeout, and
// the server never disconnects anyone over these.
pub const WARNING_INTERVAL: TimeDelta = TimeDelta::seconds(1);
pub const SILENCE_LIMIT: TimeDelta = TimeDelta::milliseconds(2000);
pub const BAD_RTT: TimeDelta = TimeDelta::milliseconds(400);

// What a session is warned about. A session is never reported both ways in
// the same pass.
pub enum Warning {
    Silent(TimeDelta),
    Lagging(TimeDelta),
}

#[derive(Default)]
pub struct Monitor {
    last_pass: Option<DateTime<Utc>>,
    paused: HashSet<Guid>,
}

impl Monitor {
    // The clock is read on the IO thread and arrives as `now`, so this stays
    // a pure function of its inputs.
    pub fn due(&mut self, now: DateTime<Utc>) -> bool {
        match self.last_pass {
            // A wall clock can step backwards, so a negative delta means run
            // the pass now and re-anchor rather than stall until it catches up.
            Some(last) => {
                let elapsed = now.signed_duration_since(last);
                if elapsed >= WARNING_INTERVAL || elapsed < TimeDelta::zero() {
                    self.last_pass = Some(now);
                    true
                } else {
                    false
                }
            }
            None => {
                self.last_pass = Some(now);
                true
            }
        }
    }

    pub fn classify(mean_rtt: TimeDelta, since_last_received: TimeDelta) -> Option<Warning> {
        if since_last_received > SILENCE_LIMIT {
            Some(Warning::Silent(since_last_received))
        } else if mean_rtt > BAD_RTT {
            Some(Warning::Lagging(mean_rtt))
        } else {
            None
        }
    }

    // Returns true when the pause set actually changed, which is what decides
    // whether the message is worth relaying.
    pub fn set_paused(&mut self, uuid: &Guid, paused: bool) -> bool {
        if paused {
            self.paused.insert(uuid.clone())
        } else {
            self.paused.remove(uuid)
        }
    }

    // A departing client is dropped from the set without any broadcast, so a
    // client that pauses and then leaves never gets an unpause on the wire.
    pub fn forget(&mut self, uuid: &Guid) {
        self.paused.remove(uuid);
    }

    pub fn paused(&self) -> impl Iterator<Item = &Guid> {
        self.paused.iter()
    }
}

// Carried from the ENet thread, which is the only place peer timing is
// reachable. Durations stay in chrono's type until a wire field needs a number.
#[derive(Debug, Clone, Copy)]
pub struct PeerStats {
    pub peer: PeerID,
    pub mean_rtt: TimeDelta,
    pub since_last_received: TimeDelta,
}
