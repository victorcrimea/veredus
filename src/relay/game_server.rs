// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;

use crate::network_message::InboundNetworkMessage;
use crate::network_message::OutboundNetworkMessage;
use crate::relay::messages::WireMessage;
use crate::relay::monitor::PeerStats;
use crate::relay::server_fsm::AnyServer;
use crate::relay::server_fsm::Config;
use crate::relay::server_fsm::Effect;
use crate::relay::server_fsm::Idle;
use crate::relay::server_fsm::Input;
use crate::relay::server_fsm::Server;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
// The FSM only needs a tick often enough to drive the connection warnings,
// which are emitted at most once a second.
const TICK_INTERVAL: TimeDelta = TimeDelta::milliseconds(100);

// The ENet thread feeds decoded events in over `event_rx` and takes effects out
// over `send_tx`. Every clock read lives here, on the IO side, so the FSM
// itself stays a pure function of the inputs it is handed.
pub fn run_game_server(
    event_rx: Receiver<InboundNetworkMessage>,
    send_tx: Sender<OutboundNetworkMessage>,
    shutdown_requested: Arc<AtomicBool>,
) {
    // Parked in the listening state, which is the phase a relay spends its
    // whole idle life in.
    let mut server = Some(AnyServer::from(
        Server::<Idle>::new(Config::default()).listen(),
    ));
    let mut latest_stats: Vec<PeerStats> = Vec::new();
    let mut last_tick: DateTime<Utc> = Utc::now();

    loop {
        loop {
            match event_rx.try_recv() {
                Ok(message) => {
                    if let Some(input) = to_input(message, &mut latest_stats) {
                        // `handle` consumes the server so a transition can be a
                        // consuming method, which is what keeps the phases typed.
                        server = Some(
                            server
                                .take()
                                .expect("server is always present")
                                .handle(input),
                        );
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The ENet thread dropped its sender, which is how a
                    // deliberate shutdown reaches this loop.
                    tracing::info!("ENet event channel disconnected, shutting down");
                    return;
                }
            }
        }

        let now = Utc::now();
        let elapsed = now.signed_duration_since(last_tick);
        // A backward wall-clock step ticks now and re-anchors, rather than
        // stalling the whole FSM until the clock catches up.
        if elapsed >= TICK_INTERVAL || elapsed < TimeDelta::zero() {
            last_tick = now;
            let input = Input::Tick {
                now,
                stats: latest_stats.clone(),
            };
            server = Some(
                server
                    .take()
                    .expect("server is always present")
                    .handle(input),
            );
        }

        if let Some(current) = server.as_mut()
            && !drain(current.take_effects(), &send_tx)
        {
            return;
        }

        if shutdown_requested.load(Ordering::SeqCst) {
            tracing::info!("shutdown requested, dropping every peer");
            let effects = server.take().expect("server is always present").shutdown();
            drain(effects, &send_tx);
            return;
        }

        std::thread::sleep(POLL_INTERVAL);
    }
}

// Stats are cached rather than fed straight in, so the FSM sees timing only on
// a tick and stays independent of how often the socket thread samples.
fn to_input(message: InboundNetworkMessage, latest_stats: &mut Vec<PeerStats>) -> Option<Input> {
    match message {
        InboundNetworkMessage::Connect { peer, addr } => match addr {
            IpAddr::V4(addr) => Some(Input::Connected { peer, addr }),
            // The stock client is IPv4 only, and the ban list is keyed by v4.
            IpAddr::V6(addr) => {
                tracing::warn!(?peer, %addr, "ignoring IPv6 peer");
                None
            }
        },
        InboundNetworkMessage::Disconnect { peer, reason } => {
            tracing::debug!(?peer, reason, "peer disconnected");
            Some(Input::Disconnected { peer })
        }
        InboundNetworkMessage::Message { peer, data } => match WireMessage::from_bytes(&data) {
            Ok(msg) => Some(Input::Received { peer, msg }),
            // One bad packet is dropped and the connection stays open.
            Err(error) => {
                tracing::debug!(?peer, %error, bytes = data.len(), "undecodable packet dropped");
                None
            }
        },
        InboundNetworkMessage::Stats { stats } => {
            *latest_stats = stats;
            None
        }
    }
}

// Returns false once the ENet thread is gone and there is nothing left to send to.
fn drain(effects: Vec<Effect>, send_tx: &Sender<OutboundNetworkMessage>) -> bool {
    for effect in effects {
        let outbound = match effect {
            Effect::Send { peer, msg } => OutboundNetworkMessage::Message {
                peer,
                data: msg.to_bytes(),
            },
            Effect::Disconnect { peer, reason } => OutboundNetworkMessage::Disconnect {
                peer,
                reason: reason as u32,
            },
            Effect::DisconnectNow { peer, reason } => OutboundNetworkMessage::DisconnectNow {
                peer,
                reason: reason as u32,
            },
        };
        if send_tx.send(outbound).is_err() {
            tracing::info!("ENet send channel closed, shutting down");
            return false;
        }
    }
    true
}
