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
# An empty string or 0 marks an optional setting as unset.\n\
# Tables run from the settings most servers change to the ones almost none do.\n\n";

// The generated file is this struct serialized, and TOML drops None, so an
// optional setting is an empty string or 0 here: otherwise the key would be
// missing from the generated file and nobody would know it exists.
// Field order is the generated file's table order: the likeliest to be
// edited first, so an operator reads what matters before what does not.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FileConfig {
    pub server: ServerSection,
    pub lobby: LobbySection,
    #[serde(rename = "match")]
    pub game_match: MatchSection,
    pub observers: ObserversSection,
    pub pause: PauseSection,
    pub saves: SavesSection,
    pub metrics: MetricsSection,
    pub log: LogSection,
    pub sidecar: SidecarSection,
    pub limits: LimitsSection,
    pub timeouts: TimeoutsSection,
    pub advanced: AdvancedSection,
}

/// Where the server listens and which engine it runs.
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
    /// Standalone mode only: stop the process once its game ends instead
    /// of hosting a fresh one on the same port, for a supervisor that
    /// restarts it.
    pub exit_after_game: bool,
}

impl Default for ServerSection {
    fn default() -> Self {
        ServerSection {
            host: Ipv4Addr::UNSPECIFIED,
            port: DEFAULT_PORT,
            pyrogenesis_path: PathBuf::from_str("../0ad/binaries/system/pyrogenesis").unwrap(),
            exit_after_game: false,
        }
    }
}

impl ServerSection {
    pub fn pyrogenesis_path(&self) -> Option<PathBuf> {
        non_empty_path(&self.pyrogenesis_path)
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
    /// The address players connect to. A lobby run with this empty could
    /// never be listed.
    pub public_ip: String,
    /// Shown in the lobby game list.
    pub server_name: String,
    /// Optional password set on every lobby game.
    pub game_password: String,
    /// How long a pooled-lobby game may sit with nobody ever having
    /// joined, or with everybody gone, before it shuts itself down and
    /// frees its account. 0 means never.
    pub idle_shutdown_secs: u64,
    /// A lobby run with this empty could never be listed.
    pub muc_room: String,
    /// A lobby run with this empty could never log in.
    pub bot_jid: String,
    /// Must match the clients.
    pub engine_version: String,
    // Last: TOML refuses a plain value after an array of tables.
    /// One-shot XMPP accounts waiting in the room; each hosts one game at
    /// a time. Replace these examples with your own. Keep this file
    /// private: it holds account passwords.
    pub accounts: Vec<AccountEntry>,
}

impl Default for LobbySection {
    fn default() -> Self {
        LobbySection {
            enabled: false,
            public_ip: String::new(),
            server_name: crate::lobby::default_server_name(),
            game_password: String::new(),
            idle_shutdown_secs: DEFAULT_IDLE_SHUTDOWN_SECS,
            muc_room: String::new(),
            bot_jid: String::new(),
            engine_version: crate::lobby::default_engine_version(),
            accounts: Vec::new(),
        }
    }
}

impl LobbySection {
    // The generated file is the only place an operator sees how accounts are
    // written, but Default must stay empty: serde fills every key a file
    // leaves out from it, and placeholder accounts would then log in.
    fn example() -> Self {
        LobbySection {
            muc_room: "arena28@conference.lobby.wildfiregames.com".to_string(),
            bot_jid: "wfgbot28@lobby.wildfiregames.com/CC".to_string(),
            accounts: ["myserver1", "myserver2"]
                .into_iter()
                .map(|name| AccountEntry {
                    jid: format!("{name}@lobby.wildfiregames.com"),
                    password: String::new(),
                })
                .collect(),
            ..LobbySection::default()
        }
    }

    // 0 means never, as for every other duration here; a zero timeout would
    // close each game on its first tick, before anyone could join it.
    pub fn idle_shutdown(&self) -> Option<TimeDelta> {
        secs_to_opt_delta(self.idle_shutdown_secs)
    }

    // The fields default to empty only so the generated file can show them;
    // a lobby run with any of them empty could never log in or be listed.
    pub fn to_lobby_config(&self) -> Result<LobbyConfig, String> {
        if self.accounts.is_empty() {
            return Err("[lobby] is enabled but lists no accounts".to_string());
        }
        // Catches the generated file's example accounts left in place.
        if let Some(account) = self.accounts.iter().find(|a| a.password.is_empty()) {
            return Err(format!("[lobby] account '{}' has no password", account.jid));
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

/// What players see and who runs a match.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct MatchSection {
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
    /// How long a decided match keeps running for the players who stay
    /// to watch or chat before the game shuts down.
    pub post_game_linger_secs: u64,
}

impl Default for MatchSection {
    fn default() -> Self {
        // Read back from the FSM's own defaults so the two cannot drift apart.
        let config = Config::default();
        MatchSection {
            server_name: config.server_name,
            welcome_message: config.welcome_message,
            controller_secret: config.controller_secret,
            allow_duplicate_names: config.allow_duplicate_names,
            post_game_linger_secs: delta_to_secs(config.post_game_linger),
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

/// Who may watch a match, and how far behind the players.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct ObserversSection {
    /// Who may watch a match already in progress: everyone, buddies or
    /// deny.
    pub policy: ObserverPolicy,
    /// How many observers a match holds at once.
    pub limit: usize,
    /// How many turns behind the players observers watch. 0 puts them on
    /// the live stream, where they may not pause.
    pub delay_turns: u32,
    /// Names that count as buddies under the buddies policy.
    pub buddies: Vec<String>,
}

impl Default for ObserversSection {
    fn default() -> Self {
        let config = Config::default();
        let mut buddies: Vec<String> = config.buddies.into_iter().collect();
        buddies.sort();
        ObserversSection {
            policy: config.late_observer_policy.into(),
            limit: config.observer_limit,
            delay_turns: config.observer_delay_turns,
            buddies,
        }
    }
}

/// How long players may hold a match paused.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct PauseSection {
    /// How long a player may hold the match paused across the whole
    /// game. Longer than any match is how the policy is turned off.
    pub budget_secs: u64,
    /// When set, the match is held for a player who drops or goes silent
    /// mid-match, charged to that player's pause budget.
    pub afk: bool,
    /// What a pause costs however soon it is lifted.
    pub min_charge_secs: u64,
}

impl Default for PauseSection {
    fn default() -> Self {
        let config = Config::default();
        PauseSection {
            budget_secs: delta_to_secs(config.pause_budget),
            afk: config.afk_pause,
            min_charge_secs: delta_to_secs(config.pause_min_charge),
        }
    }
}

/// Saving running matches so they survive a restart, and recording how
/// they ended.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct SavesSection {
    /// Where every running match keeps its save bundle; empty turns
    /// saving off. Relative to the working directory, so a checkout saves
    /// next to itself.
    pub dir: PathBuf,
    /// Resume the saved matches found in dir at startup.
    pub resume: bool,
    /// Keep a decided match's bundle instead of deleting it.
    pub keep_finished: bool,
    /// Directory each finished match's outcome is written to, as
    /// <game_id>.json; empty only logs it. Needs [server]
    /// pyrogenesis_path, which replays the match to work the outcome out.
    pub outcome_dir: PathBuf,
    /// Without a sidecar, how often one playing client is asked for the
    /// match state, which pauses the game for a moment. The copy lets the
    /// match be resumed and is served to rejoining players and observers,
    /// so they do not stall a player each time; 0 turns it off.
    pub client_state_interval_secs: u64,
}

impl Default for SavesSection {
    fn default() -> Self {
        SavesSection {
            dir: PathBuf::from(DEFAULT_SAVE_DIR),
            resume: true,
            keep_finished: false,
            outcome_dir: PathBuf::from_str("./outcome").unwrap(),
            client_state_interval_secs: 120,
        }
    }
}

impl SavesSection {
    pub fn outcome_dir(&self) -> Option<PathBuf> {
        non_empty_path(&self.outcome_dir)
    }

    pub fn dir(&self) -> Option<PathBuf> {
        non_empty_path(&self.dir)
    }
}

/// The Prometheus endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsSection {
    /// Where the endpoint listens. Loopback by default: the endpoint
    /// names players, so exposing it further is the operator's decision,
    /// not a default.
    pub host: IpAddr,
    /// Port of the endpoint; 0 disables it.
    pub port: u16,
}

impl Default for MetricsSection {
    fn default() -> Self {
        MetricsSection {
            host: DEFAULT_METRICS_HOST,
            port: DEFAULT_METRICS_PORT,
        }
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

/// Headless engine runs. Unused while [server] pyrogenesis_path is empty.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct SidecarSection {
    /// How many one-shot engine runs (dumps, checkpoints, outcome
    /// replays) the whole process may have going at once; the rest queue.
    /// 0 lifts the cap. Defaults to the host core count, since a replay
    /// is CPU-bound.
    pub max_runs: usize,
    /// Released turns between two checkpoints, each resumed from the
    /// last; joiners are served the newest one. 600 is two minutes of
    /// play at the default turn length. 0 disables them.
    pub checkpoint_interval_turns: u32,
    /// With hosted AI, how many turns apart the AI players' own state is
    /// saved, so a crashed AI host can be brought back; each save pauses
    /// the match briefly. 0 uses the checkpoint interval.
    pub ai_state_interval_turns: u32,
    /// How many times a match tries to bring its AI players back after
    /// their process is lost, before it is saved for a restart.
    pub ai_heal_attempts: u32,
    /// How long each of those tries may take to catch up with the match.
    /// 0 waits forever.
    pub ai_heal_timeout_secs: u64,
}

impl Default for SidecarSection {
    fn default() -> Self {
        let config = Config::default();
        SidecarSection {
            max_runs: default_max_sidecar_runs(),
            checkpoint_interval_turns: DEFAULT_CHECKPOINT_INTERVAL_TURNS,
            ai_state_interval_turns: 0,
            ai_heal_attempts: config.ai_heal_attempts,
            ai_heal_timeout_secs: opt_delta_to_secs(config.ai_heal_timeout),
        }
    }
}

/// Per-peer flood and abuse limits. A stock client stays far below all
/// of them.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct LimitsSection {
    /// Per-peer chat limit: what one peer may fan out to every session.
    /// 0 turns that bucket off.
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
    /// How many peers from one address may be connected but not yet
    /// admitted at once. 0 means no cap.
    pub max_pending_per_ip: usize,
}

impl Default for LimitsSection {
    fn default() -> Self {
        let config = Config::default();
        LimitsSection {
            chat_per_sec: config.chat_per_sec,
            chat_burst: config.chat_burst,
            chat_max_chars: config.chat_max_chars,
            flare_per_sec: config.flare_per_sec,
            flare_burst: config.flare_burst,
            commands_per_turn: config.commands_per_turn,
            command_bytes_per_turn: config.command_bytes_per_turn,
            flood_kick_multiple: config.flood_kick_multiple,
            join_burst: config.join_burst,
            join_interval_secs: opt_delta_to_secs(config.join_interval),
            auth_fail_burst: config.auth_fail_burst,
            auth_fail_burst_per_addr: config.auth_fail_burst_per_addr,
            auth_fail_interval_secs: opt_delta_to_secs(config.auth_fail_interval),
            max_pending_per_ip: config.max_pending_per_ip,
        }
    }
}

/// How long the server waits on slow or absent clients.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct TimeoutsSection {
    /// How long a connected peer may take to be admitted before it is
    /// dropped. 0 waits forever.
    pub handshake_secs: u64,
    /// How long the loading screen may last before whoever is still on
    /// it is dropped. Must cover a large map on a slow machine. 0 waits
    /// forever.
    pub loading_secs: u64,
    /// How long a client serializing a snapshot for a joiner may go
    /// without sending anything before the joiner is re-sourced
    /// elsewhere. 0 waits forever.
    pub join_source_stall_secs: u64,
    /// How long a resumed match waits with no original player back before
    /// it is given up. 0 waits forever.
    pub resume_wait_secs: u64,
    /// How long only the saved controller may restart a resumed match;
    /// after that any original player may.
    pub resume_controller_grace_secs: u64,
}

impl Default for TimeoutsSection {
    fn default() -> Self {
        let config = Config::default();
        TimeoutsSection {
            handshake_secs: opt_delta_to_secs(config.handshake_timeout),
            loading_secs: opt_delta_to_secs(config.loading_timeout),
            join_source_stall_secs: opt_delta_to_secs(config.join_source_stall),
            resume_wait_secs: opt_delta_to_secs(config.resume_wait),
            resume_controller_grace_secs: delta_to_secs(config.resume_controller_grace),
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

/// Protocol and resource internals. The defaults suit nearly every
/// server.
#[derive(Debug, Clone, Serialize, Deserialize, Documented, DocumentedFields)]
#[serde(default, deny_unknown_fields)]
pub struct AdvancedSection {
    /// Milliseconds between released turns. At least 1: a zero-length
    /// turn would release turns as fast as the tick loop runs.
    pub turn_length_ms: u16,
    /// Cap the FSM enforces at admission, 2 to 200. The arriving session
    /// counts, so 1 would refuse everyone.
    pub max_sessions: usize,
    /// Losing the controller is permanent in the stock server, which
    /// strands the match with nobody able to start it; when set, someone
    /// else can take over.
    pub release_controller_on_leave: bool,
    /// How far an observer may lag before it is dropped. 0 means an
    /// observer never blocks turn release.
    pub observer_lag_limit: u32,
    /// Largest packet the host sends or reassembles. The floor is 65535:
    /// the message header counts length in 16 bits, and a lower cap would
    /// drop the biggest legitimate game settings.
    pub enet_max_packet_bytes: usize,
    /// What one peer, authenticated or not, may make the relay hold in
    /// half reassembled or undelivered packets: a few maximum-size
    /// messages, which is more than a stock client ever has in flight.
    pub enet_max_waiting_bytes: usize,
    /// A crash loses at most this much of a match, and the disk sees one
    /// fsync per game this often.
    pub save_flush_ms: u64,
    /// A match that crashes the server on every resume must not crash it
    /// forever.
    pub max_resume_attempts: u32,
    // Last: TOML refuses a plain value after an array of tables.
    /// The list admission enforces against every client, so it is
    /// provably what the game runs.
    pub enabled_mods: Vec<ModEntry>,
}

impl Default for AdvancedSection {
    fn default() -> Self {
        let config = Config::default();
        AdvancedSection {
            turn_length_ms: config.turn_length_ms,
            max_sessions: config.max_sessions,
            release_controller_on_leave: config.release_controller_on_leave,
            observer_lag_limit: config.observer_lag_limit.unwrap_or(0),
            enet_max_packet_bytes: DEFAULT_ENET_MAX_PACKET_BYTES,
            enet_max_waiting_bytes: DEFAULT_ENET_MAX_WAITING_BYTES,
            save_flush_ms: DEFAULT_SAVE_FLUSH_MS,
            max_resume_attempts: DEFAULT_MAX_RESUME_ATTEMPTS,
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

impl AdvancedSection {
    pub fn enet_limits(&self) -> EnetLimits {
        EnetLimits {
            max_packet_bytes: self.enet_max_packet_bytes,
            max_waiting_bytes: self.enet_max_waiting_bytes,
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

    // None when saving is off. `lobby_account` names the account hosting
    // the game, empty in standalone mode.
    pub fn save_setup(&self, lobby_account: &str) -> Option<SaveSetup> {
        Some(SaveSetup {
            root: self.saves.dir()?,
            flush_interval: Duration::from_millis(self.advanced.save_flush_ms.max(1)),
            keep_finished: self.saves.keep_finished,
            lobby_account: lobby_account.to_string(),
        })
    }

    // The per-game base both modes start from; lobby mode layers its own
    // fields on top.
    pub fn server_config(&self, sidecar: bool) -> Config {
        let game_match = &self.game_match;
        let observers = &self.observers;
        let pause = &self.pause;
        let limits = &self.limits;
        let timeouts = &self.timeouts;
        let advanced = &self.advanced;
        let checkpoint_interval_turns = self.sidecar.checkpoint_interval_turns;
        Config {
            enabled_mods: advanced
                .enabled_mods
                .iter()
                .map(|m| EnabledMod {
                    name: m.name.clone(),
                    version: m.version.clone(),
                })
                .collect(),
            turn_length_ms: advanced.turn_length_ms,
            controller_secret: game_match.controller_secret.clone(),
            allow_duplicate_names: game_match.allow_duplicate_names,
            late_observer_policy: observers.policy.into(),
            observer_limit: observers.limit,
            observer_lag_limit: (advanced.observer_lag_limit != 0)
                .then_some(advanced.observer_lag_limit),
            observer_delay_turns: observers.delay_turns,
            buddies: observers
                .buddies
                .iter()
                .cloned()
                .collect::<HashSet<String>>(),
            max_sessions: advanced.max_sessions,
            release_controller_on_leave: advanced.release_controller_on_leave,
            server_name: game_match.server_name.clone(),
            welcome_message: game_match.welcome_message.clone(),
            pause_budget: secs_to_delta(pause.budget_secs),
            afk_pause: pause.afk,
            post_game_linger: secs_to_delta(game_match.post_game_linger_secs),
            join_source_stall: secs_to_opt_delta(timeouts.join_source_stall_secs),
            handshake_timeout: secs_to_opt_delta(timeouts.handshake_secs),
            max_pending_per_ip: limits.max_pending_per_ip,
            loading_timeout: secs_to_opt_delta(timeouts.loading_secs),
            chat_per_sec: limits.chat_per_sec,
            chat_burst: limits.chat_burst,
            chat_max_chars: limits.chat_max_chars,
            flare_per_sec: limits.flare_per_sec,
            flare_burst: limits.flare_burst,
            commands_per_turn: limits.commands_per_turn,
            command_bytes_per_turn: limits.command_bytes_per_turn,
            pause_min_charge: secs_to_delta(pause.min_charge_secs),
            flood_kick_multiple: limits.flood_kick_multiple,
            join_burst: limits.join_burst,
            join_interval: secs_to_opt_delta(limits.join_interval_secs),
            auth_fail_burst: limits.auth_fail_burst,
            auth_fail_burst_per_addr: limits.auth_fail_burst_per_addr,
            auth_fail_interval: secs_to_opt_delta(limits.auth_fail_interval_secs),
            resume_wait: secs_to_opt_delta(timeouts.resume_wait_secs),
            resume_controller_grace: secs_to_delta(timeouts.resume_controller_grace_secs),
            client_state_interval_turns: client_state_interval_turns(
                sidecar,
                self.saves.client_state_interval_secs,
                advanced.turn_length_ms,
            ),
            ai_state_interval_turns: ai_state_interval_turns(
                self.sidecar.ai_state_interval_turns,
                checkpoint_interval_turns,
            ),
            ai_heal_attempts: self.sidecar.ai_heal_attempts,
            ai_heal_timeout: secs_to_opt_delta(self.sidecar.ai_heal_timeout_secs),
            sidecar_dumps: sidecar,
            hosted_ai: sidecar,
            checkpoint_interval_turns,
            ..Config::default()
        }
    }

    // Run on the merged result, so a command line flag cannot slip a value
    // past it either.
    pub fn validate(&self) -> Result<(), String> {
        // A zero-length turn would release turns as fast as the tick loop
        // runs, and the clients would simulate none of them in real time.
        if self.advanced.turn_length_ms == 0 {
            return Err("[advanced] turn_length_ms must be at least 1".to_string());
        }
        // The arriving session counts against the cap, so 1 refuses everyone,
        // and above the ENet peer limit no peer is left over to tell a client
        // the server is full.
        if !(2..=PEER_LIMIT).contains(&self.advanced.max_sessions) {
            return Err(format!(
                "[advanced] max_sessions must be between 2 and {PEER_LIMIT}, got {}",
                self.advanced.max_sessions
            ));
        }
        // A bucket that holds nothing drops every message, which is not what
        // a rate that is switched on means.
        if self.limits.chat_per_sec != 0 && self.limits.chat_burst == 0 {
            return Err(
                "[limits] chat_burst must be at least 1 when chat_per_sec is set".to_string(),
            );
        }
        if self.limits.join_interval_secs != 0 && self.limits.join_burst == 0 {
            return Err(
                "[limits] join_burst must be at least 1 when join_interval_secs is set".to_string(),
            );
        }
        if self.limits.auth_fail_interval_secs != 0
            && (self.limits.auth_fail_burst == 0 || self.limits.auth_fail_burst_per_addr == 0)
        {
            return Err(
                "[limits] auth_fail_burst and auth_fail_burst_per_addr must be at least 1 \
                 when auth_fail_interval_secs is set"
                    .to_string(),
            );
        }
        if self.limits.flare_per_sec != 0 && self.limits.flare_burst == 0 {
            return Err(
                "[limits] flare_burst must be at least 1 when flare_per_sec is set".to_string(),
            );
        }
        if self.advanced.enet_max_packet_bytes < DEFAULT_ENET_MAX_PACKET_BYTES {
            return Err(format!(
                "[advanced] enet_max_packet_bytes must be at least {DEFAULT_ENET_MAX_PACKET_BYTES}, got {}",
                self.advanced.enet_max_packet_bytes
            ));
        }
        if self.advanced.enet_max_waiting_bytes == 0 {
            return Err("[advanced] enet_max_waiting_bytes must be at least 1".to_string());
        }
        Ok(())
    }

    pub fn write_default(path: &Path) -> Result<(), String> {
        let generated = FileConfig {
            lobby: LobbySection::example(),
            ..FileConfig::default()
        };
        let body = toml::to_string_pretty(&generated)
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
    decorate_section::<LobbySection>(doc, "lobby");
    decorate_section::<MatchSection>(doc, "match");
    decorate_section::<ObserversSection>(doc, "observers");
    decorate_section::<PauseSection>(doc, "pause");
    decorate_section::<SavesSection>(doc, "saves");
    decorate_section::<MetricsSection>(doc, "metrics");
    decorate_section::<LogSection>(doc, "log");
    decorate_section::<SidecarSection>(doc, "sidecar");
    decorate_section::<LimitsSection>(doc, "limits");
    decorate_section::<TimeoutsSection>(doc, "timeouts");
    decorate_section::<AdvancedSection>(doc, "advanced");
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

// Follows the checkpoint interval unless set, and never turns the pulls off
// with it: a match without checkpoints still needs its AI state.
fn ai_state_interval_turns(turns: u32, checkpoint_interval_turns: u32) -> u32 {
    match (turns, checkpoint_interval_turns) {
        (0, 0) => Config::default().ai_state_interval_turns,
        (0, checkpoint) => checkpoint,
        (turns, _) => turns,
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

// 0 in the file is how an optional duration is written as unset.
fn secs_to_opt_delta(secs: u64) -> Option<TimeDelta> {
    (secs != 0).then(|| secs_to_delta(secs))
}

fn delta_to_secs(delta: TimeDelta) -> u64 {
    delta.num_seconds().max(0) as u64
}

fn opt_delta_to_secs(delta: Option<TimeDelta>) -> u64 {
    delta.map_or(0, delta_to_secs)
}

#[cfg(test)]
#[path = "../tests/unit/config.rs"]
mod tests;
