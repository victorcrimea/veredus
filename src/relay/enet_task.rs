// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use chrono::TimeDelta;
use chrono::Utc;

use crate::enet;
use crate::enet::Event;
use crate::enet::EventNoRef;
use crate::enet::PeerID;
use crate::network_message::InboundCredit;
use crate::network_message::InboundNetworkMessage;
use crate::network_message::OutboundNetworkMessage;
use crate::relay::monitor::PeerStats;
use crate::relay::server_fsm::DisconnectReason;

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

// ENet's own maximum_waiting_data (EnetLimits) bounds what the host
// reassembles per peer, but that budget is released the moment a packet is
// dispatched. The copy this thread then makes for the channel is unaccounted
// for, so a peer that keeps a slow server thread busy could otherwise grow
// that queue without limit. Same order as the default max_waiting_bytes, so
// a peer's total inbound footprint stays around half a MiB.
const MAX_INBOUND_BYTES_PER_PEER: usize = 256 * 1024;
// Nothing bounds what stays queued for a peer that acknowledges just often
// enough to avoid its ENet timeout while the match keeps broadcasting turns.
const MAX_OUTGOING_QUEUE_BYTES: usize = 1024 * 1024;
// Consecutive one-second samples a peer's outgoing queue must stay over the
// cap before it is dropped. Long enough that a legitimate burst - a delayed
// observer replaying every command and seal from its snapshot turn up to the
// live turn - drains well within the window instead of tripping it.
const SLOW_PEER_GRACE_SAMPLES: u32 = 30;

// The crate defaults let any sender that completes the ENet connect, before
// any handshake, make the host buffer tens of MiB per peer.
#[derive(Debug, Clone, Copy)]
pub struct EnetLimits {
    pub max_packet_bytes: usize,
    pub max_waiting_bytes: usize,
}

pub fn run_enet_host(
    socket: UdpSocket,
    limits: EnetLimits,
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
            maximum_packet_size: limits.max_packet_bytes,
            maximum_waiting_data: limits.max_waiting_bytes,
            ..Default::default()
        },
    )
    .expect("Failed to create ENet host");

    host.set_mtu(HOST_MTU)
        .expect("Failed to set the MTU stock clients expect");

    tracing::info!(bind_addr = %bind_addr, "ENet host started");

    let mut last_stats = Utc::now();
    // Charged in InboundCredit::new as a message is built, released when the
    // credit is dropped on the server thread; only this thread inserts and
    // removes entries, so a peer id is never reused while its counter lives.
    let mut inbound: HashMap<PeerID, Arc<AtomicUsize>> = HashMap::new();
    // Consecutive over-cap samples for the outgoing-queue check below.
    let mut over_cap: HashMap<PeerID, u32> = HashMap::new();

    // The server thread ends this loop by dropping its sender, never the other
    // way round, so whatever it queued last (the shutdown farewell) is still
    // drained and flushed before the host goes away.
    let mut closing = false;
    loop {
        for _ in 0..MAX_EVENTS_PER_TICK {
            match host.service() {
                Ok(Some(event)) => {
                    let peer = match &event {
                        Event::Connect { peer, data: _ } => peer,
                        Event::Disconnect { peer, data: _ } => peer,
                        Event::Receive {
                            peer,
                            channel_id: _,
                            packet: _,
                        } => peer,
                    };
                    let peer_id = peer.id();
                    let socket_addr = peer
                        .address()
                        .expect("a peer with an event has a known address");
                    let event = event.no_ref();

                    let addr = match socket_addr {
                        SocketAddr::V4(addr) => *addr.ip(),
                        // The socket is bound IPv4 only, so this should never
                        // arrive. Dropping the peer rather than panicking keeps
                        // the socket loop, and with it the game, alive.
                        SocketAddr::V6(addr) => {
                            tracing::error!(%addr, "IPv6 peer on an IPv4 socket, dropping it");
                            if let Some(handle) = host.get_peer_mut(peer_id) {
                                handle.disconnect_now(DisconnectReason::Refused as u32);
                            }
                            continue;
                        }
                    };

                    let inbound_message = match event {
                        EventNoRef::Connect { peer, data: _ } => {
                            inbound.insert(peer, Arc::new(AtomicUsize::new(0)));
                            InboundNetworkMessage::Connect { peer, addr }
                        }
                        EventNoRef::Disconnect { peer, data } => {
                            inbound.remove(&peer);
                            over_cap.remove(&peer);
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
                        } => {
                            let data = packet.data();
                            // A peer that connected this same tick already has
                            // an entry from the Connect arm above; this is
                            // only a fallback in case ordering ever changes.
                            let outstanding = inbound
                                .entry(peer)
                                .or_insert_with(|| Arc::new(AtomicUsize::new(0)));
                            if outstanding.load(Ordering::Relaxed) + data.len()
                                > MAX_INBOUND_BYTES_PER_PEER
                            {
                                tracing::debug!(
                                    ?peer,
                                    bytes = data.len(),
                                    outstanding = outstanding.load(Ordering::Relaxed),
                                    "inbound budget exceeded, dropping packet"
                                );
                                crate::metrics::ENET_INBOUND_DROPPED_TOTAL.inc();
                                continue;
                            }
                            InboundNetworkMessage::Message {
                                peer,
                                credit: InboundCredit::new(outstanding, data.len()),
                                data: data.to_vec(),
                            }
                        }
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

            // Collected first: connected_peers() and get_peer_mut() both
            // borrow the host mutably, so the sampling pass and the
            // disconnects below cannot run as one pass.
            let mut slow_peers: Vec<PeerID> = Vec::new();
            for peer in host.connected_peers() {
                let id = peer.id();
                let queued = peer.outgoing_queue_bytes();
                let samples = over_cap.entry(id).or_insert(0);
                if queued > MAX_OUTGOING_QUEUE_BYTES {
                    *samples += 1;
                    if *samples >= SLOW_PEER_GRACE_SAMPLES {
                        tracing::warn!(
                            ?id,
                            queued,
                            "outgoing queue over cap for too long, disconnecting"
                        );
                        slow_peers.push(id);
                    }
                } else {
                    *samples = 0;
                }
            }
            for id in slow_peers {
                over_cap.remove(&id);
                crate::metrics::ENET_SLOW_PEER_DISCONNECTS_TOTAL.inc();
                if let Some(handle) = host.get_peer_mut(id) {
                    // disconnect(), not disconnect_later(): a peer whose
                    // queue stayed over the cap is, by definition, not
                    // draining it, so waiting for a flush would never
                    // complete. This also frees the queued memory at once
                    // instead of holding it until the peer's ENet timeout,
                    // and still dispatches a Disconnect event so the server
                    // thread's session cleanup runs normally.
                    handle.disconnect(DisconnectReason::Kicked as u32);
                }
            }

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
