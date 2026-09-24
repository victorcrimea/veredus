// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::io::Write;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use chrono::TimeDelta;
use documented::Documented;
use documented::DocumentedFields;
use serde::Deserialize;
use serde::Serialize;
use toml_edit::Decor;
use toml_edit::DocumentMut;

use crate::lobby::LobbyConfig;
use crate::lobby::XmppCredentials;
use crate::relay::auth::LateObserverPolicy;
use crate::relay::enet_task::EnetLimits;
use crate::relay::enet_task::PEER_LIMIT;
use crate::relay::messages::EnabledMod;
use crate::relay::server_fsm::Config;
use crate::savegame::SaveSetup;

// 0x5073, the port stock clients dial unless they are told otherwise.
const DEFAULT_PORT: u16 = 20595;
// Two minutes of play at the default 200 ms turn: long enough that a run's
// fixed cost (start, map load, deserialize) stays small next to the turns it
// replays, short enough that a joiner never catches up for long.
const DEFAULT_CHECKPOINT_INTERVAL_TURNS: u32 = 600;
// How long a pooled-lobby game may sit with nobody ever having joined, or
// with everybody gone, before it shuts itself down and frees its account.
const DEFAULT_IDLE_SHUTDOWN_SECS: u64 = 60;
// Loopback by default: the endpoint names players, so exposing it further is
// a decision for the operator, not a default.
const DEFAULT_METRICS_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
const DEFAULT_METRICS_PORT: u16 = 9091;
// The message header counts its length in 16 bits, so no packet larger than
// this can hold a message; it is also the floor, since a lower cap would drop
// the biggest legitimate game settings.
const DEFAULT_ENET_MAX_PACKET_BYTES: usize = u16::MAX as usize;
// What one peer, authenticated or not, may make the relay hold in half
// reassembled or undelivered packets: a few maximum-size messages, which is
// more than a stock client ever has in flight.
const DEFAULT_ENET_MAX_WAITING_BYTES: usize = 256 * 1024;
// Relative to the working directory, so a checkout saves next to itself.
const DEFAULT_SAVE_DIR: &str = "saves";
// A crash loses at most this much of a match, and the disk sees one fsync
// per game this often.
const DEFAULT_SAVE_FLUSH_MS: u64 = 1000;
// A match that crashes the server on every resume must not crash it forever.
const DEFAULT_MAX_RESUME_ATTEMPTS: u32 = 3;

// One engine per core: a replay is CPU-bound, so running more at once than
// there are cores only makes each one finish later. Read from the machine, which means a
// generated file carries the core count of the host that generated it.
fn default_max_sidecar_runs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

// Loaded without --config only when present, so a checkout with no file still
// runs on the built-in defaults.
pub const DEFAULT_CONFIG_PATH: &str = "config.toml";

const GENERATED_HEADER: &str = "# veredus configuration. Command line flags override these values.\n\
# An empty string or 0 marks an optional setting as unset.\n\n";

// The generated file is this struct serialized, and TOML drops None, so an
// optional setting is an empty string or 0 here: otherwise the key would be
// missing from the generated file and nobody would know it exists.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub server: ServerSection,
    pub game: GameSection,
    pub lobby: LobbySection,
    pub log: LogSection,
}

/// Network, engine runs and saving.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    /// IPv4 only: the stock client cannot reach an IPv6 address, and a
    /// dual-stack socket would hand IPv4 clients over as v4-mapped IPv6.
    pub host: Ipv4Addr,
    /// Standalone mode only; a lobby game picks its own.
    pub port: u16,
    /// Empty disables the sidecar. When set, joiners no live client can
    /// serve get a snapshot from a one-shot replay instead of a drop, and
    /// AI slots are played by a headless pyrogenesis.
    pub pyrogenesis_path: PathBuf,
    /// Directory each finished match's outcome is written to, as
    /// <game_id>.json; empty only logs it. Needs pyrogenesis_path, which
    /// replays the match to work the outcome out.
    pub outcome_dir: PathBuf,
    /// Released turns between two sidecar checkpoints, each resumed from
    /// the last; joiners are served the newest one. 600 is two minutes of
    /// play at the default turn length. 0 disables them.
    pub checkpoint_interval_turns: u32,
    /// Where the Prometheus endpoint listens. Loopback by default: the
    /// endpoint names players, so exposing it further is the operator's
    /// decision, not a default.
    pub metrics_host: IpAddr,
    /// Port of the Prometheus endpoint; 0 disables it.
    pub metrics_port: u16,
    /// Standalone mode only: stop the process once its game ends instead
    /// of hosting a fresh one on the same port, for a supervisor that
    /// restarts it.
    pub exit_after_game: bool,
    /// Largest packet the host sends or reassembles. The floor is 65535:
    /// the message header counts length in 16 bits, and a lower cap would
    /// drop the biggest legitimate game settings.
    pub enet_max_packet_bytes: usize,
    /// What one peer, authenticated or not, may make the relay hold in
    /// half reassembled or undelivered packets: a few maximum-size
    /// messages, which is more than a stock client ever has in flight.
    pub enet_max_waiting_bytes: usize,
    /// How many one-shot engine runs (dumps, checkpoints, outcome
    /// replays) the whole process may have going at once; the rest queue.
    /// 0 lifts the cap. Defaults to the host core count, since a replay
    /// is CPU-bound.
    pub max_sidecar_runs: usize,
    /// Where every running match keeps its save bundle; empty turns
    /// saving off. Relative to the working directory, so a checkout saves
    /// next to itself.
    pub save_dir: PathBuf,
    /// Resume the saved matches found in save_dir at startup.
    pub resume: bool,
    /// A crash loses at most this much of a match, and the disk sees one
    /// fsync per game this often.
    pub save_flush_ms: u64,
    /// Keep a decided match's bundle instead of deleting it.
    pub keep_finished_saves: bool,
    /// A match that crashes the server on every resume must not crash it
    /// forever.
    pub max_resume_attempts: u32,
}

impl Default for ServerSection {
    fn default() -> Self {
        ServerSection {
            host: Ipv4Addr::UNSPECIFIED,
            port: DEFAULT_PORT,
            pyrogenesis_path: PathBuf::from_str("../0ad/binaries/system/pyrogenesis").unwrap(),
            outcome_dir: PathBuf::from_str("./outcome").unwrap(),
            checkpoint_interval_turns: DEFAULT_CHECKPOINT_INTERVAL_TURNS,
            metrics_host: DEFAULT_METRICS_HOST,
            metrics_port: DEFAULT_METRICS_PORT,
            exit_after_game: false,
            enet_max_packet_bytes: DEFAULT_ENET_MAX_PACKET_BYTES,
            enet_max_waiting_bytes: DEFAULT_ENET_MAX_WAITING_BYTES,
            max_sidecar_runs: default_max_sidecar_runs(),
            save_dir: PathBuf::from(DEFAULT_SAVE_DIR),
            resume: true,
            save_flush_ms: DEFAULT_SAVE_FLUSH_MS,
            keep_finished_saves: false,
            max_resume_attempts: DEFAULT_MAX_RESUME_ATTEMPTS,
        }
    }
}

impl ServerSection {
    pub fn pyrogenesis_path(&self) -> Option<PathBuf> {
        non_empty_path(&self.pyrogenesis_path)
    }

    pub fn outcome_dir(&self) -> Option<PathBuf> {
        non_empty_path(&self.outcome_dir)
    }

    pub fn save_dir(&self) -> Option<PathBuf> {
        non_empty_path(&self.save_dir)
    }

    // None when saving is off. `lobby_account` names the account hosting
    // the game, empty in standalone mode.
    pub fn save_setup(&self, lobby_account: &str) -> Option<SaveSetup> {
        Some(SaveSetup {
            root: self.save_dir()?,
            flush_interval: Duration::from_millis(self.save_flush_ms.max(1)),
            keep_finished: self.keep_finished_saves,
            lobby_account: lobby_account.to_string(),
        })
    }

    pub fn enet_limits(&self) -> EnetLimits {
        EnetLimits {
            max_packet_bytes: self.enet_max_packet_bytes,
            max_waiting_bytes: self.enet_max_waiting_bytes,
        }
    }
}

// A mirror of LateObserverPolicy, so serde stays out of the relay core.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObserverPolicy {
    Everyone,
    Buddies,
    Deny,
}

impl From<LateObserverPolicy> for ObserverPolicy {
    fn from(policy: LateObserverPolicy) -> Self {
        match policy {
            LateObserverPolicy::Everyone => ObserverPolicy::Everyone,
            LateObserverPolicy::Buddies => ObserverPolicy::Buddies,
            LateObserverPolicy::Deny => ObserverPolicy::Deny,
        }
    }
}

impl From<ObserverPolicy> for LateObserverPolicy {
    fn from(policy: ObserverPolicy) -> Self {
        match policy {
            ObserverPolicy::Everyone => LateObserverPolicy::Everyone,
            ObserverPolicy::Buddies => LateObserverPolicy::Buddies,
            ObserverPolicy::Deny => LateObserverPolicy::Deny,
        }
    }
}

// A mirror of EnabledMod, which is a wire type and carries no serde.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModEntry {
    pub name: String,
    pub version: String,
}

/// Match rules, admission and flood limits.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct GameSection {
    /// Milliseconds between released turns. At least 1: a zero-length
    /// turn would release turns as fast as the tick loop runs.
    pub turn_length_ms: u16,
    /// The name the relay speaks under. It occupies an observer row in
    /// the slot list, where a client looks a chat sender's name up.
    pub server_name: String,
    /// Sent to each arriving client; empty greets them with nothing.
    pub welcome_message: String,
    /// Empty means the first client to authenticate becomes controller,
    /// since that is the secret every stock client sends.
    pub controller_secret: String,
    /// When set, two clients may play under the same name outside lobby
    /// mode.
    pub allow_duplicate_names: bool,
    /// Who may watch a match already in progress: everyone, buddies or
    /// deny.
    pub late_observer_policy: ObserverPolicy,
    /// How many observers a match holds at once.
    pub observer_limit: usize,
    /// How far an observer may lag before it is dropped. 0 means an
    /// observer never blocks turn release.
    pub observer_lag_limit: u32,
    /// How many turns behind the players observers watch. 0 puts them on
    /// the live stream, where they may not pause.
    pub observer_delay_turns: u32,
    /// Cap the FSM enforces at admission, 2 to 200. The arriving session
    /// counts, so 1 would refuse everyone.
    pub max_sessions: usize,
    /// Losing the controller is permanent in the stock server, which
    /// strands the match with nobody able to start it; when set, someone
    /// else can take over.
    pub release_controller_on_leave: bool,
    /// How long a player may hold the match paused across the whole
    /// game. Longer than any match is how the policy is turned off.
    pub pause_budget_secs: u64,
    /// When set, the match is held for a player who drops or goes silent
    /// mid-match, charged to that player's pause budget.
    pub afk_pause: bool,
    /// How long a decided match keeps running for the players who stay
    /// to watch or chat before the game shuts down.
    pub post_game_linger_secs: u64,
    /// How long a client serializing a snapshot for a joiner may go
    /// without sending anything before the joiner is re-sourced
    /// elsewhere. 0 waits forever.
    pub join_source_stall_secs: u64,
    /// How long a connected peer may take to be admitted before it is
    /// dropped. 0 waits forever.
    pub handshake_timeout_secs: u64,
    /// How many peers from one address may be connected but not yet
    /// admitted at once. 0 means no cap.
    pub max_pending_per_ip: usize,
    /// How long the loading screen may last before whoever is still on
    /// it is dropped. Must cover a large map on a slow machine. 0 waits
    /// forever.
    pub loading_timeout_secs: u64,
    /// Per-peer chat limit: what one peer may fan out to every session.
    /// A stock client stays far below it. 0 turns that bucket off.
    pub chat_per_sec: u32,
    /// Chat messages that fit in the bucket at once; at least 1 when the
    /// rate above is set.
    pub chat_burst: u32,
    /// Longer chat is dropped.
    pub chat_max_chars: usize,
    /// Per-peer flare limit, as for chat. 0 turns that bucket off.
    pub flare_per_sec: u32,
    /// Flares that fit in the bucket at once; at least 1 when the rate
    /// above is set.
    pub flare_burst: u32,
    /// Counted per turn a command is scheduled for, because that is what
    /// the match log and every replay of it grow by. 0 turns that cap
    /// off.
    pub commands_per_turn: u32,
    /// Byte twin of the cap above. 0 turns that cap off.
    pub command_bytes_per_turn: usize,
    /// What a pause costs however soon it is lifted.
    pub pause_min_charge_secs: u64,
    /// How many times its limit a peer may send before it is disconnected
    /// rather than merely dropped. 0 never disconnects.
    pub flood_kick_multiple: u32,
    /// How many joins into a running match one address, or one lobby
    /// name, may make before it has to wait. Every join makes a player or
    /// a sidecar serialize the match.
    pub join_burst: u32,
    /// How long each join past the burst waits. 0 turns the limit off.
    pub join_interval_secs: u64,
    /// How many wrong passwords one lobby name may send before it is
    /// turned away.
    pub auth_fail_burst: u32,
    /// How many wrong passwords one address may send. More than the name
    /// above, because players behind one NAT share it and only one of
    /// them may be guessing.
    pub auth_fail_burst_per_addr: u32,
    /// How long each wrong password past the bursts waits. 0 turns the
    /// limit off.
    pub auth_fail_interval_secs: u64,
    /// How long a resumed match waits with no original player back before
    /// it is given up. 0 waits forever.
    pub resume_wait_secs: u64,
    /// How long only the saved controller may restart a resumed match;
    /// after that any original player may.
    pub resume_controller_grace_secs: u64,
    /// Without a sidecar, how often one playing client is asked for the
    /// match state so the match can be resumed; 0 turns it off.
    pub client_state_interval_secs: u64,
    /// Names that count as buddies under the buddies observer policy.
    pub buddies: Vec<String>,
    // Last: TOML refuses a plain value after an array of tables.
    /// The list admission enforces against every client, so it is
    /// provably what the game runs.
    pub enabled_mods: Vec<ModEntry>,
}

impl Default for GameSection {
    fn default() -> Self {
        // Read back from the FSM's own defaults so the two cannot drift apart.
        let config = Config::default();
        let mut buddies: Vec<String> = config.buddies.into_iter().collect();
        buddies.sort();
        GameSection {
            turn_length_ms: config.turn_length_ms,
            server_name: config.server_name,
            welcome_message: config.welcome_message,
            controller_secret: config.controller_secret,
            allow_duplicate_names: config.allow_duplicate_names,
            late_observer_policy: config.late_observer_policy.into(),
            observer_limit: config.observer_limit,
            observer_lag_limit: config.observer_lag_limit.unwrap_or(0),
            observer_delay_turns: config.observer_delay_turns,
            max_sessions: config.max_sessions,
            release_controller_on_leave: config.release_controller_on_leave,
            pause_budget_secs: config.pause_budget.num_seconds().max(0) as u64,
            afk_pause: config.afk_pause,
            post_game_linger_secs: config.post_game_linger.num_seconds().max(0) as u64,
            join_source_stall_secs: config
                .join_source_stall
                .map_or(0, |d| d.num_seconds().max(0) as u64),
            handshake_timeout_secs: config
                .handshake_timeout
                .map_or(0, |d| d.num_seconds().max(0) as u64),
            max_pending_per_ip: config.max_pending_per_ip,
            loading_timeout_secs: config
                .loading_timeout
                .map_or(0, |d| d.num_seconds().max(0) as u64),
            chat_per_sec: config.chat_per_sec,
            chat_burst: config.chat_burst,
            chat_max_chars: config.chat_max_chars,
            flare_per_sec: config.flare_per_sec,
            flare_burst: config.flare_burst,
            commands_per_turn: config.commands_per_turn,
            command_bytes_per_turn: config.command_bytes_per_turn,
            pause_min_charge_secs: config.pause_min_charge.num_seconds().max(0) as u64,
            flood_kick_multiple: config.flood_kick_multiple,
            join_burst: config.join_burst,
            join_interval_secs: config
                .join_interval
                .map_or(0, |d| d.num_seconds().max(0) as u64),
            auth_fail_burst: config.auth_fail_burst,
            auth_fail_burst_per_addr: config.auth_fail_burst_per_addr,
            auth_fail_interval_secs: config
                .auth_fail_interval
                .map_or(0, |d| d.num_seconds().max(0) as u64),
            resume_wait_secs: config
                .resume_wait
                .map_or(0, |d| d.num_seconds().max(0) as u64),
            resume_controller_grace_secs: config.resume_controller_grace.num_seconds().max(0)
                as u64,
            client_state_interval_secs: 0,
            buddies,
            enabled_mods: config
                .enabled_mods
                .into_iter()
                .map(|m| ModEntry {
                    name: m.name,
                    version: m.version,
                })
                .collect(),
        }
    }
}

impl GameSection {
    // The per-game base both modes start from; lobby mode layers its own
    // fields on top.
    pub fn server_config(&self, sidecar: bool, checkpoint_interval_turns: u32) -> Config {
        Config {
            enabled_mods: self
                .enabled_mods
                .iter()
                .map(|m| EnabledMod {
                    name: m.name.clone(),
                    version: m.version.clone(),
                })
                .collect(),
            turn_length_ms: self.turn_length_ms,
            controller_secret: self.controller_secret.clone(),
            allow_duplicate_names: self.allow_duplicate_names,
            late_observer_policy: self.late_observer_policy.into(),
            observer_limit: self.observer_limit,
            observer_lag_limit: (self.observer_lag_limit != 0).then_some(self.observer_lag_limit),
            observer_delay_turns: self.observer_delay_turns,
            buddies: self.buddies.iter().cloned().collect::<HashSet<String>>(),
            max_sessions: self.max_sessions,
            release_controller_on_leave: self.release_controller_on_leave,
            server_name: self.server_name.clone(),
            welcome_message: self.welcome_message.clone(),
            pause_budget: secs_to_delta(self.pause_budget_secs),
            afk_pause: self.afk_pause,
            post_game_linger: secs_to_delta(self.post_game_linger_secs),
            join_source_stall: (self.join_source_stall_secs != 0)
                .then(|| secs_to_delta(self.join_source_stall_secs)),
            handshake_timeout: (self.handshake_timeout_secs != 0)
                .then(|| secs_to_delta(self.handshake_timeout_secs)),
            max_pending_per_ip: self.max_pending_per_ip,
            loading_timeout: (self.loading_timeout_secs != 0)
                .then(|| secs_to_delta(self.loading_timeout_secs)),
            chat_per_sec: self.chat_per_sec,
            chat_burst: self.chat_burst,
            chat_max_chars: self.chat_max_chars,
            flare_per_sec: self.flare_per_sec,
            flare_burst: self.flare_burst,
            commands_per_turn: self.commands_per_turn,
            command_bytes_per_turn: self.command_bytes_per_turn,
            pause_min_charge: secs_to_delta(self.pause_min_charge_secs),
            flood_kick_multiple: self.flood_kick_multiple,
            join_burst: self.join_burst,
            join_interval: (self.join_interval_secs != 0)
                .then(|| secs_to_delta(self.join_interval_secs)),
            auth_fail_burst: self.auth_fail_burst,
            auth_fail_burst_per_addr: self.auth_fail_burst_per_addr,
            auth_fail_interval: (self.auth_fail_interval_secs != 0)
                .then(|| secs_to_delta(self.auth_fail_interval_secs)),
            resume_wait: (self.resume_wait_secs != 0).then(|| secs_to_delta(self.resume_wait_secs)),
            resume_controller_grace: secs_to_delta(self.resume_controller_grace_secs),
            client_state_interval_turns: client_state_interval_turns(
                sidecar,
                self.client_state_interval_secs,
                self.turn_length_ms,
            ),
            sidecar_dumps: sidecar,
            hosted_ai: sidecar,
            checkpoint_interval_turns,
            ..Config::default()
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountEntry {
    pub jid: String,
    pub password: String,
}

/// Pool-lobby mode: accounts that wait for hostme.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct LobbySection {
    /// Set to run pool-lobby mode off this table.
    pub enabled: bool,
    /// A lobby run with this empty could never be listed.
    pub muc_room: String,
    /// A lobby run with this empty could never log in.
    pub bot_jid: String,
    /// The address players connect to. A lobby run with this empty could
    /// never be listed.
    pub public_ip: String,
    /// Shown in the lobby game list.
    pub server_name: String,
    /// Must match the clients.
    pub engine_version: String,
    /// Optional password set on every lobby game.
    pub game_password: String,
    /// How long a pooled-lobby game may sit with nobody ever having
    /// joined, or with everybody gone, before it shuts itself down and
    /// frees its account. 0 means never.
    pub idle_shutdown_secs: u64,
    // Last: TOML refuses a plain value after an array of tables.
    /// One-shot XMPP accounts waiting in the room; each hosts one game at
    /// a time. Keep this file private: it holds account passwords.
    pub accounts: Vec<AccountEntry>,
}

impl Default for LobbySection {
    fn default() -> Self {
        LobbySection {
            enabled: false,
            muc_room: String::new(),
            bot_jid: String::new(),
            public_ip: String::new(),
            server_name: crate::lobby::default_server_name(),
            engine_version: crate::lobby::default_engine_version(),
            game_password: String::new(),
            idle_shutdown_secs: DEFAULT_IDLE_SHUTDOWN_SECS,
            accounts: Vec::new(),
        }
    }
}

impl LobbySection {
    // 0 means never, as for every other duration here; a zero timeout would
    // close each game on its first tick, before anyone could join it.
    pub fn idle_shutdown(&self) -> Option<TimeDelta> {
        (self.idle_shutdown_secs != 0).then(|| secs_to_delta(self.idle_shutdown_secs))
    }

    // The fields default to empty only so the generated file can show them;
    // a lobby run with any of them empty could never log in or be listed.
    pub fn to_lobby_config(&self) -> Result<LobbyConfig, String> {
        if self.accounts.is_empty() {
            return Err("[lobby] is enabled but lists no accounts".to_string());
        }
        for (key, value) in [
            ("muc_room", &self.muc_room),
            ("bot_jid", &self.bot_jid),
            ("public_ip", &self.public_ip),
        ] {
            if value.is_empty() {
                return Err(format!("[lobby] is enabled but {key} is empty"));
            }
        }
        Ok(LobbyConfig {
            accounts: self
                .accounts
                .iter()
                .map(|a| XmppCredentials {
                    jid: a.jid.clone(),
                    password: a.password.clone(),
                })
                .collect(),
            muc_room: self.muc_room.clone(),
            bot_jid: self.bot_jid.clone(),
            public_ip: self.public_ip.clone(),
            server_name: self.server_name.clone(),
            engine_version: self.engine_version.clone(),
            game_password: self.game_password.clone(),
        })
    }
}

// Every field here loses to its environment variable, which is how a single
// debugging session turns logging up without editing the file.
/// Every field loses to its environment variable.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct LogSection {
    /// Tracing directives for stdout; RUST_LOG wins over this.
    pub directives: String,
    /// Tracing directives for the Loki sink; LOKI_LOG wins over this.
    pub loki_directives: String,
    /// The Loki sink exists only when this (or LOKI_URL) is set.
    pub loki_url: String,
    /// Stream label; LOKI_INSTANCE wins over this.
    pub loki_instance: String,
    /// Stream label; LOKI_ENV wins over this.
    pub loki_env: String,
}

impl Default for LogSection {
    fn default() -> Self {
        LogSection {
            directives: String::new(),
            loki_directives: String::new(),
            loki_url: String::new(),
            loki_instance: String::new(),
            loki_env: "dev".to_string(),
        }
    }
}

impl FileConfig {
    // `required` is set when the path was named on the command line: a file
    // the operator asked for must not be skipped silently.
    pub fn load(path: &Path, required: bool) -> Result<FileConfig, String> {
        let data = match std::fs::read_to_string(path) {
            Ok(data) => data,
            Err(error) if !required && error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(FileConfig::default());
            }
            Err(error) => {
                return Err(format!(
                    "failed to read config '{}': {error}",
                    path.display()
                ));
            }
        };
        toml::from_str(&data)
            .map_err(|error| format!("failed to parse config '{}': {error}", path.display()))
    }

    // Run on the merged result, so a command line flag cannot slip a value
    // past it either.
    pub fn validate(&self) -> Result<(), String> {
        // A zero-length turn would release turns as fast as the tick loop
        // runs, and the clients would simulate none of them in real time.
        if self.game.turn_length_ms == 0 {
            return Err("[game] turn_length_ms must be at least 1".to_string());
        }
        // The arriving session counts against the cap, so 1 refuses everyone,
        // and above the ENet peer limit no peer is left over to tell a client
        // the server is full.
        if !(2..=PEER_LIMIT).contains(&self.game.max_sessions) {
            return Err(format!(
                "[game] max_sessions must be between 2 and {PEER_LIMIT}, got {}",
                self.game.max_sessions
            ));
        }
        // A bucket that holds nothing drops every message, which is not what
        // a rate that is switched on means.
        if self.game.chat_per_sec != 0 && self.game.chat_burst == 0 {
            return Err(
                "[game] chat_burst must be at least 1 when chat_per_sec is set".to_string(),
            );
        }
        if self.game.join_interval_secs != 0 && self.game.join_burst == 0 {
            return Err(
                "[game] join_burst must be at least 1 when join_interval_secs is set".to_string(),
            );
        }
        if self.game.auth_fail_interval_secs != 0
            && (self.game.auth_fail_burst == 0 || self.game.auth_fail_burst_per_addr == 0)
        {
            return Err(
                "[game] auth_fail_burst and auth_fail_burst_per_addr must be at least 1 \
                 when auth_fail_interval_secs is set"
                    .to_string(),
            );
        }
        if self.game.flare_per_sec != 0 && self.game.flare_burst == 0 {
            return Err(
                "[game] flare_burst must be at least 1 when flare_per_sec is set".to_string(),
            );
        }
        if self.server.enet_max_packet_bytes < DEFAULT_ENET_MAX_PACKET_BYTES {
            return Err(format!(
                "[server] enet_max_packet_bytes must be at least {DEFAULT_ENET_MAX_PACKET_BYTES}, got {}",
                self.server.enet_max_packet_bytes
            ));
        }
        if self.server.enet_max_waiting_bytes == 0 {
            return Err("[server] enet_max_waiting_bytes must be at least 1".to_string());
        }
        Ok(())
    }

    pub fn write_default(path: &Path) -> Result<(), String> {
        let body = toml::to_string_pretty(&FileConfig::default())
            .map_err(|error| format!("failed to serialize the default config: {error}"))?;
        // Serializing drops every comment, so each section's doc comments are
        // attached afterwards; values and prose then share one source each.
        let mut doc: DocumentMut = body
            .parse()
            .map_err(|error| format!("failed to parse the default config: {error}"))?;
        decorate_default(&mut doc);
        // create_new, so a regenerate never wipes a tuned config.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("failed to create '{}': {error}", path.display()))?;
        file.write_all(GENERATED_HEADER.as_bytes())
            .and_then(|()| file.write_all(doc.to_string().as_bytes()))
            .map_err(|error| format!("failed to write '{}': {error}", path.display()))
    }
}

// The operator reads a section's doc comments as the generated file's
// comments, so prose written for rustdoc is never retyped for TOML.
fn decorate_default(doc: &mut DocumentMut) {
    decorate_section::<ServerSection>(doc, "server");
    decorate_section::<GameSection>(doc, "game");
    decorate_section::<LobbySection>(doc, "lobby");
    decorate_section::<LogSection>(doc, "log");
}

fn decorate_section<T: Documented + DocumentedFields>(doc: &mut DocumentMut, table: &str) {
    set_table_comment(doc, table, T::DOCS);
    for (key, comment) in T::FIELD_NAMES.iter().zip(T::FIELD_DOCS) {
        set_key_comment(doc, table, key, comment);
    }
}

// A comment above a [table] header. The blank line between tables lives in
// the old prefix, so it is kept, or the tables would run together.
fn set_table_comment(doc: &mut DocumentMut, table: &str, comment: &str) {
    let Some(item) = doc.get_mut(table) else {
        return;
    };
    let Some(as_table) = item.as_table_mut() else {
        return;
    };
    prepend_comment(as_table.decor_mut(), comment);
}

// A comment above one key. Arrays of tables carry no key decor of their own:
// a prefix there would land inside the brackets, so the first entry's table
// decor holds it instead.
fn set_key_comment(doc: &mut DocumentMut, table: &str, key: &str, comment: &str) {
    let Some(item) = doc.get_mut(table) else {
        return;
    };
    let Some(as_table) = item.as_table_mut() else {
        return;
    };
    let Some(child) = as_table.get_mut(key) else {
        return;
    };
    if let Some(first) = child
        .as_array_of_tables_mut()
        .and_then(|aot| aot.get_mut(0))
    {
        prepend_comment(first.decor_mut(), comment);
        return;
    }
    if let Some(child_table) = child.as_table_mut() {
        prepend_comment(child_table.decor_mut(), comment);
        return;
    }
    let Some(mut key_handle) = as_table.key_mut(key) else {
        return;
    };
    set_key_prefix(key_handle.leaf_decor_mut(), comment);
}

fn prepend_comment(decor: &mut Decor, comment: &str) {
    let old = decor
        .prefix()
        .and_then(|raw| raw.as_str())
        .unwrap_or_default();
    let gap = if old.starts_with('\n') { "\n" } else { "" };
    decor.set_prefix(format!("{gap}{}", comment_text(comment)));
}

fn set_key_prefix(decor: &mut Decor, comment: &str) {
    decor.set_prefix(comment_text(comment));
}

fn comment_text(comment: &str) -> String {
    let mut out = String::new();
    for line in comment.lines() {
        out.push_str("# ");
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

// Operators think in time, the match counts in turns. At least one turn, so
// a short interval never means "every input". A sidecar makes it moot.
fn client_state_interval_turns(sidecar: bool, secs: u64, turn_length_ms: u16) -> u32 {
    if sidecar || secs == 0 || turn_length_ms == 0 {
        return 0;
    }
    let turns = secs.saturating_mul(1000) / u64::from(turn_length_ms);
    u32::try_from(turns).unwrap_or(u32::MAX).max(1)
}

fn non_empty_path(value: &Path) -> Option<PathBuf> {
    (!value.as_os_str().is_empty()).then(|| value.to_path_buf())
}

fn secs_to_delta(secs: u64) -> TimeDelta {
    // A budget longer than any match is how an operator turns a limit off, so
    // an out-of-range value saturates rather than failing the load.
    i64::try_from(secs)
        .ok()
        .and_then(TimeDelta::try_seconds)
        .unwrap_or(TimeDelta::MAX)
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
