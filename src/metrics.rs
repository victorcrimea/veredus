// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::LazyLock;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use prometheus::Encoder;
use prometheus::GaugeVec;
use prometheus::Histogram;
use prometheus::HistogramVec;
use prometheus::IntCounter;
use prometheus::IntCounterVec;
use prometheus::IntGauge;
use prometheus::IntGaugeVec;
use prometheus::TextEncoder;
use prometheus::register_gauge_vec;
use prometheus::register_histogram;
use prometheus::register_histogram_vec;
use prometheus::register_int_counter;
use prometheus::register_int_counter_vec;
use prometheus::register_int_gauge;
use prometheus::register_int_gauge_vec;
use rusty_enet::PeerID;
use rusty_enet::consts::PEER_PACKET_LOSS_SCALE;

use crate::relay::monitor::PeerStats;
use crate::relay::server_fsm::Counters;
use crate::relay::server_fsm::GameSnapshot;
use crate::relay::server_fsm::Phase;

// Every metric lives in the process-wide default registry, which is what the
// endpoint serves. Games share the registry but never a series: each game
// writes only under its own game_id and port labels, and every write is an
// atomic, so no game can see or block another's state.

const GAME_LABELS: &[&str] = &["game_id", "port"];

// Player names are client-supplied, so they are capped and stripped of control
// characters before they become part of a series name.
const MAX_LABEL_LEN: usize = 64;

// Seconds spent on work that competes with a 10 ms poll, so the buckets are
// fine at the low end and only coarse where a tick is already pathological.
const TICK_BUCKETS: &[f64] = &[
    0.0005, 0.001, 0.0025, 0.005, 0.01, 0.02, 0.04, 0.1, 0.4, 1.6,
];
const MATCH_SECONDS_BUCKETS: &[f64] = &[
    60.0, 300.0, 600.0, 900.0, 1200.0, 1800.0, 2700.0, 3600.0, 5400.0, 7200.0,
];
const MATCH_TURNS_BUCKETS: &[f64] = &[
    300.0, 1500.0, 3000.0, 4500.0, 6000.0, 9000.0, 13500.0, 18000.0, 27000.0, 36000.0,
];
// A sidecar step replays anything from a few turns to a whole match, and the
// AI host lives for the whole match, so the range is wide.
const SIDECAR_SECONDS_BUCKETS: &[f64] = &[
    1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0, 7200.0,
];
const SIDECAR_RSS_BUCKETS: &[f64] = &[1e7, 5e7, 1e8, 2.5e8, 5e8, 1e9, 2e9, 4e9];

pub static ACTIVE_GAMES: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!("active_games", "Games whose server thread is running").unwrap()
});

pub static GAMES_CREATED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!("games_created_total", "Games created since startup").unwrap()
});

// game_over: the game ended itself (idle, or the post-game linger ran out).
// shutdown: the pool tore it down. enet_closed: the socket thread went away
// without being asked to. panicked: the server thread panicked.
pub static GAMES_ENDED_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "games_ended_total",
        "Games that have ended, by how they ended",
        &["outcome"]
    )
    .unwrap()
});

pub static SERVER_TICK_DURATION: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "server_tick_duration_seconds",
        "Time one game-server loop iteration took, excluding its idle sleep",
        TICK_BUCKETS.to_vec()
    )
    .unwrap()
});

pub static GAME_DURATION_SECONDS: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "game_duration_seconds",
        "Wall-clock duration of a match, from the start of play to the end of its game",
        MATCH_SECONDS_BUCKETS.to_vec()
    )
    .unwrap()
});

pub static GAME_TURNS: LazyLock<Histogram> = LazyLock::new(|| {
    register_histogram!(
        "game_turns",
        "Final ready turn of a match when its game ended",
        MATCH_TURNS_BUCKETS.to_vec()
    )
    .unwrap()
});

pub static GAME_STATE: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        "game_state",
        "Current phase (0=Setup, 1=Loading, 2=InGame, 3=PostGame)",
        GAME_LABELS
    )
    .unwrap()
});

pub static CONNECTED_CLIENTS: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        "game_connected_clients",
        "Sessions, authenticated or not",
        GAME_LABELS
    )
    .unwrap()
});

pub static PLAYERS_TOTAL: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        "game_players_total",
        "Connected players holding a slot (observers excluded)",
        GAME_LABELS
    )
    .unwrap()
});

pub static PLAYERS_PAUSED: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        "game_players_paused",
        "Clients currently holding the match paused",
        GAME_LABELS
    )
    .unwrap()
});

pub static CURRENT_TURN: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!("game_current_turn", "Current ready turn", GAME_LABELS).unwrap()
});

pub static OBSERVER_DELAY_LAG_TURNS: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        "game_observer_delay_lag_turns",
        "Turns the delayed observer feed trails the live ready turn",
        GAME_LABELS
    )
    .unwrap()
});

pub static MESSAGES_RECEIVED_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_messages_received_total",
        "Decoded messages received from clients, by type",
        &["game_id", "port", "msg_type"]
    )
    .unwrap()
});

pub static UNDECODABLE_PACKETS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_undecodable_packets_total",
        "Packets received from clients that did not decode and were dropped",
        GAME_LABELS
    )
    .unwrap()
});

pub static CLIENT_CONNECTS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_client_connects_total",
        "ENet connections accepted",
        GAME_LABELS
    )
    .unwrap()
});

pub static CLIENT_DISCONNECTS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_client_disconnects_total",
        "ENet connections that ended, whichever side ended them",
        GAME_LABELS
    )
    .unwrap()
});

pub static DISCONNECTS_SENT_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_disconnects_sent_total",
        "Disconnects the server initiated (rejections, kicks, faults, shutdown), by reason",
        &["game_id", "port", "reason"]
    )
    .unwrap()
});

pub static BYTES_RECEIVED_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_bytes_received_total",
        "Bytes received from clients, ENet wire level including protocol overhead",
        GAME_LABELS
    )
    .unwrap()
});

pub static BYTES_SENT_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_bytes_sent_total",
        "Bytes sent to clients, ENet wire level including protocol overhead",
        GAME_LABELS
    )
    .unwrap()
});

pub static OOS_ERRORS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_oos_errors_total",
        "Turns on which the players' state hashes disagreed",
        GAME_LABELS
    )
    .unwrap()
});

pub static OBSERVER_OOS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "game_observer_oos_total",
        "Turns on which an observer disagreed with the players' agreed hash",
        GAME_LABELS
    )
    .unwrap()
});

// Global rather than per game: where joiners get their state from is a
// property of the deployment, not of one match.
// checkpoint: a stored sidecar checkpoint. dump: a one-shot sidecar replay.
// client: serialized by a live client.
pub static REJOIN_STATE_SOURCE_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "rejoin_state_source_total",
        "Snapshots delivered to joining clients, by where the state came from",
        &["source"]
    )
    .unwrap()
});

pub static CLIENT_ROUND_TRIP_TIME: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "game_client_round_trip_time_seconds",
        "Current ENet round-trip time estimate per client",
        &["game_id", "port", "player_name"]
    )
    .unwrap()
});

pub static CLIENT_PACKET_LOSS_RATIO: LazyLock<GaugeVec> = LazyLock::new(|| {
    register_gauge_vec!(
        "game_client_packet_loss_ratio",
        "Current ENet packet loss ratio (0-1) per client",
        &["game_id", "port", "player_name"]
    )
    .unwrap()
});

// spawned, spawn_failed, or exited (it stopped while the game still wanted it).
pub static AI_HOST_SIDECAR_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "ai_host_sidecar_total",
        "Hosted-AI sidecar lifecycle events, by outcome",
        &["outcome"]
    )
    .unwrap()
});

// checkpoint: the deciding checkpoint's result became final. replay: the
// outcome replay at the end of the game resolved it. failed: that replay did
// not produce a result.
pub static MATCH_OUTCOME_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "match_outcome_total",
        "Match outcome resolutions, by how they were resolved",
        &["outcome"]
    )
    .unwrap()
});

pub static SIDECAR_RUNS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "sidecar_runs_total",
        "Finished pyrogenesis runs, by step and whether they succeeded",
        &["step", "outcome"]
    )
    .unwrap()
});

pub static SIDECAR_RUN_CPU_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "sidecar_run_cpu_seconds",
        "CPU time (user+sys) of one pyrogenesis run, by step",
        &["step"],
        SIDECAR_SECONDS_BUCKETS.to_vec()
    )
    .unwrap()
});

pub static SIDECAR_RUN_WALL_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "sidecar_run_wall_seconds",
        "Wall-clock lifetime of one pyrogenesis run, by step, at one-second resolution",
        &["step"],
        SIDECAR_SECONDS_BUCKETS.to_vec()
    )
    .unwrap()
});

pub static SIDECAR_RUN_MAX_RSS_BYTES: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        "sidecar_run_max_rss_bytes",
        "Peak resident memory of one pyrogenesis run, by step",
        &["step"],
        SIDECAR_RSS_BUCKETS.to_vec()
    )
    .unwrap()
});

pub static LOBBY_ACCOUNTS: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!("lobby_accounts", "XMPP accounts in the lobby pool").unwrap()
});

pub static LOBBY_ACCOUNTS_BUSY: LazyLock<IntGauge> = LazyLock::new(|| {
    register_int_gauge!(
        "lobby_accounts_busy",
        "Lobby accounts reserved for or hosting a game"
    )
    .unwrap()
});

// hosted, duplicate_sender (that sender already has a game), account_busy
// (the account that saw it was taken), failed (hashing or game creation).
pub static LOBBY_HOSTME_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "lobby_hostme_total",
        "hostme requests seen by a lobby account, by what came of them",
        &["outcome"]
    )
    .unwrap()
});

pub static LOBBY_SESSIONS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "lobby_sessions_total",
        "XMPP sessions that came online, by whether the stream was resumed",
        &["resumed"]
    )
    .unwrap()
});

pub static LOBBY_STREAM_ENDED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "lobby_stream_ended_total",
        "XMPP streams that ended without being asked to"
    )
    .unwrap()
});

// ok, no_game, banned, wrong_password, error.
pub static LOBBY_CONNECTION_DATA_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        "lobby_connection_data_total",
        "connection-data requests answered, by outcome",
        &["outcome"]
    )
    .unwrap()
});

pub static LOBBY_AUTH_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "lobby_auth_total",
        "Lobby-auth tokens received and forwarded to a game"
    )
    .unwrap()
});

// Global rather than per-game: both are counted on the ENet thread, which
// has no GameMetrics of its own (that is built on the server thread, A5).
pub static ENET_INBOUND_DROPPED_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "enet_inbound_dropped_packets_total",
        "Packets dropped because the sending peer's undelivered inbound backlog was full"
    )
    .unwrap()
});

pub static ENET_SLOW_PEER_DISCONNECTS_TOTAL: LazyLock<IntCounter> = LazyLock::new(|| {
    register_int_counter!(
        "enet_slow_peer_disconnects_total",
        "Peers disconnected because their outgoing queue stayed over the cap"
    )
    .unwrap()
});

// The statics register on first use, so without this a scrape before the
// first game or lobby event would be missing families rather than showing 0.
pub fn init() {
    LazyLock::force(&ACTIVE_GAMES);
    LazyLock::force(&GAMES_CREATED_TOTAL);
    LazyLock::force(&SERVER_TICK_DURATION);
    LazyLock::force(&GAME_DURATION_SECONDS);
    LazyLock::force(&GAME_TURNS);
    LazyLock::force(&LOBBY_ACCOUNTS);
    LazyLock::force(&LOBBY_ACCOUNTS_BUSY);
    LazyLock::force(&ENET_INBOUND_DROPPED_TOTAL);
    LazyLock::force(&ENET_SLOW_PEER_DISCONNECTS_TOTAL);
    LazyLock::force(&LOBBY_STREAM_ENDED_TOTAL);
    LazyLock::force(&LOBBY_AUTH_TOTAL);
}

pub fn encode() -> String {
    let mut buf = Vec::new();
    // Writing into a Vec cannot fail; only a malformed family could, and
    // every family here is registered through the checked macros.
    if let Err(error) = TextEncoder::new().encode(&prometheus::gather(), &mut buf) {
        tracing::warn!(%error, "failed to encode metrics");
    }
    String::from_utf8_lossy(&buf).into_owned()
}

pub fn sanitize_label(value: &str) -> String {
    value
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_LABEL_LEN)
        .collect()
}

// One finished pyrogenesis run. The wall time is whole seconds because the
// sidecar reads no clock of its own and takes it from the process table.
pub fn sidecar_run(step: &str, ok: bool, cpu_seconds: f64, run_seconds: u64, peak_rss_bytes: u64) {
    let outcome = if ok { "ok" } else { "failed" };
    SIDECAR_RUNS_TOTAL.with_label_values(&[step, outcome]).inc();
    SIDECAR_RUN_CPU_SECONDS
        .with_label_values(&[step])
        .observe(cpu_seconds);
    SIDECAR_RUN_WALL_SECONDS
        .with_label_values(&[step])
        .observe(run_seconds as f64);
    SIDECAR_RUN_MAX_RSS_BYTES
        .with_label_values(&[step])
        .observe(peak_rss_bytes as f64);
}

// A game's own series, owned by its server thread for the thread's whole
// life. Dropping it removes every series labelled with this game, so a long
// running server does not keep one dead series per match it ever hosted.
pub struct GameMetrics {
    game_id: String,
    port: String,
    state: IntGauge,
    clients: IntGauge,
    players: IntGauge,
    paused: IntGauge,
    turn: IntGauge,
    feed_lag: IntGauge,
    undecodable: IntCounter,
    connects: IntCounter,
    disconnects: IntCounter,
    bytes_received: IntCounter,
    bytes_sent: IntCounter,
    oos: IntCounter,
    observer_oos: IntCounter,
    // Resolved on first use, so only the types and reasons that occurred get
    // a series, and remembered so Drop knows what to remove.
    messages: HashMap<&'static str, IntCounter>,
    reasons: HashMap<&'static str, IntCounter>,
    player_names: HashSet<String>,
    // The FSM and ENet report running totals; these are the last ones seen,
    // so each report adds only what is new.
    last_counters: Counters,
    last_bytes: Option<(u32, u32)>,
    match_started: Option<DateTime<Utc>>,
    last_turn: u32,
    ended: bool,
}

impl GameMetrics {
    pub fn new(game_id: &str, port: u16) -> Self {
        let port = port.to_string();
        let labels = [game_id, port.as_str()];
        GAMES_CREATED_TOTAL.inc();
        ACTIVE_GAMES.inc();
        GameMetrics {
            state: GAME_STATE.with_label_values(&labels),
            clients: CONNECTED_CLIENTS.with_label_values(&labels),
            players: PLAYERS_TOTAL.with_label_values(&labels),
            paused: PLAYERS_PAUSED.with_label_values(&labels),
            turn: CURRENT_TURN.with_label_values(&labels),
            feed_lag: OBSERVER_DELAY_LAG_TURNS.with_label_values(&labels),
            undecodable: UNDECODABLE_PACKETS_TOTAL.with_label_values(&labels),
            connects: CLIENT_CONNECTS_TOTAL.with_label_values(&labels),
            disconnects: CLIENT_DISCONNECTS_TOTAL.with_label_values(&labels),
            bytes_received: BYTES_RECEIVED_TOTAL.with_label_values(&labels),
            bytes_sent: BYTES_SENT_TOTAL.with_label_values(&labels),
            oos: OOS_ERRORS_TOTAL.with_label_values(&labels),
            observer_oos: OBSERVER_OOS_TOTAL.with_label_values(&labels),
            game_id: game_id.to_string(),
            port,
            messages: HashMap::new(),
            reasons: HashMap::new(),
            player_names: HashSet::new(),
            last_counters: Counters::default(),
            last_bytes: None,
            match_started: None,
            last_turn: 0,
            ended: false,
        }
    }

    // Only the first call counts, so a game cannot be counted as ending twice.
    pub fn ended(&mut self, outcome: &str) {
        if !std::mem::replace(&mut self.ended, true) {
            GAMES_ENDED_TOTAL.with_label_values(&[outcome]).inc();
        }
    }

    pub fn tick(&self, duration: TimeDelta) {
        // A wall clock stepping backwards mid-tick yields nothing worth keeping.
        if duration >= TimeDelta::zero() {
            SERVER_TICK_DURATION.observe(seconds(duration));
        }
    }

    pub fn message_received(&mut self, name: &'static str) {
        let (game_id, port) = (&self.game_id, &self.port);
        self.messages
            .entry(name)
            .or_insert_with(|| MESSAGES_RECEIVED_TOTAL.with_label_values(&[game_id, port, name]))
            .inc();
    }

    pub fn undecodable(&self) {
        self.undecodable.inc();
    }

    pub fn connected(&self) {
        self.connects.inc();
    }

    pub fn disconnected(&self) {
        self.disconnects.inc();
    }

    pub fn disconnect_sent(&mut self, reason: &'static str) {
        let (game_id, port) = (&self.game_id, &self.port);
        self.reasons
            .entry(reason)
            .or_insert_with(|| DISCONNECTS_SENT_TOTAL.with_label_values(&[game_id, port, reason]))
            .inc();
    }

    pub fn bytes(&mut self, received: u32, sent: u32) {
        let (last_received, last_sent) = self.last_bytes.unwrap_or((0, 0));
        // The totals are u32 and wrap, which a wrapping difference absorbs.
        self.bytes_received
            .inc_by(u64::from(received.wrapping_sub(last_received)));
        self.bytes_sent
            .inc_by(u64::from(sent.wrapping_sub(last_sent)));
        self.last_bytes = Some((received, sent));
    }

    // `rtt` and `loss` are the latest ENet samples; clients the snapshot no
    // longer lists lose their series here.
    pub fn observe(
        &mut self,
        snapshot: &GameSnapshot,
        rtt: &[PeerStats],
        loss: &[(PeerID, u32)],
        now: DateTime<Utc>,
    ) {
        self.state.set(phase_value(snapshot.phase));
        self.clients.set(snapshot.sessions as i64);
        self.players.set(snapshot.players as i64);
        self.paused.set(snapshot.paused as i64);
        self.turn.set(i64::from(snapshot.ready_turn));
        self.feed_lag.set(i64::from(snapshot.feed_lag.unwrap_or(0)));
        self.last_turn = snapshot.ready_turn;
        if matches!(snapshot.phase, Phase::InGame | Phase::PostGame) {
            self.match_started.get_or_insert(now);
        }

        let counters = snapshot.counters;
        let last = std::mem::replace(&mut self.last_counters, counters);
        self.oos
            .inc_by(counters.hash_mismatches - last.hash_mismatches);
        self.observer_oos
            .inc_by(counters.observer_hash_mismatches - last.observer_hash_mismatches);
        for (source, now_count, last_count) in [
            (
                "checkpoint",
                counters.join_from_checkpoint,
                last.join_from_checkpoint,
            ),
            ("dump", counters.join_from_dump, last.join_from_dump),
            ("client", counters.join_from_client, last.join_from_client),
        ] {
            if now_count > last_count {
                REJOIN_STATE_SOURCE_TOTAL
                    .with_label_values(&[source])
                    .inc_by(now_count - last_count);
            }
        }

        let mut present = HashSet::new();
        for (peer, name) in &snapshot.clients {
            let name = sanitize_label(name);
            let labels = [self.game_id.as_str(), self.port.as_str(), name.as_str()];
            if let Some(sample) = rtt.iter().find(|s| s.peer == *peer) {
                CLIENT_ROUND_TRIP_TIME
                    .with_label_values(&labels)
                    .set(seconds(sample.mean_rtt));
            }
            if let Some((_, loss)) = loss.iter().find(|(p, _)| p == peer) {
                CLIENT_PACKET_LOSS_RATIO
                    .with_label_values(&labels)
                    .set(f64::from(*loss) / f64::from(PEER_PACKET_LOSS_SCALE));
            }
            present.insert(name);
        }
        for gone in self.player_names.difference(&present) {
            self.remove_player(gone);
        }
        self.player_names = present;
    }

    // The match figures are taken once, when the game is done with the match.
    pub fn finish(&mut self, now: DateTime<Utc>) {
        let Some(started) = self.match_started.take() else {
            return;
        };
        let elapsed = now.signed_duration_since(started);
        if elapsed >= TimeDelta::zero() {
            GAME_DURATION_SECONDS.observe(seconds(elapsed));
        }
        GAME_TURNS.observe(f64::from(self.last_turn));
    }

    fn remove_player(&self, name: &str) {
        let labels = [self.game_id.as_str(), self.port.as_str(), name];
        let _ = CLIENT_ROUND_TRIP_TIME.remove_label_values(&labels);
        let _ = CLIENT_PACKET_LOSS_RATIO.remove_label_values(&labels);
    }
}

impl Drop for GameMetrics {
    fn drop(&mut self) {
        ACTIVE_GAMES.dec();
        let labels = [self.game_id.as_str(), self.port.as_str()];
        for gauge in [
            &GAME_STATE,
            &CONNECTED_CLIENTS,
            &PLAYERS_TOTAL,
            &PLAYERS_PAUSED,
            &CURRENT_TURN,
            &OBSERVER_DELAY_LAG_TURNS,
        ] {
            let _ = gauge.remove_label_values(&labels);
        }
        for counter in [
            &UNDECODABLE_PACKETS_TOTAL,
            &CLIENT_CONNECTS_TOTAL,
            &CLIENT_DISCONNECTS_TOTAL,
            &BYTES_RECEIVED_TOTAL,
            &BYTES_SENT_TOTAL,
            &OOS_ERRORS_TOTAL,
            &OBSERVER_OOS_TOTAL,
        ] {
            let _ = counter.remove_label_values(&labels);
        }
        for name in self.messages.keys() {
            let _ = MESSAGES_RECEIVED_TOTAL.remove_label_values(&[labels[0], labels[1], name]);
        }
        for reason in self.reasons.keys() {
            let _ = DISCONNECTS_SENT_TOTAL.remove_label_values(&[labels[0], labels[1], reason]);
        }
        for name in &self.player_names {
            self.remove_player(name);
        }
    }
}

fn phase_value(phase: Phase) -> i64 {
    match phase {
        Phase::Setup => 0,
        Phase::Loading => 1,
        Phase::InGame => 2,
        Phase::PostGame => 3,
    }
}

fn seconds(delta: TimeDelta) -> f64 {
    delta.num_microseconds().unwrap_or(i64::MAX) as f64 / 1_000_000.0
}
