// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::UdpSocket;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use chrono::TimeDelta;
use chrono::Utc;
use rusty_enet as enet;
use rusty_enet::Event;
use rusty_enet::EventNoRef;

use crate::network_message::InboundNetworkMessage;
use crate::network_message::OutboundNetworkMessage;
use crate::relay::monitor::PeerStats;

// Deliberately above the stock server's 41-peer cap so more observers fit.
pub const PEER_LIMIT: usize = 200;
// Every message rides one reliable channel, so ordering is guaranteed.
const CHANNEL_LIMIT: usize = 1;
// Stock clients declare this MTU; a host that negotiates a different one is
// rejected, and the crate default (1392) does not match.
const HOST_MTU: u16 = 1372;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
const MAX_EVENTS_PER_TICK: usize = 256;
// Matches the cadence the connection warnings are emitted at, so sampling any
// faster would only produce readings nothing looks at.
const STATS_INTERVAL: TimeDelta = TimeDelta::seconds(1);

pub fn run_enet_host(
    socket: UdpSocket,
    event_tx: Sender<InboundNetworkMessage>,
    send_rx: Receiver<OutboundNetworkMessage>,
) {
    // Read before the socket moves into the host, which takes ownership of it.
    let bind_addr = socket
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    let mut host = enet::Host::new(
        socket,
        enet::HostSettings {
            peer_limit: PEER_LIMIT,
            channel_limit: CHANNEL_LIMIT,
            compressor: None,
            checksum: None,
            ..Default::default()
        },
    )
    .expect("Failed to create ENet host");

    host.set_mtu(HOST_MTU)
        .expect("Failed to set the MTU stock clients expect");

    tracing::info!(bind_addr = %bind_addr, "ENet host started");

    let mut last_stats = Utc::now();

    // The server thread ends this loop by dropping its sender, never the other
    // way round, so whatever it queued last (the shutdown farewell) is still
    // drained and flushed before the host goes away.
    let mut closing = false;
    loop {
        for _ in 0..MAX_EVENTS_PER_TICK {
            match host.service() {
                Ok(Some(event)) => {
                    let addr = match &event {
                        Event::Connect { peer, data: _ } => peer,
                        Event::Disconnect { peer, data: _ } => peer,
                        Event::Receive {
                            peer,
                            channel_id: _,
                            packet: _,
                        } => peer,
                    }
                    .address()
                    .expect("a peer with an event has a known address")
                    .ip();

                    let inbound_message = match event.no_ref() {
                        EventNoRef::Connect { peer, data: _ } => {
                            InboundNetworkMessage::Connect { peer, addr }
                        }
                        EventNoRef::Disconnect { peer, data } => {
                            InboundNetworkMessage::Disconnect {
                                peer,
                                addr,
                                reason: data,
                            }
                        }
                        EventNoRef::Receive {
                            peer,
                            channel_id: _,
                            packet,
                        } => InboundNetworkMessage::Message {
                            peer,
                            data: packet.data().to_vec(),
                        },
                    };

                    // The server thread is gone, but what it sent before going
                    // is still buffered, so the drain below still runs.
                    if let Err(error) = event_tx.send(inbound_message) {
                        tracing::error!(?error, "failed to forward ENet event to server thread");
                        break;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(?error, "ENet host error");
                    break;
                }
            }
        }

        loop {
            let message = match send_rx.try_recv() {
                Ok(message) => message,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    closing = true;
                    break;
                }
            };
            match message {
                OutboundNetworkMessage::Message { peer, data } => match host.get_peer_mut(peer) {
                    Some(handle) => {
                        let packet = enet::Packet::new(data, enet::PacketKind::Reliable);
                        // A dropped forward desyncs that client, so it cannot
                        // pass unnoticed.
                        if let Err(error) = handle.send(0, &packet) {
                            tracing::warn!(?peer, ?error, "failed to queue outbound packet");
                        }
                    }
                    // Routine: the server thread can still hold an id for a peer
                    // that just went away.
                    None => tracing::debug!(?peer, "outbound message for unknown peer"),
                },
                OutboundNetworkMessage::Disconnect { peer, reason } => {
                    match host.get_peer_mut(peer) {
                        // disconnect() resets the peer's outgoing queue on the
                        // spot, dropping anything queued earlier this same
                        // tick (such as the departing client's own final
                        // PLAYER_SLOTS). disconnect_later() waits for queued
                        // commands to flush first, falling back to an
                        // immediate disconnect when nothing is queued.
                        Some(handle) => handle.disconnect_later(reason),
                        None => tracing::debug!(?peer, "disconnect for unknown peer"),
                    }
                }
                OutboundNetworkMessage::DisconnectNow { peer, reason } => {
                    // disconnect_now() discards the peer's queue, so anything
                    // queued just before it, such as the shutdown chat line,
                    // has to be on the wire first.
                    host.flush();
                    match host.get_peer_mut(peer) {
                        Some(handle) => handle.disconnect_now(reason),
                        None => tracing::debug!(?peer, "immediate disconnect for unknown peer"),
                    }
                }
            }
        }

        if closing {
            host.flush();
            tracing::info!("ENet host shutting down");
            break;
        }

        // A wall clock can step backwards, so a negative delta samples now and
        // re-anchors rather than stalling until the clock catches up.
        let now = Utc::now();
        let elapsed = now.signed_duration_since(last_stats);
        if elapsed >= STATS_INTERVAL || elapsed < TimeDelta::zero() {
            last_stats = now;
            let enet_now = host.enet_time_get();
            let stats: Vec<PeerStats> = host
                .connected_peers()
                .map(|peer| PeerStats {
                    peer: peer.id(),
                    mean_rtt: TimeDelta::from_std(peer.round_trip_time())
                        .unwrap_or_else(|_| TimeDelta::zero()),
                    // Both sides of this are ENet's own millisecond clock.
                    since_last_received: TimeDelta::milliseconds(i64::from(
                        enet_now.wrapping_sub(peer.last_receive_time()),
                    )),
                })
                .collect();
            let packet_loss = host
                .connected_peers()
                .map(|peer| (peer.id(), peer.packet_loss()))
                .collect();
            let stats = InboundNetworkMessage::Stats {
                stats,
                packet_loss,
                bytes_received: host.total_received_data(),
                bytes_sent: host.total_sent_data(),
            };
            // The server thread is gone; the next drain sees its sender
            // closed and ends the loop.
            if event_tx.send(stats).is_err() {
                tracing::error!("failed to forward peer stats to server thread");
            }
        }

        // Queued packets otherwise wait for the next service() call, which a
        // pending inbound event makes it skip.
        host.flush();

        std::thread::sleep(POLL_INTERVAL);
    }
}
