// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::net::IpAddr;
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::thread::JoinHandle;
use std::time::Duration;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;

use crate::lobby::link::GameToLobby;
use crate::lobby::link::LobbyAuthToken;
use crate::lobby::link::LobbyLink;
use crate::network_message::InboundNetworkMessage;
use crate::network_message::OutboundNetworkMessage;
use crate::relay::messages::WireMessage;
use crate::relay::monitor::PeerStats;
use crate::relay::server_fsm::AnyServer;
use crate::relay::server_fsm::Config;
use crate::relay::server_fsm::Effect;
use crate::relay::server_fsm::Idle;
use crate::relay::server_fsm::Input;
use crate::relay::server_fsm::Server;
use crate::sidecar::AiHostProcess;
use crate::sidecar::DumpRequest;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
// The FSM only needs a tick often enough to drive the connection warnings,
// which are emitted at most once a second.
const TICK_INTERVAL: TimeDelta = TimeDelta::milliseconds(100);

// The one in-flight one-shot dump, if any. The FSM allows only one run at a
// time, so one slot is enough. Dropping it trips the cancel flag, so every
// return from the game loop, and a panic caught by the pool, also kills a
// running pyrogenesis instead of orphaning it.
#[derive(Default)]
struct DumpSlot {
    id: Option<u32>,
    cancel: Arc<AtomicBool>,
}

impl DumpSlot {
    // Records a fresh run, replacing the previous flag: a run started after a
    // cancel must not inherit the old run's trip.
    fn start(&mut self, id: u32) -> Arc<AtomicBool> {
        self.id = Some(id);
        self.cancel = Arc::new(AtomicBool::new(false));
        Arc::clone(&self.cancel)
    }

    fn cancel(&self, id: u32) {
        if self.id == Some(id) {
            self.cancel.store(true, Ordering::SeqCst);
        }
    }
}

impl Drop for DumpSlot {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

// The one in-flight checkpoint run, if any. Unlike a dump it also writes the
// match's progress to the outcome file, so the game joins it before the final
// outcome is replayed: a progress line landing after the final one would
// replace it. Dropping it only trips the flag, for the panic path.
#[derive(Default)]
struct CheckpointSlot {
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl CheckpointSlot {
    fn stop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for CheckpointSlot {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
    }
}

// What a finished checkpoint run hands back. The result JSON stays on this
// side of the FSM, which only needs the player states to judge the match.
struct CheckpointDone {
    state: Vec<u8>,
    players: Vec<String>,
    turn: u32,
    json: String,
}

// The one-shot runs this game can have in flight, with the channels their
// workers answer on. Owned by the game thread for its whole life.
struct OneShotRuns {
    pyrogenesis_path: Option<PathBuf>,
    outcome_path: Option<PathBuf>,
    dump_tx: Sender<(u32, Option<Vec<u8>>)>,
    dump_slot: DumpSlot,
    checkpoint_tx: Sender<(u32, Option<CheckpointDone>)>,
    checkpoint_slot: CheckpointSlot,
    // The newest checkpoint result as (run id, turn, JSON), kept in case the
    // FSM finds that this run decided the match and it becomes the outcome.
    last_result: Option<(u32, u32, String)>,
}

// The hosted-AI process for this game, if one is running. It lives on this
// thread so that every return from the game loop, and a panic caught by the
// pool, drops it and with it kills the engine.
struct AiHostSlot {
    pyrogenesis_path: Option<PathBuf>,
    // Where the AI host dials the relay. None when the socket is bound to an
    // address loopback cannot reach, which the FSM only honours from loopback.
    connect: Option<SocketAddrV4>,
    process: Option<AiHostProcess>,
    // A spawn that failed is reported on the next loop, as an exit is, so the
    // FSM hears about both the same way.
    exited: bool,
}

impl AiHostSlot {
    fn spawn(&mut self, name: &str) {
        self.process = None;
        let (Some(path), Some(addr)) = (self.pyrogenesis_path.as_ref(), self.connect) else {
            tracing::error!(
                "sidecar: AI host cannot be spawned: no pyrogenesis or no loopback bind"
            );
            self.exited = true;
            return;
        };
        match AiHostProcess::spawn(path, *addr.ip(), addr.port(), name) {
            Ok(process) => self.process = Some(process),
            Err(error) => {
                tracing::error!(%error, "sidecar: AI host failed to spawn");
                self.exited = true;
            }
        }
    }

    // True once per exit. Its supervisor has already reaped it by then.
    fn take_exit(&mut self) -> bool {
        if let Some(status) = self.process.as_mut().and_then(|p| p.poll_exit()) {
            tracing::warn!(%status, "sidecar: AI host exited");
            self.process = None;
            self.exited = true;
        }
        std::mem::take(&mut self.exited)
    }
}

// Everything a game needs to drive pyrogenesis. `pyrogenesis_path` enables
// one-shot state dumps, the AI host and the outcome replay; None means dump
// effects answer at once. `ai_host_connect` is where the AI host dials back
// to. `outcome_path` is where the match outcome is written; None only logs it.
pub struct SidecarSetup {
    pub pyrogenesis_path: Option<PathBuf>,
    pub ai_host_connect: Option<SocketAddrV4>,
    pub outcome_path: Option<PathBuf>,
}

// The ENet thread feeds decoded events in over `event_rx` and takes effects out
// over `send_tx`. Every clock read lives here, on the IO side, so the FSM
// itself stays a pure function of the inputs it is handed. `lobby` is None in
// standalone mode; when present, its `auth_rx` half feeds Input::LobbyAuth and
// its `events_tx` half is where lobby-listing effects go. A match that ran is
// replayed once the loop ends to work out its outcome; the returned handle is
// that replay's thread.
pub fn run_game_server(
    event_rx: Receiver<InboundNetworkMessage>,
    send_tx: Sender<OutboundNetworkMessage>,
    shutdown_requested: Arc<AtomicBool>,
    config: Config,
    lobby: Option<LobbyLink>,
    sidecar: SidecarSetup,
) -> Option<JoinHandle<()>> {
    let SidecarSetup {
        pyrogenesis_path,
        ai_host_connect,
        outcome_path,
    } = sidecar;
    // Parked in the listening state, which is the phase a relay spends its
    // whole idle life in.
    let mut server = Some(AnyServer::from(Server::<Idle>::new(config).listen()));
    let mut latest_stats: Vec<PeerStats> = Vec::new();
    let mut last_tick: DateTime<Utc> = Utc::now();
    let (dump_tx, dump_rx) = std::sync::mpsc::channel::<(u32, Option<Vec<u8>>)>();
    let (checkpoint_tx, checkpoint_rx) =
        std::sync::mpsc::channel::<(u32, Option<CheckpointDone>)>();
    let mut runs = OneShotRuns {
        pyrogenesis_path: pyrogenesis_path.clone(),
        outcome_path,
        dump_tx,
        dump_slot: DumpSlot::default(),
        checkpoint_tx,
        checkpoint_slot: CheckpointSlot::default(),
        last_result: None,
    };
    let mut outcome_request: Option<DumpRequest> = None;
    let mut ai_host = AiHostSlot {
        pyrogenesis_path: pyrogenesis_path.clone(),
        connect: ai_host_connect,
        process: None,
        exited: false,
    };
    'game: loop {
        // Drained before the ENet events, so a lobby-auth prompt is queued
        // ahead of the AUTHENTICATE it is meant to precede.
        if let Some(lobby) = &lobby {
            loop {
                match lobby.auth_rx.try_recv() {
                    Ok(LobbyAuthToken { username, token }) => {
                        server = Some(
                            server
                                .take()
                                .expect("server is always present")
                                .handle(Input::LobbyAuth { username, token }),
                        );
                    }
                    Err(TryRecvError::Empty) => break,
                    // The account was released or the lobby side is gone; the
                    // game keeps running, just with no more auth prompts.
                    Err(TryRecvError::Disconnected) => break,
                }
            }
        }

        if ai_host.take_exit() {
            server = Some(
                server
                    .take()
                    .expect("server is always present")
                    .handle(Input::AiHostExited),
            );
        }

        while let Ok((id, state)) = dump_rx.try_recv() {
            server = Some(
                server
                    .take()
                    .expect("server is always present")
                    .handle(Input::StateDumped { id, state }),
            );
        }

        while let Ok((id, done)) = checkpoint_rx.try_recv() {
            let (state, players) = match done {
                Some(CheckpointDone {
                    state,
                    players,
                    turn,
                    json,
                }) => {
                    runs.last_result = Some((id, turn, json));
                    (Some(state), players)
                }
                None => (None, Vec::new()),
            };
            server = Some(
                server
                    .take()
                    .expect("server is always present")
                    .handle(Input::Checkpointed { id, state, players }),
            );
        }

        loop {
            match event_rx.try_recv() {
                Ok(message) => {
                    if let Some(input) = to_input(message, &mut latest_stats) {
                        // `handle` consumes the server so a transition can be a
                        // consuming method, which is what keeps the phases typed.
                        server = Some(
                            server
                                .take()
                                .expect("server is always present")
                                .handle(input),
                        );
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    // The ENet thread dropped its sender, which is how a
                    // deliberate shutdown reaches this loop.
                    tracing::info!("ENet event channel disconnected, shutting down");
                    break 'game;
                }
            }
        }

        let now = Utc::now();
        let elapsed = now.signed_duration_since(last_tick);
        // A backward wall-clock step ticks now and re-anchors, rather than
        // stalling the whole FSM until the clock catches up.
        if elapsed >= TICK_INTERVAL || elapsed < TimeDelta::zero() {
            last_tick = now;
            let input = Input::Tick {
                now,
                stats: latest_stats.clone(),
            };
            server = Some(
                server
                    .take()
                    .expect("server is always present")
                    .handle(input),
            );
        }

        if let Some(current) = server.as_mut() {
            let outcome = drain(
                current.take_effects(),
                &send_tx,
                lobby.as_ref(),
                &mut runs,
                &mut ai_host,
            );
            if !outcome.channel_ok {
                break 'game;
            }
            if outcome.game_over {
                tracing::info!("game over, ending game");
                let current = server.take().expect("server is always present");
                outcome_request = current.outcome_request();
                let effects = current.shutdown();
                drain(effects, &send_tx, lobby.as_ref(), &mut runs, &mut ai_host);
                break 'game;
            }
        }

        if shutdown_requested.load(Ordering::SeqCst) {
            tracing::info!("shutdown requested, dropping every peer");
            let current = server.take().expect("server is always present");
            outcome_request = current.outcome_request();
            let effects = current.shutdown();
            drain(effects, &send_tx, lobby.as_ref(), &mut runs, &mut ai_host);
            break 'game;
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    // Before the final outcome, so no progress write can land after it.
    runs.checkpoint_slot.stop();

    // The two shutdown paths consume the server, so they read the record
    // first; every other way out leaves it in place.
    let request = outcome_request.or_else(|| server.as_ref()?.outcome_request())?;
    let path = pyrogenesis_path?;
    Some(spawn_outcome(path, request, runs.outcome_path.take()))
}

// Stats are cached rather than fed straight in, so the FSM sees timing only on
// a tick and stays independent of how often the socket thread samples.
fn to_input(message: InboundNetworkMessage, latest_stats: &mut Vec<PeerStats>) -> Option<Input> {
    match message {
        InboundNetworkMessage::Connect { peer, addr } => match addr {
            IpAddr::V4(addr) => Some(Input::Connected { peer, addr }),
            // The stock client is IPv4 only, and the ban list is keyed by v4.
            IpAddr::V6(addr) => {
                tracing::warn!(?peer, %addr, "ignoring IPv6 peer");
                None
            }
        },
        // Logged here rather than in the FSM because only this side knows
        // the ENet reason word, and a peer refused at connect, which never
        // got a session, is still logged with its address.
        InboundNetworkMessage::Disconnect { peer, addr, reason } => {
            tracing::info!(peer = peer.0, ip = %addr, enet_reason = reason, "client disconnected");
            Some(Input::Disconnected { peer })
        }
        InboundNetworkMessage::Message { peer, data } => match WireMessage::from_bytes(&data) {
            Ok(msg) => Some(Input::Received { peer, msg }),
            // One bad packet is dropped and the connection stays open.
            Err(error) => {
                tracing::debug!(?peer, %error, bytes = data.len(), "undecodable packet dropped");
                None
            }
        },
        InboundNetworkMessage::Stats { stats } => {
            *latest_stats = stats;
            None
        }
    }
}

struct DrainOutcome {
    // False once the ENet thread is gone and there is nothing left to send to.
    channel_ok: bool,
    game_over: bool,
}

fn drain(
    effects: Vec<Effect>,
    send_tx: &Sender<OutboundNetworkMessage>,
    lobby: Option<&LobbyLink>,
    runs: &mut OneShotRuns,
    ai_host: &mut AiHostSlot,
) -> DrainOutcome {
    let mut outcome = DrainOutcome {
        channel_ok: true,
        game_over: false,
    };

    for effect in effects {
        let outbound = match effect {
            Effect::Send { peer, msg } => OutboundNetworkMessage::Message {
                peer,
                data: msg.to_bytes(),
            },
            Effect::Disconnect { peer, reason } => OutboundNetworkMessage::Disconnect {
                peer,
                reason: reason as u32,
            },
            Effect::DisconnectNow { peer, reason } => OutboundNetworkMessage::DisconnectNow {
                peer,
                reason: reason as u32,
            },
            Effect::LobbyListing {
                host_username,
                nbp,
                players,
                map,
                mods,
            } => {
                if let Some(lobby) = lobby {
                    let _ = lobby.events_tx.send(GameToLobby::Listing {
                        host_username,
                        nbp,
                        players,
                        map,
                        mods,
                    });
                }
                continue;
            }
            Effect::LobbyStarted { nbp, players } => {
                if let Some(lobby) = lobby {
                    let _ = lobby.events_tx.send(GameToLobby::Started { nbp, players });
                }
                continue;
            }
            Effect::GameOver => {
                outcome.game_over = true;
                continue;
            }
            Effect::MatchEnded { checkpoint } => {
                // The deciding run already wrote this result as progress;
                // now it is marked final, and no replay has to follow.
                if let Some((id, turn, json)) = runs.last_result.take()
                    && checkpoint == Some(id)
                {
                    tracing::info!(turn, "match outcome resolved");
                    if let Some(path) = &runs.outcome_path {
                        write_outcome(path, turn, true, &json);
                    }
                }
                if let Some(lobby) = lobby {
                    let _ = lobby.events_tx.send(GameToLobby::Ended);
                }
                continue;
            }
            Effect::StateDump { id, turn, request } => {
                let cancel = runs.dump_slot.start(id);
                spawn_dump(
                    runs.pyrogenesis_path.as_ref(),
                    &runs.dump_tx,
                    id,
                    turn,
                    request,
                    cancel,
                );
                continue;
            }
            Effect::CancelStateDump { id } => {
                runs.dump_slot.cancel(id);
                continue;
            }
            Effect::Checkpoint { id, turn, request } => {
                spawn_checkpoint(runs, id, turn, request);
                continue;
            }
            Effect::SpawnAiHost { name } => {
                ai_host.spawn(&name);
                continue;
            }
            Effect::StopAiHost => {
                ai_host.process = None;
                continue;
            }
        };
        if send_tx.send(outbound).is_err() {
            tracing::info!("ENet send channel closed, shutting down");
            outcome.channel_ok = false;
            return outcome;
        }
    }

    outcome
}

// A dump runs on its own thread because the replay takes far longer than one
// tick: the game loop keeps serving while pyrogenesis replays the match.
// The result comes back over `dump_tx` and re-enters the FSM as an input.
fn spawn_dump(
    pyrogenesis_path: Option<&PathBuf>,
    dump_tx: &Sender<(u32, Option<Vec<u8>>)>,
    id: u32,
    turn: u32,
    request: crate::sidecar::DumpRequest,
    cancel: Arc<AtomicBool>,
) {
    // Both read here, on the game thread, before the spawn: the dump module
    // stays free of clock reads, and the thread's log lines carry the game id.
    let now = chrono::Utc::now();
    let span = tracing::Span::current();
    let Some(path) = pyrogenesis_path.cloned() else {
        // The FSM gate means this never happens, but a waiting joiner must
        // not hang if it does.
        let _ = dump_tx.send((id, None));
        return;
    };
    let tx = dump_tx.clone();
    std::thread::spawn(move || {
        let _guard = span.entered();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let dir = std::env::temp_dir().join(format!("veredus-dump-{}", uuid::Uuid::new_v4()));
            let result = crate::sidecar::dump_state(&path, &dir, turn, &request, now, &cancel);
            let _ = std::fs::remove_dir_all(&dir);
            result
        }));
        let state = match outcome {
            Ok(Ok(bytes)) => {
                tracing::info!(turn, bytes = bytes.len(), "sidecar: state dump ready");
                Some(bytes)
            }
            Ok(Err(error)) => {
                tracing::warn!(turn, %error, "sidecar: state dump failed");
                None
            }
            Err(_) => {
                tracing::warn!(turn, "sidecar: state dump panicked");
                None
            }
        };
        // The game may have ended while the replay ran; then there is no one
        // left to answer and the result is dropped.
        let _ = tx.send((id, state));
    });
}

// A checkpoint runs on its own thread for the same reason a dump does. On
// success it records the match's progress in the outcome file, marked as not
// final, and hands the state back to the FSM over `tx`.
fn spawn_checkpoint(runs: &mut OneShotRuns, id: u32, turn: u32, request: DumpRequest) {
    // Read here, on the game thread, for the same reasons as in spawn_dump.
    let now = chrono::Utc::now();
    let span = tracing::Span::current();
    let Some(path) = runs.pyrogenesis_path.clone() else {
        let _ = runs.checkpoint_tx.send((id, None));
        return;
    };
    // The FSM runs one checkpoint at a time, so a previous worker has
    // already answered and is at most finishing its cleanup.
    if let Some(previous) = runs.checkpoint_slot.handle.take() {
        let _ = previous.join();
    }
    let cancel = Arc::new(AtomicBool::new(false));
    runs.checkpoint_slot.cancel = Arc::clone(&cancel);
    let tx = runs.checkpoint_tx.clone();
    let outcome_path = runs.outcome_path.clone();
    let from = request.first_turn();
    runs.checkpoint_slot.handle = Some(std::thread::spawn(move || {
        let _guard = span.entered();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let dir =
                std::env::temp_dir().join(format!("veredus-checkpoint-{}", uuid::Uuid::new_v4()));
            let result = crate::sidecar::checkpoint(&path, &dir, turn, &request, now, &cancel);
            let _ = std::fs::remove_dir_all(&dir);
            result
        }));
        let state = match outcome {
            Ok(Ok((bytes, result, json))) => {
                tracing::info!(
                    from,
                    turn,
                    bytes = bytes.len(),
                    time_elapsed_ms = result.time_elapsed,
                    players = ?player_summary(&result),
                    "sidecar: checkpoint ready"
                );
                if let Some(path) = outcome_path
                    && !cancel.load(Ordering::SeqCst)
                {
                    write_outcome(&path, turn, false, &json);
                }
                let players = result.player_states.into_iter().map(|p| p.state).collect();
                Some(CheckpointDone {
                    state: bytes,
                    players,
                    turn,
                    json,
                })
            }
            Ok(Err(error)) => {
                tracing::warn!(from, turn, %error, "sidecar: checkpoint failed");
                None
            }
            Err(_) => {
                tracing::warn!(from, turn, "sidecar: checkpoint panicked");
                None
            }
        };
        let _ = tx.send((id, state));
    }));
}

// Only names and states: the full result carries every player's statistics
// for the whole match and belongs in the file, not a log.
fn player_summary(result: &crate::sidecar::ReplayResult) -> Vec<(usize, &str, &str)> {
    result
        .player_states
        .iter()
        .enumerate()
        .map(|(id, p)| (id, p.name.as_deref().unwrap_or(""), p.state.as_str()))
        .collect()
}

// The outcome replay runs the whole match, which can take minutes, so it gets
// a thread of its own and the game thread returns at once. Nothing can cancel
// it but the process exiting.
fn spawn_outcome(
    pyrogenesis_path: PathBuf,
    request: DumpRequest,
    outcome_path: Option<PathBuf>,
) -> JoinHandle<()> {
    // Read here, on the game thread, for the same reasons as in spawn_dump.
    let now = chrono::Utc::now();
    let span = tracing::Span::current();
    std::thread::spawn(move || {
        let _guard = span.entered();
        let from = request.first_turn();
        let turn = request.last_turn();
        tracing::info!(from, turn, "sidecar: replaying match for its outcome");
        let never = AtomicBool::new(false);
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let dir =
                std::env::temp_dir().join(format!("veredus-outcome-{}", uuid::Uuid::new_v4()));
            let result =
                crate::sidecar::resolve_outcome(&pyrogenesis_path, &dir, &request, now, &never);
            let _ = std::fs::remove_dir_all(&dir);
            result
        }));
        let (result, json) = match outcome {
            Ok(Ok(resolved)) => resolved,
            Ok(Err(error)) => {
                tracing::warn!(%error, "sidecar: match outcome replay failed");
                return;
            }
            Err(_) => {
                tracing::warn!("sidecar: match outcome replay panicked");
                return;
            }
        };
        tracing::info!(
            turn,
            time_elapsed_ms = result.time_elapsed,
            players = ?player_summary(&result),
            "match outcome resolved"
        );
        if let Some(path) = outcome_path {
            write_outcome(&path, turn, true, &json);
        }
    })
}

// Written aside and renamed into place, so whoever watches the directory
// never reads half a result. The engine's result is wrapped with the turn it
// describes and whether the match is over, since checkpoints rewrite the same
// file while the match runs. `result` is already JSON, so it is embedded
// as is rather than parsed and serialized again.
fn write_outcome(path: &std::path::Path, turn: u32, is_final: bool, result: &str) {
    let json = format!(r#"{{"turn":{turn},"final":{is_final},"result":{result}}}"#);
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        tracing::warn!(%error, path = %parent.display(), "cannot create outcome directory");
        return;
    }
    let tmp = path.with_extension("json.tmp");
    let written = std::fs::write(&tmp, json).and_then(|()| std::fs::rename(&tmp, path));
    match written {
        Ok(()) => tracing::info!(path = %path.display(), "match outcome written"),
        Err(error) => {
            let _ = std::fs::remove_file(&tmp);
            tracing::warn!(%error, path = %path.display(), "cannot write match outcome");
        }
    }
}
