// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::net::Ipv4Addr;

use rusty_enet::PeerID;

use crate::relay::messages::Authenticate;
use crate::relay::messages::Chat;
use crate::relay::messages::EnabledMod;
use crate::relay::messages::Flare;
use crate::relay::messages::GameSettings;
use crate::relay::messages::GamestateChunk;
use crate::relay::messages::GamestateChunkAck;
use crate::relay::messages::GamestateRequest;
use crate::relay::messages::GamestateResponse;
use crate::relay::messages::Joined;
use crate::relay::messages::Kicked;
use crate::relay::messages::LoadedGame;
use crate::relay::messages::MapPlayerIdToSlot;
use crate::relay::messages::PlayerCommand;
use crate::relay::messages::PlayerPause;
use crate::relay::messages::PreGameStatus;
use crate::relay::messages::StartSavegameSettings;
use crate::relay::messages::StartSettings;
use crate::relay::messages::StateHash;
use crate::relay::messages::SynAck;
use crate::relay::messages::TurnSealed;
use crate::relay::messages::WireMessage;

#[derive(Debug)]
pub enum Input {
    Connected { peer: PeerID, addr: Ipv4Addr },
    Received { peer: PeerID, msg: WireMessage },
    Disconnected { peer: PeerID },
    LobbyAuth { username: String, token: String },
    Tick,
}

#[derive(Debug)]
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
}

pub struct Session;

pub struct FrozenSettings {
    // Kept verbatim because JOIN must carry the same text to late joiners.
    pub json: Vec<u8>,
    pub cheats_enabled: bool,
    pub turn_length_ms: u32,
}

struct Context {
    #[expect(dead_code, reason = "read once the handlers are implemented")]
    config: Config,
    #[expect(dead_code, reason = "read once the handlers are implemented")]
    sessions: HashMap<PeerID, Session>,
    effects: Vec<Effect>,
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
    pub ready_turn: u32,
}

// Setup handlers stay available while a savegame is being fetched.
pub trait SetupPhase {}
impl SetupPhase for Setup {}
impl SetupPhase for AwaitSavegame {}

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

    pub fn shutdown(self) -> Vec<Effect> {
        todo!("unreliable DisconnectNow with ServerShuttingDown to every peer")
    }

    fn on_common_input(&mut self, input: Input) {
        match input {
            Input::Connected { peer, addr } => self.on_connected(peer, addr),
            Input::Received { peer, msg } => self.on_common_message(peer, msg),
            Input::Disconnected { peer } => self.on_disconnected(peer),
            Input::LobbyAuth { username, token } => self.on_lobby_auth(username, token),
            Input::Tick => self.on_tick(),
        }
    }

    // Messages accepted in every phase after idle. Anything reaching the
    // fallback arm is not accepted in the current phase and is dropped with
    // the connection kept open.
    fn on_common_message(&mut self, peer: PeerID, msg: WireMessage) {
        match msg {
            WireMessage::SynAck(m) => self.on_syn_ack(peer, m),
            WireMessage::Authenticate(m) => self.on_authenticate(peer, m),
            WireMessage::Chat(m) => self.on_chat(peer, m),
            WireMessage::Kicked(m) => self.on_kicked(peer, m),
            WireMessage::GamestateRequest(m) => self.on_gamestate_request(peer, m),
            WireMessage::GamestateResponse(m) => self.on_gamestate_response(peer, m),
            WireMessage::GamestateChunk(m) => {
                self.on_gamestate_chunk(peer, m);
            }
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
            }
            other => {
                tracing::debug!(
                    ?peer,
                    msg_type = other.name(),
                    "message not accepted in this phase"
                );
            }
        }
    }

    fn on_connected(&mut self, _peer: PeerID, _addr: Ipv4Addr) {
        todo!("create session, disconnect banned IPs with Banned, else send SYN")
    }

    fn on_disconnected(&mut self, _peer: PeerID) {
        todo!(
            "mark player slot disconnected, broadcast PLAYER_SLOTS, re-evaluate turn release and hash comparison"
        )
    }

    fn on_lobby_auth(&mut self, _username: String, _token: String) {
        todo!(
            "find session whose UUID equals token, store lobby user name, send empty AUTHENTICATE"
        )
    }

    fn on_tick(&mut self) {
        todo!(
            "at most once per second: LAST_SEEN for peers silent > 2000 ms, else LAGGING_CLIENTS for RTT > 400 ms"
        )
    }

    fn on_syn_ack(&mut self, _peer: PeerID, _msg: SynAck) {
        todo!(
            "check game version, then sim/mod compatibility, issue unique UUID, send ACK with lobby flag"
        )
    }

    fn on_authenticate(&mut self, _peer: PeerID, _msg: Authenticate) {
        todo!(
            "ordered auth checks, admission, AUTHENTICATE_RESULT, PLAYER_SLOTS; a joiner starts the snapshot fetch"
        )
    }

    fn on_chat(&mut self, _peer: PeerID, _msg: Chat) {
        todo!("overwrite sender_uuid, relay with empty receiver list to setup and in-game sessions")
    }

    fn on_kicked(&mut self, _peer: PeerID, _msg: Kicked) {
        todo!(
            "controller only: find session by name, optionally ban name and IP, disconnect, relay KICKED"
        )
    }

    fn on_gamestate_request(&mut self, _peer: PeerID, _msg: GamestateRequest) {
        todo!(
            "serve cached SAVEGAME or RUNNING_GAME: GAMESTATE_RESPONSE, then chunks of <= 1024 B, window 32"
        )
    }

    fn on_gamestate_response(&mut self, _peer: PeerID, _msg: GamestateResponse) {
        todo!("transfer must exist, length in 1..=8 MiB")
    }

    // Returns the payload of a transfer this chunk completed, so the savegame
    // phase can turn it into a state transition.
    fn on_gamestate_chunk(&mut self, _peer: PeerID, _msg: GamestateChunk) -> Option<Vec<u8>> {
        todo!(
            "append to transfer, send one GAMESTATE_CHUNK_ACK, error if total exceeds declared length"
        )
    }

    fn on_gamestate_chunk_ack(&mut self, _peer: PeerID, _msg: GamestateChunkAck) {
        todo!("free window slots, drop ACKs for unknown transfers or beyond what is in flight")
    }
}

impl<S: SetupPhase> Server<S> {
    fn on_pre_game_status(&mut self, _peer: PeerID, _msg: PreGameStatus) {
        todo!("overwrite uuid, relay to setup sessions, update slot status without PLAYER_SLOTS")
    }

    fn on_reset_pregame_status(&mut self, _peer: PeerID) {
        todo!("controller only: every status other than 2 becomes 0, broadcast PLAYER_SLOTS")
    }

    fn on_game_settings(&mut self, _peer: PeerID, _msg: GameSettings) {
        todo!("controller only: relay verbatim to setup sessions, controller included")
    }

    fn on_map_player_id_to_slot(&mut self, _peer: PeerID, _msg: MapPlayerIdToSlot) {
        todo!(
            "controller only: assign slot, clear it from any other holder, broadcast PLAYER_SLOTS"
        )
    }

    // Hands back what it did not consume so each setup-like phase can add its
    // own arms before the common fallback.
    fn on_setup_message(&mut self, peer: PeerID, msg: WireMessage) -> Option<WireMessage> {
        match msg {
            WireMessage::PreGameStatus(m) => self.on_pre_game_status(peer, m),
            WireMessage::ResetPregameStatus => self.on_reset_pregame_status(peer),
            WireMessage::GameSettings(m) => self.on_game_settings(peer, m),
            WireMessage::MapPlayerIdToSlot(m) => self.on_map_player_id_to_slot(peer, m),
            other => return Some(other),
        }
        None
    }
}

impl Server<Idle> {
    pub fn new(config: Config) -> Self {
        Server {
            ctx: Context {
                config,
                sessions: HashMap::new(),
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
    fn on_start_settings(&mut self, _peer: PeerID, _msg: StartSettings) -> Option<FrozenSettings> {
        todo!("controller only, reject if any connected slot has status 0, emit start effects")
    }

    fn on_start_savegame_settings(
        &mut self,
        _peer: PeerID,
        _msg: StartSavegameSettings,
    ) -> Option<Vec<u8>> {
        todo!("controller only, request SAVEGAME from the controller")
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
            WireMessage::GamestateChunk(m) => {
                if let Some(saved_state) = self.on_gamestate_chunk(peer, m) {
                    let settings = self.on_savegame_complete(peer);
                    return self.savegame_received(saved_state, settings).into();
                }
            }
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
            st: InGame {
                settings,
                // One below the 4-turn command delay, so turn 4 is the first release.
                ready_turn: 3,
            },
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
    fn on_loaded_game(&mut self, _peer: PeerID, _msg: LoadedGame) -> bool {
        todo!("others still loading: PLAYERS_LOADING; last one: LOADED_GAME turn 0 to all")
    }

    // A departure can leave everyone else loaded, which also starts the match.
    fn on_disconnected_while_loading(&mut self, _peer: PeerID) -> bool {
        todo!(
            "completion check: if everyone left is loaded, LOADED_GAME turn 0 to all, no PLAYERS_LOADING"
        )
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
        match msg {
            WireMessage::Joined(m) => self.on_joined(peer, m),
            WireMessage::PlayerPause(m) => self.on_player_pause(peer, m),
            WireMessage::PlayerCommand(m) => self.on_player_command(peer, m),
            WireMessage::Flare(m) => self.on_flare(peer, m),
            WireMessage::TurnSealed(m) => self.on_turn_sealed(peer, m),
            WireMessage::StateHash(m) => self.on_state_hash(peer, m),
            WireMessage::LoadedGame(m) => self.on_loaded_game(peer, m),
            other => self.on_common_message(peer, other),
        }
    }

    fn on_joined(&mut self, _peer: PeerID, _msg: Joined) {
        todo!(
            "overwrite uuid, broadcast to in-game incl. sender, then send current PLAYER_PAUSE states"
        )
    }

    fn on_player_pause(&mut self, _peer: PeerID, _msg: PlayerPause) {
        todo!("overwrite uuid, update paused set, send to in-game sessions except the sender")
    }

    fn on_player_command(&mut self, _peer: PeerID, _msg: PlayerCommand) {
        todo!(
            "unless cheats are on, drop if slot mismatches; echo to all in-game, keep for join replay"
        )
    }

    fn on_flare(&mut self, _peer: PeerID, _msg: Flare) {
        todo!("overwrite uuid, broadcast to in-game sessions incl. sender")
    }

    fn on_turn_sealed(&mut self, _peer: PeerID, _msg: TurnSealed) {
        todo!(
            "sequence check (OutOfSequenceTurnSeal), record ready turn, release next turn when unblocked"
        )
    }

    fn on_state_hash(&mut self, _peer: PeerID, _msg: StateHash) {
        todo!(
            "sequence check (OutOfSequenceStateHash), record hash, compare once all have reported"
        )
    }

    // Only a syncing joiner sends this once the match is running.
    fn on_loaded_game(&mut self, _peer: PeerID, _msg: LoadedGame) {
        todo!(
            "replay stored commands and turn seals from t+1, send LOADED_GAME at the ready turn, mark in-game"
        )
    }
}
