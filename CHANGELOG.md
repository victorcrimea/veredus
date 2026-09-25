# Changelog

What changed in each Veredus release, for people running the server.
The newest release is first.

## [0.4.0] - 2026-09-25

Changes since 0.3.0, including the 0.3.x patch releases.

### Changed
- **Breaking:** The config file is split into topic tables, ordered from the
  settings you are most likely to edit to the least: `[server]`, `[lobby]`,
  `[personal]`, `[match]`, `[observers]`, `[pause]`, `[saves]`, `[metrics]`,
  `[log]`, `[sidecar]`, `[limits]`, `[timeouts]`, `[advanced]`. A config
  file from 0.3.0 is refused. Regenerate it with `--gen-config` and copy
  your values across. Command line flags are unchanged.
- **Breaking:** The server no longer assumes a sidecar. Pass
  `--pyrogenesis-path` (or set it in `[sidecar]`) to use one. The `-sidecar`
  downloads come with a `config.toml` that already points at their bundled
  sidecar, so `./veredus` alone starts with it.
- **Breaking:** Metrics now listen on every interface by default, so they
  can be scraped from outside a container. Firewall port 9091, or set
  `[metrics] host` to `127.0.0.1` to keep them local.
- **Breaking:** A match saved by 0.3.0 without the sidecar cannot be
  resumed. Finish those matches before you upgrade, or start fresh with
  `--no-resume`.
- Without the sidecar, the server now asks a player's game for a copy of
  the match every 2 minutes by default (`[saves]
  client_state_interval_secs`, 0 turns it off). Rejoining players are
  served that copy, so a rejoin no longer pauses a player's game.
- A lobby game only lets others in after the player who typed `hostme` has
  joined.
- With AI opponents, when every human has left or stopped, the match waits
  for the remaining clients instead of the AI playing on at full speed.

### Added
- Personal mode (`--personal`, `[personal]` table): the server logs in with
  your own lobby account and hosts one game after another on `--port`. You
  join by IP from one of `[personal] trusted_networks` and become host; the
  game is listed in the lobby only while you are in it. Rated 1v1 games are
  reported to the lobby (needs the sidecar). On first start you are asked to
  accept the lobby's terms, as the game itself does.
- Shared player slots (`[match] shared_slots`, off by default): during
  setup a player types `!share <name>` so that observer controls the same
  civilisation.
- Lobby hosting resumes every saved lobby match after a restart, each on a
  free lobby account, and lists it again at once.
- Matches with AI opponents survive a restart, and an AI that crashes or
  drops mid-match is restarted and caught up while the players wait
  (`[sidecar] ai_heal_attempts`, `ai_heal_timeout_secs`,
  `ai_state_interval_turns`).
- With the sidecar and `--outcome-dir`, each finished match also gets a
  replay folder the game can play.
- A match saved with the sidecar can be resumed on a server without one.
- Docker images on ghcr.io, with and without the sidecar.
- `--gen-config` shows example lobby accounts.

### Fixed
- Replays, results, checkpoints and resumed matches could drift from the
  game actually played when two players' orders in one turn interacted
  (most turns with AI opponents).
- A finished match with AI opponents was taken for an AI crash and kept as
  stopped, with no result or replay.
- `hostme` from the stock game was ignored, and a fresh lobby game was not
  listed until somebody joined it.
- The save was not written safely to disk while a match was being played,
  so a power loss could lose more than the last second.

## [0.3.4] - 2026-09-25

The first release with a changelog. This entry lists everything the server
can do at this point, rather than what changed since 0.3.3.

### Features

- Relays multiplayer matches between stock 0 A.D. 0.28.0 clients, so no
  player hosts the match and it survives anyone leaving.
- Standalone mode: one game on UDP port 20595; the first player to join is
  the host, and a fresh game opens when a match ends (or the server exits,
  with `[server] exit_after_game`).
- Lobby hosting: a pool of lobby accounts waits in the lobby chat, and a
  player who types `hostme` gets a new game with them as its host. The game
  is listed with its real map and settings, and lobby accounts reconnect on
  their own after a dropped connection.
- Game passwords, lobby authentication, name deduplication, kicks and bans,
  and mod checks against every client.
- Rejoin: a player who disconnects, times out or was kicked without a ban
  gets their own slot back.
- Observers, watching a configurable number of turns behind the live game
  (`[observers] delay_turns`, 0 for live), with a policy for late observers.
- Pause budget per player, and an automatic pause that holds the match for
  a disconnected or silent player until they return or their budget runs
  out.
- Warnings in chat about lagging or silent players, and out-of-sync
  detection and reporting.
- A loading-screen timeout, so one stuck player does not hold up the start.
- Matches are saved as they go and resume after a server restart or crash,
  paused until the host types `!resume` (`--no-resume` to start fresh).
  Without the sidecar, the server periodically asks one player's game for a
  copy of the match (`[saves] client_state_interval_secs`).
- Protection against abuse: limits on chat, flares, commands and pauses per
  player, on unauthenticated connections per address, on wrong passwords and
  on rejoin spam, and bounds on how much memory any one connection can use.
- Clean shutdown on Ctrl+C or SIGTERM: players get a "server shutdown"
  message, and a running match is kept for the next start.
- Config file (`--gen-config` writes one with every setting explained);
  command line flags override it.
- Prometheus metrics (port 9091 by default) and logging to stdout, with
  optional Loki output.
- Downloads for Linux (x86_64 and ARM64, static) and Windows, and Docker
  images on ghcr.io.

### Features with the sidecar

The sidecar is a patched headless 0 A.D. 0.28.0 run next to the server
(`--pyrogenesis-path`, or a `-sidecar` download for Linux).

- Rejoins without stopping the game: the server builds the joiner's copy of
  the match instead of pausing another player's game.
- AI opponents run on the server instead of on one player's PC. Games with
  AI can be rejoined, and the AI is restarted and caught up if it crashes.
- Match results worked out on the server, even if everyone left before the
  end; with `--outcome-dir`, kept as a JSON file plus a replay the game can
  play.
- Matches with AI opponents also survive a restart.
