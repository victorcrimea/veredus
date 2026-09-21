// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::DateTime;
use chrono::Utc;

use crate::relay::messages::EnabledMod;
use crate::relay::messages::PlayerCommand;

const DECODE_ERROR_MARKER: &str = "{\"__decode_error__\":true}";

// How often a running engine step is polled for exit. Short enough that a
// cancelled run dies promptly, long enough that the dump thread spends its
// time asleep rather than spinning on the child.
const STEP_POLL: Duration = Duration::from_millis(50);
// Each engine step (decode, replay dump) gets this long before it is killed.
// Too short and a long match can never be rebuilt; too long and one hung run
// blocks every joiner in its game, since only one run is allowed at a time.
const STEP_TIMEOUT: Duration = Duration::from_secs(600);

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
}

// The long-lived hosted-AI client for one match: a headless pyrogenesis that
// joins the relay like any stock client and sends the AI players' commands.
// Dropping it kills the process, so a game that ends or panics never leaves
// an engine running behind it.
pub struct AiHostProcess {
    child: std::process::Child,
    pumps: Vec<std::thread::JoinHandle<()>>,
}

impl AiHostProcess {
    pub fn spawn(
        pyrogenesis: &Path,
        addr: Ipv4Addr,
        port: u16,
        name: &str,
    ) -> Result<Self, SidecarError> {
        let mut cmd = std::process::Command::new(pyrogenesis);
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
        let mut child = cmd.spawn().map_err(|error| SidecarError::Io {
            context: "spawning AI host".to_string(),
            error,
        })?;
        tracing::info!(pid = child.id(), %addr, port, name, "sidecar: AI host spawned");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");
        Ok(AiHostProcess {
            child,
            pumps: vec![spawn_line_pump(stdout), spawn_line_pump(stderr)],
        })
    }

    // None while it is still running. An error polling it is reported as an
    // exit: a child the relay can no longer observe is as good as gone.
    pub fn poll_exit(&mut self) -> Option<String> {
        match self.child.try_wait() {
            Ok(None) => None,
            Ok(Some(status)) => Some(status.to_string()),
            Err(error) => Some(error.to_string()),
        }
    }
}

impl Drop for AiHostProcess {
    // SIGKILL straight away: the AI host holds nothing a graceful exit would
    // save, and waiting here would stall the game thread that drops it.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Once the child is reaped its pipes read EOF, so the pumps end.
        for pump in self.pumps.drain(..) {
            let _ = pump.join();
        }
        tracing::info!("sidecar: AI host stopped");
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

    let mut cmd = std::process::Command::new(pyrogenesis);
    cmd.arg(format!("-decode-script-vals-in={}", input_path.display()))
        .arg(format!("-decode-script-vals-out={}", output_path.display()));
    let output = run_logged(&mut cmd, "decode", cancel)?;
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

    // One cursor into the turn-ordered commands: each turn writes the `cmd`
    // lines at its head and moves on, so the whole file is a single pass over
    // the turns plus a single pass over the commands.
    let mut idx = 0;
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
    let mut cmd = std::process::Command::new(pyrogenesis);
    cmd.arg(format!("-replay={}", commands_txt.display()))
        .arg(format!("-dump-state-at-turn={turn_label}"))
        .arg(format!("-dump-state-out={}", dump_path.display()));
    let output = run_logged(&mut cmd, "dump", cancel)?;
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
// child's output is folded into the log, so a slow invocation is visible in
// the same sinks as everything else.
fn run_logged(
    cmd: &mut std::process::Command,
    step: &'static str,
    cancel: &AtomicBool,
) -> Result<std::process::Output, SidecarError> {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| SidecarError::Io {
            context: format!("spawning sidecar {step}"),
            error,
        })?;
    let stdout_reader = spawn_pipe_reader(child.stdout.take().expect("stdout was piped"));
    let stderr_reader = spawn_pipe_reader(child.stderr.take().expect("stderr was piped"));
    let status = reap_child(&mut child, step, cancel);
    // Joined after the child is reaped on every path: once it is dead the
    // pipes read EOF, so the readers always terminate.
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();
    for line in String::from_utf8_lossy(&stdout).lines() {
        tracing::debug!(step, "sidecar: {line}");
    }
    for line in String::from_utf8_lossy(&stderr).lines() {
        tracing::debug!(step, "sidecar: {line}");
    }
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
    child: &mut std::process::Child,
    step: &'static str,
    cancel: &AtomicBool,
) -> Result<std::process::ExitStatus, SidecarError> {
    let max_polls = STEP_TIMEOUT.as_millis() / STEP_POLL.as_millis();
    let mut polls: u128 = 0;
    loop {
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
