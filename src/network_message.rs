// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

use rusty_enet::PeerID;

use crate::relay::monitor::PeerStats;

// A per-peer inbound byte budget is charged on the ENet thread (which is
// where every peer's messages are counted) and released whenever this drops,
// which covers the whole time the copy is alive: queued in the channel and
// being decoded on the server thread. That is what lets the budget bound the
// channel without the server thread having to remember to give bytes back.
#[derive(Debug)]
pub struct InboundCredit {
    outstanding: Arc<AtomicUsize>,
    bytes: usize,
}

impl InboundCredit {
    pub fn new(outstanding: &Arc<AtomicUsize>, bytes: usize) -> Self {
        outstanding.fetch_add(bytes, Ordering::Relaxed);
        Self {
            outstanding: Arc::clone(outstanding),
            bytes,
        }
    }
}

impl Drop for InboundCredit {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

// Raw bytes cross the thread boundary on purpose: only the game-server thread
// owns the message catalog, so the ENet thread stays a dumb packet pump.
#[derive(Debug)]
pub enum InboundNetworkMessage {
    Connect {
        peer: PeerID,
        addr: Ipv4Addr,
    },
    Disconnect {
        peer: PeerID,
        addr: Ipv4Addr,
        reason: u32,
    },
    Message {
        peer: PeerID,
        data: Vec<u8>,
        // Dropped once the server thread is done with `data`, which is what
        // charges the peer's inbound budget for the whole time the copy is
        // alive rather than just while it sits in the channel.
        credit: InboundCredit,
    },
    // Peer timing is only reachable from the thread that owns the host, so it
    // is sampled there and carried over rather than looked up on demand. The
    // packet loss and the byte totals only feed metrics, which is why they
    // travel beside the timing rather than inside it.
    Stats {
        stats: Vec<PeerStats>,
        // ENet's own scale, where PEER_PACKET_LOSS_SCALE means every packet.
        packet_loss: Vec<(PeerID, u32)>,
        // Host-wide totals since the socket opened, wrapping at u32.
        bytes_received: u32,
        bytes_sent: u32,
    },
}

#[derive(Debug)]
pub enum OutboundNetworkMessage {
    Message { peer: PeerID, data: Vec<u8> },
    Disconnect { peer: PeerID, reason: u32 },
    // Shutdown cannot wait for queued reliable traffic to drain.
    DisconnectNow { peer: PeerID, reason: u32 },
}
