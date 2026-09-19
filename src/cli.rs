// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;

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
    /// Listen port
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
}

pub fn parse_args() -> (IpAddr, u16) {
    let args = Args::parse();
    (args.host, args.port)
}
