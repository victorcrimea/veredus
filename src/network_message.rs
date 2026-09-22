// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;

use rusty_enet::PeerID;

use crate::relay::monitor::PeerStats;

// Raw bytes cross the thread boundary on purpose: only the game-server thread
// owns the message catalog, so the ENet thread stays a dumb packet pump.
#[derive(Debug)]
pub enum InboundNetworkMessage {
    Connect {
        peer: PeerID,
        addr: IpAddr,
    },
    Disconnect {
        peer: PeerID,
        addr: IpAddr,
        reason: u32,
    },
    Message {
        peer: PeerID,
        data: Vec<u8>,
    },
    // Peer timing is only reachable from the thread that owns the host, so it
    // is sampled there and carried over rather than looked up on demand.
    Stats {
        stats: Vec<PeerStats>,
    },
}

#[derive(Debug)]
pub enum OutboundNetworkMessage {
    Message { peer: PeerID, data: Vec<u8> },
    Disconnect { peer: PeerID, reason: u32 },
    // Shutdown cannot wait for queued reliable traffic to drain.
    DisconnectNow { peer: PeerID, reason: u32 },
}
