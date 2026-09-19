// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::thread::JoinHandle;

use uuid::Uuid;

use crate::network_message::InboundNetworkMessage;
use crate::network_message::OutboundNetworkMessage;
use crate::relay::enet_task::run_enet_host;
use crate::relay::game_server::run_game_server;

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
}

struct GameHandle {
    port: u16,
    /// Dropping this sender triggers the shutdown cascade: the ENet thread
    /// sees the channel close, breaks, drops `event_tx`, and the server thread
    /// then returns.
    _shutdown_tx: mpsc::Sender<()>,
    /// Set before `_shutdown_tx` is dropped so the server thread can tell a
    /// deliberate shutdown apart from the ENet thread dying on its own.
    shutdown_requested: Arc<AtomicBool>,
    server_thread: Option<JoinHandle<()>>,
    enet_thread: Option<JoinHandle<()>>,
}

pub struct GamePool {
    bind_ip: IpAddr,
    games: HashMap<GameId, GameHandle>,
    used_ports: Vec<u16>,
}

impl GamePool {
    pub fn new(bind_ip: IpAddr) -> Self {
        Self {
            bind_ip,
            games: HashMap::new(),
            used_ports: Vec::new(),
        }
    }

    fn allocate_port(&self) -> Option<u16> {
        let (start, end) = PORT_RANGE;
        (start..=end).find(|port| !self.used_ports.contains(port))
    }

    pub fn create_game(&mut self, config: GameConfig) -> Result<(GameId, u16), String> {
        let port = match config.port {
            Some(port) => {
                if self.used_ports.contains(&port) {
                    return Err(format!("port {port} is already in use"));
                }
                port
            }
            None => self
                .allocate_port()
                .ok_or_else(|| "no free port in range".to_string())?,
        };

        let bind_addr = SocketAddr::new(self.bind_ip, port);

        // Bound here rather than in the ENet thread: a bind failure has to reach
        // the caller as an error, not a panic in a thread nobody joins until
        // shutdown.
        let socket = std::net::UdpSocket::bind(bind_addr)
            .map_err(|error| format!("failed to bind {bind_addr}: {error}"))?;

        let game_id = GameId(Uuid::now_v7());

        let (event_tx, event_rx) = mpsc::channel::<InboundNetworkMessage>();
        let (send_tx, send_rx) = mpsc::channel::<OutboundNetworkMessage>();
        let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

        // Spans are thread-local, so each thread enters its own copy instead of
        // inheriting one from the pool. Every log line then carries game_id and
        // port without threading them through by hand.
        let enet_game_id = game_id.to_string();
        let enet_thread = std::thread::spawn(move || {
            let span = tracing::info_span!("game", game_id = %enet_game_id, port);
            let _guard = span.entered();
            run_enet_host(socket, event_tx, send_rx, shutdown_rx);
        });

        let server_game_id = game_id.to_string();
        let shutdown_requested = Arc::new(AtomicBool::new(false));
        let shutdown_requested_for_thread = Arc::clone(&shutdown_requested);
        let server_thread = std::thread::spawn(move || {
            let span = tracing::info_span!("game", game_id = %server_game_id, port);
            let _guard = span.entered();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run_game_server(event_rx, send_tx, shutdown_requested_for_thread);
            }));
            if let Err(panic_payload) = result {
                let message = panic_payload
                    .downcast_ref::<&str>()
                    .map(|message| message.to_string())
                    .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "non-string panic payload".to_string());
                tracing::error!(panic = %message, "server thread panicked");
            }
        });

        self.used_ports.push(port);
        self.games.insert(
            game_id.clone(),
            GameHandle {
                port,
                _shutdown_tx: shutdown_tx,
                shutdown_requested,
                server_thread: Some(server_thread),
                enet_thread: Some(enet_thread),
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

        // Dropping `shutdown_tx` triggers the cascade: the ENet thread sees the
        // channel close, breaks, drops `event_tx`, and the server thread then
        // returns.
        drop(handle._shutdown_tx);

        if let Some(thread) = handle.enet_thread.take() {
            let _ = thread.join();
        }
        if let Some(thread) = handle.server_thread.take() {
            let _ = thread.join();
        }

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
    }
}
