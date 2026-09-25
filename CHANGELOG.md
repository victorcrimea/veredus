# Changelog

What changed in each Veredus release, for people running the server.
The newest release is first.

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
