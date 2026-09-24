// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::JoinHandle;

use uuid::Uuid;

use crate::lobby::link::LobbyLink;
use crate::metrics::GameMetrics;
use crate::network_message::InboundNetworkMessage;
use crate::network_message::OutboundNetworkMessage;
use crate::relay::enet_task::EnetLimits;
use crate::relay::enet_task::run_enet_host;
use crate::relay::game_server::SidecarSetup;
use crate::relay::game_server::run_game_server;
use crate::relay::server_fsm::Config;
use crate::relay::server_fsm::SIMULATION_VERSION;
use crate::savegame::SaveItem;
use crate::savegame::SaveSetup;
use crate::savegame::bundle::ModRecord;
use crate::savegame::bundle::Mode;
use crate::savegame::writer;
use crate::savegame::writer::BundleMeta;

// A 100-port window above the default port lets one process host several games
// without asking the operator for a range.
const PORT_RANGE: (u16, u16) = (20595, 20695);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameId(Uuid);

impl std::fmt::Display for GameId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "gid_{}", self.0)
    }
}

pub struct GameConfig {
    // None takes the first free port in the pool's range.
    pub port: Option<u16>,
    pub server: Config,
    // None in standalone mode; Some when a lobby account is hosting this game.
    pub lobby: Option<LobbyLink>,
    // Path to the pyrogenesis binary for one-shot state dumps. None disables
    // the sidecar fallback.
    pub pyrogenesis_path: Option<PathBuf>,
    // Where the match outcome is written, as `<game_id>.json`. None only logs
    // it.
    pub outcome_dir: Option<PathBuf>,
    // Where the match is saved as it runs. None turns saving off.
    pub save: Option<SaveSetup>,
}

struct GameHandle {
    port: u16,
    /// Setting this starts the shutdown: the server thread tells every client
    /// why, drops its outbound sender and returns, and the ENet thread drains
    /// that last traffic, flushes and exits.
    shutdown_requested: Arc<AtomicBool>,
    // Yields the outcome replay's thread, if the match got that far.
    server_thread: Option<JoinHandle<Option<JoinHandle<()>>>>,
    enet_thread: Option<JoinHandle<()>>,
    save_thread: Option<JoinHandle<()>>,
}

pub struct GamePool {
    bind_ip: Ipv4Addr,
    enet_limits: EnetLimits,
    games: HashMap<GameId, GameHandle>,
    used_ports: Vec<u16>,
    // Outcome replays outlive their games, so that destroying a game never
    // waits minutes on one. Kept so the pool can wait for them when the
    // process shuts down, which is the only way a standalone match ends.
    outcomes: Vec<JoinHandle<()>>,
}

impl GamePool {
    pub fn new(bind_ip: Ipv4Addr, enet_limits: EnetLimits) -> Self {
        Self {
            bind_ip,
            enet_limits,
            games: HashMap::new(),
            used_ports: Vec::new(),
            outcomes: Vec::new(),
        }
    }

    // Every free port is tried in turn rather than just the first: one held
    // by another process would otherwise fail every allocation after it.
    fn bind_free_port(&self) -> Result<(u16, std::net::UdpSocket), String> {
        let (start, end) = PORT_RANGE;
        for port in (start..=end).filter(|port| !self.used_ports.contains(port)) {
            let bind_addr = SocketAddrV4::new(self.bind_ip, port);
            match std::net::UdpSocket::bind(bind_addr) {
                Ok(socket) => return Ok((port, socket)),
                Err(error) => {
                    tracing::debug!(%bind_addr, %error, "port unavailable, trying the next")
                }
            }
        }
        Err(format!("no bindable port in {start}..={end}"))
    }

    // True once the game's server thread has returned, whether the match
    // ended, the game went idle or it panicked. The game still has to be
    // destroyed to free its port.
    pub fn is_finished(&self, game_id: &GameId) -> bool {
        self.games
            .get(game_id)
            .and_then(|handle| handle.server_thread.as_ref())
            .is_some_and(|thread| thread.is_finished())
    }

    pub fn create_game(&mut self, config: GameConfig) -> Result<(GameId, u16), String> {
        let GameConfig {
            port,
            server: mut server_config,
            lobby,
            pyrogenesis_path,
            outcome_dir,
            save,
        } = config;
        server_config.saving = save.is_some();
        // Bound here rather than in the ENet thread: a bind failure has to reach
        // the caller as an error, not a panic in a thread nobody joins until
        // shutdown.
        let (port, socket) = match port {
            Some(port) => {
                if self.used_ports.contains(&port) {
                    return Err(format!("port {port} is already in use"));
                }
                let bind_addr = SocketAddrV4::new(self.bind_ip, port);
                let socket = std::net::UdpSocket::bind(bind_addr)
                    .map_err(|error| format!("failed to bind {bind_addr}: {error}"))?;
                (port, socket)
            }
            None => self.bind_free_port()?,
        };

        // The AI host runs on this machine and is only recognised from
        // loopback, so a socket bound elsewhere cannot host one.
        let ai_host_connect = (self.bind_ip.is_unspecified() || self.bind_ip.is_loopback())
            .then(|| SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));

        let game_id = GameId(Uuid::now_v7());
        let outcome_path = outcome_dir.map(|dir| dir.join(format!("{game_id}.json")));

        let (event_tx, event_rx) = mpsc::channel::<InboundNetworkMessage>();
        let (send_tx, send_rx) = mpsc::channel::<OutboundNetworkMessage>();

        // Spans are thread-local, so each thread enters its own copy instead of
        // inheriting one from the pool. Every log line then carries game_id and
        // port without threading them through by hand.
        let enet_game_id = game_id.to_string();
        let enet_limits = self.enet_limits;
        let enet_thread = std::thread::spawn(move || {
            let span = tracing::info_span!("game", game_id = %enet_game_id, port);
            let _guard = span.entered();
            run_enet_host(socket, enet_limits, event_tx, send_rx);
        });

        // Its own thread, so a slow disk or an fsync never stalls the tick
        // loop. It ends once the server thread drops its sender.
        let (save_tx, save_thread) = match save {
            Some(setup) => {
                let (tx, rx) = mpsc::channel::<SaveItem>();
                let meta = BundleMeta {
                    mode: if server_config.lobby_mode {
                        Mode::Lobby
                    } else {
                        Mode::Standalone
                    },
                    lobby_account: setup.lobby_account.clone(),
                    lobby_host_name: server_config.lobby_host_name.clone(),
                    engine_version: SIMULATION_VERSION.to_string(),
                    mods: server_config
                        .enabled_mods
                        .iter()
                        .map(|m| ModRecord {
                            name: m.name.clone(),
                            version: m.version.clone(),
                        })
                        .collect(),
                    turn_length_ms: server_config.turn_length_ms,
                };
                let save_game_id = game_id.to_string();
                let thread = std::thread::spawn(move || {
                    let span = tracing::info_span!("game", game_id = %save_game_id, port);
                    let _guard = span.entered();
                    writer::run(setup, meta, save_game_id.clone(), None, rx);
                });
                (Some(tx), Some(thread))
            }
            None => (None, None),
        };

        let server_game_id = game_id.to_string();
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let shutdown_requested_for_thread = Arc::clone(&shutdown_requested);
        let server_thread = std::thread::spawn(move || {
            let span = tracing::info_span!("game", game_id = %server_game_id, port);
            let _guard = span.entered();
            // Outside the unwind boundary, so a panicking game is still
            // counted and its series are still removed when the thread ends.
            let mut metrics = GameMetrics::new(&server_game_id, port);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_game_server(
                    event_rx,
                    send_tx,
                    shutdown_requested_for_thread,
                    server_config,
                    lobby,
                    SidecarSetup {
                        pyrogenesis_path,
                        ai_host_connect,
                        outcome_path,
                        save: save_tx,
                    },
                    &mut metrics,
                )
            }));
            match result {
                Ok(outcome) => outcome,
                Err(panic_payload) => {
                    let message = panic_payload
                        .downcast_ref::<&str>()
                        .map(|message| message.to_string())
                        .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic payload".to_string());
                    tracing::error!(panic = %message, "server thread panicked");
                    metrics.ended("panicked");
                    None
                }
            }
        });

        self.used_ports.push(port);
        self.games.insert(
            game_id.clone(),
            GameHandle {
                port,
                shutdown_requested,
                server_thread: Some(server_thread),
                enet_thread: Some(enet_thread),
                save_thread,
            },
        );

        tracing::info!(game_id = %game_id, port, "game created");

        Ok((game_id, port))
    }

    pub fn destroy_game(&mut self, game_id: GameId) {
        let Some(mut handle) = self.games.remove(&game_id) else {
            tracing::warn!(game_id = %game_id, "destroy_game: unknown game_id");
            return;
        };

        let port = handle.port;

        handle.shutdown_requested.store(true, Ordering::SeqCst);

        // Server thread first: the ENet thread only exits once the server
        // thread has queued its farewell and dropped the outbound sender.
        if let Some(thread) = handle.server_thread.take()
            && let Ok(Some(outcome)) = thread.join()
        {
            self.outcomes.push(outcome);
        }
        if let Some(thread) = handle.enet_thread.take() {
            let _ = thread.join();
        }
        // After the server thread, whose exit is what closes the writer's
        // channel: once this returns, the bundle's last flush and status
        // are on disk.
        if let Some(thread) = handle.save_thread.take() {
            let _ = thread.join();
        }
        self.outcomes.retain(|outcome| !outcome.is_finished());

        self.used_ports.retain(|used| *used != port);
        tracing::info!(game_id = %game_id, port, "game destroyed");
    }
}

impl Drop for GamePool {
    fn drop(&mut self) {
        let game_ids: Vec<GameId> = self.games.keys().cloned().collect();
        for game_id in game_ids {
            self.destroy_game(game_id);
        }
        if !self.outcomes.is_empty() {
            tracing::info!(
                pending = self.outcomes.len(),
                "waiting for match outcome replays"
            );
        }
        for outcome in self.outcomes.drain(..) {
            let _ = outcome.join();
        }
    }
}
