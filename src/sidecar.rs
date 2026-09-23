// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

use std::io::BufRead;
use std::io::Read;
use std::io::Write;
use std::net::Ipv4Addr;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::PoisonError;
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

// Bounds how many one-shot engine runs the whole process has going at once,
// across every game: each is a full engine, and nothing else stops every game
// in the pool from asking for one together. Unset means unlimited.
static RUN_LIMIT: OnceLock<RunLimiter> = OnceLock::new();

struct RunLimiter {
    queue: Mutex<RunQueue>,
    freed: Condvar,
}

struct RunQueue {
    limit: usize,
    running: usize,
    // Only urgent waiters are counted: a non-urgent run steps aside while any
    // of them is queued, so a joiner's dump never sits behind a replay.
    waiting_urgent: usize,
}

// 0 leaves runs unlimited. The limit is a process setting fixed before any
// game exists, so only the first call counts.
pub fn set_run_limit(limit: usize) {
    if limit == 0 {
        return;
    }
    let _ = RUN_LIMIT.set(RunLimiter {
        queue: Mutex::new(RunQueue {
            limit,
            running: 0,
            waiting_urgent: 0,
        }),
        freed: Condvar::new(),
    });
}

// Sidecar runs write a match's settings, commands and states to disk. They
// go under one directory per process, private to its user, so another local
// user cannot read them. The lock inside is held for the life of the process
// and released by the OS however it dies, which is how a later start tells a
// crashed instance's leftovers from a live instance's runs.
const WORK_ROOT_PREFIX: &str = "veredus-run-";
const WORK_ROOT_LOCK: &str = "lock";
// Older builds put each run straight into the temp dir under these names.
// No current build does, so any left behind belong to a dead process.
const UNROOTED_WORK_PREFIXES: [&str; 3] =
    ["veredus-dump-", "veredus-checkpoint-", "veredus-outcome-"];

static WORK_ROOT: OnceLock<WorkRoot> = OnceLock::new();

struct WorkRoot {
    path: PathBuf,
    _lock: std::fs::File,
}

// Removes what crashed or killed instances left in the temp dir, then creates
// this process's private root. A failure only costs the privacy: runs then
// fall back to the shared temp dir, rather than every joiner losing its dump.
pub fn init_work_root() {
    let tmp = std::env::temp_dir();
    sweep_work_dirs(&tmp);
    match create_work_root(&tmp) {
        Ok(root) => {
            tracing::debug!(path = %root.path.display(), "sidecar: work dir created");
            let _ = WORK_ROOT.set(root);
        }
        Err(error) => tracing::warn!(
            %error,
            dir = %tmp.display(),
            "sidecar: cannot create a private work dir, runs will be readable by other users"
        ),
    }
}

// Called on a clean exit; anything a hard exit leaves is swept at the next
// start instead.
pub fn remove_work_root() {
    if let Some(root) = WORK_ROOT.get() {
        remove_work_dir(&root.path);
    }
}

// A fresh directory per run, so a run never reads back another run's output.
pub fn work_dir(kind: &str) -> PathBuf {
    let name = format!("{kind}-{}", uuid::Uuid::new_v4());
    match WORK_ROOT.get() {
        Some(root) => root.path.join(name),
        None => std::env::temp_dir().join(format!("veredus-{name}")),
    }
}

pub fn remove_work_dir(dir: &Path) {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => {}
        // A run cancelled before it wrote anything never created it.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::debug!(%error, dir = %dir.display(), "sidecar: cannot remove work dir");
        }
    }
}

fn create_work_root(tmp: &Path) -> std::io::Result<WorkRoot> {
    let path = tmp.join(format!("{WORK_ROOT_PREFIX}{}", uuid::Uuid::new_v4()));
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&path)?;
    let lock = std::fs::File::create(path.join(WORK_ROOT_LOCK))?;
    lock.try_lock()?;
    Ok(WorkRoot { path, _lock: lock })
}

fn sweep_work_dirs(tmp: &Path) {
    let Ok(entries) = std::fs::read_dir(tmp) else {
        return;
    };
    for entry in entries.flatten() {
        // Not following links: the temp dir is shared, and a planted link
        // must not steer the removal somewhere else.
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let path = entry.path();
        let stale = if name.starts_with(WORK_ROOT_PREFIX) {
            work_root_abandoned(&path)
        } else {
            UNROOTED_WORK_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        };
        if !stale {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => tracing::info!(dir = %path.display(), "sidecar: removed stale work dir"),
            Err(error) => {
                tracing::warn!(%error, dir = %path.display(), "sidecar: cannot remove stale work dir");
            }
        }
    }
}

// Anything short of taking the lock counts as in use: a root another instance
// has only just created has no lock file yet, and one owned by another user
// cannot be opened at all.
fn work_root_abandoned(dir: &Path) -> bool {
    std::fs::File::open(dir.join(WORK_ROOT_LOCK)).is_ok_and(|lock| lock.try_lock().is_ok())
}

// Held for a whole run, decode and replay together, so a run that has
// started never queues a second time halfway through.
struct RunPermit {
    step: &'static str,
    limiter: Option<&'static RunLimiter>,
}

impl Drop for RunPermit {
    fn drop(&mut self) {
        crate::metrics::SIDECAR_RUNS_RUNNING
            .with_label_values(&[self.step])
            .dec();
        if let Some(limiter) = self.limiter {
            let mut queue = limiter.queue.lock().unwrap_or_else(PoisonError::into_inner);
            queue.running -= 1;
            drop(queue);
            limiter.freed.notify_all();
        }
    }
}

// Waits for a free slot. `urgent` is for a run someone is waiting on (a
// joiner's dump); the others only finish later for being queued. The wait
// polls `cancel` like a running step does, so a game that ends never waits
// on the queue, and it is not charged to the step's timeout.
fn acquire_run(
    step: &'static str,
    urgent: bool,
    cancel: &AtomicBool,
) -> Result<RunPermit, SidecarError> {
    let running = crate::metrics::SIDECAR_RUNS_RUNNING.with_label_values(&[step]);
    let Some(limiter) = RUN_LIMIT.get() else {
        running.inc();
        return Ok(RunPermit {
            step,
            limiter: None,
        });
    };
    let waiting = crate::metrics::SIDECAR_RUNS_WAITING.with_label_values(&[step]);
    let mut queue = limiter.queue.lock().unwrap_or_else(PoisonError::into_inner);
    let mut queued = false;
    // Only full poll intervals are counted, so the logged wait is a lower
    // bound; it is counted rather than timed because this module never reads
    // the clock.
    let mut timeouts: u64 = 0;
    loop {
        if queue.running < queue.limit && (urgent || queue.waiting_urgent == 0) {
            break;
        }
        if cancel.load(Ordering::SeqCst) {
            if queued {
                waiting.dec();
                if urgent {
                    queue.waiting_urgent -= 1;
                    drop(queue);
                    // A run that stepped aside for this one may go now.
                    limiter.freed.notify_all();
                }
            }
            return Err(SidecarError::Cancelled);
        }
        if !queued {
            queued = true;
            waiting.inc();
            if urgent {
                queue.waiting_urgent += 1;
            }
        }
        let (guard, wait) = limiter
            .freed
            .wait_timeout(queue, STEP_POLL)
            .unwrap_or_else(PoisonError::into_inner);
        queue = guard;
        if wait.timed_out() {
            timeouts += 1;
        }
    }
    if queued {
        waiting.dec();
        if urgent {
            queue.waiting_urgent -= 1;
        }
    }
    queue.running += 1;
    drop(queue);
    running.inc();
    if queued {
        let waited_ms = timeouts * STEP_POLL.as_millis() as u64;
        tracing::info!(step, waited_ms, "sidecar: run waited for a free slot");
    }
    Ok(RunPermit {
        step,
        limiter: Some(limiter),
    })
}

// A state a replay can resume from instead of turn 0: the wire turn it was
// dumped at and the dump itself, in the same compressed, turn-prefixed format
// a client sends. Shared, because the same checkpoint is both handed to
// joiners and written out for the next replay.
#[derive(Debug, Clone, PartialEq)]
pub struct BaseState {
    pub turn: u32,
    pub state: Arc<Vec<u8>>,
}

// Everything the one-shot replay needs to rebuild the state at turn T:
// the frozen settings, the engine version the clients run, the mods in load
// order, the turn lengths for the wire turns after the base (or 1..=T when
// there is none) and the commands for the same turns, in order. Plain data
// so it can ride inside an Effect.
#[derive(Debug, Clone, PartialEq)]
pub struct DumpRequest {
    pub init_attributes: Vec<u8>,
    pub engine_version: String,
    pub mods: Vec<EnabledMod>,
    pub base: Option<BaseState>,
    pub turn_lengths: Vec<u16>,
    pub commands: Vec<PlayerCommand>,
    // The players' agreed hash per wire turn, in turn order. The replay checks
    // itself against these, which is the only way to notice that it runs a
    // different build from the clients. Left empty for a dump, which would
    // otherwise pay for a full state hash every 20 turns for nothing.
    pub hashes: Vec<(u32, Vec<u8>)>,
}

impl DumpRequest {
    // The wire turn the replay starts after.
    pub fn first_turn(&self) -> u32 {
        self.base.as_ref().map_or(0, |b| b.turn)
    }

    // The wire turn the replay ends on.
    pub fn last_turn(&self) -> u32 {
        self.first_turn() + self.turn_lengths.len() as u32
    }
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
    #[error("sidecar replay diverged from the live match ({0} hash mismatches)")]
    Diverged(usize),
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
// child here is spawned by a thread that outlives it. Windows gets the same
// guarantee process-wide from `guard_orphans`; other platforms have no such
// guard and keep an orphan running until it exits on its own.
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
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn guard_orphan(_cmd: &mut Command) {}

// Windows has no parent-death signal. Instead the relay puts itself in a job
// that kills every member when its last handle closes; children join the job
// at creation, so there is no window between spawn and guard, and the handle
// is deliberately never closed, so it goes only when the process does.
#[cfg(windows)]
pub fn guard_orphans() {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    use windows_sys::Win32::System::JobObjects::CreateJobObjectW;
    use windows_sys::Win32::System::JobObjects::JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    use windows_sys::Win32::System::JobObjects::JOBOBJECT_EXTENDED_LIMIT_INFORMATION;
    use windows_sys::Win32::System::JobObjects::JobObjectExtendedLimitInformation;
    use windows_sys::Win32::System::JobObjects::SetInformationJobObject;
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    // SAFETY: plain Win32 calls on a job handle this function owns; the limit
    // struct is plain data, for which all zeroes is the documented "no limit".
    let result = unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            Err(std::io::Error::last_os_error())
        } else {
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let configured = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            ) != 0;
            if configured && AssignProcessToJobObject(job, GetCurrentProcess()) != 0 {
                Ok(())
            } else {
                let error = std::io::Error::last_os_error();
                CloseHandle(job);
                Err(error)
            }
        }
    };
    if let Err(error) = result {
        tracing::warn!(%error, "sidecar: cannot create a kill-on-exit job, engines may outlive a killed relay");
    }
}

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
    // Only a stop the game asked for counts as a success: the AI host is
    // meant to run until then.
    crate::metrics::sidecar_run(
        "ai_host",
        stopped,
        usage.cpu_seconds,
        usage.run_seconds,
        usage.peak_rss_bytes,
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
    // Nothing to replay: the base already is the state at that turn.
    if let Some(base) = request.base.as_ref().filter(|b| b.turn == turn) {
        return Ok(base.state.to_vec());
    }
    let _permit = acquire_run("dump", true, cancel)?;
    let mut cmd = prepare_replay(pyrogenesis, dir, request, now, cancel)?;
    let dump_path = add_dump_args(&mut cmd, dir, turn);
    let output = run_logged(&mut cmd, "dump", STEP_TIMEOUT, cancel)?;
    if !output.status.success() {
        return Err(SidecarError::Status {
            step: "dump",
            status: output.status,
        });
    }
    read_dump(&dump_path)
}

// One link of the rolling chain: resumes from the request's base, replays
// through wire turn `turn` checking itself against the players' hashes, and
// returns the state there together with what the engine makes of the match
// so far. A mismatch fails the link, because a diverged state would be
// carried into every later one and handed to joiners.
pub fn checkpoint(
    pyrogenesis: &Path,
    dir: &Path,
    turn: u32,
    request: &DumpRequest,
    now: DateTime<Utc>,
    cancel: &AtomicBool,
) -> Result<(Vec<u8>, ReplayResult, String), SidecarError> {
    if turn <= request.first_turn() {
        return Err(SidecarError::NoTurn);
    }
    let _permit = acquire_run("checkpoint", false, cancel)?;
    let mut cmd = prepare_replay(pyrogenesis, dir, request, now, cancel)?;
    // Quick hashes are skipped by default, and they are most of what the
    // players agreed on.
    cmd.arg("-hashtest-quick=true");
    let dump_path = add_dump_args(&mut cmd, dir, turn);
    let output = run_logged(&mut cmd, "checkpoint", STEP_TIMEOUT, cancel)?;
    if !output.status.success() {
        return Err(SidecarError::Status {
            step: "checkpoint",
            status: output.status,
        });
    }
    let mismatch_count = count_mismatches(&output);
    if mismatch_count > 0 {
        return Err(SidecarError::Diverged(mismatch_count));
    }
    let (result, json) = parse_result(&output)?;
    Ok((read_dump(&dump_path)?, result, json))
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
    let _permit = acquire_run("outcome", false, cancel)?;
    let mut cmd = prepare_replay(pyrogenesis, dir, request, now, cancel)?;
    // Quick hashes are skipped by default, and they are most of what the
    // players agreed on.
    cmd.arg("-hashtest-quick=true");
    let output = run_logged(&mut cmd, "outcome", OUTCOME_TIMEOUT, cancel)?;
    if !output.status.success() {
        return Err(SidecarError::Status {
            step: "outcome",
            status: output.status,
        });
    }

    let mismatch_count = count_mismatches(&output);
    if mismatch_count > 0 {
        // The outcome is still reported, but it may not be the match that
        // was played: the replay drifted from what the players agreed on.
        tracing::warn!(
            mismatch_count,
            "sidecar: outcome replay diverged from the live match"
        );
    }
    parse_result(&output)
}

// Decodes the request's commands, writes the replay file and the base state
// next to it, and returns the engine invocation that replays them. Only the
// turns after the base are decoded, so a chained run costs its own chunk and
// not the whole match.
fn prepare_replay(
    pyrogenesis: &Path,
    dir: &Path,
    request: &DumpRequest,
    now: DateTime<Utc>,
    cancel: &AtomicBool,
) -> Result<Command, SidecarError> {
    std::fs::create_dir_all(dir).map_err(|error| SidecarError::Io {
        context: format!("creating {}", dir.display()),
        error,
    })?;

    let decoded = decode_commands(pyrogenesis, dir, &request.commands, cancel)?;
    let commands_txt = dir.join("commands.txt");
    write_commands_txt(&commands_txt, request, &decoded, now)?;

    let mut cmd = Command::new(pyrogenesis);
    cmd.arg(format!("-replay={}", commands_txt.display()));
    if let Some(base) = &request.base {
        let base_path = dir.join("base.bin");
        std::fs::write(&base_path, base.state.as_slice()).map_err(|error| SidecarError::Io {
            context: format!("writing {}", base_path.display()),
            error,
        })?;
        cmd.arg(format!("-replay-initial-state={}", base_path.display()));
    }
    Ok(cmd)
}

// The replay labels the block executed to reach wire turn N as N-1, since
// wire turn 0 never gets its own step, so the state at wire turn N is the
// state after that block runs.
fn add_dump_args(cmd: &mut Command, dir: &Path, turn: u32) -> PathBuf {
    let dump_path = dir.join("state.bin");
    cmd.arg(format!("-dump-state-at-turn={}", turn - 1))
        .arg(format!("-dump-state-out={}", dump_path.display()));
    dump_path
}

fn read_dump(dump_path: &Path) -> Result<Vec<u8>, SidecarError> {
    std::fs::read(dump_path).map_err(|error| SidecarError::Io {
        context: format!("reading {}", dump_path.display()),
        error,
    })
}

// The replay reports a hash it disagrees with on either stream, depending on
// where the engine's logger sends it. A player may name themselves
// "MISMATCH", which then appears verbatim inside the REPLAY_RESULT line's
// player states, so matching is anchored to the engine's own line format
// rather than a bare substring search.
fn count_mismatches(output: &std::process::Output) -> usize {
    const MISMATCH_PREFIXES: [&str; 2] = ["hash MISMATCH (", "hash-quick MISMATCH ("];
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    stdout
        .lines()
        .chain(stderr.lines())
        .filter(|line| !line.starts_with(REPLAY_RESULT_PREFIX))
        .filter(|line| {
            MISMATCH_PREFIXES
                .iter()
                .any(|prefix| line.starts_with(prefix))
        })
        .count()
}

fn parse_result(output: &std::process::Output) -> Result<(ReplayResult, String), SidecarError> {
    let stdout = String::from_utf8_lossy(&output.stdout);
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
    let first_turn = request.first_turn();
    for (index, length) in request.turn_lengths.iter().enumerate() {
        let wire_turn = first_turn + index as u32 + 1;
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
// mounts, so the one mod this server runs maps back to its folder here. Also
// used by the lobby's `mods` attribute (Sec. 17.3), which needs the same
// mapping for the same reason.
pub(crate) fn mod_pathname(name: &str) -> Option<&'static str> {
    match name {
        "0ad" => Some("public"),
        _ => None,
    }
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
    // A failed engine assertion waits on stdin for what to do next; inherited
    // from a relay on a terminal, the step would sit there until it times out.
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
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
    let ok = status.as_ref().is_ok_and(|s| s.success());
    crate::metrics::sidecar_run(
        step,
        ok,
        usage.cpu_seconds,
        usage.run_seconds,
        usage.peak_rss_bytes,
    );
    tracing::info!(
        step,
        pid,
        ok,
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
