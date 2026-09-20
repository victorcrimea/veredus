// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::UdpSocket;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use rusty_enet as enet;
use rusty_enet::Event;
use rusty_enet::EventNoRef;

use crate::network_message::InboundNetworkMessage;
use crate::network_message::OutboundNetworkMessage;

// Deliberately above the stock server's 41-peer cap so more observers fit.
const PEER_LIMIT: usize = 200;
// Every message rides one reliable channel, so ordering is guaranteed.
const CHANNEL_LIMIT: usize = 1;
// Stock clients declare this MTU; a host that negotiates a different one is
// rejected, and the crate default (1392) does not match.
const HOST_MTU: u16 = 1372;
const POLL_INTERVAL: Duration = Duration::from_millis(10);
// service() yields at most one event per call, so without a drain loop the host
// would handle 100 events/sec. The cap keeps the shutdown check and the
// outbound flush running every tick even under a packet flood.
const MAX_EVENTS_PER_TICK: usize = 256;

pub fn run_enet_host(
    socket: UdpSocket,
    event_tx: Sender<InboundNetworkMessage>,
    send_rx: Receiver<OutboundNetworkMessage>,
    shutdown_rx: Receiver<()>,
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

    'outer: loop {
        match shutdown_rx.try_recv() {
            Ok(_) | Err(TryRecvError::Disconnected) => {
                tracing::info!("ENet host shutting down");
                break;
            }
            Err(TryRecvError::Empty) => {}
        }

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
                            InboundNetworkMessage::Disconnect { peer, reason: data }
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

                    if let Err(error) = event_tx.send(inbound_message) {
                        tracing::error!(?error, "failed to forward ENet event to server thread");
                        break 'outer;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    tracing::warn!(?error, "ENet host error");
                    break;
                }
            }
        }

        while let Ok(message) = send_rx.try_recv() {
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
                    match host.get_peer_mut(peer) {
                        Some(handle) => handle.disconnect_now(reason),
                        None => tracing::debug!(?peer, "immediate disconnect for unknown peer"),
                    }
                }
            }
        }

        // Queued packets otherwise wait for the next service() call, which a
        // pending inbound event makes it skip.
        host.flush();

        std::thread::sleep(POLL_INTERVAL);
    }
}
