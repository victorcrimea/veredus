// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::Ipv4Addr;

use chrono::TimeDelta;

use crate::relay::messages::Guid;

// The protocol describes six session phases, but says they are behaviour
// classes rather than a data structure and warns against mirroring them as an
// enum. The four phases before admission differ only in which handshake field
// has been filled, so they are encoded as field presence; only the three
// classes that survive admission need a discriminant.
pub struct Session {
    pub addr: Ipv4Addr,
    // None until SYN_ACK is accepted, so it doubles as the handshake-pending marker.
    pub uuid: Option<Guid>,
    // Only ever set in lobby mode, by the lobby-auth IQ.
    pub lobby_name: Option<String>,
    pub admitted: Option<Admitted>,
    pub mean_rtt: TimeDelta,
    pub since_last_received: TimeDelta,
}

pub struct Admitted {
    // u16 because that is the whole width the wire carries.
    pub client_id: u16,
    // Sanitized, and deduplicated when the policy allows duplicates.
    pub name: String,
    pub role: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Setup,
    // A joiner fetching a snapshot. It receives no in-game traffic and is not
    // registered with the turn manager, so it never blocks turn release.
    Syncing,
    InGame,
}

impl Session {
    pub fn new(addr: Ipv4Addr) -> Self {
        Session {
            addr,
            uuid: None,
            lobby_name: None,
            admitted: None,
            mean_rtt: TimeDelta::zero(),
            since_last_received: TimeDelta::zero(),
        }
    }

    pub fn role(&self) -> Option<Role> {
        self.admitted.as_ref().map(|a| a.role)
    }

    pub fn is_setup(&self) -> bool {
        self.role() == Some(Role::Setup)
    }

    pub fn is_syncing(&self) -> bool {
        self.role() == Some(Role::Syncing)
    }

    pub fn is_in_game(&self) -> bool {
        self.role() == Some(Role::InGame)
    }

    // Anything that has not finished AUTHENTICATE, whatever it is waiting on.
    pub fn is_unauthenticated(&self) -> bool {
        self.admitted.is_none()
    }

    pub fn name(&self) -> Option<&str> {
        self.admitted.as_ref().map(|a| a.name.as_str())
    }

    pub fn client_id(&self) -> Option<u16> {
        self.admitted.as_ref().map(|a| a.client_id)
    }

    pub fn set_role(&mut self, role: Role) {
        if let Some(admitted) = self.admitted.as_mut() {
            admitted.role = role;
        }
    }
}
