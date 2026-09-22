// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::sync::Arc;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use rusty_enet::PeerID;

use crate::relay::fault::PeerFault;
use crate::relay::messages::GamestateChunk;

// Chunks stay well under the 1372 MTU, and the window bounds how much of a
// transfer is in flight at once.
pub const CHUNK_SIZE: usize = 1024;
pub const WINDOW: u32 = 32;
pub const MAX_TRANSFER: u32 = 8 * 1024 * 1024;

pub const KIND_SAVEGAME: i8 = 0;
pub const KIND_RUNNING_GAME: i8 = 1;

// Why the server asked a client for state, so the completed bytes can be
// routed without a second lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    // A snapshot pulled from a playing client on behalf of a joiner. Keyed per
    // request rather than a single cache slot, so concurrent joins cannot race.
    JoinSnapshot { joiner: PeerID },
    Savegame,
}

struct Outgoing {
    data: Arc<Vec<u8>>,
    offset: usize,
    in_flight: u32,
}

struct Incoming {
    purpose: Purpose,
    declared: usize,
    buf: Vec<u8>,
    // When the stall clock last started. Only Input::Tick brings the time,
    // so progress clears it and the next tick restarts it.
    last_progress: Option<DateTime<Utc>>,
}

#[derive(Default)]
pub struct Transfers {
    // This endpoint's own request ids. The peer numbers its requests
    // independently, so the two spaces never need to agree.
    next_request_id: u32,
    // Keyed by the peer plus the id its requester allocated. The message type
    // already says which side requested, so the two maps cannot collide.
    outgoing: HashMap<(PeerID, u32), Outgoing>,
    incoming: HashMap<(PeerID, u32), Incoming>,
    // A peer may download only the snapshot its own JOIN was built for, and
    // only once. Anything else would hand the match state to whoever asks,
    // and let a delayed observer fetch state its feed has not reached yet.
    granted: HashMap<PeerID, Arc<Vec<u8>>>,
}

impl Transfers {
    pub fn allocate(&mut self) -> u32 {
        self.next_request_id += 1;
        self.next_request_id
    }

    pub fn expect(&mut self, peer: PeerID, request_id: u32, purpose: Purpose) {
        self.incoming.insert(
            (peer, request_id),
            Incoming {
                purpose,
                declared: 0,
                buf: Vec::new(),
                last_progress: None,
            },
        );
    }

    pub fn purpose(&self, peer: PeerID, request_id: u32) -> Option<Purpose> {
        self.incoming.get(&(peer, request_id)).map(|rx| rx.purpose)
    }

    // A source may take as long as it likes overall, but not sit idle:
    // nothing on the wire would ever tell the joiner it was abandoned.
    // Returns (source, joiner) for every join snapshot dropped here. A clock
    // step backwards restarts the wait rather than firing early.
    pub fn stalled(&mut self, now: DateTime<Utc>, limit: TimeDelta) -> Vec<(PeerID, PeerID)> {
        let mut dropped = Vec::new();
        self.incoming.retain(|(source, _), rx| {
            let Purpose::JoinSnapshot { joiner } = rx.purpose else {
                return true;
            };
            let since = *rx.last_progress.get_or_insert(now);
            let idle = now - since;
            if idle < TimeDelta::zero() {
                rx.last_progress = Some(now);
                return true;
            }
            if idle < limit {
                return true;
            }
            dropped.push((*source, joiner));
            false
        });
        dropped
    }

    pub fn grant(&mut self, peer: PeerID, data: Arc<Vec<u8>>) {
        self.granted.insert(peer, data);
    }

    pub fn take_grant(&mut self, peer: PeerID) -> Option<Arc<Vec<u8>>> {
        self.granted.remove(&peer)
    }

    // Returns the chunks that fit the window straight away; an empty payload
    // is refused because the client rejects a zero length and then hangs.
    pub fn begin_send(
        &mut self,
        peer: PeerID,
        request_id: u32,
        data: Arc<Vec<u8>>,
    ) -> Option<(u32, Vec<GamestateChunk>)> {
        let length = u32::try_from(data.len()).ok()?;
        if length == 0 || length > MAX_TRANSFER {
            return None;
        }
        self.outgoing.insert(
            (peer, request_id),
            Outgoing {
                data,
                offset: 0,
                in_flight: 0,
            },
        );
        Some((length, self.pump(peer, request_id)))
    }

    // Fills the window with whatever is left to send.
    fn pump(&mut self, peer: PeerID, request_id: u32) -> Vec<GamestateChunk> {
        let mut chunks = Vec::new();
        let Some(tx) = self.outgoing.get_mut(&(peer, request_id)) else {
            return chunks;
        };
        while tx.in_flight < WINDOW && tx.offset < tx.data.len() {
            let end = (tx.offset + CHUNK_SIZE).min(tx.data.len());
            chunks.push(GamestateChunk {
                request_id,
                data: tx.data[tx.offset..end].to_vec(),
            });
            tx.offset = end;
            tx.in_flight += 1;
        }
        if tx.offset >= tx.data.len() && tx.in_flight == 0 {
            self.outgoing.remove(&(peer, request_id));
        }
        chunks
    }

    // An ack for an unknown transfer, or one past what is in flight, is an
    // error on the peer's side and is simply dropped.
    pub fn on_ack(&mut self, peer: PeerID, request_id: u32, count: u32) -> Vec<GamestateChunk> {
        let Some(tx) = self.outgoing.get_mut(&(peer, request_id)) else {
            return Vec::new();
        };
        if count == 0 || count > tx.in_flight {
            return Vec::new();
        }
        tx.in_flight -= count;
        let done = tx.offset >= tx.data.len() && tx.in_flight == 0;
        if done {
            self.outgoing.remove(&(peer, request_id));
            return Vec::new();
        }
        self.pump(peer, request_id)
    }

    pub fn on_response(
        &mut self,
        peer: PeerID,
        request_id: u32,
        length: u32,
    ) -> Result<(), PeerFault> {
        let rx = self
            .incoming
            .get_mut(&(peer, request_id))
            .ok_or(PeerFault::WrongPhase)?;
        if length == 0 || length > MAX_TRANSFER {
            return Err(PeerFault::TransferOverrun);
        }
        rx.declared = length as usize;
        rx.buf = Vec::with_capacity(rx.declared);
        rx.last_progress = None;
        Ok(())
    }

    // Returns the finished payload and why it was requested, once the total
    // matches exactly what was declared.
    pub fn knows_incoming(&self, peer: PeerID, request_id: u32) -> bool {
        self.incoming.contains_key(&(peer, request_id))
    }

    pub fn on_chunk(
        &mut self,
        peer: PeerID,
        request_id: u32,
        data: &[u8],
    ) -> Result<Option<(Purpose, Vec<u8>)>, PeerFault> {
        let rx = self
            .incoming
            .get_mut(&(peer, request_id))
            .ok_or(PeerFault::WrongPhase)?;
        rx.buf.extend_from_slice(data);
        rx.last_progress = None;
        if rx.buf.len() > rx.declared {
            self.incoming.remove(&(peer, request_id));
            return Err(PeerFault::TransferOverrun);
        }
        if rx.buf.len() < rx.declared {
            return Ok(None);
        }
        let rx = self
            .incoming
            .remove(&(peer, request_id))
            .expect("just seen");
        Ok(Some((rx.purpose, rx.buf)))
    }

    // A departing peer takes both directions of its transfers, and any
    // download it was granted, with it. A snapshot fetched for it is dropped
    // too, or a later peer handed the same id would be served it. Returns the
    // joiners whose snapshot this peer was sourcing, so they can be re-sourced.
    pub fn forget(&mut self, peer: PeerID) -> Vec<PeerID> {
        self.outgoing.retain(|(p, _), _| *p != peer);
        let mut orphaned = Vec::new();
        self.incoming.retain(|(source, _), rx| match rx.purpose {
            Purpose::JoinSnapshot { joiner } if joiner == peer => false,
            Purpose::JoinSnapshot { joiner } if *source == peer => {
                orphaned.push(joiner);
                false
            }
            _ => *source != peer,
        });
        self.granted.remove(&peer);
        orphaned
    }
}
