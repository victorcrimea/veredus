// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;
use std::path::PathBuf;

use clap::Parser;

// 0x5073, the port stock clients dial unless they are told otherwise.
const DEFAULT_PORT: u16 = 20595;
const DEFAULT_HOST: &str = "0.0.0.0";

#[derive(Parser)]
#[command(name = "veredus", about = "0 A.D. relay server", version)]
struct Args {
    /// Bind address
    #[arg(long, default_value = DEFAULT_HOST)]
    host: IpAddr,
    /// Listen port (standalone mode only; a lobby-config game picks its own)
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// Path to a pooled-account lobby config; when set, runs pool-lobby mode
    /// instead of standalone
    #[arg(long)]
    lobby_config: Option<PathBuf>,
}

pub struct RunMode {
    pub host: IpAddr,
    pub port: u16,
    pub lobby_config: Option<PathBuf>,
}

pub fn parse_args() -> RunMode {
    let args = Args::parse();
    RunMode {
        host: args.host,
        port: args.port,
        lobby_config: args.lobby_config,
    }
}
