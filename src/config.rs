// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashSet;
use std::io::Write;
use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::path::Path;
use std::path::PathBuf;

use chrono::TimeDelta;
use serde::Deserialize;
use serde::Serialize;

use crate::lobby::LobbyConfig;
use crate::lobby::XmppCredentials;
use crate::relay::auth::LateObserverPolicy;
use crate::relay::enet_task::PEER_LIMIT;
use crate::relay::messages::EnabledMod;
use crate::relay::server_fsm::Config;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerSection {
    pub host: IpAddr,
    // Standalone mode only; a lobby game picks its own.
    pub port: u16,
    pub pyrogenesis_path: PathBuf,
    pub outcome_dir: PathBuf,
    pub checkpoint_interval_turns: u32,
    // Where the Prometheus endpoint listens; port 0 turns it off.
    pub metrics_host: IpAddr,
    pub metrics_port: u16,
    // Standalone mode only: stop the process once its game ends instead of
    // hosting a fresh one on the same port, for a supervisor that restarts it.
    pub exit_after_game: bool,
}

impl Default for ServerSection {
    fn default() -> Self {
        ServerSection {
            host: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: DEFAULT_PORT,
            pyrogenesis_path: PathBuf::new(),
            outcome_dir: PathBuf::new(),
            checkpoint_interval_turns: DEFAULT_CHECKPOINT_INTERVAL_TURNS,
            metrics_host: DEFAULT_METRICS_HOST,
            metrics_port: DEFAULT_METRICS_PORT,
            exit_after_game: false,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct GameSection {
    pub turn_length_ms: u16,
    pub server_name: String,
    pub welcome_message: String,
    pub controller_secret: String,
    pub allow_duplicate_names: bool,
    pub late_observer_policy: ObserverPolicy,
    pub observer_limit: usize,
    pub observer_lag_limit: u32,
    pub observer_delay_turns: u32,
    pub max_sessions: usize,
    pub release_controller_on_leave: bool,
    pub pause_budget_secs: u64,
    pub afk_pause: bool,
    pub post_game_linger_secs: u64,
    pub join_source_stall_secs: u64,
    pub handshake_timeout_secs: u64,
    pub max_pending_per_ip: usize,
    pub loading_timeout_secs: u64,
    pub buddies: Vec<String>,
    // Last: TOML refuses a plain value after an array of tables.
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LobbySection {
    pub enabled: bool,
    pub muc_room: String,
    pub bot_jid: String,
    pub public_ip: String,
    pub server_name: String,
    pub engine_version: String,
    pub game_password: String,
    pub idle_shutdown_secs: u64,
    // Last: TOML refuses a plain value after an array of tables.
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
    pub fn idle_shutdown(&self) -> TimeDelta {
        secs_to_delta(self.idle_shutdown_secs)
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
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LogSection {
    pub directives: String,
    pub loki_directives: String,
    pub loki_url: String,
    pub loki_instance: String,
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
        Ok(())
    }

    pub fn write_default(path: &Path) -> Result<(), String> {
        let body = toml::to_string_pretty(&FileConfig::default())
            .map_err(|error| format!("failed to serialize the default config: {error}"))?;
        // create_new, so a regenerate never wipes a tuned config.
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .map_err(|error| format!("failed to create '{}': {error}", path.display()))?;
        file.write_all(GENERATED_HEADER.as_bytes())
            .and_then(|()| file.write_all(body.as_bytes()))
            .map_err(|error| format!("failed to write '{}': {error}", path.display()))
    }
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
