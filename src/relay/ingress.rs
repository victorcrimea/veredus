// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::hash::Hash;
use std::net::Ipv4Addr;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;

use crate::enet::PeerID;
use crate::relay::messages::AuthenticateResultCode;
use crate::relay::messages::WireMessage;
use crate::relay::server_fsm::Config;
use crate::relay::server_fsm::DisconnectReason;
use crate::relay::server_fsm::Effect;
use crate::relay::server_fsm::Input;
use crate::relay::server_fsm::Phase;
use crate::relay::session::Session;

// What the gate lets through to the phase handlers. A refusal names its
// cause so the log line says which layer turned the peer away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Pass,
    Drop,
    Refuse {
        reason: DisconnectReason,
        cause: &'static str,
    },
}

struct Bucket {
    tokens: u32,
    anchor: DateTime<Utc>,
}

// Per-identity allowances that outlive the connection they were spent on,
// which is what a per-session limit cannot do against someone who simply
// reconnects. Only identities that spent something recently are held.
pub struct Ledger<K> {
    burst: u32,
    // None turns the ledger off.
    interval: Option<TimeDelta>,
    buckets: HashMap<K, Bucket>,
}

impl<K: Eq + Hash> Ledger<K> {
    pub fn new(burst: u32, interval: Option<TimeDelta>) -> Self {
        Ledger {
            burst,
            interval: interval.filter(|i| *i > TimeDelta::zero()),
            buckets: HashMap::new(),
        }
    }

    // An identity never seen is owed its whole burst.
    pub fn has_token(&mut self, key: &K, now: DateTime<Utc>) -> bool {
        let Some(interval) = self.interval else {
            return true;
        };
        let Some(bucket) = self.buckets.get_mut(key) else {
            return true;
        };
        refill(bucket, self.burst, interval, now);
        bucket.tokens > 0
    }

    pub fn spend(&mut self, key: K, now: DateTime<Utc>) {
        let Some(interval) = self.interval else {
            return;
        };
        let burst = self.burst;
        let bucket = self.buckets.entry(key).or_insert(Bucket {
            tokens: burst,
            anchor: now,
        });
        refill(bucket, burst, interval, now);
        // A full bucket has nothing to refill, so the wait for the token
        // spent here starts now rather than at whenever it last filled up.
        if bucket.tokens >= burst {
            bucket.anchor = now;
        }
        bucket.tokens = bucket.tokens.saturating_sub(1);
    }

    // A full bucket is the same as no bucket, so it is forgotten.
    pub fn prune(&mut self, now: DateTime<Utc>) {
        let Some(interval) = self.interval else {
            return;
        };
        let burst = self.burst;
        self.buckets.retain(|_, bucket| {
            refill(bucket, burst, interval, now);
            bucket.tokens < burst
        });
    }
}

fn refill(bucket: &mut Bucket, burst: u32, interval: TimeDelta, now: DateTime<Utc>) {
    let elapsed = now.signed_duration_since(bucket.anchor);
    // A wall clock can step backwards; that refills nothing and re-anchors
    // rather than holding the identity back until the clock catches up.
    if elapsed < TimeDelta::zero() {
        bucket.anchor = now;
        return;
    }
    let interval_ms = interval.num_milliseconds().max(1);
    let steps = elapsed.num_milliseconds() / interval_ms;
    if steps == 0 {
        return;
    }
    let gained = u32::try_from(steps).unwrap_or(u32::MAX);
    bucket.tokens = bucket.tokens.saturating_add(gained).min(burst);
    bucket.anchor = if bucket.tokens >= burst {
        now
    } else {
        bucket.anchor + TimeDelta::milliseconds(steps.saturating_mul(interval_ms))
    };
}

// Runs before the phase handlers and filters what they get to see, so each
// handler can assume its input already passed every limit that is not a game
// rule. It learns what the FSM decided by reading the effects the FSM pushed,
// rather than working admission out a second time on its own.
pub struct Gate {
    // Every join costs whoever is serving the snapshot, a player freezing to
    // serialize or a sidecar replay, so rejoining in a loop is charged to the
    // address and, in lobby mode, to the verified lobby name as well, since
    // one lobby account can come from many addresses.
    join_by_addr: Ledger<Ipv4Addr>,
    join_by_lobby_name: Ledger<String>,
    // Wrong passwords, charged the same two ways. Loopback is never charged,
    // because the AI host dials in from there and a local process failing a
    // few guesses must not keep it out of the game.
    auth_fail_by_addr: Ledger<Ipv4Addr>,
    auth_fail_by_lobby_name: Ledger<String>,
}

impl Gate {
    pub fn new(config: &Config) -> Self {
        Gate {
            join_by_addr: Ledger::new(config.join_burst, config.join_interval),
            join_by_lobby_name: Ledger::new(config.join_burst, config.join_interval),
            auth_fail_by_addr: Ledger::new(
                config.auth_fail_burst_per_addr,
                config.auth_fail_interval,
            ),
            auth_fail_by_lobby_name: Ledger::new(config.auth_fail_burst, config.auth_fail_interval),
        }
    }

    // `now` is None before the first tick, when no allowance has been spent
    // yet and there is nothing to hold anyone to.
    pub fn check(
        &mut self,
        phase: Phase,
        input: &Input,
        sessions: &HashMap<PeerID, Session>,
        now: Option<DateTime<Utc>>,
    ) -> Decision {
        match input {
            Input::Tick { now, .. } => {
                self.join_by_addr.prune(*now);
                self.join_by_lobby_name.prune(*now);
                self.auth_fail_by_addr.prune(*now);
                self.auth_fail_by_lobby_name.prune(*now);
                Decision::Pass
            }
            // Refused before the password hash and before a client id is
            // issued, so a refused peer costs nothing but the handshake.
            Input::Received {
                peer,
                msg: WireMessage::Authenticate(_),
            } => {
                let Some(now) = now else {
                    return Decision::Pass;
                };
                let Some(session) = sessions.get(peer) else {
                    return Decision::Pass;
                };
                if session.admitted.is_some() {
                    return Decision::Pass;
                }
                let lobby_key = session.lobby_name.as_ref().map(|name| name.to_lowercase());

                let addr_guessing = !session.addr.is_loopback()
                    && !self.auth_fail_by_addr.has_token(&session.addr, now);
                let name_guessing = lobby_key
                    .as_ref()
                    .is_some_and(|key| !self.auth_fail_by_lobby_name.has_token(key, now));
                if addr_guessing || name_guessing {
                    return Decision::Refuse {
                        reason: DisconnectReason::Banned,
                        cause: "password failures",
                    };
                }

                if !matches!(phase, Phase::InGame | Phase::PostGame) {
                    return Decision::Pass;
                }
                let addr_ok = self.join_by_addr.has_token(&session.addr, now);
                let name_ok = lobby_key
                    .as_ref()
                    .is_none_or(|key| self.join_by_lobby_name.has_token(key, now));
                if addr_ok && name_ok {
                    Decision::Pass
                } else {
                    Decision::Refuse {
                        reason: DisconnectReason::Refused,
                        cause: "join rate",
                    }
                }
            }
            _ => Decision::Pass,
        }
    }

    // `effects` are the ones the handlers pushed for the input just checked.
    pub fn observe(
        &mut self,
        effects: &[Effect],
        sessions: &HashMap<PeerID, Session>,
        now: Option<DateTime<Utc>>,
    ) {
        let Some(now) = now else {
            return;
        };
        for effect in effects {
            match effect {
                Effect::Send {
                    peer,
                    msg: WireMessage::AuthenticateResult(result),
                } if matches!(result.code, AuthenticateResultCode::OkRejoining) => {
                    let Some(session) = sessions.get(peer) else {
                        continue;
                    };
                    self.join_by_addr.spend(session.addr, now);
                    if let Some(name) = session.lobby_name.as_ref() {
                        self.join_by_lobby_name.spend(name.to_lowercase(), now);
                    }
                }
                Effect::PasswordRejected { peer } => {
                    let Some(session) = sessions.get(peer) else {
                        continue;
                    };
                    if !session.addr.is_loopback() {
                        self.auth_fail_by_addr.spend(session.addr, now);
                    }
                    if let Some(name) = session.lobby_name.as_ref() {
                        self.auth_fail_by_lobby_name.spend(name.to_lowercase(), now);
                    }
                }
                _ => {}
            }
        }
    }
}
