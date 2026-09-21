// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

use chrono::TimeDelta;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use veredus::cli;
use veredus::game_pool::GameConfig;
use veredus::game_pool::GameId;
use veredus::game_pool::GamePool;
use veredus::lobby::LobbyConfig;
use veredus::lobby::LobbyEvent;
use veredus::lobby::LobbyManager;
use veredus::lobby::link::LobbyLink;
use veredus::relay::password;
use veredus::relay::server_fsm::Config;

// Default directive when RUST_LOG or LOKI_LOG is unset.
const DEFAULT_LOG_DIRECTIVES: &str = "info";

// How long a pooled-lobby game may sit with nobody ever having joined, or
// with everybody gone, before it shuts itself down and frees its account.
const IDLE_SHUTDOWN: TimeDelta = TimeDelta::seconds(60);

#[tokio::main]
async fn main() {
    let stdout_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_DIRECTIVES));
    // Per-layer filters rather than one global one: tracing-loki ships over a
    // bounded channel and silently drops on overflow, so turning stdout up to
    // trace for a debugging session must not flood Loki as well.
    let loki_filter = EnvFilter::try_from_env("LOKI_LOG")
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_DIRECTIVES));
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(stdout_filter))
        .with(build_loki_layer().map(|layer| layer.with_filter(loki_filter)))
        .init();

    let mode = cli::parse_args();
    let mut pool = GamePool::new(mode.host);

    match mode.lobby_config {
        Some(path) => run_pool_lobby_mode(&mut pool, path).await,
        None => run_standalone(&mut pool, mode.port).await,
    }
}

// Only low-cardinality values may be Loki labels, since every distinct label
// combination is a separate stream. Per-game and per-peer context travels as
// span fields instead, which tracing-loki flattens into the JSON line body, so
// a query looks like {job="veredus"} | json | game_id="gid_0199...".
fn build_loki_layer() -> Option<tracing_loki::Layer> {
    // Unset means no Loki at all, so a developer checkout needs no config.
    let url = std::env::var("LOKI_URL").ok()?;
    // Tracing is not up yet, so a bad setting can only be reported on stderr.
    let url = tracing_loki::url::Url::parse(&url).unwrap_or_else(|error| {
        eprintln!("Error: invalid LOKI_URL '{url}': {error}");
        std::process::exit(1);
    });

    // Several servers can ship to one Loki, and without a distinct instance
    // label their streams would interleave.
    let instance = std::env::var("LOKI_INSTANCE")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let env = std::env::var("LOKI_ENV").unwrap_or_else(|_| "dev".to_string());

    let built = tracing_loki::builder()
        .label("job", "veredus")
        .and_then(|builder| builder.label("instance", instance))
        .and_then(|builder| builder.label("env", env))
        // A restart under the same instance would otherwise be
        // indistinguishable in a stream; as a field it costs no cardinality.
        .and_then(|builder| builder.extra_field("pid", std::process::id().to_string()))
        .and_then(|builder| builder.build_url(url));
    let (layer, task) = built.unwrap_or_else(|error| {
        eprintln!("Error: failed to build the Loki sink: {error}");
        std::process::exit(1);
    });

    // The layer only queues; this task is what actually ships to Loki, so it
    // must live on the runtime for the whole process.
    tokio::spawn(task);
    Some(layer)
}

async fn run_standalone(pool: &mut GamePool, port: u16) {
    let (game_id, port) = pool
        .create_game(GameConfig {
            port: Some(port),
            server: Config::default(),
            lobby: None,
        })
        .expect("Failed to create initial game");

    tracing::info!(game_id = %game_id, port, "game running");

    // Returning drops the pool, which stops both threads of every game.
    tokio::signal::ctrl_c().await.ok();
}

async fn run_pool_lobby_mode(pool: &mut GamePool, config_path: PathBuf) {
    let config_data = std::fs::read_to_string(&config_path).unwrap_or_else(|error| {
        eprintln!(
            "Error: failed to read lobby config '{}': {error}",
            config_path.display()
        );
        std::process::exit(1);
    });
    let lobby_config: LobbyConfig = serde_json::from_str(&config_data).unwrap_or_else(|error| {
        eprintln!("Error: failed to parse lobby config: {error}");
        std::process::exit(1);
    });
    tracing::info!(
        accounts = lobby_config.accounts.len(),
        muc_room = %lobby_config.muc_room,
        "lobby config loaded"
    );

    let engine_version = lobby_config.engine_version.clone();
    let game_password = lobby_config.game_password.clone();
    let server_name = lobby_config.server_name.clone();

    let mut lobby_mgr = LobbyManager::new(lobby_config);
    let mut events = lobby_mgr.start();

    tracing::info!("waiting for 'hostme' in MUC chat to create a game");

    // A sender is a lobby username, not an account: this is what stops one
    // impatient "hostme" spam from claiming every free account.
    let mut active_senders: HashSet<String> = HashSet::new();
    let mut account_sender: HashMap<usize, String> = HashMap::new();
    let mut account_game: HashMap<usize, GameId> = HashMap::new();

    loop {
        let event = tokio::select! {
            event = events.recv() => match event {
                Some(event) => event,
                None => break,
            },
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("Ctrl+C received, shutting down");
                break;
            }
        };

        match event {
            LobbyEvent::HostRequested {
                account,
                sender,
                host_jid,
            } => {
                if active_senders.contains(&sender) {
                    tracing::debug!(account, %sender, "ignoring duplicate hostme");
                    continue;
                }
                // Every idle account in the room reports the same hostme, so a
                // busy one is simply skipped and the next report is tried.
                if !lobby_mgr.reserve(account) {
                    tracing::debug!(account, %sender, "hostme reached a busy lobby account");
                    continue;
                }

                // Sec. 18: the lobby host stores H = hash(rawPassword,
                // hostFullJID + rawPassword + engineVersion), keyed to the
                // account's own bound JID since that is what a joining client
                // salts with.
                let salt = format!("{host_jid}{game_password}{engine_version}");
                let raw_password = game_password.clone();
                let hashed = tokio::task::spawn_blocking(move || {
                    password::hash(&raw_password, salt.as_bytes())
                })
                .await;
                // An empty hash would host the game with no password at all.
                let password_hash = match hashed {
                    Ok(password_hash) => password_hash,
                    Err(error) => {
                        tracing::error!(account, %error, "game password hash failed");
                        lobby_mgr.release(account);
                        continue;
                    }
                };

                let server_config = Config {
                    lobby_mode: true,
                    server_password_hash: password_hash.clone(),
                    server_name: server_name.clone(),
                    idle_shutdown: Some(IDLE_SHUTDOWN),
                    lobby_host_name: sender.clone(),
                    ..Config::default()
                };

                let (auth_tx, auth_rx) = std::sync::mpsc::channel();
                let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();

                match pool.create_game(GameConfig {
                    port: None,
                    server: server_config,
                    lobby: Some(LobbyLink { auth_rx, events_tx }),
                }) {
                    Ok((game_id, port)) => {
                        tracing::info!(
                            game_id = %game_id,
                            port,
                            account,
                            %sender,
                            "hosted a game for a lobby request"
                        );
                        if !lobby_mgr.assign(account, auth_tx, events_rx, port, password_hash) {
                            // The account task is gone, so nothing would ever report
                            // this game ended. The account stays reserved because it
                            // can no longer host anything.
                            tracing::error!(account, game_id = %game_id, "lobby account unusable, dropping its game");
                            tokio::task::block_in_place(|| pool.destroy_game(game_id));
                            continue;
                        }
                        active_senders.insert(sender.clone());
                        account_sender.insert(account, sender);
                        account_game.insert(account, game_id);
                    }
                    Err(error) => {
                        tracing::error!(account, %error, "failed to create game for hostme");
                        lobby_mgr.release(account);
                    }
                }
            }
            LobbyEvent::GameEnded { account } => {
                tracing::info!(account, "lobby game ended");
                if let Some(sender) = account_sender.remove(&account) {
                    active_senders.remove(&sender);
                }
                if let Some(game_id) = account_game.remove(&account) {
                    // destroy_game joins both of the game's OS threads, so it
                    // must not block the async runtime's worker thread.
                    tokio::task::block_in_place(|| pool.destroy_game(game_id));
                }
                lobby_mgr.release(account);
            }
        }
    }

    lobby_mgr.shutdown().await;
    tracing::info!("lobby event loop ended, XMPP accounts shut down");
}
