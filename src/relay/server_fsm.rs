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
use crate::relay::monitor::Monitor;
use crate::relay::monitor::PeerStats;
use crate::relay::monitor::Warning;
use crate::relay::password;
use crate::relay::pause_budget;
use crate::relay::pause_budget::BudgetEvent;
use crate::relay::pause_budget::PauseBudget;
use crate::relay::session::Admitted;
use crate::relay::session::Role;
use crate::relay::session::Session;
use crate::relay::slots::STATUS_NOT_READY;
use crate::relay::slots::Slots;
use crate::relay::slots::UNASSIGNED;
use crate::relay::turn::INITIAL_READY_TURN;
use crate::relay::turn::MatchLog;
use crate::relay::turn::TurnManager;

const SYN_CHALLENGE: u32 = 0x5073013F;
const GAME_VERSION: u32 = 0x01010019;
const SIMULATION_VERSION: &str = "0.28.0";

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
    // How long the game may sit with nobody ever having joined, or with
    // everybody gone, before it shuts itself down. None means never, which is
    // what keeps a standalone game running with nobody watching it.
    pub idle_shutdown: Option<TimeDelta>,
    // The hostme sender, used as the lobby listing's hostUsername until a
    // controller with a name of its own is admitted.
    pub lobby_host_name: String,
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
            buddies: HashSet::new(),
            max_sessions: MAX_SESSIONS,
            release_controller_on_leave: true,
            server_name: "SERVER".to_string(),
            welcome_message: String::new(),
            pause_budget: pause_budget::DEFAULT_BUDGET,
            idle_shutdown: None,
            lobby_host_name: String::new(),
        }
    }
}

pub struct FrozenSettings {
    // Kept verbatim because JOIN must carry the same text to late joiners.
    pub json: Vec<u8>,
    pub cheats_enabled: bool,
    pub turn_length_ms: u32,
}

// The server-wide phase, as the authentication rules see it. It is derived
// from the typestate rather than stored, so it cannot drift out of step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Setup,
    Loading,
    InGame,
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
    transfers: Transfers,
    monitor: Monitor,
    pause_budget: PauseBudget,
    turns: TurnManager,
    log: MatchLog,
    // Served to every joiner that asks, so it is shared rather than copied.
    join_snapshot: Option<Arc<Vec<u8>>>,
    // Both fed only by Input::Tick.now (A2). created_at is set on the first
    // tick; empty_since only once the game has held a player and lost every
    // one again.
    created_at: Option<DateTime<Utc>>,
    empty_since: Option<DateTime<Utc>>,
    effects: Vec<Effect>,
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
    fn is_observer(&self, uuid: &Guid) -> bool {
        self.slots.slot_of(uuid) == Some(UNASSIGNED) && self.controller.as_ref() != Some(uuid)
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
            self.broadcast(&msg, |s| s.is_in_game());
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
        self.broadcast(&msg, |s| s.is_in_game());
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

pub struct Loading {
    pub settings: FrozenSettings,
    pub saved_state: Option<Vec<u8>>,
}

pub struct InGame {
    pub settings: FrozenSettings,
}

// Setup handlers stay available while a savegame is being fetched.
pub trait SetupPhase {}
impl SetupPhase for Setup {}
impl SetupPhase for AwaitSavegame {}

// Lets the shared handlers apply the phase-dependent rules without knowing
// which typestate they were called from.
pub trait PhaseMarker {
    const PHASE: Phase;
}
impl PhaseMarker for Setup {
    const PHASE: Phase = Phase::Setup;
}
impl PhaseMarker for AwaitSavegame {
    const PHASE: Phase = Phase::Setup;
}
impl PhaseMarker for Loading {
    const PHASE: Phase = Phase::Loading;
}
impl PhaseMarker for InGame {
    const PHASE: Phase = Phase::InGame;
}

pub enum AnyServer {
    Idle(Server<Idle>),
    Setup(Server<Setup>),
    AwaitSavegame(Server<AwaitSavegame>),
    Loading(Server<Loading>),
    InGame(Server<InGame>),
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

impl AnyServer {
    pub fn handle(self, input: Input) -> AnyServer {
        match self {
            AnyServer::Idle(s) => s.on_input(input),
            AnyServer::Setup(s) => s.on_input(input),
            AnyServer::AwaitSavegame(s) => s.on_input(input),
            AnyServer::Loading(s) => s.on_input(input),
            AnyServer::InGame(s) => s.on_input(input),
        }
    }

    pub fn take_effects(&mut self) -> Vec<Effect> {
        match self {
            AnyServer::Idle(s) => s.take_effects(),
            AnyServer::Setup(s) => s.take_effects(),
            AnyServer::AwaitSavegame(s) => s.take_effects(),
            AnyServer::Loading(s) => s.take_effects(),
            AnyServer::InGame(s) => s.take_effects(),
        }
    }

    pub fn shutdown(self) -> Vec<Effect> {
        match self {
            AnyServer::Idle(s) => s.shutdown(),
            AnyServer::Setup(s) => s.shutdown(),
            AnyServer::AwaitSavegame(s) => s.shutdown(),
            AnyServer::Loading(s) => s.shutdown(),
            AnyServer::InGame(s) => s.shutdown(),
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
            self.ctx.effects.push(Effect::Disconnect {
                peer,
                reason: DisconnectReason::Banned,
            });
            return;
        }
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
        if S::PHASE == Phase::InGame {
            let players = self.ctx.slots.connected_players();
            let events = self.ctx.pause_budget.check(now, players);
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
        let anyone_admitted = self.ctx.sessions.values().any(|s| s.admitted.is_some());

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

        let flags = if self.ctx.config.lobby_mode {
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
        if self.ctx.config.lobby_mode && lobby_name.is_none() {
            // The client has not been prompted yet, so this cannot be its
            // real answer.
            return Err(PeerFault::WrongPhase);
        }

        let sanitized = auth::sanitize(&msg.name);

        if S::PHASE == Phase::Loading {
            self.disconnect(peer, DisconnectReason::ServerLoading);
            return Ok(());
        }

        if self.ctx.config.lobby_mode {
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
        if expected != msg.password {
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

        self.admit_session(peer, uuid, name, joining, &msg.controller_secret);
        Ok(())
    }

    // Ok(true) means the session is admitted via the joining/syncing path:
    // a recovered slot or a brand-new late observer both count. Ok(false) is
    // only ever returned by the Setup-phase branch below.
    fn admit(&self, name: &str) -> Result<bool, DisconnectReason> {
        let sessions = self.ctx.sessions.len();

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
        let is_controller =
            self.ctx.controller.is_none() && controller_secret == self.ctx.config.controller_secret;
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
            self.start_snapshot_fetch(peer);
        }
    }

    // Only an in-game client can serialize a live snapshot; asking anyone else
    // is undefined behaviour on the client side.
    fn start_snapshot_fetch(&mut self, joiner: PeerID) {
        let mut candidates: Vec<(PeerID, bool, chrono::TimeDelta)> = self
            .ctx
            .sessions
            .iter()
            .filter(|(p, s)| **p != joiner && s.is_in_game())
            .map(|(p, s)| (*p, self.ctx.turns.is_out_of_sync(*p), s.mean_rtt))
            .collect();
        candidates.sort_by_key(|(_, desynced, rtt)| (*desynced, *rtt));

        let Some((source, _, _)) = candidates.first().copied() else {
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
        // The controller can never kick itself, and an unknown name does
        // nothing at all.
        let Some(target) = target.filter(|t| *t != peer) else {
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
            KIND_RUNNING_GAME => self.ctx.join_snapshot.clone(),
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
        // Relayed verbatim, controller included. The bytes are a script value
        // the server has no reason to decode.
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

impl Server<Idle> {
    pub fn new(config: Config) -> Self {
        let pause_budget = PauseBudget::new(config.pause_budget);
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
                transfers: Transfers::default(),
                monitor: Monitor::default(),
                pause_budget,
                turns: TurnManager::default(),
                log: MatchLog::default(),
                join_snapshot: None,
                created_at: None,
                empty_since: None,
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
    pub fn start(mut self, settings: FrozenSettings) -> Server<Loading> {
        // The last listing update the match ever gets: Sec. 17.3 wants
        // register followed by changestate, and register already went out
        // from the broadcast_player_slots call in on_start_settings.
        let (nbp, players) = self.ctx.lobby_counts();
        self.ctx.effects.push(Effect::LobbyStarted { nbp, players });
        self.with_state(Loading {
            settings,
            saved_state: None,
        })
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
            WireMessage::StartSettings(m) => {
                if let Some(settings) = self.on_start_settings(peer, m) {
                    return self.start(settings).into();
                }
            }
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
    fn on_start_settings(&mut self, peer: PeerID, msg: StartSettings) -> Option<FrozenSettings> {
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

        let settings = FrozenSettings {
            cheats_enabled: cheats_enabled(&msg.init_attributes),
            json: msg.init_attributes.clone(),
            turn_length_ms: self.ctx.config.turn_length_ms,
        };

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

        let relayed = WireMessage::StartSettings(msg);
        self.ctx.broadcast(&relayed, |s| s.is_setup());

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
            self.ctx.turns.register(
                peer,
                client_id,
                INITIAL_READY_TURN,
                FIRST_SIMULATED_TURN,
                observer,
            );
        }

        Some(settings)
    }

    // Not implemented: the saved-game flow would be "controller only, request
    // SAVEGAME from the controller". Until it is, the start is refused rather
    // than panicked on, because any client can send this message.
    fn on_start_savegame_settings(
        &mut self,
        peer: PeerID,
        _msg: StartSavegameSettings,
    ) -> Option<Vec<u8>> {
        tracing::warn!(?peer, "savegame start is not implemented, ignoring");
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

impl Server<InGame> {
    fn on_input(mut self, input: Input) -> AnyServer {
        match input {
            Input::Received { peer, msg } => self.on_message(peer, msg),
            other => self.on_common_input(other),
        }
        self.into()
    }

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
            let json = self.st.settings.json.clone();
            self.ctx.send(
                joiner,
                WireMessage::Join(Join {
                    init_attributes: json,
                }),
            );
        }
        Ok(())
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
        Ok(())
    }

    fn on_player_pause(&mut self, peer: PeerID, msg: PlayerPause) -> Result<(), PeerFault> {
        let uuid = self.ctx.speaker(peer, Session::is_in_game)?;

        if msg.pause {
            // An observer owns no player, so its pause costs it nothing and
            // would freeze everyone else's screen for the whole match. This
            // holds only while observers watch the live turn stream: once they
            // run on a feed that lags the match, pausing that feed stops
            // nothing anyone else can see and can be allowed back.
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
        if !self.st.settings.cheats_enabled {
            // An observer holds a slot-table entry but owns no player, so a
            // command claiming player -1 must not be allowed to match it.
            let slot = self.ctx.slots.slot_of(&uuid).filter(|s| *s != UNASSIGNED);
            if slot.map(i32::from) != Some(msg.player) {
                // Silently, because a cheat attempt is not worth a disconnect.
                tracing::debug!(?peer, slot = ?slot, claimed = msg.player, "command slot mismatch");
                return Ok(());
            }
        }

        // Relayed unchanged, and the echo back to the sender is required: a
        // client executes its own commands only when the server returns them.
        let relayed = WireMessage::PlayerCommand(msg.clone());
        self.ctx.broadcast(&relayed, |s| s.is_in_game());
        self.ctx.log.record_command(msg);
        Ok(())
    }

    fn on_flare(&mut self, peer: PeerID, msg: Flare) -> Result<(), PeerFault> {
        let uuid = self.ctx.speaker(peer, Session::is_in_game)?;
        let relayed = WireMessage::Flare(Flare { guid: uuid, ..msg });
        self.ctx.broadcast(&relayed, |s| s.is_in_game());
        Ok(())
    }

    fn on_turn_sealed(&mut self, peer: PeerID, msg: TurnSealed) -> Result<(), PeerFault> {
        self.ctx.turns.on_turn_sealed(peer, msg.turn)?;
        let turn_length = self.st.settings.turn_length_ms;
        self.ctx.release_turns(turn_length);
        Ok(())
    }

    fn on_state_hash(&mut self, peer: PeerID, msg: StateHash) -> Result<(), PeerFault> {
        // The server runs no simulation, so it can only compare what the
        // clients report, never decide which of them is right.
        if let Some(mismatch) = self.ctx.turns.on_state_hash(peer, msg.turn, msg.hash)? {
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

        let ready_turn = self.ctx.turns.ready_turn();
        // The replay must reach at least R+1 so the joiner's own next turn is
        // contiguous, and further whenever commands are already stored beyond
        // it, so nothing already recorded is withheld.
        let last_stored = self.ctx.log.last_command_turn().unwrap_or(0);
        let upper = (ready_turn + 1).max(last_stored);
        let default_length = self.st.settings.turn_length_ms as u16;

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
        );
        Ok(())
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
