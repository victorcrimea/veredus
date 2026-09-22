// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use chrono::DateTime;
use chrono::Utc;

use crate::relay::server_fsm::Config;

// One message's worth of a bucket, in thousandths so that a rate of one a
// second still refills something on every 10 ms tick.
const COST: i64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Pass,
    // Over the limit, but not by enough to tell a burst from a flood.
    Drop,
    // Still sending long after being over the limit, which no stock client does.
    Kick,
}

pub struct TokenBucket {
    milli: i64,
    rate_per_sec: u32,
    burst: u32,
    last: Option<DateTime<Utc>>,
}

impl TokenBucket {
    // A rate of 0 turns the limit off.
    pub fn new(rate_per_sec: u32, burst: u32) -> Self {
        TokenBucket {
            milli: i64::from(burst) * COST,
            rate_per_sec,
            burst,
            last: None,
        }
    }

    // `now` is the newest tick time; None before the first tick, when the
    // bucket is still full and nothing has had time to refill.
    pub fn take(&mut self, now: Option<DateTime<Utc>>, kick_multiple: u32) -> Verdict {
        if self.rate_per_sec == 0 {
            return Verdict::Pass;
        }
        let full = i64::from(self.burst) * COST;
        if let Some(now) = now {
            if let Some(last) = self.last {
                // A wall clock can step backwards; that refills nothing and
                // re-anchors rather than stalling until it catches up.
                let elapsed_ms = now.signed_duration_since(last).num_milliseconds().max(0);
                let refill = elapsed_ms.saturating_mul(i64::from(self.rate_per_sec));
                self.milli = self.milli.saturating_add(refill).min(full);
            }
            self.last = Some(now);
        }

        if self.milli >= COST {
            self.milli -= COST;
            return Verdict::Pass;
        }
        if kick_multiple == 0 {
            // Without a kick there is nothing for debt to lead to, and letting
            // it pile up would only keep a peer muted long after it stopped.
            self.milli = self.milli.max(0);
            return Verdict::Drop;
        }
        // Dropped messages still spend, so the debt measures how long the peer
        // kept going after it was told nothing more would get through.
        self.milli -= COST;
        let floor = -full.saturating_mul(i64::from(kick_multiple - 1));
        if self.milli <= floor {
            Verdict::Kick
        } else {
            Verdict::Drop
        }
    }
}

struct TurnUse {
    turn: u32,
    count: u32,
    bytes: usize,
}

// What one peer has sent for each turn still open to commands. The caller only
// lets through turns within the command delay past the ready turn, so this
// holds a handful of entries at most.
#[derive(Default)]
pub struct TurnQuota {
    open: Vec<TurnUse>,
}

impl TurnQuota {
    // A cap of 0 turns that half of the limit off.
    pub fn charge(
        &mut self,
        turn: u32,
        bytes: usize,
        ready_turn: u32,
        max_count: u32,
        max_bytes: usize,
        kick_multiple: u32,
    ) -> Verdict {
        self.open.retain(|u| u.turn > ready_turn);
        let index = match self.open.iter().position(|u| u.turn == turn) {
            Some(index) => index,
            None => {
                self.open.push(TurnUse {
                    turn,
                    count: 0,
                    bytes: 0,
                });
                self.open.len() - 1
            }
        };
        let used = &mut self.open[index];
        // Dropped commands are counted too, for the same reason a bucket
        // spends on a drop: the excess is what tells a flood from a burst.
        used.count = used.count.saturating_add(1);
        used.bytes = used.bytes.saturating_add(bytes);

        let over_count = |cap: u32| cap != 0 && used.count > cap;
        let over_bytes = |cap: usize| cap != 0 && used.bytes > cap;
        if !over_count(max_count) && !over_bytes(max_bytes) {
            return Verdict::Pass;
        }
        let k = kick_multiple;
        if k != 0
            && (over_count(max_count.saturating_mul(k))
                || over_bytes(max_bytes.saturating_mul(k as usize)))
        {
            Verdict::Kick
        } else {
            Verdict::Drop
        }
    }
}

// Per connection, so a peer that reconnects starts afresh, but only after
// paying for a whole new handshake.
pub struct Limits {
    pub chat: TokenBucket,
    pub flare: TokenBucket,
    pub commands: TurnQuota,
}

impl Limits {
    pub fn new(config: &Config) -> Self {
        Limits {
            chat: TokenBucket::new(config.chat_per_sec, config.chat_burst),
            flare: TokenBucket::new(config.flare_per_sec, config.flare_burst),
            commands: TurnQuota::default(),
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/relay/rate_limit.rs"]
mod tests;
