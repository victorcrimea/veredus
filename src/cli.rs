// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;
use std::net::Ipv4Addr;
use std::path::Path;
use std::path::PathBuf;

use clap::Parser;

use crate::config::DEFAULT_CONFIG_PATH;
use crate::config::FileConfig;
use crate::lobby::LobbyConfig;

// No flag carries a clap default: a default would be indistinguishable from
// a flag the operator typed, and a typed flag must win over the config file
// while an untyped one must not.
#[derive(Parser)]
#[command(name = "veredus", about = "0 A.D. relay server", version)]
struct Args {
    /// TOML config file; without it, ./config.toml is loaded when present.
    /// Every flag below overrides the matching setting in the file
    #[arg(long)]
    config: Option<PathBuf>,
    /// Write the default config to PATH (default ./config.toml) and exit;
    /// refuses to overwrite an existing file
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = DEFAULT_CONFIG_PATH)]
    gen_config: Option<PathBuf>,
    /// IPv4 bind address; the stock client has no IPv6 [default: 0.0.0.0]
    #[arg(long)]
    host: Option<Ipv4Addr>,
    /// Listen port (standalone mode only; a lobby game picks its own)
    /// [default: 20595]
    #[arg(long)]
    port: Option<u16>,
    /// Path to a pooled-account lobby config (JSON); when set, runs pool-lobby
    /// mode with it in place of the config file's [lobby] table
    #[arg(long)]
    lobby_config: Option<PathBuf>,
    /// Path to the pyrogenesis binary; when set, joiners no live client can
    /// serve get their snapshot from a one-shot replay instead of a drop, and
    /// AI slots are played by a headless pyrogenesis instead of every client
    #[arg(long)]
    pyrogenesis_path: Option<PathBuf>,
    /// Directory each finished match's outcome is written to, as
    /// <game_id>.json; without it the outcome is only logged. Needs
    /// --pyrogenesis-path, which replays the match to work the outcome out
    #[arg(long)]
    outcome_dir: Option<PathBuf>,
    /// Turns between two sidecar checkpoints, each resumed from the last;
    /// joiners are served the newest one and the outcome file follows the
    /// match while it runs. 0 disables them. Needs --pyrogenesis-path
    /// [default: 600]
    #[arg(long)]
    checkpoint_interval_turns: Option<u32>,
    /// Bind address of the Prometheus /metrics endpoint [default: 127.0.0.1]
    #[arg(long)]
    metrics_host: Option<IpAddr>,
    /// Port of the Prometheus /metrics endpoint; 0 disables it [default: 9091]
    #[arg(long)]
    metrics_port: Option<u16>,
    /// Directory every running match is saved to, so it survives a restart;
    /// an empty value turns saving off [default: saves]
    #[arg(long)]
    save_dir: Option<PathBuf>,
}

pub enum Command {
    GenConfig(PathBuf),
    Run(Box<RunMode>),
}

pub struct RunMode {
    // The config file with every command line override applied.
    pub config: FileConfig,
    // Some selects pool-lobby mode.
    pub lobby: Option<LobbyConfig>,
}

// Tracing is not up yet when this runs, because the config decides how it is
// set up, so errors come back as text for main to print.
pub fn parse_args() -> Result<Command, String> {
    let args = Args::parse();
    if let Some(path) = args.gen_config {
        return Ok(Command::GenConfig(path));
    }

    let mut config = match &args.config {
        Some(path) => FileConfig::load(path, true)?,
        None => FileConfig::load(DEFAULT_CONFIG_PATH.as_ref(), false)?,
    };

    if let Some(host) = args.host {
        config.server.host = host;
    }
    if let Some(port) = args.port {
        config.server.port = port;
    }
    if let Some(path) = args.pyrogenesis_path {
        config.server.pyrogenesis_path = path;
    }
    if let Some(path) = args.outcome_dir {
        config.server.outcome_dir = path;
    }
    if let Some(turns) = args.checkpoint_interval_turns {
        config.server.checkpoint_interval_turns = turns;
    }
    if let Some(host) = args.metrics_host {
        config.server.metrics_host = host;
    }
    if let Some(port) = args.metrics_port {
        config.server.metrics_port = port;
    }
    if let Some(path) = args.save_dir {
        config.server.save_dir = path;
    }

    config.validate()?;

    let lobby = match args.lobby_config {
        Some(path) => Some(load_lobby_json(&path)?),
        None if config.lobby.enabled => Some(config.lobby.to_lobby_config()?),
        None => None,
    };
    if let Some(lobby) = &lobby {
        lobby.validate()?;
    }

    Ok(Command::Run(Box::new(RunMode { config, lobby })))
}

fn load_lobby_json(path: &Path) -> Result<LobbyConfig, String> {
    let data = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read lobby config '{}': {error}", path.display()))?;
    serde_json::from_str(&data).map_err(|error| format!("failed to parse lobby config: {error}"))
}
