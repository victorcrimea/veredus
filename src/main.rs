// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use veredus::cli;
use veredus::cli::Command;
use veredus::config::FileConfig;
use veredus::config::LogSection;
use veredus::game_pool::GameConfig;
use veredus::game_pool::GameId;
use veredus::game_pool::GamePool;
use veredus::lobby::LobbyConfig;
use veredus::lobby::LobbyEvent;
use veredus::lobby::LobbyManager;
use veredus::lobby::link::LobbyLink;
use veredus::relay::password;
use veredus::relay::server_fsm::Config;

// Default directive when neither the environment nor the config file sets one.
// Rocket logs every request and its launch banner at info through `log`,
// which tracing has already claimed by the time Rocket starts, so its own
// log_level setting cannot quiet it; only a directive here can.
const DEFAULT_LOG_DIRECTIVES: &str = "info,rocket=error,_=error,hyper=error";

// musl's own allocator takes one global lock, which every game's socket and
// tick threads would then contend on.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[rocket::get("/metrics")]
fn metrics_route() -> (rocket::http::ContentType, String) {
    (
        rocket::http::ContentType::new("text", "plain"),
        veredus::metrics::encode(),
    )
}

#[tokio::main]
async fn main() {
    // Tracing is not up yet, so a bad config can only be reported on stderr.
    let mode = match cli::parse_args() {
        Ok(Command::GenConfig(path)) => {
            if let Err(error) = FileConfig::write_default(&path) {
                eprintln!("Error: {error}");
                std::process::exit(1);
            }
            println!("wrote default config to {}", path.display());
            return;
        }
        Ok(Command::Run(mode)) => *mode,
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
    };
    let config = mode.config;

    let stdout_filter = log_filter("RUST_LOG", &config.log.directives);
    // Per-layer filters rather than one global one: tracing-loki ships over a
    // bounded channel and silently drops on overflow, so turning stdout up to
    // trace for a debugging session must not flood Loki as well.
    let loki_filter = log_filter("LOKI_LOG", &config.log.loki_directives);
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer().with_filter(stdout_filter))
        .with(build_loki_layer(&config.log).map(|layer| layer.with_filter(loki_filter)))
        .init();

    serve_metrics(config.server.metrics_host, config.server.metrics_port);

    #[cfg(windows)]
    veredus::sidecar::guard_orphans();
    veredus::sidecar::set_run_limit(config.server.max_sidecar_runs);
    let mut pool = GamePool::new(config.server.host, config.server.enet_limits());
    let pyrogenesis_path = config.server.pyrogenesis_path();
    if pyrogenesis_path.is_some() {
        veredus::sidecar::init_work_root();
    }
    let outcome_dir = config.server.outcome_dir();
    let base = config.game.server_config(
        pyrogenesis_path.is_some(),
        config.server.checkpoint_interval_turns,
    );

    let result = match mode.lobby {
        Some(lobby_config) => {
            let base = Config {
                idle_shutdown: config.lobby.idle_shutdown(),
                ..base
            };
            run_pool_lobby_mode(&mut pool, lobby_config, base, pyrogenesis_path, outcome_dir).await;
            Ok(())
        }
        None => {
            run_standalone(
                &mut pool,
                config.server.port,
                base,
                pyrogenesis_path,
                outcome_dir,
                config.server.exit_after_game,
            )
            .await
        }
    };

    // Dropping the pool stops every game and then waits for their outcome
    // replays, which can take many minutes. An operator who does not want to
    // wait signals again; the replays' pyrogenesis processes die with us.
    let wind_down = tokio::task::spawn_blocking(move || drop(pool));
    tokio::select! {
        _ = wind_down => {}
        signal = shutdown_signal() => {
            tracing::warn!(signal, "second shutdown signal, abandoning pending outcome replays");
            std::process::exit(1);
        }
    }
    veredus::sidecar::remove_work_root();

    if let Err(error) = result {
        tracing::error!(%error, "standalone game could not be hosted");
        std::process::exit(1);
    }
}

// systemd and docker stop a service with SIGTERM, so waiting on Ctrl+C alone
// would let them kill the process without any game shutting down. Returns
// the name of the signal that arrived.
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        match terminate {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => "SIGINT",
                    _ = terminate.recv() => "SIGTERM",
                }
            }
            Err(error) => {
                tracing::warn!(%error, "cannot listen for SIGTERM, only Ctrl+C stops the server cleanly");
                let _ = tokio::signal::ctrl_c().await;
                "SIGINT"
            }
        }
    }
    // A console window closed, a logoff or a shutdown each get only a few
    // seconds before Windows kills the process, but that is enough to tell
    // the peers and start the games' shutdown.
    #[cfg(windows)]
    {
        use tokio::signal::windows;
        match (
            windows::ctrl_close(),
            windows::ctrl_shutdown(),
            windows::ctrl_break(),
        ) {
            (Ok(mut close), Ok(mut shutdown), Ok(mut brk)) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => "Ctrl+C",
                    _ = close.recv() => "console close",
                    _ = shutdown.recv() => "system shutdown",
                    _ = brk.recv() => "Ctrl+Break",
                }
            }
            _ => {
                tracing::warn!(
                    "cannot listen for console close, only Ctrl+C stops the server cleanly"
                );
                let _ = tokio::signal::ctrl_c().await;
                "Ctrl+C"
            }
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "Ctrl+C"
    }
}

// The environment wins over the file so one debugging session can turn a
// module up without editing the config everyone else runs with.
fn log_filter(env_var: &str, from_file: &str) -> EnvFilter {
    EnvFilter::try_from_env(env_var).unwrap_or_else(|_| {
        let directives = if from_file.is_empty() {
            DEFAULT_LOG_DIRECTIVES
        } else {
            from_file
        };
        EnvFilter::try_new(directives).unwrap_or_else(|error| {
            eprintln!("Error: invalid log directives '{directives}': {error}");
            std::process::exit(1);
        })
    })
}

// Unlike the other settings, an empty environment variable still counts as
// set, which matches how these were read before the config file existed.
fn env_or(env_var: &str, from_file: &str) -> Option<String> {
    std::env::var(env_var)
        .ok()
        .or_else(|| (!from_file.is_empty()).then(|| from_file.to_string()))
}

// Only low-cardinality values may be Loki labels, since every distinct label
// combination is a separate stream. Per-game and per-peer context travels as
// span fields instead, which tracing-loki flattens into the JSON line body, so
// a query looks like {job="veredus"} | json | game_id="gid_0199...".
fn build_loki_layer(log: &LogSection) -> Option<tracing_loki::Layer> {
    // Unset means no Loki at all, so a developer checkout needs no config.
    let url = env_or("LOKI_URL", &log.loki_url)?;
    // Tracing is not up yet, so a bad setting can only be reported on stderr.
    let url = tracing_loki::url::Url::parse(&url).unwrap_or_else(|error| {
        eprintln!("Error: invalid LOKI_URL '{url}': {error}");
        std::process::exit(1);
    });

    // Several servers can ship to one Loki, and without a distinct instance
    // label their streams would interleave.
    let instance = env_or("LOKI_INSTANCE", &log.loki_instance)
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    let env = env_or("LOKI_ENV", &log.loki_env).unwrap_or_else(|| "dev".to_string());

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

// The endpoint is a side show: failing to bind it is reported and the relay
// keeps serving games without it.
fn serve_metrics(host: std::net::IpAddr, port: u16) {
    if port == 0 {
        tracing::info!("metrics endpoint disabled");
        return;
    }
    veredus::metrics::init();
    // Rocket would otherwise take Ctrl+C and SIGTERM for itself: SIGTERM
    // would then stop only the endpoint instead of the process, and the game
    // pool must be the one that decides how the process winds down.
    let figment = rocket::Config::figment()
        .merge(("address", host))
        .merge(("port", port))
        .merge(("shutdown.ctrlc", false))
        .merge(("shutdown.signals", Vec::<String>::new()))
        .merge(("cli_colors", false));
    tokio::spawn(async move {
        let launched = rocket::custom(figment)
            .mount("/", rocket::routes![metrics_route])
            .launch()
            .await;
        if let Err(error) = launched {
            tracing::error!(%error, %host, port, "metrics endpoint failed");
        }
    });
    tracing::info!(%host, port, "metrics endpoint listening on /metrics");
}

// Hosts one game after another on the same port, or only one when
// `exit_after_game` hands restarting over to a supervisor.
async fn run_standalone(
    pool: &mut GamePool,
    port: u16,
    server_config: Config,
    pyrogenesis_path: Option<PathBuf>,
    outcome_dir: Option<PathBuf>,
    exit_after_game: bool,
) -> Result<(), String> {
    // One future for the whole run, so a signal that arrives between two
    // games is not missed.
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        let (game_id, port) = pool.create_game(GameConfig {
            port: Some(port),
            server: server_config.clone(),
            lobby: None,
            pyrogenesis_path: pyrogenesis_path.clone(),
            outcome_dir: outcome_dir.clone(),
        })?;
        tracing::info!(game_id = %game_id, port, "game running");

        // A standalone game has no lobby link to report its end over, so its
        // thread is watched instead.
        let mut poll = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                signal = &mut shutdown => {
                    tracing::info!(signal, "shutting down");
                    // Returning drops the pool, which stops both threads of
                    // every game.
                    return Ok(());
                }
                _ = poll.tick() => {
                    if pool.is_finished(&game_id) {
                        break;
                    }
                }
            }
        }

        // destroy_game joins both of the game's OS threads, so it must not
        // block the async runtime's worker thread.
        tokio::task::block_in_place(|| pool.destroy_game(game_id));
        if exit_after_game {
            tracing::info!("game ended, exiting as configured");
            return Ok(());
        }
        tracing::info!("game ended, hosting a fresh one");
    }
}

async fn run_pool_lobby_mode(
    pool: &mut GamePool,
    lobby_config: LobbyConfig,
    base: Config,
    pyrogenesis_path: Option<PathBuf>,
    outcome_dir: Option<PathBuf>,
) {
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

    // One future for the whole loop, so a signal that arrives while an event
    // is being handled is not missed.
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);

    loop {
        let event = tokio::select! {
            event = events.recv() => match event {
                Some(event) => event,
                None => break,
            },
            signal = &mut shutdown => {
                tracing::info!(signal, "shutting down");
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
                    hostme_outcome("duplicate_sender");
                    continue;
                }
                // Every idle account in the room reports the same hostme, so a
                // busy one is simply skipped and the next report is tried.
                if !lobby_mgr.reserve(account) {
                    tracing::debug!(account, %sender, "hostme reached a busy lobby account");
                    hostme_outcome("account_busy");
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
                        hostme_outcome("failed");
                        lobby_mgr.release(account);
                        continue;
                    }
                };

                let server_config = Config {
                    lobby_mode: true,
                    server_password_hash: password_hash.clone(),
                    server_name: server_name.clone(),
                    lobby_host_name: sender.clone(),
                    ..base.clone()
                };

                let (auth_tx, auth_rx) = std::sync::mpsc::channel();
                let (events_tx, events_rx) = tokio::sync::mpsc::unbounded_channel();

                match pool.create_game(GameConfig {
                    port: None,
                    server: server_config,
                    lobby: Some(LobbyLink { auth_rx, events_tx }),
                    pyrogenesis_path: pyrogenesis_path.clone(),
                    outcome_dir: outcome_dir.clone(),
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
                            hostme_outcome("failed");
                            tokio::task::block_in_place(|| pool.destroy_game(game_id));
                            continue;
                        }
                        hostme_outcome("hosted");
                        active_senders.insert(sender.clone());
                        account_sender.insert(account, sender);
                        account_game.insert(account, game_id);
                    }
                    Err(error) => {
                        tracing::error!(account, %error, "failed to create game for hostme");
                        hostme_outcome("failed");
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

fn hostme_outcome(outcome: &str) {
    veredus::metrics::LOBBY_HOSTME_TOTAL
        .with_label_values(&[outcome])
        .inc();
}
