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
    #[error("gamestate transfer exceeds declared length")]
    TransferOverrun,
}

impl PeerFault {
    // None means the message is dropped and the connection stays open, which
    // is what the protocol asks for anything unaccepted in the current phase.
    pub fn reason(&self) -> Option<DisconnectReason> {
        match self {
            PeerFault::TurnSealOutOfSequence { .. } => {
                Some(DisconnectReason::OutOfSequenceTurnSeal)
            }
            PeerFault::StateHashOutOfSequence { .. } => {
                Some(DisconnectReason::OutOfSequenceStateHash)
            }
            PeerFault::NoSession
            | PeerFault::NotController
            | PeerFault::WrongPhase
            | PeerFault::Malformed(_)
            | PeerFault::TransferOverrun => None,
        }
    }
}
