// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::Path;
use std::process::Child;
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::Sender;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use sysinfo::Pid;
use sysinfo::ProcessRefreshKind;
use sysinfo::ProcessesToUpdate;
use sysinfo::Signal;
use sysinfo::System;

use crate::relay::messages::EnabledMod;
use crate::relay::messages::PlayerCommand;
use crate::relay::turn::needs_full_hash;

const DECODE_ERROR_MARKER: &str = "{\"__decode_error__\":true}";
const REPLAY_RESULT_PREFIX: &str = "REPLAY_RESULT ";

// How often a running engine step is polled for exit. Short enough that a
// cancelled run dies promptly, long enough that the dump thread spends its
// time asleep rather than spinning on the child.
const STEP_POLL: Duration = Duration::from_millis(50);
// Each engine step (decode, replay dump) gets this long before it is killed.
// Too short and a long match can never be rebuilt; too long and one hung run
// blocks every joiner in its game, since only one run is allowed at a time.
const STEP_TIMEOUT: Duration = Duration::from_secs(600);
// The outcome replay runs the whole match, not just up to a joiner's turn,
// and nobody is waiting on it, so it gets far longer than a dump before it is
// given up on.
const OUTCOME_TIMEOUT: Duration = Duration::from_secs(1800);
// A one-shot run is sampled once a second: often enough to catch the peak of
// a run lasting seconds, rare enough to cost nothing next to the replay.
const RUN_SAMPLE_POLLS: u64 = 20;
// The AI host is sampled every 5 s, so its final figures are never far
// behind, but a match-long process changes slowly, so only every 30 s becomes
// a debug line.
const AI_HOST_SAMPLE_POLLS: u64 = 100;
const AI_HOST_LOG_POLLS: u64 = 600;
// How long the AI host gets to leave on its own after being asked to stop,
// before it is killed outright.
const TERM_GRACE_POLLS: u64 = 60;

// Everything the one-shot replay needs to rebuild the state at turn T:
// the frozen settings, the engine version the clients run, the mods in load
// order, the turn lengths for wire turns 1..=T and the commands for the same
// turns, in order. Plain data so it can ride inside an Effect.
#[derive(Debug, Clone, PartialEq)]
pub struct DumpRequest {
    pub init_attributes: Vec<u8>,
    pub engine_version: String,
    pub mods: Vec<EnabledMod>,
    pub turn_lengths: Vec<u16>,
    pub commands: Vec<PlayerCommand>,
    // The players' agreed hash per wire turn, in turn order. The replay checks
    // itself against these, which is the only way to notice that it runs a
    // different build from the clients. Left empty for a dump, which would
    // otherwise pay for a full state hash every 20 turns for nothing.
    pub hashes: Vec<(u32, Vec<u8>)>,
}

#[derive(Debug, thiserror::Error)]
pub enum SidecarError {
    #[error("sidecar {context}: {error}")]
    Io {
        context: String,
        #[source]
        error: std::io::Error,
    },
    #[error("sidecar {step} exited with {status}")]
    Status {
        step: &'static str,
        status: std::process::ExitStatus,
    },
    #[error("sidecar decode returned {got} lines, expected {expected}")]
    LineCount { expected: usize, got: usize },
    #[error("sidecar settings are not valid JSON: {0}")]
    SettingsJson(#[from] serde_json::Error),
    #[error("sidecar settings JSON root is not an object")]
    BadSettings,
    #[error("sidecar cannot dump state at turn 0")]
    NoTurn,
    #[error("sidecar {step} timed out")]
    TimedOut { step: &'static str },
    #[error("sidecar step cancelled")]
    Cancelled,
    #[error("sidecar replay printed no result")]
    NoResult,
    #[error("sidecar replay result is not valid JSON: {0}")]
    ResultJson(#[source] serde_json::Error),
}

// What the replay reports once the last turn has run. Only the fields the
// relay reads are modelled; the complete JSON is kept as text for the record.
#[derive(Debug, Deserialize)]
pub struct ReplayResult {
    #[serde(rename = "timeElapsed", default)]
    pub time_elapsed: f64,
    #[serde(rename = "playerStates", default)]
    pub player_states: Vec<PlayerState>,
}

// Indexed by player id, so entry 0 is Gaia.
#[derive(Debug, Deserialize)]
pub struct PlayerState {
    #[serde(default)]
    pub name: Option<String>,
    // "active", "won" or "defeated".
    #[serde(default)]
    pub state: String,
}

// What a process cost, as last seen. CPU time is only as fresh as the last
// sample: once the child has been reaped there is nothing left to read, and
// the exact figure would need a platform-specific wait.
#[derive(Debug, Clone, Copy, Default)]
struct Usage {
    cpu_seconds: f64,
    peak_rss_bytes: u64,
    run_seconds: u64,
}

// Samples one child through the OS process table, which works the same way
// on every platform the relay builds for.
struct ResourceWatch {
    system: System,
    pid: Pid,
    usage: Usage,
}

impl ResourceWatch {
    fn new(pid: u32) -> Self {
        ResourceWatch {
            system: System::new(),
            pid: Pid::from_u32(pid),
            usage: Usage::default(),
        }
    }

    fn sample(&mut self) -> Usage {
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing().with_memory().with_cpu(),
        );
        if let Some(process) = self.system.process(self.pid) {
            self.usage.cpu_seconds = process.accumulated_cpu_time() as f64 / 1000.0;
            // A zombie reports no memory, so the peak is kept, not the last.
            self.usage.peak_rss_bytes = self.usage.peak_rss_bytes.max(process.memory());
            self.usage.run_seconds = process.run_time();
        }
        self.usage
    }

    // False where the platform has no such signal, or the process is gone.
    fn signal(&self, signal: Signal) -> bool {
        self.system
            .process(self.pid)
            .and_then(|process| process.kill_with(signal))
            .unwrap_or(false)
    }
}

// Asks the kernel to kill the child when the thread that spawned it exits, so
// a relay killed outright does not leave engines running with nobody to reap
// them. The signal is tied to the spawning thread, not the process, so every
// child here is spawned by a thread that outlives it. Other platforms have no
// such guard and keep an orphan running until it exits on its own.
#[cfg(target_os = "linux")]
fn guard_orphan(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    let parent = std::process::id() as libc::pid_t;
    // SAFETY: the closure runs in the forked child before exec and calls only
    // prctl and getppid, both async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            // The parent may have died between fork and prctl, in which case
            // the signal would never come.
            if libc::getppid() != parent {
                return Err(std::io::Error::other("parent exited before exec"));
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn guard_orphan(_cmd: &mut Command) {}

// The long-lived hosted-AI client for one match: a headless pyrogenesis that
// joins the relay like any stock client and sends the AI players' commands.
// The process belongs to a supervisor thread, so stopping it never stalls the
// game thread. Dropping this handle is the stop request.
pub struct AiHostProcess {
    _stop_tx: Sender<()>,
    exit_rx: Receiver<String>,
}

impl AiHostProcess {
    pub fn spawn(
        pyrogenesis: &Path,
        addr: Ipv4Addr,
        port: u16,
        name: &str,
    ) -> Result<Self, SidecarError> {
        let mut cmd = Command::new(pyrogenesis);
        // No replay of its own: the relay already keeps the match log, and a
        // sidecar writing one for every hosted match would only fill the disk.
        cmd.arg(format!("-autostart-client={addr}"))
            .arg(format!("-autostart-port={port}"))
            .arg("-autostart-ai-host")
            .arg("-autostart-nonvisual")
            .arg("-autostart-disable-replay")
            .arg(format!("-autostart-playername={name}"))
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        guard_orphan(&mut cmd);

        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        let (exit_tx, exit_rx) = std::sync::mpsc::channel::<String>();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::io::Result<u32>>();
        let span = tracing::Span::current();
        // The child is spawned on the supervisor itself, because the orphan
        // guard ends it with whichever thread spawned it.
        std::thread::spawn(move || {
            let _guard = span.entered();
            supervise_ai_host(cmd, stop_rx, ready_tx, exit_tx);
        });
        let spawned = ready_rx
            .recv()
            .unwrap_or_else(|_| Err(std::io::Error::other("supervisor ended before spawning")));
        let pid = spawned.map_err(|error| SidecarError::Io {
            context: "spawning AI host".to_string(),
            error,
        })?;
        tracing::info!(pid, %addr, port, name, "sidecar: AI host spawned");
        Ok(AiHostProcess {
            _stop_tx: stop_tx,
            exit_rx,
        })
    }

    // None while it is still running. A supervisor that is gone without a
    // word is reported as an exit: its child cannot outlive it.
    pub fn poll_exit(&mut self) -> Option<String> {
        match self.exit_rx.try_recv() {
            Ok(status) => Some(status),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some("supervisor ended".to_string()),
        }
    }
}

fn supervise_ai_host(
    mut cmd: Command,
    stop_rx: Receiver<()>,
    ready_tx: Sender<std::io::Result<u32>>,
    exit_tx: Sender<String>,
) {
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = ready_tx.send(Err(error));
            return;
        }
    };
    let pid = child.id();
    let _ = ready_tx.send(Ok(pid));
    let pumps = [
        spawn_line_pump(child.stdout.take().expect("stdout was piped")),
        spawn_line_pump(child.stderr.take().expect("stderr was piped")),
    ];
    let mut watch = ResourceWatch::new(pid);

    let mut polls: u64 = 0;
    let (status, stopped) = loop {
        if polls.is_multiple_of(AI_HOST_SAMPLE_POLLS) {
            watch.sample();
        }
        if polls.is_multiple_of(AI_HOST_LOG_POLLS) {
            let usage = watch.usage;
            tracing::debug!(
                pid,
                cpu_seconds = usage.cpu_seconds,
                rss_peak_bytes = usage.peak_rss_bytes,
                run_seconds = usage.run_seconds,
                "sidecar: AI host usage"
            );
        }
        match child.try_wait() {
            Ok(Some(status)) => break (status.to_string(), false),
            Ok(None) => {}
            Err(error) => {
                // A child that can no longer be polled is killed, so it is at
                // least reaped rather than left behind.
                let _ = child.kill();
                let _ = child.wait();
                break (error.to_string(), false);
            }
        }
        // The handle is dropped to stop it; a message would mean the same.
        if !matches!(stop_rx.try_recv(), Err(TryRecvError::Empty)) {
            watch.sample();
            break (terminate(&mut child, &watch), true);
        }
        std::thread::sleep(STEP_POLL);
        polls += 1;
    };

    // Once the child is reaped its pipes read EOF, so the pumps end.
    for pump in pumps {
        let _ = pump.join();
    }
    let usage = watch.usage;
    tracing::info!(
        pid,
        %status,
        stopped,
        cpu_seconds = usage.cpu_seconds,
        peak_rss_bytes = usage.peak_rss_bytes,
        run_seconds = usage.run_seconds,
        "sidecar: AI host ended"
    );
    if !stopped {
        let _ = exit_tx.send(status);
    }
}

// A polite request first, where the platform has one, so the engine can drop
// its connection cleanly; a kill once the grace runs out, because nothing it
// holds is worth waiting longer for.
fn terminate(child: &mut Child, watch: &ResourceWatch) -> String {
    if watch.signal(Signal::Term) {
        for _ in 0..TERM_GRACE_POLLS {
            match child.try_wait() {
                Ok(Some(status)) => return status.to_string(),
                Ok(None) => std::thread::sleep(STEP_POLL),
                Err(_) => break,
            }
        }
        tracing::debug!("sidecar: AI host ignored TERM, killing it");
    }
    let _ = child.kill();
    match child.wait() {
        Ok(status) => status.to_string(),
        Err(error) => error.to_string(),
    }
}

// A match lasts far longer than a pipe buffer, so the output is drained line
// by line as it comes. The spawning thread's span is carried over so every
// line is filed under its game.
fn spawn_line_pump(pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<()> {
    let span = tracing::Span::current();
    std::thread::spawn(move || {
        let _guard = span.entered();
        for line in std::io::BufReader::new(pipe).lines() {
            match line {
                Ok(line) => tracing::debug!("sidecar: {line}"),
                Err(_) => break,
            }
        }
    })
}

// Replays the recorded match through a one-shot pyrogenesis and returns the
// serialized state at wire turn `turn`, as the same compressed, turn-prefixed
// buffer a client sends, so it stays opaque to the relay. `now` is passed in
// so this module never reads the clock. `cancel` is polled while each engine
// step runs, so a joiner that left no longer burns a whole replay.
pub fn dump_state(
    pyrogenesis: &Path,
    dir: &Path,
    turn: u32,
    request: &DumpRequest,
    now: DateTime<Utc>,
    cancel: &AtomicBool,
) -> Result<Vec<u8>, SidecarError> {
    if turn < 1 {
        return Err(SidecarError::NoTurn);
    }
    std::fs::create_dir_all(dir).map_err(|error| SidecarError::Io {
        context: format!("creating {}", dir.display()),
        error,
    })?;

    let decoded = decode_commands(pyrogenesis, dir, &request.commands, cancel)?;
    let commands_txt = dir.join("commands.txt");
    write_commands_txt(&commands_txt, request, &decoded, now)?;

    // The replay labels the block executed to reach wire turn N as N-1, since
    // wire turn 0 never gets its own step, so the state at wire turn N is the
    // state after that block runs.
    let turn_label = turn - 1;
    let dump_path = dir.join("state.bin");
    run_dump_cli(pyrogenesis, &commands_txt, turn_label, &dump_path, cancel)?;
    std::fs::read(&dump_path).map_err(|error| SidecarError::Io {
        context: format!("reading {}", dump_path.display()),
        error,
    })
}

// Replays the whole recorded match through a one-shot pyrogenesis and
// returns what the engine's own victory logic made of it, together with the
// raw result JSON for the record.
pub fn resolve_outcome(
    pyrogenesis: &Path,
    dir: &Path,
    request: &DumpRequest,
    now: DateTime<Utc>,
    cancel: &AtomicBool,
) -> Result<(ReplayResult, String), SidecarError> {
    std::fs::create_dir_all(dir).map_err(|error| SidecarError::Io {
        context: format!("creating {}", dir.display()),
        error,
    })?;

    let decoded = decode_commands(pyrogenesis, dir, &request.commands, cancel)?;
    let commands_txt = dir.join("commands.txt");
    write_commands_txt(&commands_txt, request, &decoded, now)?;

    let mut cmd = Command::new(pyrogenesis);
    // Quick hashes are skipped by default, and they are most of what the
    // players agreed on.
    cmd.arg(format!("-replay={}", commands_txt.display()))
        .arg("-hashtest-quick=true");
    let output = run_logged(&mut cmd, "outcome", OUTCOME_TIMEOUT, cancel)?;
    if !output.status.success() {
        return Err(SidecarError::Status {
            step: "outcome",
            status: output.status,
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let mismatch_count = stdout
        .lines()
        .chain(stderr.lines())
        .filter(|line| line.contains("MISMATCH"))
        .count();
    if mismatch_count > 0 {
        // The outcome is still reported, but it may not be the match that
        // was played: the replay drifted from what the players agreed on.
        tracing::warn!(
            mismatch_count,
            "sidecar: outcome replay diverged from the live match"
        );
    }

    let line = stdout
        .lines()
        .find_map(|line| line.strip_prefix(REPLAY_RESULT_PREFIX))
        .ok_or(SidecarError::NoResult)?;
    let result = serde_json::from_str(line).map_err(SidecarError::ResultJson)?;
    Ok((result, line.to_string()))
}

fn decode_commands(
    pyrogenesis: &Path,
    dir: &Path,
    commands: &[PlayerCommand],
    cancel: &AtomicBool,
) -> Result<Vec<String>, SidecarError> {
    use base64::Engine as _;
    let input_path = dir.join("decode_in.b64");
    let output_path = dir.join("decode_out.json");

    let mut input = std::fs::File::create(&input_path).map_err(|error| SidecarError::Io {
        context: format!("creating {}", input_path.display()),
        error,
    })?;
    for command in commands {
        let encoded = base64::engine::general_purpose::STANDARD.encode(&command.data);
        writeln!(input, "{encoded}").map_err(|error| SidecarError::Io {
            context: format!("writing {}", input_path.display()),
            error,
        })?;
    }
    drop(input);

    let mut cmd = Command::new(pyrogenesis);
    cmd.arg(format!("-decode-script-vals-in={}", input_path.display()))
        .arg(format!("-decode-script-vals-out={}", output_path.display()));
    let output = run_logged(&mut cmd, "decode", STEP_TIMEOUT, cancel)?;
    if !output.status.success() {
        return Err(SidecarError::Status {
            step: "decode",
            status: output.status,
        });
    }

    let text = std::fs::read_to_string(&output_path).map_err(|error| SidecarError::Io {
        context: format!("reading {}", output_path.display()),
        error,
    })?;
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    if lines.len() != commands.len() {
        return Err(SidecarError::LineCount {
            expected: commands.len(),
            got: lines.len(),
        });
    }
    for line in &lines {
        if line == DECODE_ERROR_MARKER {
            // One command failed to decode but the rest are usable, so the
            // replay still runs with the marker in place of that command.
            tracing::warn!("sidecar: command failed to decode");
        }
    }
    Ok(lines)
}

fn write_commands_txt(
    path: &Path,
    request: &DumpRequest,
    decoded: &[String],
    now: DateTime<Utc>,
) -> Result<(), SidecarError> {
    let mut out = std::fs::File::create(path).map_err(|error| SidecarError::Io {
        context: format!("creating {}", path.display()),
        error,
    })?;
    let start_json = build_start_json(request, now)?;
    writeln!(out, "start {start_json}").map_err(|error| SidecarError::Io {
        context: format!("writing {}", path.display()),
        error,
    })?;

    // One cursor each into the turn-ordered commands and hashes: each turn
    // writes the lines at their heads and moves on, so the whole file is a
    // single pass over the turns plus a single pass over each list.
    let mut idx = 0;
    let mut hash_idx = 0;
    for (index, length) in request.turn_lengths.iter().enumerate() {
        let wire_turn = index as u32 + 1;
        writeln!(out, "turn {} {length}", wire_turn - 1).map_err(|error| SidecarError::Io {
            context: format!("writing {}", path.display()),
            error,
        })?;
        while idx < request.commands.len() && request.commands[idx].turn == wire_turn {
            writeln!(out, "cmd {} {}", request.commands[idx].player, decoded[idx]).map_err(
                |error| SidecarError::Io {
                    context: format!("writing {}", path.display()),
                    error,
                },
            )?;
            idx += 1;
        }
        writeln!(out, "end").map_err(|error| SidecarError::Io {
            context: format!("writing {}", path.display()),
            error,
        })?;
        // A hash is checked right after the block that reaches its turn. The
        // full or quick kind is not on the wire; each client derives it from
        // the turn number, and so does the replay's label.
        while hash_idx < request.hashes.len() && request.hashes[hash_idx].0 <= wire_turn {
            let (turn, hash) = &request.hashes[hash_idx];
            hash_idx += 1;
            if *turn != wire_turn {
                continue;
            }
            let label = if needs_full_hash(*turn) {
                "hash"
            } else {
                "hash-quick"
            };
            writeln!(out, "{label} {}", hex::encode(hash)).map_err(|error| SidecarError::Io {
                context: format!("writing {}", path.display()),
                error,
            })?;
        }
    }
    Ok(())
}

// The start line is the frozen settings plus the fields the engine logger
// adds when it opens a replay.
fn build_start_json(request: &DumpRequest, now: DateTime<Utc>) -> Result<String, SidecarError> {
    let mut attribs: serde_json::Value = serde_json::from_slice(&request.init_attributes)?;
    let Some(obj) = attribs.as_object_mut() else {
        return Err(SidecarError::BadSettings);
    };
    obj.insert("timestamp".to_string(), serde_json::json!(now.timestamp()));
    obj.insert(
        "engine_serialization_version".to_string(),
        serde_json::json!(request.engine_version),
    );
    obj.insert(
        "mods".to_string(),
        serde_json::json!(
            request
                .mods
                .iter()
                .map(|m| match mod_pathname(&m.name) {
                    Some(pathname) => {
                        serde_json::json!({"mod": pathname, "name": m.name, "version": m.version})
                    }
                    None => serde_json::json!({"name": m.name, "version": m.version}),
                })
                .collect::<Vec<_>>()
        ),
    );
    serde_json::to_string(&attribs).map_err(SidecarError::SettingsJson)
}

// The wire carries only a mod name and version, never the folder the engine
// mounts, so the one mod this server runs maps back to its folder here.
fn mod_pathname(name: &str) -> Option<&'static str> {
    match name {
        "0ad" => Some("public"),
        _ => None,
    }
}

fn run_dump_cli(
    pyrogenesis: &Path,
    commands_txt: &Path,
    turn_label: u32,
    dump_path: &Path,
    cancel: &AtomicBool,
) -> Result<(), SidecarError> {
    let mut cmd = Command::new(pyrogenesis);
    cmd.arg(format!("-replay={}", commands_txt.display()))
        .arg(format!("-dump-state-at-turn={turn_label}"))
        .arg(format!("-dump-state-out={}", dump_path.display()));
    let output = run_logged(&mut cmd, "dump", STEP_TIMEOUT, cancel)?;
    if !output.status.success() {
        return Err(SidecarError::Status {
            step: "dump",
            status: output.status,
        });
    }
    Ok(())
}

// Runs the child with its pipes drained on reader threads, so a chatty engine
// can never block on a full pipe buffer, and polls it to completion. The
// child's output and its cost are folded into the log, so a slow invocation
// is visible in the same sinks as everything else.
fn run_logged(
    cmd: &mut Command,
    step: &'static str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<std::process::Output, SidecarError> {
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    guard_orphan(cmd);
    let mut child = cmd.spawn().map_err(|error| SidecarError::Io {
        context: format!("spawning sidecar {step}"),
        error,
    })?;
    let pid = child.id();
    let stdout_reader = spawn_pipe_reader(child.stdout.take().expect("stdout was piped"));
    let stderr_reader = spawn_pipe_reader(child.stderr.take().expect("stderr was piped"));
    let mut watch = ResourceWatch::new(pid);
    let status = reap_child(&mut child, step, timeout, cancel, &mut watch);
    // Joined after the child is reaped on every path: once it is dead the
    // pipes read EOF, so the readers always terminate.
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    for line in String::from_utf8_lossy(&stdout).lines() {
        // The result line runs to tens of kilobytes, past what a log sink
        // accepts as one line; the caller reports it in its own terms.
        if line.starts_with(REPLAY_RESULT_PREFIX) {
            continue;
        }
        tracing::debug!(step, "sidecar: {line}");
    }
    for line in String::from_utf8_lossy(&stderr).lines() {
        tracing::debug!(step, "sidecar: {line}");
    }
    let usage = watch.usage;
    tracing::info!(
        step,
        pid,
        ok = status.as_ref().is_ok_and(|s| s.success()),
        cpu_seconds = usage.cpu_seconds,
        peak_rss_bytes = usage.peak_rss_bytes,
        run_seconds = usage.run_seconds,
        "sidecar: step finished"
    );
    Ok(std::process::Output {
        status: status?,
        stdout,
        stderr,
    })
}

fn spawn_pipe_reader(pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::BufReader::new(pipe).read_to_end(&mut buf);
        buf
    })
}

// Polls the child until it exits, is cancelled, or outlasts its budget. The
// budget counts sleeps rather than reading the clock: oversleeping only makes
// the real limit a little longer, never shorter.
fn reap_child(
    child: &mut Child,
    step: &'static str,
    timeout: Duration,
    cancel: &AtomicBool,
    watch: &mut ResourceWatch,
) -> Result<std::process::ExitStatus, SidecarError> {
    let max_polls = (timeout.as_millis() / STEP_POLL.as_millis()) as u64;
    let mut polls: u64 = 0;
    loop {
        if polls.is_multiple_of(RUN_SAMPLE_POLLS) {
            watch.sample();
        }
        if cancel.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SidecarError::Cancelled);
        }
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) => {}
            Err(error) => {
                // The caller waits for the child's pipes to close, which a
                // child left running would never do, so it is killed first.
                let _ = child.kill();
                let _ = child.wait();
                return Err(SidecarError::Io {
                    context: format!("polling sidecar {step}"),
                    error,
                });
            }
        }
        if polls >= max_polls {
            let _ = child.kill();
            let _ = child.wait();
            return Err(SidecarError::TimedOut { step });
        }
        std::thread::sleep(STEP_POLL);
        polls += 1;
    }
}
