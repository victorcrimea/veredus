// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use crate::relay::fault::ParseError;
use crate::utils::hex_dump;

mod ack;
mod authenticate;
mod authenticate_result;
mod authenticate_result_code;
mod chat;
mod enabled_mod;
mod flare;
mod game_settings;
mod gamestate_chunk;
mod gamestate_chunk_ack;
mod gamestate_request;
mod gamestate_response;
mod guid;
mod host;
mod join;
mod joined;
mod kicked;
mod lagging_clients;
mod last_seen;
mod loaded_game;
mod map_player_id_to_slot;
mod performance_entry;
pub(crate) mod player_command;
mod player_pause;
mod player_slots;
mod players_loading;
mod pre_game_status;
mod start_savegame_settings;
mod start_settings;
mod state_hash;
mod syn;
mod syn_ack;
mod turn_sealed;
mod wrong_hash_players;

pub use ack::Ack;
pub use authenticate::Authenticate;
pub use authenticate_result::AuthenticateResult;
pub use authenticate_result_code::AuthenticateResultCode;
pub use chat::Chat;
pub use enabled_mod::EnabledMod;
pub use flare::Flare;
pub use game_settings::GameSettings;
pub use gamestate_chunk::GamestateChunk;
pub use gamestate_chunk_ack::GamestateChunkAck;
pub use gamestate_request::GamestateRequest;
pub use gamestate_response::GamestateResponse;
pub use join::Join;
pub use joined::Joined;
pub use kicked::Kicked;
pub use lagging_clients::LaggingClients;
pub use last_seen::LastSeen;
pub use loaded_game::LoadedGame;
pub use map_player_id_to_slot::MapPlayerIdToSlot;
pub use player_command::PlayerCommand;
pub use player_pause::PlayerPause;
pub use player_slots::PlayerSlots;
pub use players_loading::PlayersLoading;
pub use pre_game_status::PreGameStatus;
pub use start_savegame_settings::StartSavegameSettings;
pub use start_settings::StartSettings;
pub use state_hash::StateHash;
pub use syn::Syn;
pub use syn_ack::SynAck;
pub use turn_sealed::TurnSealed;
pub use wrong_hash_players::WrongHashPlayers;

#[derive(Debug)]
pub enum WireMessage {
    Syn(Syn),
    SynAck(SynAck),
    Ack(Ack),
    Authenticate(Authenticate),
    AuthenticateResult(AuthenticateResult),
    Chat(Chat),
    PreGameStatus(PreGameStatus),
    ResetPregameStatus,
    GameSettings(GameSettings),
    MapPlayerIdToSlot(MapPlayerIdToSlot),
    PlayerSlots(PlayerSlots),
    GamestateRequest(GamestateRequest),
    GamestateResponse(GamestateResponse),
    GamestateChunk(GamestateChunk),
    GamestateChunkAck(GamestateChunkAck),
    Join(Join),
    Joined(Joined),
    Kicked(Kicked),
    LastSeen(LastSeen),
    LaggingClients(LaggingClients),
    PlayersLoading(PlayersLoading),
    PlayerPause(PlayerPause),
    LoadedGame(LoadedGame),
    StartSettings(StartSettings),
    StartSavegameSettings(StartSavegameSettings),
    TurnSealed(TurnSealed),
    StateHash(StateHash),
    WrongHashPlayers(WrongHashPlayers),
    PlayerCommand(PlayerCommand),
    Flare(Flare),
}

impl WireMessage {
    pub fn id(&self) -> u8 {
        match self {
            WireMessage::Syn(_) => 1,
            WireMessage::SynAck(_) => 2,
            WireMessage::Ack(_) => 3,
            WireMessage::Authenticate(_) => 4,
            WireMessage::AuthenticateResult(_) => 5,
            WireMessage::Chat(_) => 6,
            WireMessage::PreGameStatus(_) => 7,
            WireMessage::ResetPregameStatus => 8,
            WireMessage::GameSettings(_) => 9,
            WireMessage::MapPlayerIdToSlot(_) => 10,
            WireMessage::PlayerSlots(_) => 11,
            WireMessage::GamestateRequest(_) => 12,
            WireMessage::GamestateResponse(_) => 13,
            WireMessage::GamestateChunk(_) => 14,
            WireMessage::GamestateChunkAck(_) => 15,
            WireMessage::Join(_) => 16,
            WireMessage::Joined(_) => 17,
            WireMessage::Kicked(_) => 18,
            WireMessage::LastSeen(_) => 19,
            WireMessage::LaggingClients(_) => 20,
            WireMessage::PlayersLoading(_) => 21,
            WireMessage::PlayerPause(_) => 22,
            WireMessage::LoadedGame(_) => 23,
            WireMessage::StartSettings(_) => 24,
            WireMessage::StartSavegameSettings(_) => 25,
            WireMessage::TurnSealed(_) => 26,
            WireMessage::StateHash(_) => 27,
            WireMessage::WrongHashPlayers(_) => 28,
            WireMessage::PlayerCommand(_) => 29,
            WireMessage::Flare(_) => 30,
        }
    }
    pub const ALL_NAMES: &'static [&'static str] = &[
        "Syn",
        "SynAck",
        "Ack",
        "Authenticate",
        "AuthenticateResult",
        "Chat",
        "PreGameStatus",
        "ResetPregameStatus",
        "GameSettings",
        "MapPlayerIdToSlot",
        "PlayerSlots",
        "GamestateRequest",
        "GamestateResponse",
        "GamestateChunk",
        "GamestateChunkAck",
        "Join",
        "Joined",
        "Kicked",
        "LastSeen",
        "LaggingClients",
        "PlayersLoading",
        "PlayerPause",
        "LoadedGame",
        "StartSettings",
        "StartSavegameSettings",
        "TurnSealed",
        "StateHash",
        "WrongHashPlayers",
        "PlayerCommand",
        "Flare",
        "ConnectComplete",
        "ConnectionLost",
        "Invalid",
    ];

    pub fn name(&self) -> &'static str {
        match self {
            WireMessage::Syn(_) => "Syn",
            WireMessage::SynAck(_) => "SynAck",
            WireMessage::Ack(_) => "Ack",
            WireMessage::Authenticate(_) => "Authenticate",
            WireMessage::AuthenticateResult(_) => "AuthenticateResult",
            WireMessage::Chat(_) => "Chat",
            WireMessage::PreGameStatus(_) => "PreGameStatus",
            WireMessage::ResetPregameStatus => "ResetPregameStatus",
            WireMessage::GameSettings(_) => "GameSettings",
            WireMessage::MapPlayerIdToSlot(_) => "MapPlayerIdToSlot",
            WireMessage::PlayerSlots(_) => "PlayerSlots",
            WireMessage::GamestateRequest(_) => "GamestateRequest",
            WireMessage::GamestateResponse(_) => "GamestateResponse",
            WireMessage::GamestateChunk(_) => "GamestateChunk",
            WireMessage::GamestateChunkAck(_) => "GamestateChunkAck",
            WireMessage::Join(_) => "Join",
            WireMessage::Joined(_) => "Joined",
            WireMessage::Kicked(_) => "Kicked",
            WireMessage::LastSeen(_) => "LastSeen",
            WireMessage::LaggingClients(_) => "LaggingClients",
            WireMessage::PlayersLoading(_) => "PlayersLoading",
            WireMessage::PlayerPause(_) => "PlayerPause",
            WireMessage::LoadedGame(_) => "LoadedGame",
            WireMessage::StartSettings(_) => "StartSettings",
            WireMessage::StartSavegameSettings(_) => "StartSavegameSettings",
            WireMessage::TurnSealed(_) => "TurnSealed",
            WireMessage::StateHash(_) => "StateHash",
            WireMessage::WrongHashPlayers(_) => "WrongHashPlayers",
            WireMessage::PlayerCommand(_) => "PlayerCommand",
            WireMessage::Flare(_) => "Flare",
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let body = match self {
            Self::Syn(m) => m.to_bytes(),
            Self::SynAck(m) => m.to_bytes(),
            Self::Ack(m) => m.to_bytes(),
            Self::Authenticate(m) => m.to_bytes(),
            Self::AuthenticateResult(m) => m.to_bytes(),
            Self::Chat(m) => m.to_bytes(),
            Self::PreGameStatus(m) => m.to_bytes(),
            Self::ResetPregameStatus => vec![],
            Self::GameSettings(m) => m.to_bytes(),
            Self::MapPlayerIdToSlot(m) => m.to_bytes(),
            Self::PlayerSlots(m) => m.to_bytes(),
            Self::GamestateRequest(m) => m.to_bytes(),
            Self::GamestateResponse(m) => m.to_bytes(),
            Self::GamestateChunk(m) => m.to_bytes(),
            Self::GamestateChunkAck(m) => m.to_bytes(),
            Self::Join(m) => m.to_bytes(),
            Self::Flare(m) => m.to_bytes(),
            Self::Joined(m) => m.to_bytes(),
            Self::Kicked(m) => m.to_bytes(),
            Self::LastSeen(m) => m.to_bytes(),
            Self::LaggingClients(m) => m.to_bytes(),
            Self::PlayersLoading(m) => m.to_bytes(),
            Self::PlayerPause(m) => m.to_bytes(),
            Self::LoadedGame(m) => m.to_bytes(),
            Self::StartSettings(m) => m.to_bytes(),
            Self::StartSavegameSettings(m) => m.to_bytes(),
            Self::TurnSealed(m) => m.to_bytes(),
            Self::StateHash(m) => m.to_bytes(),
            Self::WrongHashPlayers(m) => m.to_bytes(),
            Self::PlayerCommand(m) => m.to_bytes(),
        };

        let total_size = 3 + body.len(); // header + body
        let mut bytes = vec![self.id()]; // 1 byte: message type
        bytes.extend_from_slice(&(total_size as u16).to_be_bytes()); // 2 bytes: size
        bytes.extend(body);

        // Per-message hex dumps are trace-only: this runs on every serialized
        // message and hex_dump allocates. tracing evaluates field expressions
        // only when the level is enabled, so the cost disappears otherwise.
        if !matches!(self, Self::GameSettings(_)) {
            tracing::trace!(msg_type = self.name(), dump = %hex_dump(&bytes), "message serialized");
        }

        bytes
    }

    pub fn from_bytes(buffer: &[u8]) -> Result<Self, ParseError> {
        if buffer.len() < 3 {
            return Err(ParseError::Truncated { field: "header" });
        }

        let msg_type = buffer[0];

        let size = u16::from_be_bytes([buffer[1], buffer[2]]) as usize;

        // One application message per packet, so a declared size that is not
        // the packet length means the packet is not one whole message.
        if buffer.len() != size {
            return Err(ParseError::SizeMismatch {
                declared: size,
                actual: buffer.len(),
            });
        }

        let body = &buffer[3..];

        match msg_type {
            1 => Ok(Self::Syn(Syn::from_bytes(body)?)),
            2 => Ok(Self::SynAck(SynAck::from_bytes(body)?)),
            3 => Ok(Self::Ack(Ack::from_bytes(body)?)),
            4 => Ok(Self::Authenticate(Authenticate::from_bytes(body)?)),
            5 => Ok(Self::AuthenticateResult(AuthenticateResult::from_bytes(
                body,
            )?)),
            6 => Ok(Self::Chat(Chat::from_bytes(body)?)),
            7 => Ok(Self::PreGameStatus(PreGameStatus::from_bytes(body)?)),
            8 => Ok(Self::ResetPregameStatus),
            9 => Ok(Self::GameSettings(GameSettings::from_bytes(body)?)),
            10 => Ok(Self::MapPlayerIdToSlot(MapPlayerIdToSlot::from_bytes(
                body,
            )?)),
            11 => Ok(Self::PlayerSlots(PlayerSlots::from_bytes(body)?)),
            12 => Ok(Self::GamestateRequest(GamestateRequest::from_bytes(body)?)),
            13 => Ok(Self::GamestateResponse(GamestateResponse::from_bytes(
                body,
            )?)),
            14 => Ok(Self::GamestateChunk(GamestateChunk::from_bytes(body)?)),
            15 => Ok(Self::GamestateChunkAck(GamestateChunkAck::from_bytes(
                body,
            )?)),
            16 => Ok(Self::Join(Join::from_bytes(body)?)),
            17 => Ok(Self::Joined(Joined::from_bytes(body)?)),
            18 => Ok(Self::Kicked(Kicked::from_bytes(body)?)),
            19 => Ok(Self::LastSeen(LastSeen::from_bytes(body)?)),
            20 => Ok(Self::LaggingClients(LaggingClients::from_bytes(body)?)),
            21 => Ok(Self::PlayersLoading(PlayersLoading::from_bytes(body)?)),
            22 => Ok(Self::PlayerPause(PlayerPause::from_bytes(body)?)),
            23 => Ok(Self::LoadedGame(LoadedGame::from_bytes(body)?)),
            24 => Ok(Self::StartSettings(StartSettings::from_bytes(body)?)),
            25 => Ok(Self::StartSavegameSettings(
                StartSavegameSettings::from_bytes(body)?,
            )),
            26 => Ok(Self::TurnSealed(TurnSealed::from_bytes(body)?)),
            27 => Ok(Self::StateHash(StateHash::from_bytes(body)?)),
            28 => Ok(Self::WrongHashPlayers(WrongHashPlayers::from_bytes(body)?)),
            29 => Ok(Self::PlayerCommand(PlayerCommand::from_bytes(body)?)),
            30 => Ok(Self::Flare(Flare::from_bytes(body)?)),
            _ => Err(ParseError::UnknownType(msg_type)),
        }
    }
}
