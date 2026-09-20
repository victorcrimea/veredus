// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::messages::Guid;
use crate::relay::messages::Host;
use crate::relay::messages::PlayerSlots;

// -1 means observer or unassigned; 1..=8 are the playable slots.
pub const UNASSIGNED: i8 = -1;

// Status 2 is "stay ready": it is the one value a pre-game reset leaves alone.
pub const STATUS_NOT_READY: u8 = 0;
pub const STATUS_STAY_READY: u8 = 2;

pub struct PlayerSlot {
    pub uuid: Guid,
    pub name: String,
    pub slot: i8,
    pub status: u8,
    pub connected: bool,
}

// Disconnected entries are kept so a returning player can reclaim its slot,
// and omitted from every broadcast.
#[derive(Default)]
pub struct Slots {
    entries: Vec<PlayerSlot>,
}

impl Slots {
    // `recover` is set once the match has started: only then may an arriving
    // client take over a slot left behind by a departed one. The UUID of the
    // entry that was displaced is returned, so a caller that keys per-match
    // state off the UUID can carry it over to the one now holding the slot.
    pub fn add(&mut self, uuid: Guid, name: String, recover: bool) -> Option<Guid> {
        let mut slot = UNASSIGNED;
        let mut displaced = None;

        if recover && let Some(index) = self.recoverable(&uuid, &name) {
            let previous = self.entries.remove(index);
            slot = previous.slot;
            displaced = Some(previous.uuid);
        }

        self.entries.push(PlayerSlot {
            uuid,
            name,
            slot,
            status: STATUS_NOT_READY,
            connected: true,
        });

        displaced
    }

    // By UUID first, then by name, and never a slot a connected player holds.
    fn recoverable(&self, uuid: &Guid, name: &str) -> Option<usize> {
        let free = |slot: i8| {
            slot == UNASSIGNED || !self.entries.iter().any(|e| e.connected && e.slot == slot)
        };

        self.entries
            .iter()
            .position(|e| !e.connected && &e.uuid == uuid && free(e.slot))
            .or_else(|| {
                self.entries
                    .iter()
                    .position(|e| !e.connected && e.name == name && free(e.slot))
            })
    }

    pub fn mark_disconnected(&mut self, uuid: &Guid) {
        if let Some(entry) = self.entries.iter_mut().find(|e| &e.uuid == uuid) {
            entry.connected = false;
        }
    }

    // Any slot value is accepted and the status is untouched; the engine, not
    // the relay, decides what a slot number means.
    pub fn assign(&mut self, slot: i8, uuid: &Guid) {
        if slot != UNASSIGNED {
            for entry in self.entries.iter_mut() {
                if entry.slot == slot && &entry.uuid != uuid {
                    entry.slot = UNASSIGNED;
                }
            }
        }
        if let Some(entry) = self.entries.iter_mut().find(|e| &e.uuid == uuid) {
            entry.slot = slot;
        }
    }

    pub fn reset_pregame(&mut self) {
        for entry in self.entries.iter_mut() {
            if entry.status != STATUS_STAY_READY {
                entry.status = STATUS_NOT_READY;
            }
        }
    }

    pub fn set_status(&mut self, uuid: &Guid, status: u8) {
        if let Some(entry) = self.entries.iter_mut().find(|e| &e.uuid == uuid) {
            entry.status = status;
        }
    }

    pub fn slot_of(&self, uuid: &Guid) -> Option<i8> {
        self.entries
            .iter()
            .find(|e| &e.uuid == uuid)
            .map(|e| e.slot)
    }

    // Start is refused while any connected player still sits at "not ready".
    pub fn all_ready(&self) -> bool {
        !self
            .entries
            .iter()
            .any(|e| e.connected && e.status == STATUS_NOT_READY)
    }

    pub fn connected_players(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| e.connected && e.slot != UNASSIGNED)
            .count()
    }

    pub fn disconnected_players(&self) -> usize {
        self.entries
            .iter()
            .filter(|e| !e.connected && e.slot != UNASSIGNED)
            .count()
    }

    // A returning client is a joiner only if it matches a slot someone left.
    pub fn has_disconnected_named(&self, name: &str) -> bool {
        self.entries.iter().any(|e| !e.connected && e.name == name)
    }

    // Connected entries only, ordered by UUID string as the clients expect.
    pub fn to_message(&self) -> PlayerSlots {
        let mut hosts: Vec<&PlayerSlot> = self.entries.iter().filter(|e| e.connected).collect();
        hosts.sort_by(|a, b| a.uuid.0.cmp(&b.uuid.0));

        PlayerSlots {
            hosts: hosts
                .into_iter()
                .map(|e| Host {
                    guid: e.uuid.clone(),
                    name: e.name.clone(),
                    player_id: e.slot,
                    status: e.status,
                })
                .collect(),
        }
    }
}
