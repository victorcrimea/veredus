// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use tracing_subscriber::EnvFilter;

use veredus::cli;
use veredus::game_pool::GameConfig;
use veredus::game_pool::GamePool;

// Default directive when RUST_LOG is unset.
const DEFAULT_LOG_DIRECTIVES: &str = "info";

#[tokio::main]
async fn main() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_DIRECTIVES));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let (host, port) = cli::parse_args();

    let mut pool = GamePool::new(host);
    let (game_id, port) = pool
        .create_game(GameConfig { port: Some(port) })
        .expect("Failed to create initial game");

    tracing::info!(game_id = %game_id, port, "game running");

    // Returning drops the pool, which stops both threads of every game.
    tokio::signal::ctrl_c().await.ok();
}
