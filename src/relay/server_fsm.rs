// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::collections::HashSet;
use std::net::Ipv4Addr;
use std::sync::Arc;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use rusty_enet::PeerID;

use crate::lobby::link::LobbyMap;
use crate::relay::auth;
use crate::relay::auth::LateObserverPolicy;
use crate::relay::fault::PeerFault;
use crate::relay::gamestate_transfer::KIND_RUNNING_GAME;
use crate::relay::gamestate_transfer::KIND_SAVEGAME;
use crate::relay::gamestate_transfer::Purpose;
use crate::relay::gamestate_transfer::Transfers;
use crate::relay::messages::Ack;
use crate::relay::messages::Authenticate;
use crate::relay::messages::AuthenticateResult;
use crate::relay::messages::AuthenticateResultCode;
use crate::relay::messages::Chat;
use crate::relay::messages::EnabledMod;
use crate::relay::messages::Flare;
use crate::relay::messages::GameSettings;
use crate::relay::messages::GamestateChunk;
use crate::relay::messages::GamestateChunkAck;
use crate::relay::messages::GamestateRequest;
use crate::relay::messages::GamestateResponse;
use crate::relay::messages::Guid;
use crate::relay::messages::Host;
use crate::relay::messages::Join;
use crate::relay::messages::Joined;
use crate::relay::messages::Kicked;
use crate::relay::messages::LaggingClients;
use crate::relay::messages::LastSeen;
use crate::relay::messages::LoadedGame;
use crate::relay::messages::MapPlayerIdToSlot;
use crate::relay::messages::PerformanceEntry;
use crate::relay::messages::PlayerCommand;
use crate::relay::messages::PlayerPause;
use crate::relay::messages::PlayersLoading;
use crate::relay::messages::PreGameStatus;
use crate::relay::messages::StartSavegameSettings;
use crate::relay::messages::StartSettings;
use crate::relay::messages::StateHash;
use crate::relay::messages::Syn;
use crate::relay::messages::SynAck;
use crate::relay::messages::TurnSealed;
use crate::relay::messages::WireMessage;
use crate::relay::messages::WrongHashPlayers;
use crate::relay::monitor::AFK_SILENCE_LIMIT;
use crate::relay::monitor::Monitor;
use crate::relay::monitor::PeerStats;
use crate::relay::monitor::Warning;
use crate::relay::observer_feed;
use crate::relay::observer_feed::ObserverFeed;
use crate::relay::password;
use crate::relay::pause_budget;
use crate::relay::pause_budget::BudgetEvent;
use crate::relay::pause_budget::PauseBudget;
use crate::relay::script_value;
use crate::relay::session::Admitted;
use crate::relay::session::Role;
use crate::relay::session::Session;
use crate::relay::slots::STATUS_NOT_READY;
use crate::relay::slots::Slots;
use crate::relay::slots::UNASSIGNED;
use crate::relay::turn::INITIAL_READY_TURN;
use crate::relay::turn::MatchLog;
use crate::relay::turn::TurnManager;
use crate::sidecar::BaseState;
use crate::sidecar::DumpRequest;

const SYN_CHALLENGE: u32 = 0x5073013F;
const GAME_VERSION: u32 = 0x01010019;
pub(crate) const SIMULATION_VERSION: &str = "0.28.0";

// The one flag bit the ACK carries, telling the client to authenticate over
// the lobby instead of answering straight away.
const ACK_FLAG_LOBBY_AUTH: u32 = 0x1;

// Matches the ENet peer limit, so the socket layer rather than this cap is
// what a client hits first. The last peer stays a spare: admission rejects on
// reaching the cap, so a client arriving at a full server can still be told
// that it is full.
const MAX_SESSIONS: usize = 200;

const LOGIN_MESSAGE: &str = "Logged in";

// A client seals this many turns ahead of the one it is about to simulate.
const COMMAND_DELAY: u32 = 4;

// Turn 0 is never simulated, so a fresh client owes its first state hash for
// turn 1 and its simulated-turn counter starts one below that.
const FIRST_SIMULATED_TURN: u32 = 0;

// How many fresh UUIDs to try before giving up on issuing a unique one.
const UUID_ATTEMPTS: usize = 8;

// How long a held start waits for the AI host to be admitted. Long enough for
// a cold pyrogenesis to boot and connect, short enough that a controller whose
// start is refused is not left waiting for nothing.
const AI_HOST_ADMIT_TIMEOUT: TimeDelta = TimeDelta::seconds(30);

// The per-slot fields that make a stock client register and run an AI.
const AI_FIELDS: [&str; 3] = ["AI", "AIDiff", "AIBehavior"];

#[derive(Debug)]
pub enum Input {
    Connected {
        peer: PeerID,
        addr: Ipv4Addr,
    },
    Received {
        peer: PeerID,
        msg: WireMessage,
    },
    Disconnected {
        peer: PeerID,
    },
    LobbyAuth {
        username: String,
        token: String,
    },
    // Time and peer timing both enter here, so the FSM never reads a clock.
    Tick {
        now: DateTime<Utc>,
        stats: Vec<PeerStats>,
    },
    // A finished one-shot state dump from the IO side. None means the dump
    // failed and the joiners waiting on it need the fallback path. The id
    // matches the run that produced it, so a stale reply from a cancelled run
    // is ignored instead of being credited to a newer one.
    StateDumped {
        id: u32,
        state: Option<Vec<u8>>,
    },
    // The AI host process could not be spawned or has exited, from the IO
    // side, which is the only place that can see the child.
    AiHostExited,
    // A finished link of the rolling checkpoint chain. None means the run
    // failed or diverged. The id plays the same role as in StateDumped.
    // `players` is each player's state at that turn as the replay saw it,
    // indexed by player id with gaia first; empty when the run failed.
    Checkpointed {
        id: u32,
        state: Option<Vec<u8>>,
        players: Vec<String>,
    },
}

#[derive(Debug, PartialEq)]
pub enum Effect {
    Send {
        peer: PeerID,
        msg: WireMessage,
    },
    Disconnect {
        peer: PeerID,
        reason: DisconnectReason,
    },
    // Shutdown must not wait for queued reliable traffic.
    DisconnectNow {
        peer: PeerID,
        reason: DisconnectReason,
    },
    // Sec. 17.3: sent once a map is selected and again on any settings or
    // player-slot change, while still in setup. Debounced and suppressed
    // when unchanged, but that is the lobby layer's job, not the FSM's.
    LobbyListing {
        host_username: String,
        nbp: u32,
        players: String,
        map: Option<LobbyMap>,
        mods: Vec<EnabledMod>,
    },
    // Sec. 17.3: sent once, right after the last LobbyListing before a match
    // starts.
    LobbyStarted {
        nbp: u32,
        players: String,
    },
    // The idle-shutdown timeout elapsed (A7). The game thread reacts by
    // shutting every session down and returning.
    GameOver,
    // A one-shot pyrogenesis run should rebuild the state at `turn` for
    // joiners no live client can serve. The IO side answers with
    // Input::StateDumped. The id pairs each reply with its run, so a late
    // reply from a cancelled run cannot be taken for the current one.
    StateDump {
        id: u32,
        turn: u32,
        request: DumpRequest,
    },
    // The last joiner waiting on a dump left, so the run is no longer needed.
    // The IO side kills its pyrogenesis when the id matches its in-flight run.
    CancelStateDump {
        id: u32,
    },
    // A match with AI slots is starting, so the IO side launches the AI host
    // that will join under `name` and play those slots.
    SpawnAiHost {
        name: String,
    },
    // The AI host is no longer wanted; the IO side kills and reaps it.
    StopAiHost,
    // A one-shot pyrogenesis run should advance the rolling checkpoint chain
    // to `turn`, resuming from the request's base. The IO side also records
    // the match's progress from the same run, and answers with
    // Input::Checkpointed.
    Checkpoint {
        id: u32,
        turn: u32,
        request: DumpRequest,
    },
    // The match has been decided. `checkpoint` names the run whose result
    // proved it, so the IO side can record that result as final; None when
    // it was worked out without one.
    MatchEnded {
        checkpoint: Option<u32>,
    },
}

// Values travel as the ENet disconnect `data` word; only the number reaches the client.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisconnectReason {
    ServerShuttingDown = 2,
    GameVersionMismatch = 3,
    ServerLoading = 4,
    MatchInProgress = 5,
    Kicked = 6,
    Banned = 7,
    NameInUse = 8,
    ServerFull = 9,
    LobbyAuthFailed = 10,
    NoUuid = 11,
    OutOfSequenceTurnSeal = 12,
    OutOfSequenceStateHash = 13,
    Refused = 14,
    SimulationOrModMismatch = 17,
}

#[derive(Clone)]
pub struct Config {
    pub enabled_mods: Vec<EnabledMod>,
    pub lobby_mode: bool,
    pub turn_length_ms: u32,
    // The stored hash H, not a plaintext password. Empty means an open server,
    // which is the only thing direct-IP clients can join.
    pub server_password_hash: String,
    // Empty means the first client to authenticate becomes controller, because
    // that is the secret every stock client sends.
    pub controller_secret: String,
    pub allow_duplicate_names: bool,
    pub late_observer_policy: LateObserverPolicy,
    pub observer_limit: usize,
    // None means an observer never blocks turn release.
    pub observer_lag_limit: Option<u32>,
    // How many turns behind the players observers watch. 0 puts them on the
    // live stream, where they may not pause.
    pub observer_delay_turns: u32,
    pub buddies: HashSet<String>,
    pub max_sessions: usize,
    // Losing the controller is permanent in the stock server, which strands
    // the match with nobody able to start or configure it.
    pub release_controller_on_leave: bool,
    // The name the relay speaks under. It occupies an observer row in the slot
    // list, because that list is where a client looks a chat sender's name up.
    // Displayed behind a reserved prefix that no client may authenticate with,
    // so whatever an operator puts here cannot collide with a player.
    pub server_name: String,
    // Empty greets an arriving client with nothing at all.
    pub welcome_message: String,
    // How long a player may hold the match paused across the whole game. A
    // value longer than any match anyone would play is how the policy is
    // turned off, so there is no second switch to keep in step with this one.
    pub pause_budget: TimeDelta,
    // When set, the match is held for a player who drops or goes silent
    // mid-match, charged to that player's pause budget, so leaving is never
    // a way around it.
    pub afk_pause: bool,
    // How long the game may sit with nobody ever having joined, or with
    // everybody gone, before it shuts itself down. None means never, which is
    // what keeps a standalone game running with nobody watching it.
    pub idle_shutdown: Option<TimeDelta>,
    // The hostme sender, used as the lobby listing's hostUsername until a
    // controller with a name of its own is admitted.
    pub lobby_host_name: String,
    // When set, a joiner no live client can serve gets its snapshot from a
    // one-shot pyrogenesis instead of being dropped, and a match that ran is
    // replayed for its outcome once it ends. The IO side owns the actual
    // path; this flag is what the FSM gates on.
    pub sidecar_dumps: bool,
    // When set, a match whose settings have AI slots gets a pyrogenesis AI
    // host that plays them, so no stock client has to compute the AI.
    pub hosted_ai: bool,
    // How many released turns a match runs between two sidecar checkpoints.
    // Each one resumes from the last, so a joiner is served a recent state
    // without anyone serializing for it, the outcome is followed while the
    // match runs, and no replay ever starts from turn 0 again. 0 turns them
    // off; they also need sidecar_dumps.
    pub checkpoint_interval_turns: u32,
    // How long a decided match keeps running for the players who stay to
    // watch or chat before the game shuts down.
    pub post_game_linger: TimeDelta,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            enabled_mods: vec![EnabledMod {
                name: "0ad".to_string(),
                version: SIMULATION_VERSION.to_string(),
            }],
            lobby_mode: false,
            turn_length_ms: 200,
            server_password_hash: String::new(),
            controller_secret: String::new(),
            allow_duplicate_names: false,
            late_observer_policy: LateObserverPolicy::default(),
            observer_limit: 8,
            observer_lag_limit: None,
            observer_delay_turns: observer_feed::DEFAULT_DELAY_TURNS,
            buddies: HashSet::new(),
            max_sessions: MAX_SESSIONS,
            release_controller_on_leave: true,
            server_name: "SERVER".to_string(),
            welcome_message: String::new(),
            pause_budget: pause_budget::DEFAULT_BUDGET,
            afk_pause: true,
            idle_shutdown: None,
            lobby_host_name: String::new(),
            sidecar_dumps: false,
            hosted_ai: false,
            checkpoint_interval_turns: 0,
            post_game_linger: TimeDelta::minutes(5),
        }
    }
}

pub struct FrozenSettings {
    // Kept verbatim because JOIN must carry the same text to late joiners.
    pub json: Vec<u8>,
    pub cheats_enabled: bool,
    pub turn_length_ms: u32,
    // Every player id in the match, AI included. Empty when the settings
    // could not be read, which keeps the resign rule from ever firing.
    pub player_ids: Vec<i32>,
    // With no victory condition the engine never declares anyone the
    // winner, so running out of opponents does not end the match.
    pub endless: bool,
}

// The server-wide phase, as the authentication rules see it. It is derived
// from the typestate rather than stored, so it cannot drift out of step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Loading,
    InGame,
    PostGame,
}

pub(crate) struct Context {
    config: Config,
    sessions: HashMap<PeerID, Session>,
    slots: Slots,
    controller: Option<Guid>,
    // The relay's own identity as a chat sender. Issued once per game and
    // never reused for a session, so it names the server and nothing else.
    server_uuid: Guid,
    next_client_id: u16,
    banned_ips: HashSet<Ipv4Addr>,
    banned_names: HashSet<String>,
    // Players whose leaving the match does not wait for, because they were
    // removed on purpose or gave up and are not coming back.
    forfeited: HashSet<Guid>,
    resigned: HashSet<i32>,
    transfers: Transfers,
    monitor: Monitor,
    pause_budget: PauseBudget,
    turns: TurnManager,
    log: MatchLog,
    feed: ObserverFeed,
    // Served to every joiner that asks, so it is shared rather than copied.
    join_snapshot: Option<Arc<Vec<u8>>>,
    // The one in-flight state dump, if any. One run at a time bounds how many
    // engine processes a game can accumulate, and joiners asking together
    // share its cost.
    dump: Option<PendingDump>,
    next_dump_id: u32,
    checkpoints: CheckpointChain,
    // Both fed only by Input::Tick.now (A2). created_at is set on the first
    // tick; empty_since only once the game has held a player and lost every
    // one again.
    created_at: Option<DateTime<Utc>>,
    empty_since: Option<DateTime<Utc>>,
    // The name the AI host authenticates under. Fixed per game, and only
    // honoured from loopback while an AI host is expected, so a remote client
    // that copies it off the slot list gains nothing.
    ai_host_name: String,
    // Present from the moment a hosted-AI start is requested until the AI
    // host is gone.
    ai_host: Option<AiHost>,
    // The controller's latest GAME_SETTINGS, decoded for the lobby listing
    // (Sec. 17.3). None until the controller has sent one; a decode failure
    // keeps whatever was last decoded rather than clearing it.
    lobby_map: Option<LobbyMap>,
    effects: Vec<Effect>,
}

struct AiHost {
    // None until it is admitted.
    peer: Option<PeerID>,
    // The AI player ids it may send commands for.
    players: HashSet<i32>,
}

// The one in-flight one-shot state dump: the turn it rebuilds and every
// joiner waiting on it. Joiners asking while it runs attach to it whatever
// turn they asked for, instead of starting a second engine process.
struct PendingDump {
    id: u32,
    turn: u32,
    waiting: Vec<PeerID>,
}

// The rolling checkpoint chain: the states already built, ascending by turn,
// and the one run that extends it, if any. Only the newest state at or before
// the delayed feed and the ones after it are kept, since nothing ever asks
// for an older one.
#[derive(Default)]
struct CheckpointChain {
    states: Vec<BaseState>,
    pending: Option<PendingCheckpoint>,
    next_id: u32,
    // Consecutive failed runs that resumed from a checkpoint. A bad state
    // would fail every run after it, so enough of them drop the chain and
    // the next run starts over from turn 0.
    failures: u8,
    // Set when a run from turn 0 fails. It would fail again at the next
    // interval, paying for the whole match every time, so the match goes
    // without checkpoints instead.
    disabled: bool,
    // Set while the match is held for an absent player: the turn is frozen,
    // so a run now lands on exactly the state the returning player needs,
    // and nobody still playing has to serialize it for them.
    prefetch: bool,
    // Set when something suggests the match may just have been decided: a
    // player resigned or left. The next run is due once the ready turn has
    // passed this turn, rather than at the next interval, so the end is
    // noticed while the players are still there to be told.
    probe: Option<u32>,
}

struct PendingCheckpoint {
    id: u32,
    turn: u32,
    from_scratch: bool,
}

// Two in a row is not a one-off: a run that times out on a loaded host has
// already been retried once from the same base.
const CHECKPOINT_FAILURE_LIMIT: u8 = 2;

impl CheckpointChain {
    fn at_or_before(&self, turn: u32) -> Option<&BaseState> {
        self.states.iter().rev().find(|b| b.turn <= turn)
    }

    // The earliest hint wins, so a later one cannot push back a check that
    // is already due.
    fn probe_at(&mut self, turn: u32) {
        let probe = self.probe.get_or_insert(turn);
        *probe = (*probe).min(turn);
    }
}

// What a completed inbound transfer was for. The payload of a join snapshot
// is cached here; only the savegame bytes travel back out, because that is
// what drives a phase transition.
pub(crate) enum TransferDone {
    JoinSnapshot { joiner: PeerID },
    Savegame(Vec<u8>),
}

impl Context {
    fn send(&mut self, peer: PeerID, msg: WireMessage) {
        self.effects.push(Effect::Send { peer, msg });
    }

    // Fan-out is expanded here because the transport moves one packet per peer.
    fn broadcast(&mut self, msg: &WireMessage, accept: impl Fn(&Session) -> bool) {
        let peers: Vec<PeerID> = self
            .sessions
            .iter()
            .filter(|(_, s)| accept(s))
            .map(|(p, _)| *p)
            .collect();
        for peer in peers {
            self.effects.push(Effect::Send {
                peer,
                msg: msg.clone(),
            });
        }
    }

    fn broadcast_except(
        &mut self,
        except: PeerID,
        msg: &WireMessage,
        accept: impl Fn(&Session) -> bool,
    ) {
        let peers: Vec<PeerID> = self
            .sessions
            .iter()
            .filter(|(p, s)| **p != except && accept(s))
            .map(|(p, _)| *p)
            .collect();
        for peer in peers {
            self.effects.push(Effect::Send {
                peer,
                msg: msg.clone(),
            });
        }
    }

    // Everyone taking part in the match, whichever phase they are in.
    fn broadcast_player_slots(&mut self) {
        let mut slots = self.slots.to_message();
        // A client resolves a chat sender's name out of this list, and faults
        // on a sender it cannot find there, so the relay needs a row of its
        // own before it can say anything. The row is appended rather than
        // sorted in, and the slot table never holds it: it is not a player,
        // so readiness, slot recovery and the kick lookup must not see it.
        slots.hosts.push(Host {
            guid: self.server_uuid.clone(),
            name: auth::server_display_name(&self.config.server_name),
            player_id: UNASSIGNED,
            status: STATUS_NOT_READY,
        });
        let msg = WireMessage::PlayerSlots(slots);
        self.broadcast(&msg, |s| s.is_setup() || s.is_syncing() || s.is_in_game());
    }

    // Sec. 17.3: nbp/players are the connected player slots, not sessions;
    // an observer holds no slot and is never counted.
    fn lobby_counts(&self) -> (u32, String) {
        let names = self.slots.connected_player_names();
        (names.len() as u32, names.join(", "))
    }

    // The controller's name once one is admitted, else the hostme sender
    // that is standing in for it (Sec. 17.3's hostUsername).
    fn lobby_host_username(&self) -> String {
        self.controller
            .as_ref()
            .and_then(|c| self.name_of(c))
            .unwrap_or_else(|| self.config.lobby_host_name.clone())
    }

    fn push_lobby_listing(&mut self) {
        let (nbp, players) = self.lobby_counts();
        let host_username = self.lobby_host_username();
        self.effects.push(Effect::LobbyListing {
            host_username,
            nbp,
            players,
            map: self.lobby_map.clone(),
            mods: self.config.enabled_mods.clone(),
        });
    }

    // A line in the stock chat box, spoken by the relay itself. `to` is None
    // for everyone admitted, whichever phase they are in, because a client
    // renders chat from the moment it finishes authenticating. The receiver
    // list is empty exactly as a relayed copy of player chat is, so a targeted
    // line is indistinguishable from a broadcast one on the wire.
    fn server_chat(&mut self, to: Option<PeerID>, text: &str) {
        let msg = WireMessage::Chat(Chat {
            sender_guid: self.server_uuid.clone(),
            message: text.to_string(),
            receivers: Vec::new(),
        });
        match to {
            Some(peer) => self.send(peer, msg),
            None => self.broadcast(&msg, |s| s.admitted.is_some()),
        }
    }

    // The updated slot list goes out before the peer is dropped, so the
    // departing client still sees itself leave.
    fn disconnect(&mut self, peer: PeerID, reason: DisconnectReason) {
        let admitted = self
            .sessions
            .get(&peer)
            .is_some_and(|s| s.admitted.is_some());
        if admitted {
            if let Some(uuid) = self.uuid_of(peer) {
                self.slots.mark_disconnected(&uuid);
            }
            self.broadcast_player_slots();
        }
        self.effects.push(Effect::Disconnect { peer, reason });
    }

    fn fault(&mut self, peer: PeerID, fault: PeerFault) {
        match fault.reason() {
            Some(reason) => {
                tracing::info!(?peer, %fault, ?reason, "disconnecting peer");
                self.disconnect(peer, reason);
            }
            None => tracing::debug!(?peer, %fault, "message dropped"),
        }
    }

    fn uuid_of(&self, peer: PeerID) -> Option<Guid> {
        self.sessions.get(&peer)?.uuid.clone()
    }

    fn is_ai_host(&self, peer: PeerID) -> bool {
        self.ai_host.as_ref().is_some_and(|h| h.peer == Some(peer))
    }

    fn is_ai_host_uuid(&self, uuid: &Guid) -> bool {
        self.ai_host
            .as_ref()
            .and_then(|h| h.peer)
            .and_then(|p| self.uuid_of(p))
            .is_some_and(|u| &u == uuid)
    }

    // The AI host is launched on this machine and dials loopback, so only a
    // loopback session can be it, and only before it has been admitted.
    fn ai_host_expected(&self, addr: Ipv4Addr) -> bool {
        addr.is_loopback() && self.ai_host.as_ref().is_some_and(|h| h.peer.is_none())
    }

    // Which messages a session may send depends on the session's own phase,
    // not the server's. Handing back the UUID from the same call is what keeps
    // the two together: a handler cannot name its sender without first
    // establishing that the sender was allowed to speak.
    fn speaker(&self, peer: PeerID, allowed: impl Fn(&Session) -> bool) -> Result<Guid, PeerFault> {
        let session = self.sessions.get(&peer).ok_or(PeerFault::NoSession)?;
        if !allowed(session) {
            return Err(PeerFault::WrongPhase);
        }
        session.uuid.clone().ok_or(PeerFault::NoSession)
    }

    // The players the match should be held for. A reclaimed slot counts
    // until its client is back in-game, so a rejoiner cannot stall the
    // others in sync for free, and a player whose quota is spent is let go.
    fn afk_absent(&self) -> Vec<Guid> {
        self.slots
            .entries()
            .filter(|e| e.slot != UNASSIGNED)
            .filter(|e| !self.forfeited.contains(&e.uuid))
            .filter(|e| !self.resigned.contains(&i32::from(e.slot)))
            .filter(|e| self.pause_budget.remaining(&e.uuid) > TimeDelta::zero())
            .filter(|e| {
                let session = self
                    .sessions
                    .values()
                    .find(|s| s.uuid.as_ref() == Some(&e.uuid));
                match session {
                    None => true,
                    Some(s) if !e.connected || !s.is_in_game() => true,
                    Some(s) => s.since_last_received > AFK_SILENCE_LIMIT,
                }
            })
            .map(|e| e.uuid.clone())
            .collect()
    }

    // The slot table outlives the session, so it still has a name for a
    // player who has left.
    fn absent_name(&self, uuid: &Guid) -> String {
        self.slots
            .name_of(uuid)
            .map(str::to_string)
            .unwrap_or_else(|| uuid.to_string())
    }

    fn peer_of(&self, uuid: &Guid) -> Option<PeerID> {
        self.sessions
            .iter()
            .find(|(_, s)| s.uuid.as_ref() == Some(uuid))
            .map(|(p, _)| *p)
    }

    // The name a client would show for this UUID, which is what a line the
    // relay speaks about someone has to use. None once they have left.
    fn name_of(&self, uuid: &Guid) -> Option<String> {
        self.sessions
            .values()
            .find(|s| s.uuid.as_ref() == Some(uuid))
            .and_then(|s| s.admitted.as_ref())
            .map(|a| a.name.clone())
    }

    // A message from anyone but the controller is silently ignored: no
    // disconnect, and no error back to the sender.
    fn require_controller(&self, peer: PeerID) -> Result<Guid, PeerFault> {
        let uuid = self.uuid_of(peer).ok_or(PeerFault::NoSession)?;
        if self.controller.as_ref() == Some(&uuid) {
            Ok(uuid)
        } else {
            Err(PeerFault::NotController)
        }
    }

    // An observer for turn-release purposes only. The controller keeps
    // blocking even when it holds no slot.
    // The AI host holds no slot but plays every AI slot, so it blocks like a
    // player and must never be moved onto the delayed feed.
    fn is_observer(&self, uuid: &Guid) -> bool {
        self.slots.slot_of(uuid) == Some(UNASSIGNED)
            && self.controller.as_ref() != Some(uuid)
            && !self.is_ai_host_uuid(uuid)
    }

    fn is_delayed_observer(&self, uuid: &Guid) -> bool {
        self.config.observer_delay_turns > 0 && self.is_observer(uuid)
    }

    // The turn stream and flares as the players see them. Delayed observers
    // get the same messages later, from the feed.
    fn broadcast_live(&mut self, msg: &WireMessage) {
        let peers: Vec<PeerID> = self
            .sessions
            .iter()
            .filter(|(p, s)| s.is_in_game() && !self.turns.is_delayed(**p))
            .map(|(p, _)| *p)
            .collect();
        for peer in peers {
            self.send(peer, msg.clone());
        }
    }

    fn broadcast_delayed(&mut self, msg: &WireMessage) {
        for peer in self.turns.delayed_peers() {
            self.send(peer, msg.clone());
        }
    }

    fn release_turns(&mut self, turn_length_ms: u32) {
        let released = self.turns.release(self.config.observer_lag_limit);
        let length = turn_length_ms as u16;
        for turn in released {
            self.log.record_turn_length(turn, length);
            let msg = WireMessage::TurnSealed(TurnSealed {
                turn,
                turn_length: length,
            });
            self.broadcast_live(&msg);
        }
        self.advance_feed();
    }

    // Counts a player who is rejoining too, so a reconnect does not drain the
    // feed for good in the moment the player is away.
    fn players_remain(&self) -> bool {
        self.sessions.values().any(|s| {
            (s.is_in_game() || s.is_syncing())
                && s.uuid
                    .as_ref()
                    .and_then(|u| self.slots.slot_of(u))
                    .is_some_and(|slot| slot != UNASSIGNED)
        })
    }

    // Every turn is sealed live before it is sealed here, so its commands and
    // its length are all in the match log by the time it is due.
    fn advance_feed(&mut self) {
        let draining = !self.players_remain();
        let due = self.feed.advance(self.turns.ready_turn(), draining);
        for turn in due {
            let commands: Vec<PlayerCommand> = self.log.commands_for(turn).to_vec();
            for command in commands {
                self.broadcast_delayed(&WireMessage::PlayerCommand(command));
            }
            let turn_length = self
                .log
                .turn_length(turn)
                .unwrap_or(self.config.turn_length_ms as u16);
            self.broadcast_delayed(&WireMessage::TurnSealed(TurnSealed { turn, turn_length }));
        }
        for flare in self.feed.due_flares() {
            self.broadcast_delayed(&WireMessage::Flare(flare));
        }
        for (joiner, join) in self.feed.due_joins() {
            self.send(joiner, WireMessage::Join(join));
        }
    }

    fn report_mismatch(&mut self, mismatch: crate::relay::turn::HashMismatch) {
        let names: Vec<String> = mismatch
            .mismatched
            .iter()
            .filter_map(|p| self.sessions.get(p))
            .filter_map(|s| s.name().map(str::to_string))
            .collect();
        let msg = WireMessage::WrongHashPlayers(WrongHashPlayers {
            turn: mismatch.turn,
            hash_expected: mismatch.reference,
            player_names: names,
        });
        match mismatch.recipient {
            Some(peer) => self.send(peer, msg),
            None => self.broadcast(&msg, |s| s.is_in_game()),
        }
    }
}

pub struct Server<S> {
    ctx: Context,
    st: S,
}

pub struct Idle;

pub struct Setup;

// Still server phase `setup` on the wire, but START_* must not be accepted again
// while the controller's save file is in transit.
pub struct AwaitSavegame {
    pub settings_json: Vec<u8>,
}

// Still server phase `setup` on the wire: the controller has started a match
// with AI slots, and the start is held until the AI host that will play them
// has been admitted, so it is there to receive the start with everyone else.
pub struct AwaitAiHost {
    pub settings: FrozenSettings,
    // What every stock client is sent, and the AI host's own copy.
    pub start: StartSettings,
    pub ai_host_start: StartSettings,
    // Anchored on the first tick, because the FSM only learns the time there.
    pub requested_at: Option<DateTime<Utc>>,
}

pub struct Loading {
    pub settings: FrozenSettings,
    pub saved_state: Option<Vec<u8>>,
}

pub struct InGame {
    pub settings: FrozenSettings,
}

// Still in-game on the wire: the clients' simulations keep running, so turns
// are still released for whoever stays to watch or chat. What ends is
// everything that only makes sense while the match can still be won: nobody
// is waited for, nobody new is let in, and no more checkpoints are taken.
pub struct PostGame {
    pub settings: FrozenSettings,
    // The checkpoint run whose result decided the match, whose outcome is
    // then already final and needs no replay of its own.
    pub resolved_by: Option<u32>,
    // Anchored on the first tick, because the FSM only learns the time there.
    pub ended_at: Option<DateTime<Utc>>,
}

// Setup handlers stay available while a savegame is being fetched.
pub trait SetupPhase {}
impl SetupPhase for Setup {}
impl SetupPhase for AwaitSavegame {}
impl SetupPhase for AwaitAiHost {}

// Lets the shared handlers apply the phase-dependent rules without knowing
// which typestate they were called from.
pub trait PhaseMarker {
    const PHASE: Phase;
    // The frozen settings, once a match has been configured. Setup-like
    // phases have none, so snapshot dumps stay unreachable there.
    fn settings(&self) -> Option<&FrozenSettings> {
        None
    }
}
impl PhaseMarker for Setup {
    const PHASE: Phase = Phase::Setup;
}
impl PhaseMarker for AwaitSavegame {
    const PHASE: Phase = Phase::Setup;
}
impl PhaseMarker for AwaitAiHost {
    const PHASE: Phase = Phase::Setup;
}
impl PhaseMarker for Loading {
    const PHASE: Phase = Phase::Loading;
    fn settings(&self) -> Option<&FrozenSettings> {
        Some(&self.settings)
    }
}
impl PhaseMarker for InGame {
    const PHASE: Phase = Phase::InGame;
    fn settings(&self) -> Option<&FrozenSettings> {
        Some(&self.settings)
    }
}
impl PhaseMarker for PostGame {
    const PHASE: Phase = Phase::PostGame;
    fn settings(&self) -> Option<&FrozenSettings> {
        Some(&self.settings)
    }
}

// The phases in which turns are being released, which share every handler
// of the running match.
pub trait MatchPhase: PhaseMarker {
    fn frozen(&self) -> &FrozenSettings;
}
impl MatchPhase for InGame {
    fn frozen(&self) -> &FrozenSettings {
        &self.settings
    }
}
impl MatchPhase for PostGame {
    fn frozen(&self) -> &FrozenSettings {
        &self.settings
    }
}

pub enum AnyServer {
    Idle(Server<Idle>),
    Setup(Server<Setup>),
    AwaitSavegame(Server<AwaitSavegame>),
    AwaitAiHost(Server<AwaitAiHost>),
    Loading(Server<Loading>),
    InGame(Server<InGame>),
    PostGame(Server<PostGame>),
}

impl From<Server<Idle>> for AnyServer {
    fn from(s: Server<Idle>) -> Self {
        AnyServer::Idle(s)
    }
}

impl From<Server<Setup>> for AnyServer {
    fn from(s: Server<Setup>) -> Self {
        AnyServer::Setup(s)
    }
}

impl From<Server<AwaitSavegame>> for AnyServer {
    fn from(s: Server<AwaitSavegame>) -> Self {
        AnyServer::AwaitSavegame(s)
    }
}

impl From<Server<AwaitAiHost>> for AnyServer {
    fn from(s: Server<AwaitAiHost>) -> Self {
        AnyServer::AwaitAiHost(s)
    }
}

impl From<Server<Loading>> for AnyServer {
    fn from(s: Server<Loading>) -> Self {
        AnyServer::Loading(s)
    }
}

impl From<Server<InGame>> for AnyServer {
    fn from(s: Server<InGame>) -> Self {
        AnyServer::InGame(s)
    }
}

impl From<Server<PostGame>> for AnyServer {
    fn from(s: Server<PostGame>) -> Self {
        AnyServer::PostGame(s)
    }
}

impl AnyServer {
    // Read once the game thread is done with the FSM, however it got there:
    // the thread also ends when its socket closes, which no input announces.
    pub fn outcome_request(&self) -> Option<DumpRequest> {
        match self {
            AnyServer::InGame(s) => s.outcome_request(),
            // A match decided by a checkpoint already has its final outcome.
            AnyServer::PostGame(s) if s.st.resolved_by.is_none() => s.outcome_request(),
            _ => None,
        }
    }

    pub fn handle(self, input: Input) -> AnyServer {
        match self {
            AnyServer::Idle(s) => s.on_input(input),
            AnyServer::Setup(s) => s.on_input(input),
            AnyServer::AwaitSavegame(s) => s.on_input(input),
            AnyServer::AwaitAiHost(s) => s.on_input(input),
            AnyServer::Loading(s) => s.on_input(input),
            AnyServer::InGame(s) => s.on_input(input),
            AnyServer::PostGame(s) => s.on_input(input),
        }
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        match self {
            AnyServer::Idle(s) => s.take_effects(),
            AnyServer::Setup(s) => s.take_effects(),
            AnyServer::AwaitSavegame(s) => s.take_effects(),
            AnyServer::AwaitAiHost(s) => s.take_effects(),
            AnyServer::Loading(s) => s.take_effects(),
            AnyServer::InGame(s) => s.take_effects(),
            AnyServer::PostGame(s) => s.take_effects(),
        }
    }

    pub fn shutdown(self) -> Vec<Effect> {
        match self {
            AnyServer::Idle(s) => s.shutdown(),
            AnyServer::Setup(s) => s.shutdown(),
            AnyServer::AwaitSavegame(s) => s.shutdown(),
            AnyServer::AwaitAiHost(s) => s.shutdown(),
            AnyServer::Loading(s) => s.shutdown(),
            AnyServer::InGame(s) => s.shutdown(),
            AnyServer::PostGame(s) => s.shutdown(),
        }
    }
}

impl<S> Server<S> {
    fn with_state<T>(self, st: T) -> Server<T> {
        Server { ctx: self.ctx, st }
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.ctx.effects)
    }

    pub fn shutdown(mut self) -> Vec<Effect> {
        let peers: Vec<PeerID> = self.ctx.sessions.keys().copied().collect();
        for peer in peers {
            self.ctx.effects.push(Effect::DisconnectNow {
                peer,
                reason: DisconnectReason::ServerShuttingDown,
            });
        }
        self.ctx.effects
    }
}

impl<S: PhaseMarker> Server<S> {
    fn on_common_input(&mut self, input: Input) {
        match input {
            Input::Connected { peer, addr } => self.on_connected(peer, addr),
            Input::Received { peer, msg } => self.on_common_message(peer, msg),
            Input::Disconnected { peer } => self.on_disconnected(peer),
            Input::LobbyAuth { username, token } => self.on_lobby_auth(username, token),
            Input::Tick { now, stats } => self.on_tick(now, stats),
            // Only the in-game phase waits on dumps; everywhere else the
            // result arrives for a game that has moved on.
            Input::StateDumped { .. } => {}
            Input::Checkpointed { .. } => {}
            // Only a held start acts on this; once the match runs, the AI
            // host's departure is handled when its connection drops.
            Input::AiHostExited => tracing::info!("sidecar: AI host process exited"),
        }
    }

    // Messages accepted in every phase after idle. Anything reaching the
    // fallback arm is not accepted in the current phase and is dropped with
    // the connection kept open.
    fn on_common_message(&mut self, peer: PeerID, msg: WireMessage) {
        let outcome = match msg {
            WireMessage::SynAck(m) => self.on_syn_ack(peer, m),
            WireMessage::Authenticate(m) => self.on_authenticate(peer, m),
            WireMessage::Chat(m) => self.on_chat(peer, m),
            WireMessage::Kicked(m) => self.on_kicked(peer, m),
            WireMessage::GamestateRequest(m) => self.on_gamestate_request(peer, m),
            WireMessage::GamestateResponse(m) => self.on_gamestate_response(peer, m),
            WireMessage::GamestateChunk(m) => self.on_gamestate_chunk(peer, m).map(|_| ()),
            WireMessage::GamestateChunkAck(m) => self.on_gamestate_chunk_ack(peer, m),
            WireMessage::Syn(_)
            | WireMessage::Ack(_)
            | WireMessage::AuthenticateResult(_)
            | WireMessage::PlayerSlots(_)
            | WireMessage::Join(_)
            | WireMessage::LastSeen(_)
            | WireMessage::LaggingClients(_)
            | WireMessage::PlayersLoading(_)
            | WireMessage::WrongHashPlayers(_) => {
                tracing::trace!(?peer, msg_type = msg.name(), "dropped S->C-only message");
                Ok(())
            }
            other => {
                tracing::debug!(
                    ?peer,
                    msg_type = other.name(),
                    "message not accepted in this phase"
                );
                Ok(())
            }
        };
        if let Err(fault) = outcome {
            self.ctx.fault(peer, fault);
        }
    }

    // Wraps the Context methods of the same name so a lobby listing update
    // rides along whenever the slot table changes during setup (Sec. 17.3).
    // Context itself cannot make that call: it is phase-erased by design
    // (A2), so the decision has to be made here, where S::PHASE is known.
    fn broadcast_player_slots(&mut self) {
        self.ctx.broadcast_player_slots();
        if S::PHASE == Phase::Setup {
            self.ctx.push_lobby_listing();
        }
    }

    fn disconnect(&mut self, peer: PeerID, reason: DisconnectReason) {
        self.ctx.disconnect(peer, reason);
        if S::PHASE == Phase::Setup {
            self.ctx.push_lobby_listing();
        }
    }

    fn on_connected(&mut self, peer: PeerID, addr: Ipv4Addr) {
        // The ban is checked before anything else, so a banned address never
        // gets a session or a handshake.
        if self.ctx.banned_ips.contains(&addr) {
            tracing::info!(peer = peer.0, ip = %addr, "connection refused: banned address");
            self.ctx.effects.push(Effect::Disconnect {
                peer,
                reason: DisconnectReason::Banned,
            });
            return;
        }
        tracing::info!(peer = peer.0, ip = %addr, "client connected");
        self.ctx.sessions.insert(peer, Session::new(addr));

        // The client compares its mismatch report against this SYN, so it must
        // list exactly the mods the clients run, in load order.
        let syn = Syn {
            magic: SYN_CHALLENGE,
            protocol_version: GAME_VERSION,
            engine_version: SIMULATION_VERSION.into(),
            enabled_mods: self.ctx.config.enabled_mods.clone(),
        };
        self.ctx.send(peer, WireMessage::Syn(syn));
    }

    fn on_disconnected(&mut self, peer: PeerID) {
        let Some(session) = self.ctx.sessions.remove(&peer) else {
            return;
        };
        self.ctx.transfers.forget(peer);
        self.ctx.turns.forget(peer);
        self.ctx.feed.forget(peer);
        if let Some(dump) = self.ctx.dump.as_mut() {
            dump.waiting.retain(|p| *p != peer);
            if dump.waiting.is_empty() {
                // Nobody is left to answer, so the run is cancelled rather
                // than burning a whole replay for no one.
                let id = dump.id;
                self.ctx.dump = None;
                self.ctx.effects.push(Effect::CancelStateDump { id });
            }
        }

        if self.ctx.is_ai_host(peer) {
            // It is not respawned: a new one would have to rebuild the match
            // state first, so its players simply stop acting.
            tracing::warn!(?peer, "sidecar: AI host left the match");
            self.ctx.ai_host = None;
            self.ctx.effects.push(Effect::StopAiHost);
            self.ctx
                .server_chat(None, "The AI host left; AI players will stand idle.");
        }

        if let Some(uuid) = session.uuid.as_ref() {
            self.ctx.pause_budget.clear_pausing(uuid);
            if self.ctx.controller.as_ref() == Some(uuid)
                && self.ctx.config.release_controller_on_leave
            {
                // Without this the match is stranded: nobody else can ever be
                // promoted, so setup and start stay unreachable.
                tracing::info!(?peer, "controller left, role released");
                self.ctx.controller = None;
            }
            // Losing a session before admission has no further effect.
            if session.admitted.is_some() {
                self.ctx.slots.mark_disconnected(uuid);
                self.broadcast_player_slots();
            }
        }

        // A departed client no longer blocks release, and hash comparison no
        // longer waits for it.
        let turn_length = self.ctx.config.turn_length_ms;
        self.ctx.release_turns(turn_length);
        for mismatch in self.ctx.turns.recheck_pending() {
            self.ctx.report_mismatch(mismatch);
        }
    }

    fn on_lobby_auth(&mut self, username: String, token: String) {
        let Some(peer) = self.ctx.peer_of(&Guid(token.clone())) else {
            tracing::debug!(%username, "lobby auth token matches no session");
            return;
        };
        if let Some(session) = self.ctx.sessions.get_mut(&peer) {
            session.lobby_name = Some(username);
        }
        // The empty AUTHENTICATE is the prompt the client waits for before it
        // sends its real credentials.
        self.ctx.send(
            peer,
            WireMessage::Authenticate(Authenticate {
                name: String::new(),
                password: String::new(),
                controller_secret: String::new(),
            }),
        );
    }

    fn on_tick(&mut self, now: DateTime<Utc>, stats: Vec<PeerStats>) {
        for sample in &stats {
            if let Some(session) = self.ctx.sessions.get_mut(&sample.peer) {
                session.mean_rtt = sample.mean_rtt;
                session.since_last_received = sample.since_last_received;
            }
        }

        if self.idle_shutdown_due(now) {
            self.ctx.effects.push(Effect::GameOver);
        }

        // Before the warning gate, on its own anchor: the budget is charged
        // from the elapsed it works out itself, so it neither depends on nor
        // disturbs the once-a-second warning cadence.
        if S::PHASE == Phase::InGame || S::PHASE == Phase::PostGame {
            let players = self.ctx.slots.connected_players();
            // Once the match is decided nobody is worth waiting for, so an
            // empty list lifts a hold still in place and starts no new one.
            let absent = if self.ctx.config.afk_pause && S::PHASE == Phase::InGame {
                self.ctx.afk_absent()
            } else {
                Vec::new()
            };
            let events = self.ctx.pause_budget.check(now, players, &absent);
            for event in events {
                self.on_budget_event(event);
            }
        }

        if !self.ctx.monitor.due(now) {
            return;
        }

        let reports: Vec<(PeerID, WireMessage)> = self
            .ctx
            .sessions
            .iter()
            .filter(|(_, s)| s.admitted.is_some())
            .filter_map(|(peer, s)| {
                let uuid = s.uuid.clone()?;
                let warning = Monitor::classify(s.mean_rtt, s.since_last_received)?;
                // The wire carries plain milliseconds, so this is where the
                // chrono types finally become numbers.
                let msg = match warning {
                    Warning::Silent(since) => WireMessage::LastSeen(LastSeen {
                        guid: uuid,
                        last_received_time: since.num_milliseconds().max(0) as u32,
                    }),
                    Warning::Lagging(rtt) => WireMessage::LaggingClients(LaggingClients {
                        clients: vec![PerformanceEntry {
                            guid: uuid,
                            mean_rtt: rtt.num_milliseconds().max(0) as u32,
                        }],
                    }),
                };
                Some((*peer, msg))
            })
            .collect();

        let in_setup = S::PHASE == Phase::Setup;
        for (reported, msg) in reports {
            // The reported client is never told about itself.
            self.ctx.broadcast_except(reported, &msg, |s| {
                s.is_in_game() || (in_setup && s.is_setup())
            });
        }
    }

    // GameOver fires once in either case: nobody has ever been admitted
    // within the timeout of creation, or nobody admitted has been present for
    // the timeout. A negative delta re-anchors instead of firing early or
    // stalling (A7): a wall-clock step backwards just restarts the wait.
    fn idle_shutdown_due(&mut self, now: DateTime<Utc>) -> bool {
        let Some(timeout) = self.ctx.config.idle_shutdown else {
            return false;
        };

        let created_at = *self.ctx.created_at.get_or_insert(now);
        let ever_admitted = self.ctx.next_client_id > 1;
        // The AI host alone must not keep a game nobody plays in alive.
        let anyone_admitted = self
            .ctx
            .sessions
            .iter()
            .any(|(p, s)| s.admitted.is_some() && !self.ctx.is_ai_host(*p));

        if anyone_admitted {
            self.ctx.empty_since = None;
            return false;
        }

        let anchor = if ever_admitted {
            *self.ctx.empty_since.get_or_insert(now)
        } else {
            created_at
        };

        let elapsed = now.signed_duration_since(anchor);
        if elapsed < TimeDelta::zero() {
            if ever_admitted {
                self.ctx.empty_since = Some(now);
            } else {
                self.ctx.created_at = Some(now);
            }
            return false;
        }
        elapsed >= timeout
    }

    fn on_budget_event(&mut self, event: BudgetEvent) {
        match event {
            BudgetEvent::Expired { uuid } => {
                tracing::info!(%uuid, "pause budget exhausted, resuming");
                // Broadcast rather than broadcast_except: the one client that
                // must hear this is the pauser, whose overlay is still up.
                let relayed = WireMessage::PlayerPause(PlayerPause {
                    guid: uuid.clone(),
                    pause: false,
                });
                self.ctx.broadcast(&relayed, |s| s.is_in_game());
                if let Some(name) = self.ctx.name_of(&uuid) {
                    let text = format!("{} is out of pause budget.", name);
                    self.ctx.server_chat(None, &text);
                }
            }
            BudgetEvent::Status { uuid, remaining } => {
                if let Some(name) = self.ctx.name_of(&uuid) {
                    let text = format!(
                        "{} is paused. {}s of pause budget left.",
                        name,
                        remaining.num_seconds()
                    );
                    self.ctx.server_chat(None, &text);
                }
            }
            BudgetEvent::CoordinatedStarted => self.ctx.server_chat(
                None,
                "Everyone is paused, so nobody's pause budget is draining.",
            ),
            BudgetEvent::CoordinatedEnded => self
                .ctx
                .server_chat(None, "Pause budgets are draining again."),
            BudgetEvent::AutoPauseStarted { uuids } => {
                tracing::info!(absent = uuids.len(), "holding the match for absent players");
                let relayed = WireMessage::PlayerPause(PlayerPause {
                    guid: self.ctx.server_uuid.clone(),
                    pause: true,
                });
                self.ctx.broadcast(&relayed, |s| s.is_in_game());
                for uuid in uuids {
                    let text = format!(
                        "Waiting for {}. {}s of pause budget left.",
                        self.ctx.absent_name(&uuid),
                        self.ctx.pause_budget.remaining(&uuid).num_seconds()
                    );
                    self.ctx.server_chat(None, &text);
                }
                self.ctx.checkpoints.prefetch = true;
            }
            BudgetEvent::AutoPauseEnded => {
                tracing::info!("no absent player left to wait for, resuming");
                let relayed = WireMessage::PlayerPause(PlayerPause {
                    guid: self.ctx.server_uuid.clone(),
                    pause: false,
                });
                self.ctx.broadcast(&relayed, |s| s.is_in_game());
                self.ctx.checkpoints.prefetch = false;
                self.ctx.server_chat(None, "Resuming.");
            }
            BudgetEvent::AbsentExpired { uuid } => {
                tracing::info!(%uuid, "absent player out of pause budget");
                let text = format!(
                    "{} did not return in time and is out of pause budget.",
                    self.ctx.absent_name(&uuid)
                );
                self.ctx.server_chat(None, &text);
            }
            BudgetEvent::AbsentStatus { uuid, remaining } => {
                let text = format!(
                    "Waiting for {}. {}s of pause budget left.",
                    self.ctx.absent_name(&uuid),
                    remaining.num_seconds()
                );
                self.ctx.server_chat(None, &text);
            }
        }
    }

    fn on_syn_ack(&mut self, peer: PeerID, msg: SynAck) -> Result<(), PeerFault> {
        let session = self.ctx.sessions.get(&peer).ok_or(PeerFault::NoSession)?;
        // A session that already holds a UUID is past the handshake. Issuing a
        // second one would strand the slot, the controller record and the
        // paused set, which all key off the UUID this session no longer claims.
        if session.uuid.is_some() {
            return Err(PeerFault::WrongPhase);
        }
        if msg.protocol_version != GAME_VERSION {
            self.ctx
                .disconnect(peer, DisconnectReason::GameVersionMismatch);
            return Ok(());
        }
        if !auth::compatible(
            SIMULATION_VERSION,
            &self.ctx.config.enabled_mods,
            &msg.engine_version,
            &msg.enabled_mods,
        ) {
            // Code 17 rather than 16: only on 17 does the client attach its
            // own mismatch details to the message it shows.
            self.ctx
                .disconnect(peer, DisconnectReason::SimulationOrModMismatch);
            return Ok(());
        }

        let Some(uuid) = self.issue_uuid() else {
            self.disconnect(peer, DisconnectReason::NoUuid);
            return Ok(());
        };
        if let Some(session) = self.ctx.sessions.get_mut(&peer) {
            session.uuid = Some(uuid.clone());
        }

        // The AI host has no lobby account to authenticate with.
        let addr = self.ctx.sessions.get(&peer).map(|s| s.addr);
        let ai_host = addr.is_some_and(|a| self.ctx.ai_host_expected(a));
        let flags = if self.ctx.config.lobby_mode && !ai_host {
            ACK_FLAG_LOBBY_AUTH
        } else {
            0
        };
        self.ctx.send(
            peer,
            WireMessage::Ack(Ack {
                use_protocol_version: GAME_VERSION,
                flags,
                guid: uuid,
            }),
        );
        Ok(())
    }

    fn issue_uuid(&self) -> Option<Guid> {
        (0..UUID_ATTEMPTS).find_map(|_| {
            let candidate = Guid::new();
            // The relay's own UUID is taken too: a session sharing it would
            // make every slot row and chat line ambiguous.
            let taken = candidate == self.ctx.server_uuid
                || self
                    .ctx
                    .sessions
                    .values()
                    .any(|s| s.uuid.as_ref() == Some(&candidate));
            (!taken).then_some(candidate)
        })
    }

    // The checks run in the order below and the first match ends processing.
    // The order is wire-observable: when two conditions hold at once, the code
    // that reaches the client says which check ran first.
    fn on_authenticate(&mut self, peer: PeerID, msg: Authenticate) -> Result<(), PeerFault> {
        let session = self.ctx.sessions.get(&peer).ok_or(PeerFault::NoSession)?;
        let Some(uuid) = session.uuid.clone() else {
            // Still awaiting the handshake, so nothing but SYN_ACK counts.
            return Err(PeerFault::WrongPhase);
        };
        if session.admitted.is_some() {
            return Err(PeerFault::WrongPhase);
        }
        let lobby_name = session.lobby_name.clone();
        // It has no lobby account and is never handed the game password, so
        // the checks that depend on either are skipped for it alone.
        let is_ai_host =
            self.ctx.ai_host_expected(session.addr) && msg.name == self.ctx.ai_host_name;
        if self.ctx.config.lobby_mode && lobby_name.is_none() && !is_ai_host {
            // The client has not been prompted yet, so this cannot be its
            // real answer.
            return Err(PeerFault::WrongPhase);
        }

        let sanitized = auth::sanitize(&msg.name);

        if S::PHASE == Phase::Loading {
            self.disconnect(peer, DisconnectReason::ServerLoading);
            return Ok(());
        }

        if self.ctx.config.lobby_mode && !is_ai_host {
            let expected = lobby_name.unwrap_or_default().to_lowercase();
            if auth::suffix_stripped(&sanitized).to_lowercase() != expected {
                self.disconnect(peer, DisconnectReason::LobbyAuthFailed);
                return Ok(());
            }
        }

        // Salted with the raw name, unlike every other name-based check, and
        // always run: an empty server password hashes to "", which is exactly
        // what a client with no password sends.
        let expected = password::hash(&self.ctx.config.server_password_hash, msg.name.as_bytes());
        if expected != msg.password && !is_ai_host {
            self.disconnect(peer, DisconnectReason::Refused);
            return Ok(());
        }

        // The relay's own slot row wears the reserved prefix, so a client that
        // claims it would put two identically named rows in the slot list.
        // Refused as a name collision, which is what it is.
        if auth::reserved(&sanitized) {
            self.disconnect(peer, DisconnectReason::NameInUse);
            return Ok(());
        }

        let duplicates_allowed =
            !self.ctx.config.lobby_mode && self.ctx.config.allow_duplicate_names;
        let name = if duplicates_allowed {
            let sessions = &self.ctx.sessions;
            auth::deduplicate(&sanitized, |candidate| {
                sessions.values().any(|s| s.name() == Some(candidate))
            })
        } else {
            if self
                .ctx
                .sessions
                .values()
                .any(|s| s.name() == Some(sanitized.as_str()))
            {
                self.disconnect(peer, DisconnectReason::NameInUse);
                return Ok(());
            }
            sanitized
        };

        let ban_key = if self.ctx.config.lobby_mode {
            auth::suffix_stripped(&name)
        } else {
            name.as_str()
        };
        if self.ctx.banned_names.contains(ban_key) {
            self.disconnect(peer, DisconnectReason::Banned);
            return Ok(());
        }

        let joining = match self.admit(&name) {
            Ok(joining) => joining,
            Err(reason) => {
                self.disconnect(peer, reason);
                return Ok(());
            }
        };

        if is_ai_host && let Some(host) = self.ctx.ai_host.as_mut() {
            host.peer = Some(peer);
            tracing::info!(?peer, "sidecar: AI host admitted");
        }
        self.admit_session(peer, uuid, name, joining, &msg.controller_secret);
        Ok(())
    }

    // Ok(true) means the session is admitted via the joining/syncing path:
    // a recovered slot or a brand-new late observer both count. Ok(false) is
    // only ever returned by the Setup-phase branch below.
    fn admit(&self, name: &str) -> Result<bool, DisconnectReason> {
        let sessions = self.ctx.sessions.len();

        // A decided match has nothing left to join, and a joiner would only
        // cost a snapshot for a game that is about to close.
        if S::PHASE == Phase::PostGame {
            return Err(DisconnectReason::MatchInProgress);
        }

        if S::PHASE == Phase::Setup {
            // The arriving session is already counted, which is what leaves a
            // spare peer to answer a full server with.
            if sessions >= self.ctx.config.max_sessions {
                return Err(DisconnectReason::ServerFull);
            }
            return Ok(false);
        }

        // Join detection is by name, and only against slots someone left.
        if self.ctx.slots.has_disconnected_named(name) {
            return Ok(true);
        }

        match self.ctx.config.late_observer_policy {
            LateObserverPolicy::Deny => return Err(DisconnectReason::MatchInProgress),
            LateObserverPolicy::Buddies => {
                if !self
                    .ctx
                    .config
                    .buddies
                    .contains(auth::suffix_stripped(name))
                {
                    return Err(DisconnectReason::MatchInProgress);
                }
            }
            LateObserverPolicy::Everyone => {}
        }

        let connected = self.ctx.slots.connected_players();
        let disconnected = self.ctx.slots.disconnected_players();
        if sessions.saturating_sub(connected) > self.ctx.config.observer_limit
            || sessions + disconnected >= self.ctx.config.max_sessions
        {
            return Err(DisconnectReason::ServerFull);
        }
        Ok(true)
    }

    fn admit_session(
        &mut self,
        peer: PeerID,
        uuid: Guid,
        name: String,
        joining: bool,
        controller_secret: &str,
    ) {
        let client_id = self.ctx.next_client_id;
        self.ctx.next_client_id = self.ctx.next_client_id.saturating_add(1);

        // The controller flag only ever reaches a client here; there is no
        // message that promotes an already-connected one.
        let is_controller = self.ctx.controller.is_none()
            && controller_secret == self.ctx.config.controller_secret
            && !self.ctx.is_ai_host(peer);
        if is_controller {
            self.ctx.controller = Some(uuid.clone());
        }

        let code = if joining {
            AuthenticateResultCode::OkRejoining
        } else {
            AuthenticateResultCode::Ok
        };
        self.ctx.send(
            peer,
            WireMessage::AuthenticateResult(AuthenticateResult {
                code,
                host_id: client_id,
                is_controller,
                message: LOGIN_MESSAGE.to_string(),
            }),
        );

        let role = if joining { Role::Syncing } else { Role::Setup };
        if let Some(session) = self.ctx.sessions.get_mut(&peer) {
            session.admitted = Some(Admitted {
                client_id,
                name: name.clone(),
                role,
            });
        }

        // A slot is only reclaimed once the match itself is running. The
        // returning client authenticates under a fresh UUID, so its pause
        // quota has to follow the slot or a reconnect would refill it.
        let displaced = self
            .ctx
            .slots
            .add(uuid.clone(), name, S::PHASE == Phase::InGame);
        if let Some(old) = displaced {
            self.ctx.pause_budget.inherit(&old, &uuid);
        }
        self.broadcast_player_slots();

        // After the slot broadcast, so the name behind the sender UUID is
        // already known to the client when the line arrives.
        if !self.ctx.config.welcome_message.is_empty() {
            let text = self.ctx.config.welcome_message.clone();
            self.ctx.server_chat(Some(peer), &text);
        }

        if joining {
            self.start_snapshot_fetch(peer, true);
        }
    }

    // Asks the one-shot sidecar to rebuild the state at `turn` for `joiner`.
    // True means the joiner now waits on a dump: either the run already in
    // flight it attached to, or a fresh run the IO side will answer. False
    // means no dump was started and the caller should try the next source.
    fn request_state_dump(&mut self, joiner: PeerID, turn: u32) -> bool {
        if !self.ctx.config.sidecar_dumps || turn < 1 {
            return false;
        }
        if self.st.settings().is_none() {
            return false;
        }
        if let Some(dump) = self.ctx.dump.as_mut() {
            // One run per game: a joiner arriving mid-run waits behind it
            // whatever turn it asked for, instead of starting a second engine
            // process. A delayed joiner is still held to the run's turn, and
            // a live joiner just replays further from it.
            dump.waiting.push(joiner);
            return true;
        }
        let Some(request) = self.match_record(turn) else {
            return false;
        };
        let id = self.ctx.next_dump_id;
        self.ctx.next_dump_id = self.ctx.next_dump_id.wrapping_add(1);
        self.ctx.dump = Some(PendingDump {
            id,
            turn,
            waiting: vec![joiner],
        });
        tracing::info!(?joiner, turn, "sidecar: requesting state dump");
        self.ctx
            .effects
            .push(Effect::StateDump { id, turn, request });
        true
    }

    // Everything a one-shot replay needs to rebuild the match through wire
    // turn `turn`, resuming from the newest checkpoint that is not past it.
    // None before the settings are frozen.
    fn match_record(&self, turn: u32) -> Option<DumpRequest> {
        let base = self.ctx.checkpoints.at_or_before(turn).cloned();
        self.match_record_from(base, turn)
    }

    fn match_record_from(&self, base: Option<BaseState>, turn: u32) -> Option<DumpRequest> {
        let settings = self.st.settings()?;
        let default_length = settings.turn_length_ms as u16;
        let first = base.as_ref().map_or(0, |b| b.turn);
        let mut turn_lengths = Vec::new();
        for t in first + 1..=turn {
            turn_lengths.push(self.ctx.log.turn_length(t).unwrap_or(default_length));
        }
        let mut commands = Vec::new();
        for t in first + 1..=turn {
            commands.extend(self.ctx.log.commands_for(t).iter().cloned());
        }
        Some(DumpRequest {
            init_attributes: settings.json.clone(),
            engine_version: SIMULATION_VERSION.to_string(),
            mods: self.ctx.config.enabled_mods.clone(),
            base,
            turn_lengths,
            commands,
            hashes: Vec::new(),
        })
    }

    // Only an in-game client can serialize a live snapshot; asking anyone else
    // is undefined behaviour on the client side. A source on the same feed as
    // the joiner is preferred: a delayed observer's state is already old
    // enough for a delayed joiner to load straight away, and a live joiner
    // sourced from one would only have a longer replay to sit through.
    // A delayed joiner tries the sidecar at the feed head first, so it can
    // start watching without interrupting a player; a live joiner uses the
    // sidecar only when no client can serve it. `allow_dump` is false on the
    // retry after a failed dump, so one failure cannot loop back here.
    // A checkpoint beats all of them: it is already built, so the joiner
    // starts at once and no player stalls to serialize for it. A delayed
    // joiner needs one its feed has already passed.
    fn start_snapshot_fetch(&mut self, joiner: PeerID, allow_dump: bool) {
        let joiner_delayed = self
            .ctx
            .uuid_of(joiner)
            .is_some_and(|u| self.ctx.is_delayed_observer(&u));
        let newest_usable = if joiner_delayed {
            self.ctx.feed.head()
        } else {
            u32::MAX
        };
        if let Some(base) = self.ctx.checkpoints.at_or_before(newest_usable).cloned() {
            tracing::info!(
                ?joiner,
                turn = base.turn,
                "join served from a sidecar checkpoint"
            );
            self.deliver_snapshot(joiner, base.state, base.turn);
            return;
        }
        if allow_dump && joiner_delayed && self.request_state_dump(joiner, self.ctx.feed.head()) {
            return;
        }
        let mut candidates: Vec<(PeerID, bool, bool, chrono::TimeDelta)> = self
            .ctx
            .sessions
            .iter()
            // The AI host's state carries a live AI registration, which would
            // start the joiner computing the AI too.
            .filter(|(p, s)| **p != joiner && s.is_in_game() && !self.ctx.is_ai_host(**p))
            .map(|(p, s)| {
                let other_feed = self.ctx.turns.is_delayed(*p) != joiner_delayed;
                (
                    *p,
                    other_feed,
                    self.ctx.turns.is_out_of_sync(*p),
                    s.mean_rtt,
                )
            })
            .collect();
        candidates.sort_by_key(|(_, other_feed, desynced, rtt)| (*other_feed, *desynced, *rtt));

        let Some((source, _, _, _)) = candidates.first().copied() else {
            if allow_dump && self.request_state_dump(joiner, self.ctx.turns.ready_turn()) {
                return;
            }
            // Nobody can supply the state, so the join cannot succeed.
            tracing::info!(?joiner, "no in-game session to source a snapshot from");
            self.ctx
                .disconnect(joiner, DisconnectReason::MatchInProgress);
            return;
        };

        let request_id = self.ctx.transfers.allocate();
        self.ctx
            .transfers
            .expect(source, request_id, Purpose::JoinSnapshot { joiner });
        self.ctx.send(
            source,
            WireMessage::GamestateRequest(GamestateRequest {
                request_type: KIND_RUNNING_GAME,
                request_id,
            }),
        );
    }

    // Hands a snapshot to one joiner: a delayed joiner waits on the feed
    // until its state is due, everyone else loads straight away. A live
    // joiner downloads from the shared cache, so its snapshot always goes
    // there, even one older than live: any snapshot turn is valid, because
    // the join replay catches the joiner up from wherever it loads.
    fn deliver_snapshot(&mut self, joiner: PeerID, snapshot: Arc<Vec<u8>>, bound: u32) {
        let Some(settings) = self.st.settings() else {
            return;
        };
        let join = Join {
            init_attributes: settings.json.clone(),
        };
        let joiner_delayed = self
            .ctx
            .uuid_of(joiner)
            .is_some_and(|u| self.ctx.is_delayed_observer(&u));
        if joiner_delayed {
            self.ctx.feed.hold_join(joiner, bound, join, snapshot);
            self.ctx.advance_feed();
        } else {
            self.ctx.join_snapshot = Some(snapshot);
            self.ctx.send(joiner, WireMessage::Join(join));
        }
    }

    fn on_chat(&mut self, peer: PeerID, msg: Chat) -> Result<(), PeerFault> {
        // A joiner still pulling its snapshot receives chat but cannot send it.
        let uuid = self.ctx.speaker(peer, |s| s.is_setup() || s.is_in_game())?;
        let receivers = msg.receivers;
        // The relayed copy carries an empty receiver list, and the sender's
        // claimed UUID is replaced so it cannot speak as anyone else.
        let relayed = WireMessage::Chat(Chat {
            sender_guid: uuid,
            message: msg.message,
            receivers: Vec::new(),
        });

        if receivers.is_empty() {
            self.ctx
                .broadcast(&relayed, |s| s.is_setup() || s.is_in_game());
        } else {
            self.ctx.broadcast(&relayed, |s| {
                s.uuid.as_ref().is_some_and(|u| receivers.contains(u))
            });
        }
        Ok(())
    }

    fn on_kicked(&mut self, peer: PeerID, msg: Kicked) -> Result<(), PeerFault> {
        self.ctx.require_controller(peer)?;

        let target = self
            .ctx
            .sessions
            .iter()
            .find(|(_, s)| s.name() == Some(msg.name.as_str()))
            .map(|(p, _)| *p);
        // The controller can never kick itself or the AI host, and an unknown
        // name does nothing at all.
        let Some(target) = target.filter(|t| *t != peer && !self.ctx.is_ai_host(*t)) else {
            return Ok(());
        };

        if msg.ban {
            let key = if self.ctx.config.lobby_mode {
                auth::suffix_stripped(&msg.name).to_string()
            } else {
                msg.name.clone()
            };
            self.ctx.banned_names.insert(key);
            if let Some(session) = self.ctx.sessions.get(&target) {
                self.ctx.banned_ips.insert(session.addr);
            }
        }

        if let Some(uuid) = self.ctx.uuid_of(target) {
            self.ctx.forfeited.insert(uuid);
        }

        let reason = if msg.ban {
            DisconnectReason::Banned
        } else {
            DisconnectReason::Kicked
        };
        self.disconnect(target, reason);

        let relayed = WireMessage::Kicked(msg);
        self.ctx.broadcast(&relayed, |s| {
            s.is_setup() || s.is_syncing() || s.is_in_game()
        });
        Ok(())
    }

    fn on_gamestate_request(
        &mut self,
        peer: PeerID,
        msg: GamestateRequest,
    ) -> Result<(), PeerFault> {
        let data = match msg.request_type {
            KIND_RUNNING_GAME => self
                .ctx
                .feed
                .snapshot_for(peer)
                .or_else(|| self.ctx.join_snapshot.clone()),
            KIND_SAVEGAME => {
                // Savegame start is not implemented, so there is never a
                // cached saved state to answer with.
                tracing::debug!(?peer, "savegame request ignored: no saved state cached");
                None
            }
            other => {
                tracing::debug!(?peer, kind = other, "unknown gamestate request kind");
                None
            }
        };
        let Some(data) = data else {
            // Answering with length 0 would leave the client stuck, so an
            // unavailable file is simply not answered.
            return Ok(());
        };

        if let Some((length, chunks)) = self.ctx.transfers.begin_send(peer, msg.request_id, data) {
            self.ctx.send(
                peer,
                WireMessage::GamestateResponse(GamestateResponse {
                    request_id: msg.request_id,
                    length,
                }),
            );
            for chunk in chunks {
                self.ctx.send(peer, WireMessage::GamestateChunk(chunk));
            }
        }
        Ok(())
    }

    fn on_gamestate_response(
        &mut self,
        peer: PeerID,
        msg: GamestateResponse,
    ) -> Result<(), PeerFault> {
        self.ctx
            .transfers
            .on_response(peer, msg.request_id, msg.length)
    }

    // Returns what a completed transfer was for, so the phase that cares can
    // turn it into a state transition.
    fn on_gamestate_chunk(
        &mut self,
        peer: PeerID,
        msg: GamestateChunk,
    ) -> Result<Option<TransferDone>, PeerFault> {
        // An ack is what frees a slot in the sender's window, so a chunk that
        // belongs to no transfer has nothing to pace and gets no answer.
        if !self.ctx.transfers.knows_incoming(peer, msg.request_id) {
            return Err(PeerFault::WrongPhase);
        }

        // Otherwise exactly one ack per data message, whatever happens next.
        self.ctx.send(
            peer,
            WireMessage::GamestateChunkAck(GamestateChunkAck {
                request_id: msg.request_id,
                num_packets: 1,
            }),
        );

        match self
            .ctx
            .transfers
            .on_chunk(peer, msg.request_id, &msg.data)?
        {
            Some((Purpose::JoinSnapshot { joiner }, bytes)) => {
                self.ctx.join_snapshot = Some(Arc::new(bytes));
                Ok(Some(TransferDone::JoinSnapshot { joiner }))
            }
            Some((Purpose::Savegame, bytes)) => Ok(Some(TransferDone::Savegame(bytes))),
            None => Ok(None),
        }
    }

    fn on_gamestate_chunk_ack(
        &mut self,
        peer: PeerID,
        msg: GamestateChunkAck,
    ) -> Result<(), PeerFault> {
        let chunks = self
            .ctx
            .transfers
            .on_ack(peer, msg.request_id, msg.num_packets);
        for chunk in chunks {
            self.ctx.send(peer, WireMessage::GamestateChunk(chunk));
        }
        Ok(())
    }
}

impl<S: SetupPhase + PhaseMarker> Server<S> {
    fn on_pre_game_status(&mut self, peer: PeerID, msg: PreGameStatus) -> Result<(), PeerFault> {
        let uuid = self.ctx.speaker(peer, Session::is_setup)?;
        let relayed = WireMessage::PreGameStatus(PreGameStatus {
            guid: uuid.clone(),
            status: msg.status,
        });
        // Only setup sessions accept this, so relaying wider would be dropped
        // by the receiver anyway.
        self.ctx.broadcast(&relayed, |s| s.is_setup());
        // Deliberately no PLAYER_SLOTS broadcast: the status travels in the
        // relayed message instead.
        self.ctx.slots.set_status(&uuid, msg.status);
        Ok(())
    }

    fn on_reset_pregame_status(&mut self, peer: PeerID) -> Result<(), PeerFault> {
        self.ctx.require_controller(peer)?;
        self.ctx.slots.reset_pregame();
        self.broadcast_player_slots();
        Ok(())
    }

    fn on_game_settings(&mut self, peer: PeerID, msg: GameSettings) -> Result<(), PeerFault> {
        self.ctx.require_controller(peer)?;
        // Decoded only for the lobby listing (Sec. 17.3, A6); relay stays
        // verbatim and content is never validated (Sec. 10.2), so a decode
        // failure here must not become a peer fault.
        match script_value::decode(&msg.data).map(|v| LobbyMap::from_settings(&v)) {
            Ok(Some(map)) if Some(&map) != self.ctx.lobby_map.as_ref() => {
                self.ctx.lobby_map = Some(map);
                self.ctx.push_lobby_listing();
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(error = %e, "GAME_SETTINGS did not decode for the lobby listing");
            }
        }
        let relayed = WireMessage::GameSettings(msg);
        self.ctx.broadcast(&relayed, |s| s.is_setup());
        Ok(())
    }

    fn on_map_player_id_to_slot(
        &mut self,
        peer: PeerID,
        msg: MapPlayerIdToSlot,
    ) -> Result<(), PeerFault> {
        self.ctx.require_controller(peer)?;
        // The controller's setup screen assigns every joiner a free slot, AI
        // slots included, but a slot would make the AI host a player.
        if self.ctx.is_ai_host_uuid(&msg.guid) {
            return Ok(());
        }
        self.ctx.slots.assign(msg.player_id, &msg.guid);
        self.broadcast_player_slots();
        Ok(())
    }

    // Hands back what it did not consume so each setup-like phase can add its
    // own arms before the common fallback.
    fn on_setup_message(&mut self, peer: PeerID, msg: WireMessage) -> Option<WireMessage> {
        let outcome = match msg {
            WireMessage::PreGameStatus(m) => self.on_pre_game_status(peer, m),
            WireMessage::ResetPregameStatus => self.on_reset_pregame_status(peer),
            WireMessage::GameSettings(m) => self.on_game_settings(peer, m),
            WireMessage::MapPlayerIdToSlot(m) => self.on_map_player_id_to_slot(peer, m),
            other => return Some(other),
        };
        if let Err(fault) = outcome {
            self.ctx.fault(peer, fault);
        }
        None
    }
}

impl<S: SetupPhase + PhaseMarker> Server<S> {
    fn freeze(&self, json: &[u8]) -> FrozenSettings {
        let (player_ids, endless) = match_players(json);
        FrozenSettings {
            cheats_enabled: cheats_enabled(json),
            json: json.to_vec(),
            turn_length_ms: self.ctx.config.turn_length_ms,
            player_ids,
            endless,
        }
    }

    // Sends the start and registers every setup session for turn release.
    // `ai_host_start` is the AI host's own copy, which keeps the AI slots
    // that every other session's copy has stripped.
    fn begin_match(&mut self, start: StartSettings, ai_host_start: Option<StartSettings>) {
        // The clients observe these three in exactly this order.
        let stale: Vec<PeerID> = self
            .ctx
            .sessions
            .iter()
            .filter(|(_, s)| s.is_unauthenticated())
            .map(|(p, _)| *p)
            .collect();
        for peer in stale {
            self.disconnect(peer, DisconnectReason::ServerLoading);
        }

        self.broadcast_player_slots();

        let ai_host_peer = self.ctx.ai_host.as_ref().and_then(|h| h.peer);
        let relayed = WireMessage::StartSettings(start);
        match (ai_host_peer, ai_host_start) {
            (Some(ai_peer), Some(own)) => {
                self.ctx
                    .broadcast_except(ai_peer, &relayed, |s| s.is_setup());
                self.ctx.send(ai_peer, WireMessage::StartSettings(own));
            }
            _ => self.ctx.broadcast(&relayed, |s| s.is_setup()),
        }

        // From here on every session that was in setup counts for turn
        // release, and owes a seal for turn 4 and a hash for turn 1.
        let starting: Vec<(PeerID, u16, Guid)> = self
            .ctx
            .sessions
            .iter()
            .filter(|(_, s)| s.is_setup())
            .filter_map(|(p, s)| Some((*p, s.client_id()?, s.uuid.clone()?)))
            .collect();
        for (peer, client_id, uuid) in starting {
            let observer = self.ctx.is_observer(&uuid);
            let delayed = self.ctx.is_delayed_observer(&uuid);
            self.ctx.turns.register(
                peer,
                client_id,
                INITIAL_READY_TURN,
                FIRST_SIMULATED_TURN,
                observer,
                delayed,
            );
        }
        if let Some(ai_peer) = ai_host_peer {
            self.ctx.turns.mark_ai_host(ai_peer);
        }
    }

    fn enter_loading(mut self, settings: FrozenSettings) -> Server<Loading> {
        // The last listing update the match ever gets: Sec. 17.3 wants
        // register followed by changestate, and register already went out
        // from the broadcast_player_slots call in begin_match.
        let (nbp, players) = self.ctx.lobby_counts();
        self.ctx.effects.push(Effect::LobbyStarted { nbp, players });
        self.with_state(Loading {
            settings,
            saved_state: None,
        })
    }
}

impl Server<Idle> {
    pub fn new(config: Config) -> Self {
        let pause_budget = PauseBudget::new(config.pause_budget);
        let delay = config.observer_delay_turns;
        Server {
            ctx: Context {
                config,
                sessions: HashMap::new(),
                slots: Slots::default(),
                controller: None,
                server_uuid: Guid::new(),
                // Client ids start at 1 and only ever increase.
                next_client_id: 1,
                banned_ips: HashSet::new(),
                banned_names: HashSet::new(),
                forfeited: HashSet::new(),
                resigned: HashSet::new(),
                transfers: Transfers::default(),
                monitor: Monitor::default(),
                pause_budget,
                turns: TurnManager::default(),
                log: MatchLog::default(),
                feed: ObserverFeed::new(delay),
                join_snapshot: None,
                dump: None,
                next_dump_id: 0,
                checkpoints: CheckpointChain::default(),
                created_at: None,
                empty_since: None,
                ai_host_name: format!("AI host {}", &Guid::new().0[..8]),
                ai_host: None,
                lobby_map: None,
                effects: Vec::new(),
            },
            st: Idle,
        }
    }

    pub fn listen(self) -> Server<Setup> {
        self.with_state(Setup)
    }

    fn on_input(self, input: Input) -> AnyServer {
        // No socket is open yet, so any input here is a wiring bug upstream.
        tracing::debug!(?input, "input ignored while idle");
        self.into()
    }
}

impl Server<Setup> {
    pub fn start(self, settings: FrozenSettings) -> Server<Loading> {
        self.enter_loading(settings)
    }

    pub fn start_savegame(self, settings_json: Vec<u8>) -> Server<AwaitSavegame> {
        self.with_state(AwaitSavegame { settings_json })
    }

    fn on_input(mut self, input: Input) -> AnyServer {
        match input {
            Input::Received { peer, msg } => self.on_message(peer, msg),
            other => {
                self.on_common_input(other);
                self.into()
            }
        }
    }

    fn on_message(mut self, peer: PeerID, msg: WireMessage) -> AnyServer {
        let Some(msg) = self.on_setup_message(peer, msg) else {
            return self.into();
        };
        match msg {
            WireMessage::StartSettings(m) => match self.on_start_settings(peer, m) {
                Some(StartOutcome::Started(settings)) => return self.start(settings).into(),
                Some(StartOutcome::AwaitAiHost(held)) => return self.with_state(held).into(),
                None => {}
            },
            WireMessage::StartSavegameSettings(m) => {
                if let Some(json) = self.on_start_savegame_settings(peer, m) {
                    return self.start_savegame(json).into();
                }
            }
            other => self.on_common_message(peer, other),
        }
        self.into()
    }

    // None means the start was rejected or ignored and nothing was sent.
    fn on_start_settings(&mut self, peer: PeerID, msg: StartSettings) -> Option<StartOutcome> {
        if self.ctx.require_controller(peer).is_err() {
            return None;
        }
        // Rejecting outright is a deliberate deviation: the stock server
        // relays the start anyway and then cannot accept the LOADED_GAMEs
        // that follow, stranding every client on the loading screen.
        if !self.ctx.slots.all_ready() {
            tracing::info!(?peer, "start rejected: not every connected player is ready");
            return None;
        }

        let split = if self.ctx.config.hosted_ai {
            split_ai_settings(&msg.init_attributes)
        } else {
            None
        };
        let Some(split) = split else {
            let settings = self.freeze(&msg.init_attributes);
            self.begin_match(msg, None);
            return Some(StartOutcome::Started(settings));
        };

        // The start goes out only once the AI host is in, so that it loads
        // with everyone else and holds its place in turn release from turn 1.
        tracing::info!(ai_players = ?split.players, "sidecar: holding start for the AI host");
        let settings = self.freeze(&split.stock);
        self.ctx.ai_host = Some(AiHost {
            peer: None,
            players: split.players,
        });
        let name = self.ctx.ai_host_name.clone();
        self.ctx.effects.push(Effect::SpawnAiHost { name });
        Some(StartOutcome::AwaitAiHost(AwaitAiHost {
            settings,
            start: StartSettings {
                init_attributes: split.stock,
            },
            ai_host_start: StartSettings {
                init_attributes: split.ai_host,
            },
            requested_at: None,
        }))
    }

    // Refused, and deliberately left unimplemented: a stock client offers
    // its savegame picker only when it hosts the server itself, never when
    // it joins one, so nothing but a modified client can send this to a
    // dedicated server. Loading saved games is meant to come from the server
    // side instead, with the relay supplying the saved state itself. Refused
    // rather than panicked on, because any client can send this message.
    fn on_start_savegame_settings(
        &mut self,
        peer: PeerID,
        _msg: StartSavegameSettings,
    ) -> Option<Vec<u8>> {
        tracing::warn!(?peer, "savegame start is not supported, refusing");
        self.ctx.server_chat(
            Some(peer),
            "Loading a saved game is not supported on this server.",
        );
        None
    }
}

impl Server<AwaitSavegame> {
    pub fn savegame_received(
        self,
        saved_state: Vec<u8>,
        settings: FrozenSettings,
    ) -> Server<Loading> {
        self.with_state(Loading {
            settings,
            saved_state: Some(saved_state),
        })
    }

    fn on_input(mut self, input: Input) -> AnyServer {
        match input {
            Input::Received { peer, msg } => self.on_message(peer, msg),
            other => {
                self.on_common_input(other);
                self.into()
            }
        }
    }

    fn on_message(mut self, peer: PeerID, msg: WireMessage) -> AnyServer {
        let Some(msg) = self.on_setup_message(peer, msg) else {
            return self.into();
        };
        match msg {
            WireMessage::GamestateChunk(m) => match self.on_gamestate_chunk(peer, m) {
                Ok(Some(TransferDone::Savegame(saved_state))) => {
                    let settings = self.on_savegame_complete(peer);
                    return self.savegame_received(saved_state, settings).into();
                }
                Ok(_) => {}
                Err(fault) => self.ctx.fault(peer, fault),
            },
            other => self.on_common_message(peer, other),
        }
        self.into()
    }

    fn on_savegame_complete(&mut self, _peer: PeerID) -> FrozenSettings {
        todo!("freeze settings, relay START_SAVEGAME_SETTINGS to setup sessions")
    }
}

impl Server<AwaitAiHost> {
    fn on_input(mut self, input: Input) -> AnyServer {
        match input {
            Input::Received { peer, msg } => self.on_message(peer, msg),
            Input::AiHostExited => self.refuse_start("the AI host process exited").into(),
            Input::Tick { now, stats } => {
                self.on_tick(now, stats);
                if self.admit_timed_out(now) {
                    return self
                        .refuse_start("the AI host was not admitted in time")
                        .into();
                }
                self.into()
            }
            Input::Disconnected { peer } => {
                self.on_disconnected(peer);
                // Nobody would be left to take the match through loading.
                let controller_present = self
                    .ctx
                    .controller
                    .as_ref()
                    .is_some_and(|c| self.ctx.peer_of(c).is_some());
                if !controller_present {
                    return self.refuse_start("the controller left").into();
                }
                self.into()
            }
            other => {
                self.on_common_input(other);
                self.into()
            }
        }
    }

    fn on_message(mut self, peer: PeerID, msg: WireMessage) -> AnyServer {
        let Some(msg) = self.on_setup_message(peer, msg) else {
            return self.into();
        };
        match msg {
            WireMessage::StartSettings(_) | WireMessage::StartSavegameSettings(_) => {
                tracing::debug!(?peer, "start ignored: a start is already held");
            }
            WireMessage::Authenticate(m) => {
                self.on_common_message(peer, WireMessage::Authenticate(m));
                if self.ctx.ai_host.as_ref().is_some_and(|h| h.peer.is_some()) {
                    return self.ai_host_admitted().into();
                }
            }
            other => self.on_common_message(peer, other),
        }
        self.into()
    }

    // Rebuilt as Setup only to borrow its start path: no input is handled in
    // between, so nothing can observe the intermediate phase.
    fn ai_host_admitted(self) -> Server<Loading> {
        let Server { ctx, st } = self;
        let mut server = Server { ctx, st: Setup };
        server.begin_match(st.start, Some(st.ai_host_start));
        server.enter_loading(st.settings)
    }

    // A negative delta re-anchors rather than firing early (A7).
    fn admit_timed_out(&mut self, now: DateTime<Utc>) -> bool {
        let anchor = *self.st.requested_at.get_or_insert(now);
        let elapsed = now.signed_duration_since(anchor);
        if elapsed < TimeDelta::zero() {
            self.st.requested_at = Some(now);
            return false;
        }
        elapsed >= AI_HOST_ADMIT_TIMEOUT
    }

    // Nothing of the start has reached any client yet, so going back to setup
    // is enough; the chat line is what tells the controller to try again.
    fn refuse_start(mut self, why: &str) -> Server<Setup> {
        tracing::warn!(why, "sidecar: hosted-AI start refused");
        self.ctx.ai_host = None;
        self.ctx.effects.push(Effect::StopAiHost);
        self.ctx.server_chat(
            None,
            "The AI host failed to start; the game was not started.",
        );
        self.with_state(Setup)
    }
}

impl Server<Loading> {
    pub fn all_loaded(self) -> Server<InGame> {
        let settings = self.st.settings;
        Server {
            ctx: self.ctx,
            st: InGame { settings },
        }
    }

    fn on_input(mut self, input: Input) -> AnyServer {
        match input {
            Input::Received { peer, msg } => self.on_message(peer, msg),
            Input::Disconnected { peer } => {
                if self.on_disconnected_while_loading(peer) {
                    return self.all_loaded().into();
                }
                self.into()
            }
            other => {
                self.on_common_input(other);
                self.into()
            }
        }
    }

    fn on_message(mut self, peer: PeerID, msg: WireMessage) -> AnyServer {
        match msg {
            WireMessage::LoadedGame(m) => {
                if self.on_loaded_game(peer, m) {
                    return self.all_loaded().into();
                }
            }
            WireMessage::PreGameStatus(_) => {
                tracing::debug!(?peer, "pre-game status ignored while loading");
            }
            other => self.on_common_message(peer, other),
        }
        self.into()
    }

    // Returns true when the sender was the last one still loading.
    fn on_loaded_game(&mut self, peer: PeerID, _msg: LoadedGame) -> bool {
        let Some(session) = self.ctx.sessions.get_mut(&peer) else {
            return false;
        };
        if !session.is_setup() {
            return false;
        }
        session.set_role(Role::InGame);

        self.finish_if_everyone_loaded(true)
    }

    // A departure can leave everyone else loaded, which also starts the match.
    fn on_disconnected_while_loading(&mut self, peer: PeerID) -> bool {
        self.on_disconnected(peer);
        // No PLAYERS_LOADING is ever sent for a departure.
        self.finish_if_everyone_loaded(false)
    }

    fn finish_if_everyone_loaded(&mut self, announce_progress: bool) -> bool {
        // All sessions must load, observers included; a session still in setup
        // is a session still on the loading screen.
        let still_loading: Vec<Guid> = self
            .ctx
            .sessions
            .values()
            .filter(|s| s.is_setup())
            .filter_map(|s| s.uuid.clone())
            .collect();

        if !still_loading.is_empty() {
            if announce_progress {
                let msg = WireMessage::PlayersLoading(PlayersLoading {
                    clients: still_loading,
                });
                // The sender has just become in-game, so this reaches it too.
                self.ctx.broadcast(&msg, |s| s.is_in_game());
            }
            return false;
        }

        // Everyone leaving is not everyone loading.
        if !self.ctx.sessions.values().any(|s| s.is_in_game()) {
            return false;
        }

        let msg = WireMessage::LoadedGame(LoadedGame { current_turn: 0 });
        self.ctx.broadcast(&msg, |s| s.is_in_game());
        true
    }
}

impl<S: MatchPhase> Server<S> {
    // The whole match as released, with the players' agreed hashes, for the
    // replay that works out who won. None when no turn was ever released,
    // since there is then no match to judge, or when there is no sidecar.
    fn outcome_request(&self) -> Option<DumpRequest> {
        let turn = self.ctx.turns.ready_turn();
        if !self.ctx.config.sidecar_dumps || turn <= INITIAL_READY_TURN {
            return None;
        }
        let mut request = self.match_record(turn)?;
        request.hashes = self.reference_hashes(request.first_turn(), turn);
        Some(request)
    }

    // The players' agreed hashes for the wire turns after `after`, through
    // `through`, for a replay to check itself against.
    fn reference_hashes(&self, after: u32, through: u32) -> Vec<(u32, Vec<u8>)> {
        (after + 1..=through)
            .filter_map(|t| self.ctx.turns.reference(t).map(|hash| (t, hash.to_vec())))
            .collect()
    }
}

impl Server<InGame> {
    fn on_input(mut self, input: Input) -> AnyServer {
        match input {
            Input::Received { peer, msg } => self.on_message(peer, msg),
            Input::StateDumped { id, state } => self.on_state_dumped(id, state),
            Input::Checkpointed { id, state, players } => {
                if self.on_checkpointed(id, state, &players) {
                    return self.end_match(Some(id)).into();
                }
            }
            Input::Disconnected { peer } => {
                // The losing side often just leaves, so a player going is
                // worth an early look at whether the match is over.
                let player_left = self
                    .ctx
                    .uuid_of(peer)
                    .and_then(|u| self.ctx.slots.slot_of(&u))
                    .is_some_and(|slot| slot != UNASSIGNED);
                if player_left {
                    let turn = self.ctx.turns.ready_turn();
                    self.ctx.checkpoints.probe_at(turn);
                }
                self.on_common_input(Input::Disconnected { peer });
            }
            other => self.on_common_input(other),
        }
        if self.resigned_out() {
            return self.end_match(None).into();
        }
        self.maybe_checkpoint();
        self.into()
    }

    // Certain without simulating: a resign is a defeat, and after a defeat
    // the engine declares the one player left, if any, the winner. Without a
    // resign it never checks, so a match played alone does not end here.
    // Resigns the relay never sees, such as a client-computed AI's, only
    // make this miss an ending, never invent one.
    fn resigned_out(&self) -> bool {
        let settings = &self.st.settings;
        if settings.endless || settings.player_ids.is_empty() || self.ctx.resigned.is_empty() {
            return false;
        }
        let remaining = settings
            .player_ids
            .iter()
            .filter(|p| !self.ctx.resigned.contains(p))
            .count();
        remaining <= 1
    }

    fn end_match(self, resolved_by: Option<u32>) -> Server<PostGame> {
        let Server { mut ctx, st } = self;
        tracing::info!(
            turn = ctx.turns.ready_turn(),
            resolved = resolved_by.is_some(),
            "match decided, entering post-game"
        );
        ctx.effects.push(Effect::MatchEnded {
            checkpoint: resolved_by,
        });
        let text = format!(
            "The match is over. This server closes in {}, or once everyone has left.",
            describe_span(ctx.config.post_game_linger)
        );
        ctx.server_chat(None, &text);
        Server {
            ctx,
            st: PostGame {
                settings: st.settings,
                resolved_by,
                ended_at: None,
            },
        }
    }

    // Starts the next link once the match has run a whole interval past the
    // last one. At most one run is in flight, so a run slower than the
    // interval just makes the next chunk longer instead of queueing more.
    fn maybe_checkpoint(&mut self) {
        let interval = self.ctx.config.checkpoint_interval_turns;
        let chain = &self.ctx.checkpoints;
        if !self.ctx.config.sidecar_dumps
            || interval == 0
            || chain.disabled
            || chain.pending.is_some()
        {
            return;
        }
        let turn = self.ctx.turns.ready_turn();
        let base = chain.states.last().cloned();
        let last = base.as_ref().map_or(0, |b| b.turn);
        let due = if chain.prefetch {
            turn > last
        } else {
            turn >= last.saturating_add(interval)
                || chain.probe.is_some_and(|p| turn > p && turn > last)
        };
        if !due {
            return;
        }
        let from_scratch = base.is_none();
        let Some(mut request) = self.match_record_from(base, turn) else {
            return;
        };
        request.hashes = self.reference_hashes(last, turn);
        let chain = &mut self.ctx.checkpoints;
        chain.prefetch = false;
        if chain.probe.is_some_and(|p| p < turn) {
            chain.probe = None;
        }
        let id = chain.next_id;
        chain.next_id = chain.next_id.wrapping_add(1);
        chain.pending = Some(PendingCheckpoint {
            id,
            turn,
            from_scratch,
        });
        tracing::debug!(from = last, turn, "sidecar: requesting checkpoint");
        self.ctx
            .effects
            .push(Effect::Checkpoint { id, turn, request });
    }

    // True when the run shows the match decided: every player has either
    // won or been defeated. The chain is updated either way.
    fn on_checkpointed(&mut self, id: u32, state: Option<Vec<u8>>, players: &[String]) -> bool {
        let chain = &mut self.ctx.checkpoints;
        if chain.pending.as_ref().is_none_or(|p| p.id != id) {
            return false;
        }
        let pending = chain.pending.take().expect("pending was just checked");
        let Some(bytes) = state else {
            if pending.from_scratch {
                tracing::warn!(
                    turn = pending.turn,
                    "sidecar: checkpoint from turn 0 failed, no more checkpoints this match"
                );
                chain.disabled = true;
                return false;
            }
            chain.failures += 1;
            if chain.failures >= CHECKPOINT_FAILURE_LIMIT {
                tracing::warn!(
                    turn = pending.turn,
                    "sidecar: checkpoints keep failing, rebuilding the chain from turn 0"
                );
                chain.states.clear();
                chain.failures = 0;
            }
            return false;
        };
        chain.failures = 0;
        chain.states.push(BaseState {
            turn: pending.turn,
            state: Arc::new(bytes),
        });
        let head = self.ctx.feed.head();
        let chain = &mut self.ctx.checkpoints;
        if let Some(keep_from) = chain.states.iter().rposition(|b| b.turn <= head) {
            chain.states.drain(..keep_from);
        }
        tracing::info!(
            turn = pending.turn,
            kept = chain.states.len(),
            "sidecar: checkpoint stored"
        );
        // Gaia comes first and is never decided, so it is skipped.
        players.len() > 1
            && players[1..]
                .iter()
                .all(|state| state == "won" || state == "defeated")
    }
}

impl<S: MatchPhase> Server<S> {
    fn on_message(&mut self, peer: PeerID, msg: WireMessage) {
        let outcome = match msg {
            WireMessage::Joined(m) => self.on_joined(peer, m),
            WireMessage::PlayerPause(m) => self.on_player_pause(peer, m),
            WireMessage::PlayerCommand(m) => self.on_player_command(peer, m),
            WireMessage::Flare(m) => self.on_flare(peer, m),
            WireMessage::TurnSealed(m) => self.on_turn_sealed(peer, m),
            WireMessage::StateHash(m) => self.on_state_hash(peer, m),
            WireMessage::LoadedGame(m) => self.on_loaded_game(peer, m),
            WireMessage::GamestateChunk(m) => self.on_snapshot_chunk(peer, m),
            other => {
                self.on_common_message(peer, other);
                Ok(())
            }
        };
        if let Err(fault) = outcome {
            self.ctx.fault(peer, fault);
        }
    }

    // A completed snapshot is what unblocks the joiner that asked for it.
    fn on_snapshot_chunk(&mut self, peer: PeerID, msg: GamestateChunk) -> Result<(), PeerFault> {
        if let Some(TransferDone::JoinSnapshot { joiner }) = self.on_gamestate_chunk(peer, msg)? {
            // The snapshot cannot be past the last turn its source was
            // sealed, so a delayed joiner waits until the feed reaches that
            // turn before it sees anything.
            let bound = if self.ctx.turns.is_delayed(peer) {
                self.ctx.feed.head()
            } else {
                self.ctx.turns.ready_turn()
            };
            let Some(snapshot) = self.ctx.join_snapshot.clone() else {
                return Ok(());
            };
            self.deliver_snapshot(joiner, snapshot, bound);
        }
        Ok(())
    }

    fn on_state_dumped(&mut self, id: u32, state: Option<Vec<u8>>) {
        // A reply from a cancelled run carries its old id and is dropped, so
        // it can never be credited to the newer run waiting behind it.
        if self.ctx.dump.as_ref().is_none_or(|dump| dump.id != id) {
            return;
        }
        let dump = self.ctx.dump.take().expect("dump was just checked");
        let turn = dump.turn;
        let waiting = dump.waiting;
        match state {
            Some(bytes) => {
                tracing::info!(turn, joiners = waiting.len(), "sidecar: state dump ready");
                let snapshot = Arc::new(bytes);
                for joiner in waiting {
                    let still_syncing = self
                        .ctx
                        .sessions
                        .get(&joiner)
                        .is_some_and(|s| s.is_syncing());
                    if still_syncing {
                        self.deliver_snapshot(joiner, snapshot.clone(), turn);
                    }
                }
            }
            None => {
                for joiner in waiting {
                    let Some(session) = self.ctx.sessions.get(&joiner) else {
                        continue;
                    };
                    if !session.is_syncing() {
                        continue;
                    }
                    let delayed = session
                        .uuid
                        .clone()
                        .is_some_and(|u| self.ctx.is_delayed_observer(&u));
                    if delayed {
                        // A failed dump falls back to a live client rather
                        // than dropping the observer outright.
                        self.start_snapshot_fetch(joiner, false);
                    } else {
                        self.ctx
                            .disconnect(joiner, DisconnectReason::MatchInProgress);
                    }
                }
            }
        }
    }

    fn on_joined(&mut self, peer: PeerID, _msg: Joined) -> Result<(), PeerFault> {
        let uuid = self.ctx.speaker(peer, Session::is_in_game)?;
        let relayed = WireMessage::Joined(Joined { guid: uuid });
        self.ctx.broadcast(&relayed, |s| s.is_in_game());

        // The joiner missed every pause that happened before it arrived.
        let paused: Vec<Guid> = self.ctx.pause_budget.pausing().cloned().collect();
        for guid in paused {
            self.ctx.send(
                peer,
                WireMessage::PlayerPause(PlayerPause { guid, pause: true }),
            );
        }
        // Sent even to the player the match was held for: the next tick
        // lifts it for everyone at once, this client included.
        if self.ctx.pause_budget.auto_paused() {
            let guid = self.ctx.server_uuid.clone();
            self.ctx.send(
                peer,
                WireMessage::PlayerPause(PlayerPause { guid, pause: true }),
            );
        }
        Ok(())
    }

    fn on_player_pause(&mut self, peer: PeerID, msg: PlayerPause) -> Result<(), PeerFault> {
        let uuid = self.ctx.speaker(peer, Session::is_in_game)?;

        // A delayed observer pausing stops only its own feed, which blocks
        // nobody's release, so it is neither relayed nor charged.
        if self.ctx.turns.is_delayed(peer) {
            return Ok(());
        }

        if msg.pause {
            // An observer on the live stream owns no player, so its pause
            // costs it nothing and would freeze everyone else's screen for the
            // whole match.
            let slot = self.ctx.slots.slot_of(&uuid).filter(|s| *s != UNASSIGNED);
            if slot.is_none() {
                self.ctx
                    .server_chat(Some(peer), "Only players can pause the game.");
                self.ctx.send(
                    peer,
                    WireMessage::PlayerPause(PlayerPause {
                        guid: uuid,
                        pause: false,
                    }),
                );
                return Ok(());
            }

            let left = self.ctx.pause_budget.remaining(&uuid);
            if left <= TimeDelta::zero() {
                // The client pauses itself the moment it asks, so refusing is
                // not enough: it has to be told to lift its own overlay.
                self.ctx
                    .server_chat(Some(peer), "You are out of pause budget.");
                self.ctx.send(
                    peer,
                    WireMessage::PlayerPause(PlayerPause {
                        guid: uuid,
                        pause: false,
                    }),
                );
                return Ok(());
            }

            self.ctx.pause_budget.set_pausing(&uuid, true);
            let relayed = WireMessage::PlayerPause(PlayerPause {
                guid: uuid.clone(),
                pause: true,
            });
            // Advisory only, and the client accepts it nowhere but in-game.
            self.ctx
                .broadcast_except(peer, &relayed, |s| s.is_in_game());

            if let Some(name) = self.ctx.name_of(&uuid) {
                let text = format!(
                    "{} paused. {}s of pause budget left.",
                    name,
                    left.num_seconds()
                );
                self.ctx.server_chat(None, &text);
            }
            return Ok(());
        }

        self.ctx.pause_budget.set_pausing(&uuid, false);
        let relayed = WireMessage::PlayerPause(PlayerPause {
            guid: uuid,
            pause: false,
        });
        self.ctx
            .broadcast_except(peer, &relayed, |s| s.is_in_game());
        Ok(())
    }

    fn on_player_command(&mut self, peer: PeerID, msg: PlayerCommand) -> Result<(), PeerFault> {
        let uuid = self.ctx.speaker(peer, Session::is_in_game)?;
        let ai_player = self.ctx.is_ai_host(peer)
            && self
                .ctx
                .ai_host
                .as_ref()
                .is_some_and(|h| h.players.contains(&msg.player));
        if !self.st.frozen().cheats_enabled {
            // An observer holds a slot-table entry but owns no player, so a
            // command claiming player -1 must not be allowed to match it.
            let slot = self.ctx.slots.slot_of(&uuid).filter(|s| *s != UNASSIGNED);
            if slot.map(i32::from) != Some(msg.player) && !ai_player {
                // Silently, because a cheat attempt is not worth a disconnect.
                tracing::debug!(?peer, slot = ?slot, claimed = msg.player, "command slot mismatch");
                return Ok(());
            }
        }

        // Checked against the sender's own slot even with cheats on, where
        // commands for any player are relayed, so nobody can release the
        // match from waiting on someone else.
        // The AI host speaks for its AI players, so their resigning counts
        // the same as a player's own.
        let own_slot = self.ctx.slots.slot_of(&uuid).map(i32::from) == Some(msg.player);
        if (own_slot || ai_player)
            && PlayerCommand::extract_command_type(&msg.data).as_deref() == Some("resign")
        {
            self.ctx.resigned.insert(msg.player);
            // The resign takes effect at the turn it is scheduled for, so
            // that is the earliest turn a replay could show the match over.
            self.ctx.checkpoints.probe_at(msg.turn);
        }

        // Relayed unchanged, and the echo back to the sender is required: a
        // client executes its own commands only when the server returns them.
        let relayed = WireMessage::PlayerCommand(msg.clone());
        self.ctx.broadcast_live(&relayed);
        self.ctx.log.record_command(msg);
        Ok(())
    }

    fn on_flare(&mut self, peer: PeerID, msg: Flare) -> Result<(), PeerFault> {
        let uuid = self.ctx.speaker(peer, Session::is_in_game)?;
        let flare = Flare { guid: uuid, ..msg };
        self.ctx.broadcast_live(&WireMessage::Flare(flare.clone()));

        // A flare marks a place at the moment it is raised, which for a
        // player is the live turn and would tell a delayed observer where
        // things are going to be.
        let turn = if self.ctx.turns.is_delayed(peer) {
            self.ctx.feed.head()
        } else {
            self.ctx.turns.ready_turn()
        };
        if turn <= self.ctx.feed.head() {
            self.ctx.broadcast_delayed(&WireMessage::Flare(flare));
        } else {
            self.ctx.feed.queue_flare(turn, flare);
        }
        Ok(())
    }

    fn on_turn_sealed(&mut self, peer: PeerID, msg: TurnSealed) -> Result<(), PeerFault> {
        self.ctx.turns.on_turn_sealed(peer, msg.turn)?;
        let turn_length = self.st.frozen().turn_length_ms;
        self.ctx.release_turns(turn_length);
        Ok(())
    }

    fn on_state_hash(&mut self, peer: PeerID, msg: StateHash) -> Result<(), PeerFault> {
        // The server runs no simulation, so it can only compare what the
        // clients report, never decide which of them is right.
        for mismatch in self.ctx.turns.on_state_hash(peer, msg.turn, msg.hash)? {
            self.ctx.report_mismatch(mismatch);
        }
        Ok(())
    }

    // Only a syncing joiner sends this once the match is running.
    fn on_loaded_game(&mut self, peer: PeerID, msg: LoadedGame) -> Result<(), PeerFault> {
        let session = self.ctx.sessions.get(&peer).ok_or(PeerFault::NoSession)?;
        if !session.is_syncing() {
            return Err(PeerFault::WrongPhase);
        }
        let uuid = session.uuid.clone().ok_or(PeerFault::NoSession)?;
        let client_id = session.client_id().ok_or(PeerFault::NoSession)?;

        if self.ctx.is_delayed_observer(&uuid) {
            self.resume_delayed(peer, client_id, msg.current_turn);
            return Ok(());
        }

        let ready_turn = self.ctx.turns.ready_turn();
        // The replay must reach at least R+1 so the joiner's own next turn is
        // contiguous, and further whenever commands are already stored beyond
        // it, so nothing already recorded is withheld.
        let last_stored = self.ctx.log.last_command_turn().unwrap_or(0);
        let upper = (ready_turn + 1).max(last_stored);
        let default_length = self.st.frozen().turn_length_ms as u16;

        for turn in (msg.current_turn + 1)..=upper {
            let commands: Vec<PlayerCommand> = self.ctx.log.commands_for(turn).to_vec();
            for command in commands {
                self.ctx.send(peer, WireMessage::PlayerCommand(command));
            }
            // Seals must be contiguous from the snapshot turn, because the
            // client asserts that each one is exactly its ready turn plus one.
            if turn <= ready_turn {
                let turn_length = self.ctx.log.turn_length(turn).unwrap_or(default_length);
                self.ctx.send(
                    peer,
                    WireMessage::TurnSealed(TurnSealed { turn, turn_length }),
                );
            }
        }

        self.ctx.send(
            peer,
            WireMessage::LoadedGame(LoadedGame {
                current_turn: ready_turn,
            }),
        );
        if let Some(session) = self.ctx.sessions.get_mut(&peer) {
            session.set_role(Role::InGame);
        }

        let observer = self.ctx.is_observer(&uuid);
        self.ctx.turns.register(
            peer,
            client_id,
            ready_turn + COMMAND_DELAY - 1,
            ready_turn,
            observer,
            false,
        );
        Ok(())
    }

    // The live join replay, cut at the feed's head instead of the live turn.
    // Commands past the head are not replayed: the feed sends each turn's
    // commands together with its seal, so they are still to come.
    fn resume_delayed(&mut self, peer: PeerID, client_id: u16, snapshot_turn: u32) {
        let head = self.ctx.feed.head();
        let default_length = self.st.frozen().turn_length_ms as u16;
        for turn in (snapshot_turn + 1)..=head {
            let commands: Vec<PlayerCommand> = self.ctx.log.commands_for(turn).to_vec();
            for command in commands {
                self.ctx.send(peer, WireMessage::PlayerCommand(command));
            }
            let turn_length = self.ctx.log.turn_length(turn).unwrap_or(default_length);
            self.ctx.send(
                peer,
                WireMessage::TurnSealed(TurnSealed { turn, turn_length }),
            );
        }

        self.ctx.send(
            peer,
            WireMessage::LoadedGame(LoadedGame { current_turn: head }),
        );
        if let Some(session) = self.ctx.sessions.get_mut(&peer) {
            session.set_role(Role::InGame);
        }
        self.ctx.feed.forget(peer);
        self.ctx
            .turns
            .register(peer, client_id, head + COMMAND_DELAY - 1, head, true, true);
    }
}

impl Server<PostGame> {
    fn on_input(mut self, input: Input) -> AnyServer {
        match input {
            Input::Received { peer, msg } => self.on_message(peer, msg),
            Input::StateDumped { id, state } => self.on_state_dumped(id, state),
            Input::Tick { now, stats } => {
                self.on_tick(now, stats);
                if self.close_due(now) {
                    self.ctx.effects.push(Effect::GameOver);
                }
            }
            other => self.on_common_input(other),
        }
        self.into()
    }

    // Closes once nobody is left to watch, or the linger has run out. The AI
    // host alone keeps nothing open. A negative delta re-anchors rather than
    // firing early (A7).
    fn close_due(&mut self, now: DateTime<Utc>) -> bool {
        let anyone_admitted = self
            .ctx
            .sessions
            .iter()
            .any(|(p, s)| s.admitted.is_some() && !self.ctx.is_ai_host(*p));
        if !anyone_admitted {
            return true;
        }
        let anchor = *self.st.ended_at.get_or_insert(now);
        let elapsed = now.signed_duration_since(anchor);
        if elapsed < TimeDelta::zero() {
            self.st.ended_at = Some(now);
            return false;
        }
        elapsed >= self.ctx.config.post_game_linger
    }
}

// A duration as the chat line announcing it reads it.
fn describe_span(span: TimeDelta) -> String {
    let secs = span.num_seconds().max(0);
    if secs == 60 {
        "1 minute".to_string()
    } else if secs > 60 && secs % 60 == 0 {
        format!("{} minutes", secs / 60)
    } else {
        format!("{secs} seconds")
    }
}

// The only thing the server reads out of the settings blob. A full JSON parser
// would be a dependency and a decode the relay otherwise never needs.
fn cheats_enabled(json: &[u8]) -> bool {
    const KEY: &[u8] = b"\"CheatsEnabled\"";
    let Some(at) = json.windows(KEY.len()).position(|w| w == KEY) else {
        return false;
    };
    let rest = &json[at + KEY.len()..];
    let value: Vec<u8> = rest
        .iter()
        .copied()
        .skip_while(|c| c.is_ascii_whitespace() || *c == b':')
        .take(4)
        .collect();
    value.starts_with(b"true")
}

// The player ids a match is played between, and whether it can end at all.
// PlayerData starts at player 1: the engine puts gaia in front of it itself.
// Settings that cannot be read give no players and an endless match, so the
// resign rule never ends a match it cannot see.
fn match_players(json: &[u8]) -> (Vec<i32>, bool) {
    let Ok(attribs) = serde_json::from_slice::<serde_json::Value>(json) else {
        return (Vec::new(), true);
    };
    let Some(settings) = attribs.get("settings") else {
        return (Vec::new(), true);
    };
    let count = settings
        .get("PlayerData")
        .and_then(|pd| pd.as_array())
        .map_or(0, |pd| pd.len());
    let player_ids = (1..=count as i32).collect();
    let endless = settings
        .get("VictoryConditions")
        .and_then(|vc| vc.as_array())
        .is_none_or(|vc| vc.is_empty());
    (player_ids, endless)
}

// What START_SETTINGS becomes when a match has AI slots: every stock client
// gets `stock` and the AI host gets `ai_host`.
struct AiSettingsSplit {
    stock: Vec<u8>,
    ai_host: Vec<u8>,
    players: HashSet<i32>,
}

enum StartOutcome {
    Started(FrozenSettings),
    AwaitAiHost(AwaitAiHost),
}

// None when there is no AI slot, or the settings cannot be read, in which
// case the match starts the stock way with every client running its own AI.
//
// The AI slots are stripped from what stock clients receive: a client that
// sees a slot as AI computes it locally and flags the player as AI, and that
// flag changes how commands are processed, so it has to read the same on
// every client. The map is also forced explored, because an unflagged AI no
// longer skips the fog check when it places buildings, and it has no
// scouting of its own to explore with. The AI host keeps the AI fields so it
// still registers the bots.
fn split_ai_settings(json: &[u8]) -> Option<AiSettingsSplit> {
    let mut attribs: serde_json::Value = serde_json::from_slice(json).ok()?;
    let settings = attribs.get_mut("settings")?.as_object_mut()?;
    // PlayerData starts at player 1: the engine puts gaia in front of it
    // itself, so a slot's player id is its index plus one.
    let players: HashSet<i32> = settings
        .get("PlayerData")?
        .as_array()?
        .iter()
        .enumerate()
        .filter(|(_, pd)| {
            pd.get("AI")
                .and_then(|ai| ai.as_str())
                .is_some_and(|ai| !ai.is_empty())
        })
        .map(|(i, _)| i as i32 + 1)
        .collect();
    if players.is_empty() {
        return None;
    }
    settings.insert("ExploreMap".to_string(), serde_json::Value::Bool(true));
    let ai_host = serde_json::to_vec(&attribs).ok()?;

    let player_data = attribs
        .get_mut("settings")?
        .get_mut("PlayerData")?
        .as_array_mut()?;
    for &player in &players {
        if let Some(slot) = player_data
            .get_mut(player as usize - 1)
            .and_then(|pd| pd.as_object_mut())
        {
            for field in AI_FIELDS {
                slot.remove(field);
            }
        }
    }
    let stock = serde_json::to_vec(&attribs).ok()?;
    Some(AiSettingsSplit {
        stock,
        ai_host,
        players,
    })
}
