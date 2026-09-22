# Veredus - 0 A.D. Dedicated Server

A dedicated, headless server for [0 A.D.](https://play0ad.com/), the free and
open-source RTS game. It runs as a network relay that forwards game traffic
between clients and manages lobby, session, turn and match state so games can
be hosted around the clock without a player acting as host. The relay itself
never runs game simulation: that happens in the players' own 0 A.D. clients,
or in a headless `pyrogenesis` process ("sidecar") the server spawns for
hosted-AI opponents, rejoin snapshots and post-match processing.

## Table of contents

- [Features](#features)
- [Architecture](#architecture)
- [Requirements](#requirements)
- [Installation](#installation)
- [Usage](#usage)
- [Configuration](#configuration)
- [Observability and logging](#observability-and-logging)
- [Further reading](#further-reading)
- [License](#license)

## Features

- **Always-on hosting** - games run without any client acting as network
  host, so a match survives the original host disconnecting.
- **Out-of-sync (OOS) detection** - full-state hashes are compared every turn
  and a desync is reported instead of silently corrupting the match.
- **Rejoin support** - a client that drops and reconnects is served a stored
  sidecar checkpoint (or a one-shot sidecar dump when no client can serve
  it) and resumes rather than being locked out.
- **Rolling checkpoints** - every few hundred released turns the sidecar
  replays the match so far and stores the state; joiners catch up from the
  newest checkpoint and the match outcome file follows the game while it
  runs.
- **Hosted AI opponents** - a headless `pyrogenesis` sidecar can join as an AI
  player, letting a game run below the human player count it was set up for.
- **Automatic outcome resolution** - the match result is produced by replaying
  the agreed turns in the sidecar, even when no human player stays to the
  end, and written as `<game_id>.json` when `--outcome-dir` is set.
- **Delayed observers** - observers can watch a configurable number of turns
  behind live play, so streaming a match does not leak live positions.
- **Password-protected games** - optional password gating at the ENet layer,
  matching the stock client/host handshake.
- **Flood and pause limits** - per-peer chat, flare and command quotas plus a
  shared pause budget (with AFK auto-pause) keep one client from stalling or
  spamming the match.
- **Per-client network metrics** - RTT and packet loss are tracked per
  client and exported to Prometheus, alongside per-game message and byte
  counters.
- **Lobby integration** - connects to the 0 A.D. XMPP lobby with a pool of
  accounts and hosts a fresh game on the `hostme` command in the MUC room,
  one game per available account.
- **Graceful disconnects** - kicks, bans and timeouts carry defined
  numeric disconnect reasons, matching what stock clients display, and the
  updated slot list goes out before the peer is dropped so the departing
  client still sees itself leave.

## Architecture

```
 Clients (stock 0 A.D., unmodified)
        |
        v  ENet/UDP
+----------------------------------------------------------+
|              veredus server, per game                     |
|                                                           |
|  ENet socket thread <--mpsc--> synchronous Server FSM    |
|  (send/recv, ENet host loop)   (turn scheduling, OOS,     |
|                                  session/disconnect state) |
|                                       |                    |
|                                       | spawns (optional)  |
|                                       v                    |
|                          pyrogenesis sidecar processes     |
|                    (hosted AI, checkpoint dumps,           |
|                     rejoin snapshots, outcome replay)      |
+----------------------------------------------------------+
        ^
        | XMPP (pool-lobby mode only)
+----------------------------------------------------------+
|   tokio runtime: lobby client + Prometheus/Rocket metrics |
+----------------------------------------------------------+
```

Each game gets two OS threads communicating over a channel: an ENet socket
thread and a synchronous state-machine thread. The relay is networking-only
by design - it never simulates, and any simulation it needs (hosted AI,
rejoin state, checkpoint and outcome replays) runs in a spawned
`pyrogenesis` process, never inline.

## Requirements

- To run: stock, unmodified 0 A.D. clients connect with no changes at all.
- For the sidecar features (hosted AI, checkpoint/rejoin snapshots, outcome
  replay): a built `pyrogenesis` binary from the `feature/server-sidecar`
  branch of our 0 A.D. fork, supplied with `--pyrogenesis-path` (or
  `[server] pyrogenesis_path`). Without it the relay still hosts matches,
  but joiners no live client can serve are dropped instead of snapshotted.
- To build from source: current stable Rust (edition 2024, no pinned MSRV).

## Installation

### Build from source

```sh
cargo build --release
```

The resulting binary is `target/release/veredus`. There are no prebuilt
release archives yet.

## Usage

The server listens on port 20595 by default and exposes Prometheus metrics
on port 9091. Show all options with `cargo run -- --help` (or
`./veredus --help`).

### Standalone

Runs one game after another with no lobby:

```sh
cargo run -- --pyrogenesis-path ../0ad/binaries/system/pyrogenesis
```

With `[server] exit_after_game` (or a supervisor setup) it stops after one
game instead of hosting a fresh one on the same port.

### Pool lobby

Hosts on the `hostme` command in the configured MUC room, allocating one
game per available account. Either enable the `[lobby]` table in the config
file or point at a pooled-account lobby config (JSON), which replaces the
`[lobby]` table:

```sh
cargo run -- --lobby-config lobby.json
```

Every account is one-shot today: once its game ends the account is released
back to the pool. A pool-lobby game shuts itself down after the idle
timeout (`[lobby] idle_shutdown_secs`, default 60 s) with nobody admitted:
either nobody ever joined, or everyone left. The hosted-AI sidecar alone
does not keep it alive.

### Match outcomes

With `--outcome-dir <dir>` (needs `--pyrogenesis-path`), each finished
match's outcome is written to `<dir>/<game_id>.json` as
`{"turn", "final", "result"}`: rewritten with `final: false` at every
checkpoint while the match runs, then once with `final: true`. Without it
the outcome is only logged.

## Configuration

Settings resolve as built-in defaults, then the config file, then command
line flags, each overriding the one before. The `[log]` table is the
exception: its environment variables win over the file.

Write the default config to `./config.toml` (or a given path) and exit:

```sh
cargo run -- --gen-config
```

`./config.toml` is loaded when present; `--config` names another file:

```sh
cargo run -- --config prod.toml --port 20600
```

The file has four tables:

- `[server]` - `host`, `port` (standalone mode only; a lobby game picks its
  own), `pyrogenesis_path`, `outcome_dir`, `checkpoint_interval_turns`
  (default 600, 0 disables), `metrics_host` / `metrics_port` (loopback by
  default, port 0 disables), `exit_after_game`, ENet packet/waiting caps.
- `[game]` - turn length, server name, welcome message, controller secret,
  duplicate names, late-observer policy and limits, observer delay, session
  cap, pause budget, AFK pause, post-game linger, handshake/loading/join
  timeouts, flood limits, buddies, enabled mods.
- `[lobby]` - `enabled` selects pool-lobby mode, plus the MUC room, bot JID,
  public IP, server name, engine version, game password, idle shutdown and
  the account list. Keep credentials out of source control.
- `[log]` - `directives`, `loki_directives`, `loki_url`, `loki_instance`,
  `loki_env`. See below.

Unknown keys are an error, so a typo fails loudly instead of being ignored.
An optional setting is an empty string or 0 rather than a missing key, so
the generated file always shows every knob.

## Observability and logging

Prometheus metrics are served at:

```text
http://localhost:9091/metrics
```

Logging uses `tracing` with two independent sinks and filters:

- `RUST_LOG` controls the stdout sink (falls back to `[log] directives`,
  else a built-in default if unset), for example to trace one module, one
  game or one client:

  ```sh
  RUST_LOG='error,server::relay::net_server=trace' cargo run -- ...
  RUST_LOG='error,[game{game_id=gid_0199...}]=trace' cargo run -- ...
  RUST_LOG='error,[client{name=bob}]=trace' cargo run -- ...
  ```

- `LOKI_LOG` independently controls what is shipped to a Loki sink, using
  the same filter syntax. The sink exists only when `LOKI_URL` (or
  `[log] loki_url`) is set; `LOKI_INSTANCE` (default: the hostname) and
  `LOKI_ENV` (default `dev`) become stream labels next to `job="veredus"`.

Every line logged while handling one client's input runs inside a `client`
span (a child of `game`) carrying peer, uuid, lobby name, client id and
name. The client IP is deliberately not a span field: it is logged only on
connect, disconnect and refused-connection lines.

## Further reading

- [PROTOCOL.md](PROTOCOL.md) - the wire protocol, message layouts and lobby
  IQ formats. Normative for anything on the wire; never guess a field.
- [AGENTS.md](AGENTS.md) - contributor rules, the threading model and
  invariants, and the module-by-module code map.

## License

Apache-2.0. See [LICENSE](LICENSE).
