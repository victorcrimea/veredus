// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::server_fsm::DisconnectReason;

// Field names are &'static str rather than String: every variant is reachable
// from a hostile packet, so decoding a bad message must not allocate.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseError {
    #[error("buffer too short for {field}")]
    Truncated { field: &'static str },
    #[error("invalid UTF-8 in {field}")]
    BadUtf8 { field: &'static str },
    #[error("declared size {declared} but got {actual} bytes")]
    SizeMismatch { declared: usize, actual: usize },
    #[error("Wrong field size {declared} but should be {should_be} bytes")]
    WrongSize {
        field: &'static str,
        declared: usize,
        should_be: usize,
    },
    #[error("unknown message type {0}")]
    UnknownType(u8),
}

// A peer fault is never the server's error: it is the peer's input being
// unacceptable, and every variant maps to exactly one wire consequence.
#[derive(Debug, thiserror::Error)]
pub enum PeerFault {
    #[error("no session for peer")]
    NoSession,
    #[error("sender is not the controller")]
    NotController,
    #[error("message not accepted in this phase")]
    WrongPhase,
    #[error("malformed payload: {0}")]
    Malformed(#[from] ParseError),
    #[error("turn seal {got} out of sequence, expected {want}")]
    TurnSealOutOfSequence { got: u32, want: u32 },
    #[error("state hash for turn {got} out of sequence, expected {want}")]
    StateHashOutOfSequence { got: u32, want: u32 },
    #[error("turn seal {got} is past what ready turn {ready} allows")]
    TurnSealAhead { got: u32, ready: u32 },
    #[error("state hash for turn {got} is past ready turn {ready}")]
    StateHashAhead { got: u32, ready: u32 },
    #[error("gamestate transfer exceeds declared length")]
    TransferOverrun,
    #[error("kept sending far past its rate limit")]
    Flooding,
    #[error("loaded a game before any snapshot was handed to it")]
    LoadedBeforeSnapshot,
    #[error("loaded at turn {got}, but its snapshot is at turn {lo}..={hi}")]
    LoadedTurnOutOfRange { got: u32, lo: u32, hi: u32 },
}

impl PeerFault {
    // None means the message is dropped and the connection stays open, which
    // is what the protocol asks for anything unaccepted in the current phase.
    pub fn reason(&self) -> Option<DisconnectReason> {
        match self {
            PeerFault::TurnSealOutOfSequence { .. } | PeerFault::TurnSealAhead { .. } => {
                Some(DisconnectReason::OutOfSequenceTurnSeal)
            }
            PeerFault::StateHashOutOfSequence { .. } | PeerFault::StateHashAhead { .. } => {
                Some(DisconnectReason::OutOfSequenceStateHash)
            }
            // The protocol has no code for flooding; Kicked is what the
            // client shows for being removed over its own behaviour.
            PeerFault::Flooding => Some(DisconnectReason::Kicked),
            // Likewise no code exists for a joiner lying about its snapshot,
            // which only a tampered client can do.
            PeerFault::LoadedBeforeSnapshot | PeerFault::LoadedTurnOutOfRange { .. } => {
                Some(DisconnectReason::Kicked)
            }
            PeerFault::NoSession
            | PeerFault::NotController
            | PeerFault::WrongPhase
            | PeerFault::Malformed(_)
            | PeerFault::TransferOverrun => None,
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/relay/fault.rs"]
mod tests;
