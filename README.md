# Veredus - Dedicated Server

![Veredus banner](banner.png)

[![CI](https://github.com/victorcrimea/veredus/actions/workflows/ci.yml/badge.svg)](https://github.com/victorcrimea/veredus/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/victorcrimea/veredus?sort=semver)](https://github.com/victorcrimea/veredus/releases)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)

Veredus is an unofficial dedicated server for [0 A.D.](https://play0ad.com/)
that relays multiplayer matches between the players' unmodified game
clients. Normally one player's computer hosts the match, so that player's
connection, lag or departure affects everyone; with Veredus no player hosts.

**Works with 0 A.D. Release 28 (0.28.0).**

## Key features

- **No player hosts the match.** In a normal game the host's PC and
  connection carry it. Here the match survives anyone leaving, and waits for
  a dropped or lagging player only as long as their pause budget lasts.
- **Nothing to install for players.** They join with the stock 0 A.D. 0.28.0
  game.
- **Lobby games on request.** Instead of someone setting up each game, a
  player types `hostme` in the lobby chat and gets a fresh game with them as
  its host; many games run at once.
- **Your slot is kept.** A player who disconnects or times out gets their own
  slot back when they return; so does a kicked player who was not banned.
- **Matches survive a server restart or crash.** Every match is saved as it
  goes and continues when the server starts again, paused until the host
  types `!resume`.

With the [sidecar](#extra-features-with-the-sidecar), a patched headless
0 A.D. run next to the server:

- **Rejoins without stopping the game.** Normally another player's game
  stops to send the joiner a copy of the match; here the server builds it.
- **AI runs on the server.** It no longer loads one player's PC.
- **Games with AI can be rejoined.**
- **The AI recovers from a crash.** The server pauses the match, restarts
  the AI and catches it up.
- **Match results and replays.** The server works out who won even if
  everyone left before the end and, with `--outcome-dir`, keeps the result
  with a replay the game can play (for any match that played at least one
  turn).

## Quick start

1. Download the file for your system from the
   [Releases page](https://github.com/victorcrimea/veredus/releases). The
   server is a single file with nothing to install. Each Linux file also
   comes as a `-sidecar` version with the sidecar included.

2. Unpack it and start the server.

   Linux, for example on a 64-bit PC:

   ```sh
   tar xzf veredus-*-x86_64-unknown-linux-musl.tar.gz
   cd veredus-*-x86_64-unknown-linux-musl
   ./veredus
   ```

   Windows: unzip it, open a terminal in that folder and run `veredus.exe`.
   When Windows Firewall asks, allow access.

3. Optional: use the [sidecar](#extra-features-with-the-sidecar). A Linux
   `-sidecar` download (glibc 2.36 or newer, such as Debian 12, Ubuntu
   24.04 and later) comes with a `config.toml` that already points at it, so
   `./veredus` started from that folder uses it.

   On any other system, [build the sidecar](#building-the-sidecar) yourself
   and start the server with
   `./veredus --pyrogenesis-path /path/to/0ad/binaries/system/pyrogenesis`.

4. Allow **UDP port 20595** through your firewall. At home, also forward
   that port on your router to the server machine.

5. Players start 0 A.D., choose **Multiplayer**, then **Join game**, and
   enter the server's IP address and port 20595.

6. The first player to join is the host: they pick the map and settings and
   start the game. (A lobby game's host is the player who typed `hostme`.)
   When a match ends, the server opens a fresh game on the same port, unless
   `[server] exit_after_game` is set in the config file.

7. Stop the server with Ctrl+C. Players get a "server shutdown" message
   instead of just timing out.

To host games from the in-game lobby instead, see
[Hosting in the multiplayer lobby](#hosting-in-the-multiplayer-lobby).

## Limitations

- Lobby hosting needs your own lobby, or approval from the Wildfire Games
  lobby operators to use the official one.
- Map hacks are still possible. Every client runs the whole game, and a
  relay server cannot hide anything from it.
- Server-hosted AI gets none of the bonuses AI levels above Medium rely on
  (they raise the AI's gather rate inside the simulation), so it plays
  weaker than those levels.
- Server-hosted AI needs "Explore map" turned on. The map is not revealed.
- A saved game cannot be loaded from the game client. The server resumes
  only its own saves, automatically.

**Veredus is not affiliated with or endorsed by Wildfire Games.**

## Links

- [Releases](https://github.com/victorcrimea/veredus/releases) and
  [0 A.D.](https://play0ad.com/)
- On this page: [the sidecar](#extra-features-with-the-sidecar),
  [restarts](#restarts), [lobby hosting](#hosting-in-the-multiplayer-lobby),
  [configuration](#configuration), [running as a service](#running-as-a-service),
  [Docker](#running-with-docker),
  [building from source](#building-from-source),
  [building the sidecar](#building-the-sidecar) and [license](#license).
- `./veredus --help` lists every flag; `./veredus --gen-config` writes a
  config file with every setting explained.

## Extra features with the sidecar

The server itself never runs the game. Some features need a real copy of
the game engine running next to it, which we call the sidecar:

- **AI opponents** played on the server instead of on one player's PC.
- **Rejoining** even when no other player's game can send the joiner a copy
  of the match.
- **Match results** worked out on the server and written to a file.
- **Matches that survive a restart.** When you stop the server during a
  match, or it crashes, the match continues when you start it again. See
  [Restarts](#restarts).

The sidecar is 0 A.D. 0.28.0 with a small set of patches, kept in this
repository. The easiest way to get it is a `-sidecar` download for Linux:
it has the sidecar in its `sidecar` folder, next to the server, and a
`config.toml` whose `pyrogenesis_path` points at it. Start the server from
that folder:

```sh
./veredus
```

The sidecar runs wherever 0 A.D. itself runs; only Linux gets a
ready-made one. It needs glibc 2.36 or newer, such as Debian 12 or Ubuntu
24.04 and later, even in the musl downloads. On Windows, macOS or an older
Linux, [build it from source](#building-the-sidecar) and point the server
at it:

```sh
./veredus --pyrogenesis-path /path/to/0ad/binaries/system/pyrogenesis
```

To keep each match's result as a JSON file, add `--outcome-dir results`.
Next to each result the server also keeps the match's replay, as a folder
named after the game holding `commands.txt` and `metadata.json`, the same
files the game keeps for its own replays.

## Restarts

The server saves every running match as it goes, into a `saves` folder next
to where you start it (`--save-dir` picks another folder). If you stop the
server during a match, or it crashes, it picks the match up again the next
time it starts:

1. Players reconnect the way they joined. Outside the lobby that is the
   same address and port. In the lobby, the match shows up in the game
   list again, under the same server name.
2. Each player gets their own slot back. Anyone else can only watch. AI
   opponents come back on their own.
3. The match stays paused until the host types `!resume` in chat. After
   five minutes, any returning player can.

This works best with the sidecar, and matches with AI opponents need it.
Without the sidecar, every 2 minutes the server asks one player's game for a
copy of the match, which pauses the game for a moment. The same copy is
given to players and observers who rejoin, so they do not stop the game
each time. Change how often with `[saves] client_state_interval_secs`, or
set it to 0 to turn it off. To start fresh instead, run with `--no-resume`.

## Hosting in the multiplayer lobby

Veredus can also appear in the in-game multiplayer lobby. You give it one
or more lobby accounts, which wait in the lobby chat. A player who types
`hostme` in the lobby chat gets a new game from a free account. The game
appears in the game list, the player who asked is its host, and each
account hosts one game at a time.

**Before you do this on the official Wildfire Games lobby, get approval
from the lobby's operators.** The server accounts behave like bots, and the
lobby is a shared community space.

Write a config file with `./veredus --gen-config` (see
[Configuration](#configuration)) and fill in its `[lobby]` table:

```toml
[lobby]
enabled = true
public_ip = "203.0.113.10"
server_name = "My Veredus server"
muc_room = "arena28@conference.lobby.wildfiregames.com"
bot_jid = "wfgbot28@lobby.wildfiregames.com/CC"

[[lobby.accounts]]
jid = "myserver1@lobby.wildfiregames.com"
password = "..."

[[lobby.accounts]]
jid = "myserver2@lobby.wildfiregames.com"
password = "..."
```

- `enabled = true` turns lobby hosting on.
- `public_ip` is the address players connect to.
- `game_password` is optional and sets a password for every game.
- Keep `config.toml` private: it holds account passwords.

Then start the server as usual with `./veredus`.

Lobby games use UDP ports **20595 to 20695**, so open that whole range.

## Configuration

The defaults are fine to start with. To change anything, write a config
file, edit it and restart:

```sh
./veredus --gen-config          # writes ./config.toml
```

`./config.toml` is loaded automatically when it exists. You can pick
another file with `--config other.toml`. Command line flags override the
file. `./veredus --help` lists every flag.

Things people commonly change:

- `[server] host` and `port` set where the server listens; `port` is the
  game port for a server outside the lobby.
- `[lobby]` turns lobby hosting on and holds the public address and the
  accounts.
- `[match] server_name` and `welcome_message` set what players see.
- `[observers] delay_turns` sets how far behind observers watch
  (0 means live).
- `[pause] budget_secs` limits how long each player may pause.

The file runs from the settings most servers change to the ones almost none
do; `[advanced]` at the end is best left alone. A config file written for an
older version is refused with an "unknown field" error: write a fresh one with
`--gen-config` and copy your values over.

## Running as a service

On Linux, a systemd unit keeps the server running across reboots and
crashes. Save this as `/etc/systemd/system/veredus.service`:

```ini
[Unit]
Description=Veredus 0 A.D. server
After=network-online.target
Wants=network-online.target

[Service]
User=veredus
WorkingDirectory=/opt/veredus
ExecStart=/opt/veredus/veredus
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

Create the `veredus` user (or change `User=`), then run
`sudo systemctl enable --now veredus`. The logs are in
`journalctl -u veredus`.

On Windows, run `veredus.exe` from Task Scheduler ("At startup") or wrap it
as a service with a tool such as [NSSM](https://nssm.cc/).

## Running with Docker

Images for x86_64 and ARM64 Linux are published with each release. The
server alone:

```sh
docker run -d --name veredus --restart unless-stopped --stop-timeout 60 \
  -p 20595:20595/udp -v veredus:/data ghcr.io/victorcrimea/veredus
```

With the [sidecar](#extra-features-with-the-sidecar), use the
`ghcr.io/victorcrimea/veredus:latest-sidecar` image instead; nothing else
changes. A version number such as `:0.3.3` or `:0.3.3-sidecar` pins a
release.

Everything the server keeps (its config file, saved matches, results) lives
in `/data`, so keep that volume to let a match continue after the container
is recreated. To edit the config, put `config.toml` in the volume, or
mount a folder of your own there with `-v /srv/veredus:/data`; that folder
must be writable by user id 1000. Flags go after the image name, for
example `ghcr.io/victorcrimea/veredus --outcome-dir results`.

For [lobby hosting](#hosting-in-the-multiplayer-lobby), use
`--network host` instead of `-p`, because lobby games use the whole port
range 20595 to 20695, and set `public_ip` in the config: the server cannot
find out its public address from inside a container.

`--stop-timeout 60` gives the server time to finish writing a match's result
when you stop it.

## Building from source

With a current stable [Rust](https://rustup.rs/) toolchain:

```sh
cargo build --release
```

The binary is `target/release/veredus` (`veredus.exe` on Windows).

## Building the sidecar

The sidecar is the official 0 A.D. 0.28.0 release plus the patches in this
repository's `sidecar/patches` folder. Get them by cloning this repository,
or from the source code archive on the
[Releases page](https://github.com/victorcrimea/veredus/releases). The
sidecar must match the 0.28.0 game your players run.

It builds like 0 A.D. itself. First install the build dependencies listed
in the official
[build instructions](https://gitea.wildfiregames.com/0ad/0ad/wiki/BuildInstructions),
plus [Git LFS](https://git-lfs.com/) for the game data. Then, on Linux:

```sh
git lfs install
git clone --branch v0.28.0 --depth 1 https://gitea.wildfiregames.com/0ad/0ad.git
cd 0ad
for p in /path/to/veredus/sidecar/patches/*.patch; do git apply "$p"; done
libraries/build-source-libs.sh -j"$(nproc)"
build/workspaces/update-workspaces.sh --without-atlas --without-tests
make -C build/workspaces/gcc config=release -j"$(nproc)"
```

The result is `binaries/system/pyrogenesis`. Keep it inside the cloned
folder, because it loads the game data from there, and pass its path to
`--pyrogenesis-path`.

On Windows and macOS, clone and apply the patches the same way, then follow
the official build instructions for your system. The result is
`binaries\system\pyrogenesis.exe` on Windows.

## License

Veredus is Apache-2.0, see [LICENSE](LICENSE). The sidecar patches in
`sidecar/patches` change 0 A.D., so they are GPL-2.0 or later like 0 A.D.'s
code, see [sidecar/LICENSE](sidecar/LICENSE); 0 A.D.'s art is CC BY-SA 3.0.
The bundled ENet code is MIT, see
[src/enet/LICENSE.rusty_enet](src/enet/LICENSE.rusty_enet).
