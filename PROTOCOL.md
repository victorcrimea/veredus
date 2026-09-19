# 0 A.D. Multiplayer Hosting Protocol - Functional Specification

**Purpose.** This document specifies, in implementation-neutral terms, everything a
replacement **dedicated multiplayer server** (written in any language) must do to be
fully interoperable with the **unmodified 0 A.D. (pyrogenesis) game client**: transport,
wire format, the server's per-session phases, lockstep turn management, out-of-sync
detection, join, game state transfer, and the optional XMPP-lobby integration.


**Baseline.** 0 A.D. `0.28.0`, with the protocol values and behaviour described below.

**Version anchors:**

| Anchor | Value |
|---|---|
| Game version (exchanged in handshakes) | `0x01010019` |
| Simulation version string  | `"0.28.0"` |
| Default port | `0x5073` = **20595** |

This document describes behaviour.  It contains requirement prose, tables, byte layouts
and message sequences, not engine source code. Where it names a message, a numeric code or
a fixed value, that name is this document's own label for something observable on the wire;
the wire ID or numeric value is what is normative, and a label may differ from any name the
engine uses. Behaviour is described at the wire level. Ordering, thresholds and recipient
sets are specified only where a client can observe them; where the document gives a sequence
of steps, only the resulting observable effect is normative.

**Terms:** **server** = the entity this document specifies; **client** = a stock 0 A.D.
game instance connecting to it; **controller** = the privileged client that drives game
setup and start (in the stock game, the hosting player).


---

## Table of Contents

1. [Architecture Overview](#1-architecture-overview)
2. [Transport Layer (ENet)](#2-transport-layer-enet)
3. [Message Framing and Primitive Encodings](#3-message-framing-and-primitive-encodings)
4. [Message Catalog](#4-message-catalog)
5. [Script-Value Binary Encoding](#5-script-value-binary-encoding)
6. [Server-Observable State](#6-server-observable-state)
7. [Session Phases](#7-session-phases)
8. [Connection and Handshake](#8-connection-and-handshake)
9. [Authentication](#9-authentication)
10. [Setup Phase](#10-setup-phase)
11. [Game Start and Loading](#11-game-start-and-loading)
12. [In-game: Lockstep Turns, Commands, Out-of-sync Detection](#12-in-game-lockstep-turns-commands-out-of-sync-detection)
13. [Join and Late Observers](#13-join-and-late-observers)
14. [Game State Transfer Sub-protocol](#14-game-state-transfer-sub-protocol)
15. [Connection Monitoring, Pause, Disconnects](#15-connection-monitoring-pause-disconnects)
16. [What the Client Expects (Client Model)](#16-what-the-client-expects-client-model)
17. [Lobby Integration (XMPP)](#17-lobby-integration-xmpp)
18. [Password Hashing](#18-password-hashing)
19. [Constants and Configuration](#19-constants-and-configuration)
20. [Dedicated-Server Design Considerations](#20-dedicated-server-design-considerations)
21. [Security Considerations](#21-security-considerations)
22. [Known Quirks of the Stock Server](#22-known-quirks-of-the-stock-server)
23. [Conformance Checklist](#23-conformance-checklist)
24. [Sequence Diagrams](#24-sequence-diagrams)
25. [Open Questions / To Verify by Capture](#25-open-questions--to-verify-by-capture)

---

## 1. Architecture Overview

- 0 A.D. multiplayer is **deterministic lockstep**. Every client runs the full simulation
  itself. The **server never simulates**; state hashes are computed by clients and only
  *compared* by the server. It acts as:
  1. a **gatekeeper**: ENet transport, handshake, game/simulation/mod compatibility checks,
     password or lobby (XMPP) authentication;
  2. a **relay** for simulation commands, chat, flares, game-settings data, and pre-game status and pause state;
  3. a **referee** for turn advancement: it declares a turn "ready" only when every
     required client has finished sending commands for it;
  4. an **out-of-sync detector**: it compares per-turn state hashes reported by clients;
  5. a **broker** for join and saved-game state: it obtains a serialized snapshot from
     one connected client and streams it to a joining client (game state transfer);
  6. the **authority** on identity: it issues UUIDs, client IDs, player slot assignments,
     controller role, kicks and bans; and emits connection warnings.
- A dedicated server therefore needs **no game logic, no map loading, no simulation**.
  It needs: an ENet stack, a message codec (Sec. 3-4), the session phases (Sec. 7), the
   flows (Sec. 8-15), and optionally lobby/XMPP integration (Sec. 17).
- One connected client is the **controller** (the "host" in the UI). Only the controller
  may change game settings, map player IDs to slots, reset pre-game status, kick or ban, and start the game.
  In the stock game the controller is the player whose game process also runs the server.
  A dedicated server must choose a controller policy (see Sec. 20.1).
- Clients are identified by:
  - **UUID**: a 16-character uppercase hex string issued by the server at handshake. It
    is random, not stable across reconnects, keys all server-side maps (including player slots), and doubles as the lobby-auth token.
  - **Client ID**: a `u32` issued at authentication, starting at 1 and incremented per
    successful authentication. It is used by the turn manager and ends up in simulation
    command envelopes. Truncated to 2 bytes in `AUTHENTICATE_RESULT`.
  - **Name**: a sanitized display name. Join detection is by name; slot recovery uses
    UUID first, then name.
- Player slot IDs: `-1` means observer or unassigned; `1..8` are player slots.

**Hard external dependency: ENet.** The transport below the message layer is ENet's
reliable-UDP protocol (connection handshake, sequenced reliable delivery, peer timeouts).
The wire format is **ENet 1.3-compatible**; reusing an existing ENet implementation is
strongly recommended, and reimplementing ENet itself is out of scope.

---

## 2. Transport Layer (ENet)

| Property | Value |
|---|---|
| Wire protocol | ENet 1.3.x UDP protocol. Must interoperate with stock ENet 1.3 peers.|
| Default port | **20595** (`0x5073`) UDP |
| Server peer limit | **41** — room for a full 8-player match plus observers, plus one spare peer kept free so a client arriving when the server is full can still be told so |
| Channel count | **1** (both server host and client connect) |
| Channel used | **0** for all messages |
| Packet flags | **All application packets are reliable**. No unreliable traffic. Single reliable channel -> fully ordered message stream. |
| Bandwidth limits | incoming = 0, outgoing = 0 (unlimited) |
| MTU | Host MTU forced to **1372** |
| Connect `data` | Client connects with channelCount 1, data 0 |
| Disconnect `data` | The **disconnect reason code** (Sec. 19.3) as the 32-bit ENet disconnect `data`. The client shows it to the user. |
| Peer timeouts / ping | No explicit per-peer timeout configuration |

Rules a server must honor:

- **Exactly one application message per ENet packet** (Sec. 3). The receiver checks that
  the declared size equals the packet length. Never coalesce two messages into one
  packet; never split one across packets.
- Actual connection-loss detection is delegated to **ENet's own peer timeout**; the
  application layer never disconnects for silence (it only *warns*, Sec. 15.1).


---

## 3. Message Framing and Primitive Encodings

### 3.1 Header (all messages)

```
offset size field
0      1    type   u8   (message type, Sec. 4.1)
1      2    size   u16  BIG-endian; TOTAL message length INCLUDING this 3-byte header
3      ...  body   (size - 3) bytes
```

- The receiver rejects the message if fewer than 3 bytes arrive, or if
  `size != packet length`.
- **Size limit:** the size field is 16 bits. Larger values are truncated when written, so
  any message longer than **65 535 bytes is unusable** (it fails the size check on
  receipt). This caps e.g. the game-settings blob. Never emit larger messages; use game state
  transfer (Sec. 14) for bulk data.
- No alignment, padding, checksum, MAC, compression, or encryption at this layer.
- Unknown `type` -> the message is dropped and the connection stays open. Type 0 is
  invalid and is never sent.

### 3.2 Primitive field encodings

Fields are sequences of fixed-width integers and two string encodings. All multi-byte
integers are **big-endian** with a fixed declared width. Only widths 1, 2 and 4 are used. There are no bool/u64/float/double
field types; booleans travel as `u8`.

| Type | Encoding |
|---|---|
| `int(N)` | N bytes big-endian (N in {1,2,4}; signedness only affects interpretation) |
| `bytes` (u32-prefixed byte string) | `u32` BE byte length, then that many raw bytes. **No terminator.** Opaque bytes (ASCII/UTF-8 in practice). |
| `text` (0x0000-terminated UTF-16BE string) | Sequence of 16-bit **big-endian** code units, then a `0x0000` terminator. **No length prefix.** Serialized length = 2*n + 2. |
| Array | **No count prefix.** Elements (each a fixed sequence of fields) are concatenated **until the end of the message**. An array is therefore always the last field. An empty array is encoded as zero bytes. |

Decoders must bail out (drop the message) if a field would read past the end.

### 3.3 Two exceptional messages (mixed endianness - critical)

`PLAYER_COMMAND` (29) and `GAME_SETTINGS` (9) bodies use the **little-endian**
script-value serializer described in Sec. 5, not the fixed-width integer encoding. Their
3-byte header is still the big-endian header from Sec. 3.1.

```
PLAYER_COMMAND: [type u8][size u16 BE][client u32 LE][player i32 LE][turn u32 LE][ScriptValue]
GAME_SETTINGS:         [type u8][size u16 BE][ScriptValue]
```

Every other message is purely big-endian per Sec. 3.2.

---

## 4. Message Catalog

### 4.1 Message type IDs

Message type IDs are wire-significant and frozen. Type 0 and unknown types are invalid:
they never appear on the wire and must be dropped on receipt.

| ID | Hex | Name | Direction(s) on the wire |
|---:|---|---|---|
| 1 | 0x01 | `SYN` | S->C |
| 2 | 0x02 | `SYN_ACK` | C->S |
| 3 | 0x03 | `ACK` | S->C |
| 4 | 0x04 | `AUTHENTICATE` | C->S (credentials); S->C (empty, "please authenticate now" - lobby auth) |
| 5 | 0x05 | `AUTHENTICATE_RESULT` | S->C |
| 6 | 0x06 | `CHAT` | C->S, S->C |
| 7 | 0x07 | `PRE_GAME_STATUS` | C->S, S->C |
| 8 | 0x08 | `RESET_PREGAME_STATUS` | C->S (controller) |
| 9 | 0x09 | `GAME_SETTINGS` | C->S (controller), S->C |
| 10 | 0x0A | `MAP_PLAYER_ID_TO_SLOT` | C->S (controller) |
| 11 | 0x0B | `PLAYER_SLOTS` | S->C |
| 12 | 0x0C | `GAMESTATE_REQUEST` | both |
| 13 | 0x0D | `GAMESTATE_RESPONSE` | both |
| 14 | 0x0E | `GAMESTATE_CHUNK` | both |
| 15 | 0x0F | `GAMESTATE_CHUNK_ACK` | both |
| 16 | 0x10 | `JOIN` | S->C |
| 17 | 0x11 | `JOINED` | C->S, S->C |
| 18 | 0x12 | `KICKED` | C->S (controller request), S->C (notification) |
| 19 | 0x13 | `LAST_SEEN` | S->C |
| 20 | 0x14 | `LAGGING_CLIENTS` | S->C |
| 21 | 0x15 | `PLAYERS_LOADING` | S->C |
| 22 | 0x16 | `PLAYER_PAUSE` | C->S, S->C |
| 23 | 0x17 | `LOADED_GAME` | C->S, S->C |
| 24 | 0x18 | `START_SETTINGS` | C->S (controller), S->C |
| 25 | 0x19 | `START_SAVEGAME_SETTINGS` | C->S (controller), S->C |
| 26 | 0x1A | `TURN_SEALED` | C->S, S->C |
| 27 | 0x1B | `STATE_HASH` | C->S |
| 28 | 0x1C | `WRONG_HASH_PLAYERS` | S->C |
| 29 | 0x1D | `PLAYER_COMMAND` | C->S, S->C |
| 30 | 0x1E | `FLARE` | C->S, S->C |

### 4.2 Protocol constants

| Value | Meaning |
|---|---|
| `0x5073013F` | the fixed challenge value the server carries in `SYN` |
| `0x50630121` | the fixed response value the client carries in `SYN_ACK` |
| `0x01010019` | the game version; must match exactly, else reject |
| `0x5073` (decimal 20595) | the default UDP port |
| `0x1` | the lobby-auth bit of the `ACK` flags |
| `"0.28.0"` | the simulation version string: major.minor.serialization-compatible-patch |

### 4.3 Field layouts

Notation: `name: type` in wire order. `int(N)` = N-byte BE integer. `bytes` and `text`
are the string encodings from Sec. 3.2. `[ ... ]*` = array to end of message.

| ID | Message | Body fields |
|---:|---|---|
| 1 | SYN | `challenge: int(4)`, `game_version: int(4)`, `simulation_version: bytes`, `[ mod_name: bytes, mod_version: bytes ]*` |
| 2 | SYN_ACK | `response: int(4)`, `game_version: int(4)`, `simulation_version: bytes`, `[ mod_name: bytes, mod_version: bytes ]*` |
| 3 | ACK | `game_version: int(4)`, `flags: int(4)`, `uuid: bytes` |
| 4 | AUTHENTICATE | `name: text`, `password: bytes` (hashed, Sec. 18), `controller_proof: bytes`. The server's prompt variant sends all fields empty. |
| 5 | AUTHENTICATE_RESULT | `game_stage: int(4)`, `client_id: int(2)` (**u32 truncated to 16 bits**), `controller: int(1)`, `gui_text: text` |
| 6 | CHAT | `sender_uuid: bytes` (ignored C->S, overwritten by server), `message: text`, `[ receiver_uuid: bytes ]*` (empty = everyone; **emptied in the relayed copy**) |
| 7 | PRE_GAME_STATUS | `uuid: bytes` (empty C->S; filled S->C), `status: int(1)` |
| 8 | RESET_PREGAME_STATUS | *(empty)* |
| 9 | GAME_SETTINGS | script value (Sec. 5), no envelope; opaque to server |
| 10 | MAP_PLAYER_ID_TO_SLOT | `playerSlot: int(1)` (signed), `uuid: bytes` |
| 11 | PLAYER_SLOTS | `[ uuid: bytes, name: text, playerSlot: int(1) signed, status: int(1) ]*` |
| 12 | GAMESTATE_REQUEST | `state_type: int(1)` signed (0 = SAVEGAME, 1 = RUNNING_GAME), `stream_id: int(4)` |
| 13 | GAMESTATE_RESPONSE | `stream_id: int(4)`, `length: int(4)` |
| 14 | GAMESTATE_CHUNK | `stream_id: int(4)`, `data: bytes` (raw bytes, <=1024) |
| 15 | GAMESTATE_CHUNK_ACK | `stream_id: int(4)`, `chunks_delivered: int(4)` |
| 16 | JOIN | `settings_json: bytes` (JSON text) |
| 17 | JOINED | `uuid: bytes` (empty C->S; filled S->C) |
| 18 | KICKED | `name: text`, `ban: int(1)` |
| 19 | LAST_SEEN | `uuid: bytes`, `ms_ago: int(4)` (ms) |
| 20 | LAGGING_CLIENTS | `[ uuid: bytes, mean_rtt_ms: int(4) ]*` |
| 21 | PLAYERS_LOADING | `[ uuid: bytes ]*` |
| 22 | PLAYER_PAUSE | `uuid: bytes` (empty C->S; filled S->C), `paused: int(1)` |
| 23 | LOADED_GAME | `turn: int(4)` |
| 24 | START_SETTINGS | `settings_json: bytes` (JSON text) |
| 25 | START_SAVEGAME_SETTINGS | `settings_json: bytes` (JSON text) |
| 26 | TURN_SEALED | `turn: int(4)`, `turn_duration_ms: int(2)` (**u32 truncated to 16 bits**; ms; clients send 0, server ignores it) |
| 27 | STATE_HASH | `turn: int(4)`, `hash: bytes` (opaque **raw binary** digest - 16-byte MD5 in the current engine, not hex) |
| 28 | WRONG_HASH_PLAYERS | `turn: int(4)`, `reference_hash: bytes` (raw binary), `[ mismatched_name: text ]*` |
| 29 | PLAYER_COMMAND | LE envelope + script value (Sec. 3.3, Sec. 5.1) |
| 30 | FLARE | `uuid: bytes` (ignored C->S; filled S->C), `x: bytes`, `y: bytes`, `z: bytes` (**decimal number strings**, not floats) |

### 4.4 Enumerations carried in messages

**Authentication game stage** (`AUTHENTICATE_RESULT.game_stage`):

| Value | Meaning |
|---:|---|
| 0 | joined a new game setup |
| 1 | joined; server was created to continue a saved game |
| 2 | joined a game already in progress (join or late observer) |
| 3 | invalid password; defined but **never sent** (wrong passwords cause a disconnect with code 14 instead) |

**Pre-game status** (`PRE_GAME_STATUS.status`, `PLAYER_SLOTS.status`): `0` = not ready, `1` = ready,
`2` = "stay ready" (survives RESET_PREGAME_STATUS).

**Disconnect reasons**: see Sec. 19.3.

---

## 5. Script-Value Binary Encoding

Used by `PLAYER_COMMAND` (29) and `GAME_SETTINGS` (9). This is a **binary structured
clone** (not JSON). **All integers here are LITTLE-endian.**

> **Relay simplification.** A dedicated server does not need to decode script values in
> order to relay them. It can treat everything after the fixed fields as opaque bytes and
> forward the original packet byte-for-byte. This sidesteps re-serialization fidelity and
> is the recommended strategy. It must still read the fixed-offset envelope of
> `PLAYER_COMMAND` (Sec. 5.1). A decoder is needed only for features such as building
> lobby registration data from `GAME_SETTINGS` (Sec. 17.3) or inspecting commands.

### 5.1 PLAYER_COMMAND envelope

```
offset (from message start)
0   u8      type = 0x1D
1   u16 BE  size
3   u32 LE  client   - sender's client ID as the client believes it (NOT rewritten or validated by server)
7   i32 LE  slot     - player slot the command is for
11  u32 LE  turn     - turn at which to execute (sender's current turn + 4)
15  ...     command  - script value (Sec. 5.2), runs to end of message, Spidermonkey format
```

### 5.2 Value grammar

```
value      := tag:u8 payload
tag values:
  0  VOID (undefined)          payload: none
  1  NULL                      payload: none
  2  ARRAY                     payload: arrayLength:u32  props
  3  OBJECT (plain)            payload: props
  4  STRING                    payload: string_value
  5  INT                       payload: i32
  6  DOUBLE                    payload: f64 (8 raw bytes, sender-native order - see note)
  7  BOOLEAN                   payload: u8 (0|1; decoder rejects other values)
  8  PRIOR_OBJECT              payload: number; must be 0..2^31-1 and refer to an already-decoded object
  9  TYPED_ARRAY               payload: element_type_code:u8 view_offset:u32 length:u32 backing_buffer:value
  10 ARRAY_BUFFER              payload: byteLength:u32 bytes[byteLength]
  11 OBJECT_PROTOTYPE          payload: prototype_name:utf8_bytes  (single_embedded_value:value | props)   - see below
  12 OBJECT_NUMBER  (new Number)  payload: f64 (sender-native order, as DOUBLE)
  13 OBJECT_STRING  (new String)  payload: string_value
  14 OBJECT_BOOLEAN (new Boolean) payload: u8 (0|1)
  15 OBJECT_MAP                payload: count:u32  (key:value val:value){count}
  16 OBJECT_SET                payload: count:u32  (val:value){count}

props           := count:u32 (property_key:string_value property_value:value){count}
string_value    := encoding_flag:u8(0|1) length:u32 chars
                   encoding_flag=1: chars = length bytes, Latin-1
                   encoding_flag=0: chars = 2*length bytes, UTF-16LE code units
                   (no NUL terminator)
utf8_bytes      := byteLength:u32 bytes (UTF-8)     - only used for prototype names (max 256 chars)
element_type_code := 0 Int8 | 1 Uint8 | 2 Int16 | 3 Uint16 | 4 Int32 | 5 Uint32 | 6 Float32 | 7 Float64 | 8 Uint8Clamped
```

**Double byte order.** The 8 bytes of a double are copied with **no byte-order
conversion**, unlike the integer paths. The format is **IEEE-754 binary64 in
sender-native order**; little-endian on all shipped platforms. Treat it as
little-endian. 

Rules and semantics:

- **Numbers.** A number that is exactly representable as a 32-bit signed integer is
  always encoded as `INT`, otherwise as `DOUBLE`. `NaN` cannot be serialized. Encoders must follow the same rule.
- **Arrays.** Tag 2, then `arrayLength` (the JS `length`, which may exceed the
  number of enumerated elements for holes), then a normal `props` block. **Elements are
  encoded as properties whose names are the decimal index strings** `"0"`, `"1"`, ...,
  in enumeration order, followed by any non-index properties.
- **Property order** is the object's own enumerable property order (insertion order,
  with integer-like keys first in ascending order per Spidermonkey semantics). Getter
  properties are rejected.
- **Property names never have a type tag.** Each property key travels as a bare
  `string_value` (encoding flag, character count, characters).
- **String representation.** Either Latin-1 or UTF-16LE may appear for any string,
  depending on the sender's internal representation. ASCII strings are typically Latin-1.
  Decoders must accept both. Encoders may choose either; Latin-1 is valid only
  if all code units are <= 0xFF.
- **Reference numbering for repeatable objects.** Objects that can be named again later
  are numbered from **1** in pre-order: a value with tag 2, 3, 9, 10, 11, 12, 13, 14, 15
  or 16 is assigned the next number when it is first encountered, before its payload is
  written. Any later mention of an already-numbered object travels as
  `PRIOR_OBJECT(number)`. A typed-array view (tag 9) is numbered **before** its buffer
  value (tag 10), so the view gets the lower number; maps and sets (tags 15, 16) are
  numbered before their entries; arrays, plain objects and prototype objects are numbered
  before their properties.
- **OBJECT_PROTOTYPE (11)** is followed by `prototype_name` and then exactly one of three
  data-format variants:
  - one standalone data `value`, with no property block following;
  - nothing further;
  - a normal `props` block.


- **Functions** and objects of any other built-in class cannot be serialized.

---

## 6. Server-Observable State

### 6.1 Server phases

The server is in one of four phases:

- **idle**: before the server accepts connections.
- **setup**: a match is being configured and clients are joining.
- **loading**: the match has started and clients are loading the map.
- **in-game**: the match is running.

The server starts in `idle`. Opening the listening socket enters `setup`. Starting a
match enters `loading`, and once all sessions have loaded it enters `in-game`.

A `PLAYER_SLOTS` list is ordered by UUID string; disconnected players keep their
player slot so they can join. Client IDs start at 1 and increase. Saved commands and the
turn length used for each turn are retained for the whole match because join replay
needs both (Sec. 13.3).

---

## 7. Session Phases

Each connection is in one of these phases. The table states what the client observes
while its connection is in that phase; the names are this document's vocabulary for
those behaviour classes, not a prescribed internal design (see the note after the
table).

| Phase | What the client observes |
|---|---|
| `closed` | no connection; nothing is sent to or accepted from it |
| `await-handshake` | the server sent `SYN` and ignores every message except `SYN_ACK` |
| `await-lobby-auth` | the server sent `ACK` with the lobby flag; the client's `AUTHENTICATE` is ignored until the server sends the empty `AUTHENTICATE` prompt, which follows acceptance of its lobby-auth IQ (Sec. 17.2) |
| `await-auth` | the server sent `ACK` without the lobby flag; `AUTHENTICATE` is answered as soon as it arrives |
| `setup` | the client takes part in setup: its chat and pre-game status messages are relayed, and it receives `PLAYER_SLOTS` and `GAME_SETTINGS` relays; it stays in this phase while it loads the map after `START_SETTINGS` |
| `syncing` | a joining client: it receives `JOIN` and the replay stream; in-game traffic is not sent to it, and its own messages have no effect on the match, until it reports `LOADED_GAME` |
| `in-game` | full participation: its commands are echoed back, and it receives turn seals, out-of-sync reports, pauses and every other in-game broadcast |

These phases are behaviour classes, not a data structure. Only the observable
accept/ignore and send behaviour specified below is normative; an implementation need
not store these phases at all, and should not mirror the table one-to-one as an enum.

A session moves from `setup` to `in-game` only when it reports `LOADED_GAME`.

"Controller only" below = a message accepted only if the sender's UUID equals the stored
controller UUID; otherwise it is **silently ignored** (no disconnect, no error to sender).

**Messages accepted per phase.** A message with no listed effect in the current phase is
ignored and the connection stays open.

| Phase | Accepted message | Server behaviour | New phase |
|---|---|---|---|
| `await-handshake` | `SYN_ACK` | validate and issue a UUID (Sec. 8.2) | `await-auth` or `await-lobby-auth` |
| `await-lobby-auth` | `AUTHENTICATE` | authenticate (Sec. 9.2) | `setup` or `syncing` |
| `await-auth` | `AUTHENTICATE` | authenticate (Sec. 9.2) | `setup` or `syncing` |
| `setup` | CHAT, PRE_GAME_STATUS, RESET_PREGAME_STATUS, GAME_SETTINGS, MAP_PLAYER_ID_TO_SLOT, KICKED | apply the permitted operation | `setup` |
| `setup` | START_SETTINGS, START_SAVEGAME_SETTINGS | begin loading (the server-wide phase becomes `loading`) | `setup` (the session's own phase is unchanged) |
| `setup` | LOADED_GAME | record loading progress | `in-game` |
| `syncing` | KICKED | apply the kick | `syncing` |
| `syncing` | LOADED_GAME | replay state and enter the match | `in-game` |
| `in-game` | JOINED, KICKED, PLAYER_PAUSE, CHAT, PLAYER_COMMAND, FLARE, STATE_HASH, TURN_SEALED | apply the permitted operation | `in-game` |

- The four `GAMESTATE_*` messages are accepted in any phase (Sec. 14).
- Sessions in `syncing` do not receive in-game broadcasts and are not registered with the
  turn manager until they finish syncing.

**Disconnect(session, reason).** This operation, used throughout this spec, has two
wire-observable effects: the remaining clients receive the updated `PLAYER_SLOTS`
(the session's player slot is marked disconnected and omitted, Sec. 10.1), and the
session's peer receives an ENet disconnect carrying `reason`. The player slot update is
broadcast first, so the departing client receives that final `PLAYER_SLOTS` before
its peer is dropped. Afterwards no further messages are delivered to the session.
Connection loss in `closed`, `await-handshake`, `await-lobby-auth` or `await-auth` has
no further effect.

---

## 8. Connection and Handshake

### 8.1 ENet CONNECT

1. Create a session in `await-handshake`.
2. If the peer IPv4 is banned, disconnect with code 7 (banned) immediately, before
   any handshake.
3. Otherwise send **SYN**:
   - `challenge` = the fixed challenge value, `game_version` = the game version,
     `simulation_version` = the simulation version string (all three in Sec. 4.2);
   - `mods` = the enabled mods as (name, version) in **load order**. The dedicated server
     must advertise exactly what its clients run, so make this list configurable. The list
     must equal the client's list exactly, in order. For vanilla (`0.28.0`) this is exactly
     **`[("0ad","0.28.0")]`**.

### 8.2 SYN_ACK processing

When a `SYN_ACK` arrives, the server MUST apply these checks in order and
disconnect with the first applicable code:

| Condition | Disconnect code |
|---|---|
| `game_version` != `0x01010019` | 3 (game version mismatch) |
| the compatibility check fails | 17 (simulation or mod mismatch) |
| no unique UUID can be issued | 11 (no UUID could be issued) |

Otherwise it issues a fresh UUID, stores it for the session, and sends
`ACK { game_version = 0x01010019, flags, uuid }`.

The compatibility check passes when the client's `simulation_version` equals the server's, the
mod counts are equal, and for every index the server's `mod_name + "-" + mod_version`
equals the client's value at the same position (positional; order matters). The client's
`response` is **not** checked.

`flags` is 0, except in lobby mode where bit 0, the lobby-auth bit, is set. A set
flag puts the session in `await-lobby-auth`; a clear flag puts it in `await-auth`.

**UUID format:** two independent random 32-bit values, each formatted as 8 uppercase
hex digits and concatenated, e.g. `"1F0A33C49B7E0012"`. A UUID must not collide with any
session's UUID. A server MAY disconnect with code 11 if no unique UUID can be issued.

When the client sees a disconnect with reason 17 (simulation or mod mismatch), it compares
its own handshake with the stored SYN to show which
component differs. The server's handshake must therefore be accurate. **Send 17, not 16**
(see Sec. 22.14).

### 8.3 What the client does next

- `flags & 0x1` **clear** -> the client immediately sends `AUTHENTICATE`.
- `flags & 0x1` **set** -> the client sends an XMPP `lobbyauth` IQ carrying its UUID to
  the host JID (Sec. 17.2) and **waits** for the server to send it an **empty
  `AUTHENTICATE`** message. It then sends its real `AUTHENTICATE`. If the client has no
  lobby connection, it aborts locally with code 10 (lobby authentication failed).
---

## 9. Authentication

### 9.1 Lobby-auth token processing (lobby mode only)

When the XMPP side receives a lobby-auth IQ from lobby user `U` (the JID node part) with
token `T`:

- find the session whose UUID equals `T`; if none, ignore the IQ;
- set that session's lobby user name to `U`;
- send that session an `AUTHENTICATE` message with all fields empty (a 13-byte message).

A dedicated server MAY accept this IQ only while the session is in `await-lobby-auth`; a
server that processes it in any phase matches the stock client more closely.

### 9.2 AUTHENTICATE processing

Three name forms are used below:

- **raw name** - the `name` field exactly as sent;
- **sanitized name** - the raw name after sanitization (Sec. 9.3);
- **suffix-stripped name** - the sanitized name up to the first `" ("`, or the whole
  sanitized name when that substring is absent.

Checks run in the order below; the first condition that matches ends processing with the
listed disconnect code. This order is itself wire-observable, not incidental control
flow: when two conditions hold at once (for example, a banned name that would also fail
the password check), only one disconnect code ever reaches the client, and that code
reveals which check ran first. The order below is read off that observable priority.

| # | Condition | Result |
|---|---|---|
| 1 | the server phase is `loading` | disconnect 4 (server is loading) |
| 2 | lobby mode and the suffix-stripped name (lowercased) differs from the session's lobby user name (lowercased) | disconnect 10 (lobby authentication failed) |
| 3 | `hash(serverPassword, rawNameUtf8) != password` | disconnect 14 (refused) |
| 4 | not (non-lobby mode with duplicate names allowed) and another session already uses the sanitized name | disconnect 8 (player name in use) |
| 5 | (lobby mode ? suffix-stripped name : sanitized name) is in the ban list | disconnect 7 (banned) |
| 6 | the client cannot be admitted (see below) | disconnect 5 (match already in progress) or 9 (server full) |

If check 3 passes, the empty-password case is handled by the hash rule itself: an empty
server password hashes to `""`, so without a password the client must send an empty
`password`. Check 3 always runs, even without a password.

If non-lobby duplicate names are allowed, the sanitized name is first replaced by the
deduplicated candidate (Sec. 9.3) and check 4 is skipped. The salt for check 3 is the
**raw** name; every other name-based check uses the sanitized or suffix-stripped form.

**Admission (check 6).** In server phase `setup`, the client is admitted unless the
number of sessions `S` is already 41 (unauthenticated sessions included); capacity
rejection uses code 9.

In `loading` or `in-game`, the client is admitted only if it is **joining** or the
late-join policy allows it:

- Joining means a **disconnected** player slot exists with the same sanitized name. Its
  old slot and observer flag are restored (Sec. 10.1). Join detection compares against
  disconnected player slots only, and is by name.
- The late-join policy is a server choice: admit everyone, admit only names on a buddy
  list (matched on the suffix-stripped name), or deny. A denied late join is rejected with
  code 5.
- Capacity: let `S` be the number of sessions, `P` the number of connected player slots
  (connected player slots with slot != -1), `D` the number of disconnected player slots
  (disconnected player slots with slot != -1), and `O` the observer limit. An observer is
  rejected with code 9 when `S - P > O` or `S + D >= 41`.

**Admission result.**

- Issue the next client ID (starting at 1 and increasing) and store the sanitized (or
  deduplicated) name and client ID on the session.
- `AUTHENTICATE_RESULT`: for a join `game_stage` is 2; when the server continues a saved
  game it is 1; otherwise 0. `client_id` carries the new client ID, `controller` is 0, and
  `gui_text` is `"Logged in"`. When the message's `controller_proof` matches the server's
  controller secret and the controller role is still unclaimed, the session is promoted to
  controller (`controller = 1`).
- Add the session to the player slots table (Sec. 10.1) and send it the full
  `PLAYER_SLOTS`.
- A non-joining session moves to `setup`. A joining session starts the snapshot fetch
  (Sec. 13.1) and moves to `syncing`.

**Controller-secret pitfall.** If a dedicated server leaves the controller secret empty,
the *first* client sending an empty secret (all stock clients do) becomes controller. See
Sec. 20.1.

### 9.3 Name sanitization and deduplication

**Sanitization.** Names are stored and compared in normalized form. A normalized name
contains no `[` or `]` (they become `{` and `}`), is at most 32 code units long, has no
leading or trailing whitespace, and is never empty (`"Anonymous"` is substituted). Length
is limited before surrounding whitespace is removed, so the result may be shorter than 32.

None of this is observable on the wire. The client never inspects or validates the shape
of a name, so the exact rule is a parity choice: the rule above (the `"Anonymous"`
fallback included) reproduce the stock server's behaviour and are this document's
configuration default, not an interoperability requirement.

**Deduplication.** When duplicate names are permitted (Sec. 9.2) and a candidate name is
already displayed by some session, the server appends a counter suffix — first `" (2)"`,
then `" (3)"`, and so on — until it reaches a candidate that no session displays.

The suffix spelling, unlike the sanitization steps, is constrained. Lobby authentication
compares the XMPP username against the name cut at the first `" ("` (Sec. 17.2), so a
deduplicated name has to remain recoverable by cutting at `" ("`; any other separator
would break lobby-mode name matching.

---

## 10. Setup Phase

### 10.1 Player slots

A player slot holds: the client UUID, the display name, a slot value (`-1` = observer or
unassigned, `1..8` = player slot), a ready status, and whether the client is connected.
`PLAYER_SLOTS` arrays list the connected player slots in **UUID string order**.
Disconnected players keep their player slot (so they can join) and are **omitted** from
`PLAYER_SLOTS` broadcasts.

**Adding a client.** When a client is admitted (Sec. 9.2):

- Start with slot `-1` and status 0.
- If the match has already started (server phase `in-game`), try to recover a disconnected
  slot: first a player slot with the same UUID whose slot is not held by a connected
  player, then a player slot with the same name whose slot is not held by a connected
  player. If one is found, take its slot and drop the old player slot.
- Store the new player slot as connected and broadcast `PLAYER_SLOTS`.

**Removing a client.** On disconnect, mark the player slot disconnected and broadcast
`PLAYER_SLOTS`.

**Broadcast contents.** `PLAYER_SLOTS` carries only connected player slots, each as
(UUID, name, slot, status), and is sent to every session in `setup`, `syncing` and
`in-game`.

**Player slot assignment.** `MAP_PLAYER_ID_TO_SLOT{playerSlot, uuid}` sets the named player slot's value to
`playerSlot` and clears that slot from any other player slot that currently holds it. Any
slot value is accepted; status is unchanged. Then broadcast `PLAYER_SLOTS`.

**Reset pre-game status.** Every player slot whose status is not 2 becomes status 0. Then
broadcast `PLAYER_SLOTS`.

`PLAYER_SLOTS` is broadcast on add, remove, player slot assignment, reset pre-game status, and game start -
**not** on an individual PRE_GAME_STATUS (Sec. 10.2).

### 10.2 Handlers (sessions in `setup`)

| Message | Behaviour |
|---|---|
| **CHAT** | Overwrite `sender_uuid` with the session's UUID (spoof-proofing). Take the receiver list and pass it to delivery; the relayed copies carry an **empty** receiver list. An empty receiver list means all sessions in `setup` and `in-game`; otherwise only the listed UUIDs receive it. The sender receives its own message if it is in scope. **No content validation, length cap, or rate limit.** The same handler runs in `in-game`. |
| **PRE_GAME_STATUS** | Ignore while the server is in `loading`. Otherwise overwrite `uuid` with the session's UUID, relay the PRE_GAME_STATUS to `setup` sessions, then set the player slot's status to the message's status. **No** `PLAYER_SLOTS` is broadcast. Relaying only to `setup` sessions is forced: the client's own state machine accepts PRE_GAME_STATUS only in its `setup` state (Sec. 16.1), so a session outside `setup` would drop it anyway. |
| **RESET_PREGAME_STATUS** | Controller only -> reset pre-game status (Sec. 10.1). |
| **GAME_SETTINGS** | Ignored unless the server is in `setup`. Controller only. Relay **verbatim** to `setup` sessions (including the controller). No content validation. late joiners get settings only when the controller's UI sends them again. |
| **MAP_PLAYER_ID_TO_SLOT** | Controller only -> player slot assignment (Sec. 10.1). |
| **KICKED** | Controller only -> kick (Sec. 10.3). |
| **START_SETTINGS** | Controller only -> begin the match (Sec. 11.1). |
| **START_SAVEGAME_SETTINGS** | Controller only -> saved-game flow (Sec. 11.2). |
| **LOADED_GAME** | Sec. 11.3. |

### 10.3 Kick

On a controller `KICKED{name, ban}`:

- Find the first session whose display name equals `name`. If none is found, or the match
  is the controller's own session, do nothing: the controller can never be kicked.
- If `ban` is set, add the name to the banned-name list (the suffix-stripped form in
  lobby mode, otherwise the plain name) and the session's IPv4 to the banned-IP list.
- Disconnect that session with code 7 (banned) when banning, otherwise code 6
  (kicked). This first removes the player and broadcasts the updated
  player slots list (Sec. 7).
- Relay `KICKED{name, ban}` to `setup`, `syncing` and `in-game` sessions.

Ban lists are in-memory only and are checked at connect (IP) and authentication (name).

---

## 11. Game Start and Loading

### 11.1 Starting a match

When the controller sends `START_SETTINGS`, the server MUST first enforce readiness: if any
connected player slot has status 0, the start is **rejected** and nothing is sent. (This
gate is a deliberate deviation from the stock server, which relays `START_SETTINGS` anyway
and then cannot accept the clients' `LOADED_GAME`; see Sec. 22.1.)

Otherwise the server prepares the match - it chooses the turn length (default
200 ms), freezes the game settings JSON (needed later for
`JOIN` and the cheats check) and enters server phase `loading` - and the
clients receive, in order:

1. a session that has not completed authentication (any phase before `setup`) receives
   an ENet disconnect with code 4 (server is loading);
2. every session in `setup`, `syncing` and `in-game` receives the updated
   `PLAYER_SLOTS` (player slots of disconnected players are dropped);
3. every session in `setup` receives `START_SETTINGS{settings_json}` **verbatim**.

From the `START_SETTINGS`, every session that was in `setup` now counts for turn release
(Sec. 12.4): the server expects its first `TURN_SEALED` for turn 4 and its first
`STATE_HASH` for turn 1, and it counts as a player or an observer for turn release per
Sec. 12.4 (observer = slot -1 and not the controller).

The `START_SETTINGS` JSON is the game's settings. The server itself reads only
`settings.CheatsEnabled` (a boolean) from it (Sec. 12.3).

### 11.2 Saved game start

1. The controller (which holds the save file) sends `START_SAVEGAME_SETTINGS{settings_json}`. The
   server asks the controller's session for a **SAVEGAME** file
   (`GAMESTATE_REQUEST{kind=0}`, Sec. 14).
2. The controller answers with its saved state: zlib-compressed with a 4-byte length
   header (Sec. 14.4).
3. When the transfer completes, cache the bytes as the saved state, prepare the match as
   in Sec. 11.1, and relay `START_SAVEGAME_SETTINGS{settings_json}` to `setup` sessions.
4. Each client (including the controller) then requests `SAVEGAME` from the server, which
   answers with the cached saved state. (`AUTHENTICATE_RESULT.game_stage` for such servers is 1.)
5. The cached state is cleared once no other session is still loading (Sec. 11.3).

### 11.3 LOADED_GAME from a setup session

On `LOADED_GAME{turn}` from a session whose server phase is `loading`, what the clients
receive depends on whether other sessions are still loading. Sending `PLAYERS_LOADING`
only to sessions in `setup` (this document's vocabulary) or already `in-game` is forced:
those sessions are in the client's own `loading` or `in-game` state, and `PLAYERS_LOADING`
is accepted only there, never in the client's `setup` state (Sec. 16.1).

- **Others are still loading:** the sender receives `PLAYERS_LOADING` listing the UUIDs
  of all sessions that have not yet loaded (the sender excluded), and every session
  already `in-game` receives the same message. Nothing else is sent; the sender now
  counts as loaded.
- **The sender was the last to load:** no `PLAYERS_LOADING` is sent. Every session in
  `setup` and `in-game`, the sender included, receives `LOADED_GAME{turn = 0}`, and the
  server phase becomes `in-game`.

All sessions - observers included - must load. A disconnect during `loading` also runs
the completion check, so the match can start without the departed client: when the
departure leaves every remaining session loaded, they all receive `LOADED_GAME{turn =
0}` (and no `PLAYERS_LOADING`).

(Housekeeping, not wire behaviour: the cached saved state of Sec. 11.2 can be dropped
once no session is still loading.)

Once the match is in-game, the server expects each loaded client to send
`TURN_SEALED` for turn 4 and then `STATE_HASH` for turn 1 (Sec. 12).

---

## 12. In-game: Lockstep Turns, Commands, Out-of-sync Detection

### 12.1 Timing constants

| Constant | Value |
|---|---|
| Turn length | 200 ms |
| Command delay | 4 turns (a command posted during turn *t* executes at turn *t+4*) |
| Server initial ready turn | 3 (one turn below the command delay) |
| First server-released turn | 4 (turns 1-3 run without a turn seal; turn 0 is never simulated) |
| Full-state hash turns | turn 1 and every turn divisible by 20; other turns use a "quick" hash. (Client-side; opaque to the server.) |

The **server** chooses the turn length. Clients learn the current value from each server
`TURN_SEALED.turn_duration_ms`, never from local config. A dedicated server may keep it
constant at 200 or expose an admin knob.

### 12.2 Client behaviour the server relies on

Each client keeps a current turn (starting at 0) and a ready turn (starting at 3). While
the ready turn is greater than the current turn and enough time has passed, a client
repeatedly:

1. sends `TURN_SEALED{turn = current turn + 4, turn_duration_ms = 0}` ("I have sent all
   my commands for that turn");
2. increments the current turn;
3. executes the turn with all queued commands for it, ordered by the envelope `client`
   field and then by arrival order (client-internal: the server does not need to
   reproduce this ordering itself, only relay every command in one consistent order to
   all sessions so that every client sorts the same input identically, Sec. 12.3);
4. hashes its state and sends `STATE_HASH{turn = current turn, hash}`. The hash is **MD5,
   16 raw bytes**; whether a turn uses the quick or the full hash is client-side and
   opaque to the server.

Between turns, commands the player issues are sent as
`PLAYER_COMMAND{client = client_id, slot, turn = current turn + 4, data}`. The client
does **not** queue its own commands locally; it executes them only when the server echoes
them back.

On `TURN_SEALED{turn, turn_duration_ms}` from the server, the client requires
`turn == ready turn + 1` (a gap or duplicate is fatal for the client). It then adopts the
turn and turn length for the following turns.

### 12.3 PLAYER_COMMAND processing

On a `PLAYER_COMMAND`:

- determine whether cheats are enabled from the frozen settings
  (`settings.CheatsEnabled`, default false);
- if cheats are not enabled and the sender's player slot is missing or its slot does not
  equal the message's `slot` field, drop the message silently;
- otherwise broadcast the message **unchanged** to all `in-game` sessions, **including
  the sender** (the echo is required);
- keep the message for join replay, indexed by its `turn`.

- The envelope `client` and `turn` fields are **not validated** (Sec. 22.4).


Steady state:

```
C -> S  PLAYER_COMMAND(turn=N+4, ...)        per command
C -> S  TURN_SEALED(turn=N+4, len=0)       after simulating turn N
S -> *  PLAYER_COMMAND(...)                  immediate echo/relay
S -> *  TURN_SEALED(turn=N+4, len=server)  once all required clients are ready
```

### 12.4 Server turn management

The server tracks a ready turn `R` (initially 3) and the current turn length.

**Release rule.** When a client reports `TURN_SEALED{turn}`, the server first checks
sequencing: if `turn` is not that client's previous ready turn plus one, it disconnects
the client with code 12 (out-of-sequence turn seal). This check is forced: the client
itself requires `turn == ready turn + 1` on every `TURN_SEALED` it receives and
trips an assertion otherwise (Sec. 12.2, Sec. 16.1), so a server that let a gap or
duplicate through would already have broken every client in the match. Otherwise it records the
new ready turn and re-evaluates release. The next turn is released only when every client
that blocks (see below) has a ready turn greater than `R`. On release:

- `R` becomes `R + 1`;
- the server broadcasts `TURN_SEALED{turn = R, turn_duration_ms = current turn length}`
  to all `in-game` sessions;
- the turn length used for turn `R` is recorded for join replay (Sec. 13.3).

**Who blocks.** Every non-observer client blocks, **and the controller even if it is an
observer**. An observer blocks only when an observer lag limit is configured (default:
none) and it is at least that many turns behind `R`. There is **no timeout**: a stalled
player blocks release forever, and only its disconnect unblocks the game. When a client
leaves, its records are forgotten and release is re-evaluated.

**Hash comparison.** When a client reports `STATE_HASH{turn}`, the server first checks
sequencing: if `turn` is not that client's previous simulated turn plus one, it
disconnects the client with code 13 (out-of-sequence state hash). Otherwise it
records the hash for that turn. A turn is compared once all registered clients have
reported it:

- the reference value for the turn is whichever hash the client with the smallest
  client ID reported;
- clients whose hash differs are out of sync; their display names are listed in
  `WRONG_HASH_PLAYERS{turn, reference_hash, mismatched_names}`;
- one `WRONG_HASH_PLAYERS` is broadcast, and no further comparison is done until every
  out-of-sync client has left; once they all leave, comparison resumes;
- a client that leaves before reporting a turn's hash no longer counts: the turn is
  compared once every remaining registered client has reported it, so a departure can
  complete a comparison that was waiting on that client.

The server never disconnects a client for being out of sync, and it cannot verify hashes
itself (it runs no simulation): it is a pure comparator.

### 12.5 FLARE processing

Overwrite `uuid` with the session's UUID; broadcast to `in-game` including the sender.
Coordinates are decimal strings.

---

## 13. Join and Late Observers

An `AUTHENTICATE` accepted while the server phase is not `setup` (i.e. `loading` or
`in-game`), when the client is admitted as joining (Sec. 9.2), follows this path. That
includes former players, former observers (the observer flag is restored from their old
slot) and late observers (admitted by policy, subject to the observer limit and the
41-session cap).

### 13.1 Snapshot acquisition

When a joining client is admitted, the server selects an `in-game` session to source the
snapshot from (Sec. 20.4) and asks it for a **RUNNING_GAME** file
(`GAMESTATE_REQUEST{kind=1}`). The joiner enters `syncing` immediately. When the
transfer completes, the server caches the bytes and sends the joiner
`JOIN{settings_json}` (the frozen settings, JSON-serialized).

The source client serializes its **current** simulation state as
`u32 LE uncompressedLength || zlib( u32 LE turn || simulation state )` - the turn
is **inside** the compressed payload (Sec. 14.4). The server treats it as opaque bytes.

### 13.2 Joiner side (client behaviour)

1. On `JOIN` the joiner requests `RUNNING_GAME` from the server
   (`GAMESTATE_REQUEST{kind=1}`). The server answers with the cached snapshot.
   Concurrent joiners race on this single cache slot (Sec. 22.6).
2. When the download completes, the client acts as if it received `START_SETTINGS` with those
   settings and loads the map. It stays in its client syncing state.
3. After loading, it decompresses the snapshot, reads the turn, sets its current and ready
   turns to the snapshot turn, and sends `LOADED_GAME{turn = snapshot turn}`.
4. While join-syncing, it applies incoming PLAYER_COMMANDs and TURN_SEALEDes,
   fast-forwarding the simulation after each turn seal without sending hashes.
5. On `LOADED_GAME` from the server it starts normal play and sends `JOINED`.

### 13.3 LOADED_GAME from a syncing session

On `LOADED_GAME{turn = t}` from a session in `syncing`:

- Let `R` be the current ready turn.
- Replay saved commands to the joiner, in stored order, for every turn from `t+1` up to
  `max(R+1, last stored command turn)`: the range must reach at least `R+1` so the
  joiner's own subsequent `LOADED_GAME{R}` lands on a contiguous turn, and it extends
  further whenever commands already exist for turns beyond `R+1`, so nothing already
  stored is withheld. Where a replayed turn is at or below `R`, follow each turn's
  commands with `TURN_SEALED{turn = i, turn_duration_ms = saved turn length for i}`.
  Turn seals MUST be contiguous from `t+1`, because the client requires
  `turn == ready turn + 1`.
- Commands already queued for turns later than `R` (up to `R+4`) are sent too.
- Send the joiner `LOADED_GAME{turn = R}` and mark it `in-game`.
- The joiner now counts for turn release like any other client (Sec. 12.4): the server
  expects its first `TURN_SEALED` for turn `R+4` and its first `STATE_HASH` for
  turn `R+1`, and it counts as a player or an observer for turn release per Sec. 12.4
  (observer = slot -1 and not the controller).

Saved commands and the turn length used for each turn are retained for the whole match,
because join replay needs both.

### 13.4 JOINED processing

Overwrite `uuid` with the session's UUID; broadcast `JOINED` to `in-game` including the
sender. Then send the session `PLAYER_PAUSE{uuid, paused=1}` for every currently paused
player.

### 13.5 Notes

- The live game **does not pause** during join. The joiner is not registered with the
  turn manager until sync completes, so it does not block turns while syncing.
- The state blob is produced and consumed by **clients**; the server only caches and
  re-serves it.
- If no `in-game` client exists to source the state from, join is impossible; the
  server must refuse the join gracefully with code 5, match already in progress
  (Sec. 20.4). This refusal is a deliberate deviation from the stock server, which asks
  its oldest connection regardless of phase (Sec. 22.6).

---

## 14. Game State Transfer Sub-protocol

Each endpoint allocates its own stream IDs, starting at 1. The four GAMESTATE
messages are accepted in any phase.

### 14.1 Roles and sequence

- **Requester**: allocates an ID and sends `GAMESTATE_REQUEST{kind, id}`.
- **Responder**: on receiving a request, sends `GAMESTATE_RESPONSE{id, length}` and
  then streams the data.

```
requester -> responder  GAMESTATE_REQUEST{state_type i8, stream_id u32}
responder -> requester  GAMESTATE_RESPONSE{stream_id, length u32}  (1..8 MiB)
responder -> requester  GAMESTATE_CHUNK{stream_id, data} x N        (<=1024 B chunks)
requester -> responder  GAMESTATE_CHUNK_ACK{stream_id, chunks_delivered=1}   per data packet
```

What each side serves:

| Requester -> Responder | kind | Responder sends |
|---|---|---|
| server -> client (join source) | RUNNING_GAME (1) | the client's live snapshot (compressed, serialized on demand) |
| server -> controller | SAVEGAME (0) | the controller's saved-game state (compressed) |
| client -> server | RUNNING_GAME (1) | the cached join snapshot |
| client -> server | SAVEGAME (0) | the cached saved-game state |

### 14.2 Sender

For each active transfer the sender starts by sending
`GAMESTATE_RESPONSE{id, length}`, then streams chunks of up to **1024 bytes** (keeping
packets under the 1372 MTU), allowing at most **32** unacknowledged chunks in flight.
Each acknowledged packet frees one window slot. An ACK for an unknown transfer, or one
that would make the acknowledged count exceed what is in flight, is an error and is
dropped.

### 14.3 Receiver

- On `GAMESTATE_RESPONSE`: a matching transfer must exist; the length must be greater
  than 0 and at most 8 MiB.
- On `GAMESTATE_CHUNK`: the transfer must exist; append the chunk; send
  `GAMESTATE_CHUNK_ACK{id, chunks_delivered = 1}` (one ACK per data message); if the total
  exceeds the declared length, that is an error; when the total equals the length, deliver
  the bytes and finish the transfer.

Errors are logged and the message dropped. There is no failure notification to the
requester and no timeout. An **empty** payload (length 0) is rejected by the receiver, so
never answer a request with an empty file.

### 14.4 Payload framing (clients produce and consume it; opaque to the server)

`u32 LE uncompressedLength || zlib-stream(payload)` (standard zlib `compress` format). For
RUNNING_GAME the payload is `u32 LE turn || engine-serialized simulation state`. For SAVEGAME it
is the saved-game state blob.

---

## 15. Connection Monitoring, Pause, Disconnects

### 15.1 Connection monitoring

The server emits connection warnings at most once per wall-clock second. They are
advisory: the server never disconnects anyone because of them.

- A session whose peer has been silent for more than 2000 ms is reported as
  `LAST_SEEN{uuid, ms_ago}`.
- A session that is not silent but whose mean round-trip time exceeds 400 ms is reported
  as `LAGGING_CLIENTS{[ (uuid, rtt) ]}`, one entry per message.
- A session is never reported both ways in the same check.
- Recipients: every session other than the reported one that is `in-game`, or in `setup`
  while the server is in `setup`. The reported client is never told about itself.

Real disconnection is ENet's peer timeout.

### 15.2 PLAYER_PAUSE processing

Overwrite `uuid` with the session's UUID. If `paused` is set, add the UUID to the paused
set when absent; otherwise remove it when present. Send the message to every `in-game`
session except the sender - forced, since the client accepts `PLAYER_PAUSE` only in its
`in-game` state (Sec. 16.1). Purely advisory (GUI overlay) - turns keep running.

### 15.3 Disconnect handling

On an ENet disconnect, the remaining clients observe:

- the updated `PLAYER_SLOTS` (the departed client's player slot is marked
  disconnected and omitted, Sec. 10.1), when the session was in `setup`, `syncing` or
  `in-game`;
- if the server is in `loading` and the departure leaves every remaining session loaded,
  `LOADED_GAME{turn = 0}` for all of them (Sec. 11.3); no `PLAYERS_LOADING` is sent for
  a departure;
- possibly an immediately released `TURN_SEALED` (Sec. 12.4): a departed client no
  longer blocks turn release, and hash comparison no longer waits for it (Sec. 12.4).

No `PLAYER_PAUSE{paused = 0}` is ever sent for a departed client, even if it had paused
(Sec. 22.7). A `syncing` session was never part of turn management, so its departure
changes nothing there.

- If the controller disconnects, the stored controller UUID stays set to the departed
  UUID. Nobody else can become controller, and a reconnecting controller has a new UUID
  (Sec. 20.1).
- On server shutdown: send an immediate (unreliable) ENet disconnect with code 2
  (server shutting down) to every peer.
---

## 16. What the Client Expects (Client Model)

### 16.1 Client state machine (for interoperability testing)

States: `idle`, `connecting`, `handshaking`, `authenticating`, `setup`, `loading`,
`syncing`, `in-game`.

| State | Accepts | -> |
|---|---|---|
| `idle` | (ENet connect) | `connecting` |
| `connecting` | SYN (replies SYN_ACK) | `handshaking` |
| `handshaking` | ACK (stores UUID; sends AUTHENTICATE or lobby IQ) | `authenticating` |
| `authenticating` | AUTHENTICATE (replies with credentials) | `authenticating` |
| `authenticating` | AUTHENTICATE_RESULT (stores client_id, join flag, controller flag) | `setup` |
| `setup` | CHAT, PRE_GAME_STATUS, GAME_SETTINGS, PLAYER_SLOTS, KICKED, LAST_SEEN, LAGGING_CLIENTS | `setup` |
| `setup` | START_SETTINGS / START_SAVEGAME_SETTINGS | `loading` |
| `setup` | JOIN | `syncing` |
| `syncing` | CHAT, GAME_SETTINGS, PLAYER_SLOTS, KICKED, LAST_SEEN, LAGGING_CLIENTS, PLAYER_COMMAND, TURN_SEALED | `syncing` |
| `syncing` | START_SAVEGAME_SETTINGS | `loading` |
| `syncing` | LOADED_GAME | `in-game` |
| `loading` | CHAT, GAME_SETTINGS, PLAYER_SLOTS, KICKED, LAST_SEEN, LAGGING_CLIENTS, PLAYERS_LOADING | `loading` |
| `loading` | LOADED_GAME | `in-game` |
| `in-game` | JOINED, KICKED, LAST_SEEN, LAGGING_CLIENTS, PLAYERS_LOADING, PLAYER_PAUSE, CHAT, GAME_SETTINGS, PLAYER_SLOTS, PLAYER_COMMAND, FLARE, WRONG_HASH_PLAYERS, TURN_SEALED | `in-game` |

Consequences for the server:

- **The client never validates** the server's challenge, game version, simulation version or
  mods. The server is the only one that enforces compatibility.
- **PLAYERS_LOADING is not accepted in `setup`**, and **PLAYER_PAUSE only in `in-game`**.
  The server's recipient-phase choices must respect this.
- **PRE_GAME_STATUS is accepted only in `setup`**, which is why the server relays PRE_GAME_STATUS to `setup`
  sessions only.
- Messages sent in the wrong client state are logged and ignored by the client.
- **TURN_SEALED must be strictly sequential** per client
  (`turn == ready turn + 1`). A violation trips an assertion in the client.
- A client in `loading` that receives `LOADED_GAME` starts the turn manager. It should
  only receive it after everyone has loaded.
- Game state transfer requests are served by clients in any state. A RUNNING_GAME request sent to a
  client that has no running game (e.g. still authenticating) is undefined behavior
  (probably crash on client). Only ask `in-game` clients for snapshots (Sec. 20.4).
- A client finishing its load sends `LOADED_GAME{turn}`: 0 for a fresh game, the snapshot
  turn for a join.
- When `KICKED` names the local player, the stock UI handles it; the actual disconnection
  comes from the ENet disconnect reason.

---

## 17. Lobby Integration (XMPP)

Optional. A dedicated server can operate without any of this (direct-IP model: empty
password, no registration). It is needed only to list the game in the official (or a
compatible) lobby and let players join through it. All lobby features assume the server
holds its **own XMPP account**, logged in to the lobby server (TLS required by default),
and handles the IQs below. Lobby mode implies lobby authentication is required. The stock
game refuses to register games otherwise. **No IP/port is ever published** in the game
list.

### 17.1 Namespaces and JIDs

| Extension | Element | Namespace |
|---|---|---|
| Game list | `<query>` | `jabber:iq:gamelist` |
| Board list | `<query>` | `jabber:iq:boardlist` |
| Game report | `<query>` | `jabber:iq:gamereport` |
| Profile | `<query>` | `jabber:iq:profile` |
| Lobby auth | `<auth>` | `jabber:iq:lobbyauth` |
| Connection data | `<connectiondata>` | `jabber:iq:connectiondata` |

The stock 0 A.D. lobby uses: lobby server `lobby.wildfiregames.com` (TLS), MUC room
`arena28`, game bot account `wfgbot28`, rating bot `echelon28`. Bot JIDs have the form
**`<bot>@<lobby-server>/CC`** (e.g. `wfgbot28@lobby.wildfiregames.com/CC`). Game list
responses are accepted only from the game bot's JID. These values are defaults a dedicated
server may reconfigure.

### 17.2 Lobby authentication (client -> host)

After the client receives ACK with flag 0x1, it sends to the
**host JID** (the full JID it obtained from the game list / connection data, Sec. 17.4):

```xml
<iq type='set' to='HOST_FULL_JID' id='...'>
  <auth xmlns='jabber:iq:lobbyauth'><token>CLIENT_UUID</token></auth>
</iq>
```

The host must:

1. reply with an empty `<iq type='result' to='...' id='same'/>`;
2. take `username = from.node` (the JID localpart), as vouched for by the XMPP server;
3. apply Sec. 9.1 with (`username`, `token`).

This binds the XMPP identity to the game-session UUID. The name later sent in
AUTHENTICATE must equal this username case-insensitively, after removing any `" (...)"`
suffix.


### 17.3 Game registration (host -> game bot)

Register or update:

```xml
<iq type='set' to='GAME_BOT_JID' id='...'>
  <query xmlns='jabber:iq:gamelist'>
    <command>register</command>
    <game name='...' hostUsername='...' hostJID='HOST_FULL_JID' mapName='...' niceMapName='...'
          mapSize='...' mapType='...' victoryConditions='...' nbp='...' maxnbp='...'
          players='...' mods='...' hasPassword='...'/>
  </query>
</iq>
```

**When each command is sent:**

- `register` - once a map is selected, and again on any settings or player slot change.
  Host guidance: debounce by about 500 ms and suppress when no attribute changed.
- **At game start:** `register` once more, then `changestate`:

  ```xml
  <query xmlns='jabber:iq:gamelist'><command>changestate</command><game nbp='...' players='...'/></query>
  ```

  `changestate` is sent **only** at game start, not on ordinary player-count changes.
- **On leaving setup / shutting down:**

  ```xml
  <query xmlns='jabber:iq:gamelist'><command>unregister</command><game/></query>
  ```

| Attribute | Stock value |
|---|---|
| `name` | server name chosen by host |
| `hostUsername` | host's lobby nick |
| `hostJID` | always overwritten with the host's own full JID |
| `mapName` | map id/path from game settings |
| `niceMapName` | translatable map display name |
| `mapSize` | for `mapType == "random"`: map size value; else `"Default"` |
| `mapType` | e.g. `random`, `skirmish`, `scenario` |
| `victoryConditions` | active victory condition ids joined with `,` |
| `nbp` | number of connected player slots with slot != -1 |
| `maxnbp` | configured player count |
| `players` | stringified team list of all connected player slots; observers carry team `observer` |
| `mods` | JSON text of the host's mod list shape |
| `hasPassword` | `"true"` or `""` |

All values are strings. 

Bot-side rules (e.g. whether the host must be present in the MUC room, and when the
listing expires) are **not** part of this specification.

To fill these attributes, a dedicated server must track settings, which in stock play
arrive only inside `GAME_SETTINGS` script values. That requires the Sec. 5 decoder, or an
out-of-band configuration.

### 17.4 Connection data (client -> host)

Before connecting over ENet, a joining lobby client asks the host for the game address
and proves it knows the game password:

```xml
<iq type='get' to='HOST_FULL_JID' id='...'>
  <connectiondata xmlns='jabber:iq:connectiondata'>
    <isLocalIP>0</isLocalIP>
    <password>P</password>
    <clientsalt>NAME</clientsalt>
  </connectiondata>
</iq>
```

`password` is the double-hashed game password `P` and `clientsalt` the player name,
exactly the values later sent in `AUTHENTICATE` (Sec. 18). `isLocalIP` is `"0"` on the
first request. Children are omitted when empty, so a request never carries `ip`, `port`
or `error`.

The host checks in order and answers every case with an IQ `result` to the client's full
JID with the same `id`; errors are in-band children, not IQ errors:

| Condition | Reply children |
|---|---|
| the host is not running a game server | `<error>not_server</error>` |
| the sender's XMPP username has 3 failed password checks on record (below) | `<error>banned</error>` |
| `hash(storedPassword, clientsalt) != password` | `<error>invalid_password</error>` |
| any valid request | `<ip>PUBLIC_IP</ip><port>PUBLIC_PORT</port>` |

`PUBLIC_IP`/`PUBLIC_PORT` are the dedicated server's configured, externally reachable
address and port. The server does not perform address discovery or NAT traversal. An
error reply aborts the client's join attempt: it reports the failure to its UI and does
not fall back to another address.

**Failed-password ban.** The host counts failed password checks per XMPP username. The
first three failures are answered `invalid_password`; once three have accumulated, every
later connection-data IQ from that username is answered `<error>banned</error>` (the
lobby password failure threshold of Sec. 19.1). A successful check resets
the counter. The count is in-memory only and is independent of the kick/ban lists of
Sec. 10.3: it gates only connection-data replies, not the ENet-side authentication.

---

## 18. Password Hashing

**Password hash.** `hash(input, salt)` returns `""` when `input` is empty. Otherwise:

1. Derive a 16-byte value from the `salt` bytes with keyed BLAKE2b: the key is the fixed
   16 bytes `EB 52 1D 14 87 A8 B8 61 07 F0 30 6D 08 22 9E 20`, and the output length is 16.
2. Run Argon2id v1.3 with `input` as the password, the derived 16-byte value as the salt,
   output length 32, ops limit 2, and memory limit 32768 bytes.
3. Return the output as 64 uppercase hex characters.

The 32 KiB / 2-ops parameters are deliberately weak and frozen for compatibility; do not
change them.

**Test data.** Captured packets live in `./tests/fixtures/real_messages/*`.

**Password chain:**

| Who | Value |
|---|---|
| Non-lobby host | no password set -> server password `""`; any client sending `""` passes (empty short-circuit) |
| Lobby host stores | `H = hash(rawPassword, hostFullJID + rawPassword + "0.28.0")` |
| Joining client computes | the same `H` (it knows hostJID and rawPassword), then `P = hash(H, player_nameUtf8)` |
| Client sends | `P` in connection-data `<password>` with `<clientsalt>` = player_nameUtf8 (Sec. 17.4), and `P` again in `AUTHENTICATE.password` |
| Host verifies | `hash(H, clientsalt) == password` (connection data) and `hash(H, utf8(raw AUTHENTICATE.name)) == AUTHENTICATE.password` |

- Empty raw password means `H = ""` and `P = ""`. Joining succeeds only with an empty
  password field.
- **Direct IP joins (no lobby):** the stock client never sets a password, so it always
  sends `""`. A server with a non-empty password therefore rejects every direct-join
  client with code 14 (refused). Password-protected games are effectively
  lobby-only.
- The dedicated server's host JID must be the exact full JID (including resource) that
  appears as `hostJID` in the game list, because clients salt with it.

---

## 19. Constants and Configuration

### 19.1 Fixed constants

| Constant | Value |
|---|---|
| Max ENet peers / sessions | 41 |
| Channel count | 1 |
| Host MTU | 1372 |
| Connection-warning cadence | once per second |
| Client warning timeout | 2000 ms |
| Bad round-trip time | 400 ms |
| Turn length | 200 ms |
| Command delay | 4 turns |
| Server initial ready turn | 3 |
| First released turn | 4 |
| Full state-hash cadence | turn 1 and every turn divisible by 20 |
| Name max length | 32 code units |
| Lobby password failures allowed before ban (Sec. 17.4) | 3 |
| Game state transfer chunk size / window / max size | 1024 bytes / 32 packets / 8 MiB |
| Default port | 20595 |

### 19.2 Configuration policies

The following are policies a dedicated server should expose. The stock 0 A.D. value is
given in parentheses; a conforming server may choose different values as long as the wire
behaviour above is preserved.

| Policy | Stock default | Effect |
|---|---|---|
| Duplicate names allowed | false | deduplicate a new name instead of rejecting it (non-lobby only; Sec. 9.2) |
| Late-observer policy | `everyone` | `everyone`, `buddies`, or deny (Sec. 9.2) |
| Observer limit | 8 | cap on late observers (Sec. 9.2) |
| Observer lag limit | none | turns an observer may lag before blocking release; none = never block (Sec. 12.4) |
| ENet MTU | 1372 | ENet MTU |
| Enabled mods | `("0ad","0.28.0")` | mod list advertised in the handshake (Sec. 8.1) |
| Lobby buddies | none | names admitted by the `buddies` late-observer policy |
| Turn length | 200 ms | server-chosen turn length (Sec. 12.1) |
| Lobby server / room / game bot / rating bot | `lobby.wildfiregames.com` / `arena28` / `wfgbot28` / `echelon28` | XMPP lobby integration (Sec. 17) |

### 19.3 Disconnect reasons (ENet disconnect `data`)

The ENet disconnect `data` word carries a numeric reason code. The labels below are this
document's vocabulary for referring to a code in prose; only the numeric value is
wire-significant. The codes are grouped by the point in the connection lifecycle at which
the server emits them, following the protocol's own shape: connection and handshake,
authentication and admission, moderation, turn synchronisation, shutdown.

**Connection and handshake**

| Code | Label used here | Emission condition |
|---:|---|---|
| 3 | game version mismatch | game version mismatch |
| 17 | simulation or mod mismatch | simulation/mod mismatch |
| 11 | no UUID could be issued | no unique UUID could be issued |
| 7 | banned | banned IP on connect |

**Authentication and admission**

| Code | Label used here | Emission condition |
|---:|---|---|
| 4 | server is loading | auth during loading; unauthenticated session at game start |
| 10 | lobby authentication failed | lobby name mismatch |
| 14 | refused | wrong password |
| 8 | player name in use | duplicate name |
| 7 | banned | banned name at auth |
| 5 | match already in progress | unknown user after start and late join not allowed |
| 9 | server full | capacity / observer limit |

**Moderation (kick / ban)**

| Code | Label used here | Emission condition |
|---:|---|---|
| 6 | kicked | kicked |
| 7 | banned | ban |

**Turn synchronisation (in-game)**

| Code | Label used here | Emission condition |
|---:|---|---|
| 12 | out-of-sequence turn seal | out-of-sequence TURN_SEALED |
| 13 | out-of-sequence state hash | out-of-sequence STATE_HASH |

**Shutdown**

| Code | Label used here | Emission condition |
|---:|---|---|
| 2 | server shutting down | server shutting down |

**Not emitted by the server.** Code 0 (unknown — a server must avoid it) and code 1
(connection request timed out — produced on the client side).

---

## 20. Dedicated-Server Design Considerations

### 20.1 Controller role without a local host

- Stock clients send an empty controller secret (only the hosting process knows the real
  one).
- A dedicated server that does nothing else therefore gets
  "first joiner controls" by default.
- The controller flag reaches a client **only** in its AUTHENTICATE_RESULT. There is no
  message that promotes an already-connected client. If the controller leaves, no one can
  control setup until the server resets the stored controller UUID *and* a client
  reconnects. Without a controller, nobody can change settings or start the match.
- Recommended policy options:
  1. first-joiner;
  2. a configured, non-empty secret given to the admin player (requires a client that can
     present it), or a configured admin name: grant the controller flag when an
     authenticating client matches, and clear the stored controller UUID when the
     controller disconnects so the next matching join takes over;
  3. server-driven setup: nobody is controller, and the server itself emits GAME_SETTINGS,
     player slots and START_SETTINGS. Requires a Sec. 5 encoder and knowledge of the
     game-settings schema.
- **The host is just another client** in the stock game (it self-connects via `127.0.0.1`
  and occupies one of the 41 slots). A dedicated server frees that slot and simply has
  zero local clients; nothing in the protocol requires a session to exist before others
  join.
- Parts of the stock setup UI assume the controller also hosts, e.g. lobby
  registration requires a local server. Test controller-without-server flows with a real
  client.

### 20.2 What the server must decode

| Data | Needed? |
|---|---|
| fixed-width fields of all messages except 9 and 29 | yes |
| PLAYER_COMMAND envelope (`slot`, `turn`) | yes (validation, storage) |
| PLAYER_COMMAND / GAME_SETTINGS script values | no (relay bytes); only for lobby metadata or admin features |
| START_SETTINGS / START_SAVEGAME_SETTINGS / JOIN JSON | parse `settings.CheatsEnabled`; keep the text for JOIN (it has the same value in the same game) |
| Game state transfer payloads, hashes | no (opaque) |

### 20.3 What you can skip

- Simulation, renderer, map loading, game-content parsing.
- Script-value decoding (relay raw bytes; Sec. 5).
- Lobby and UPnP - both optional (direct-IP hosting needs neither).

### 20.4 Join source selection

A robust server should choose an **`in-game`** session to source the snapshot: prefer the
lowest RTT, and preferably a non-out-of-sync one. If none exists, reject the join with
code 5, match already in progress (no client can supply state). Restricting the choice
to `in-game` sessions, and refusing the join when none exists, is a deliberate deviation
from the stock server, which asks its oldest connection - possibly unauthenticated,
loading, or the joiner itself (Sec. 22.6). Optionally cache the
latest snapshot proactively to survive all-but-one clients dropping. Key the snapshot
buffer per request to avoid the concurrent-join race.

### 20.5 Memory and lifecycle

- Saved commands and the per-turn turn lengths grow for the whole match; the 8 MiB
  game state transfer cap does not apply there. Budget accordingly; dropping them breaks join.
  Consider bounding once no join is possible.
- Delete completed game-state sends.
- Clear the cached join snapshot after join completes if no other join is pending.

### 20.6 Running without the lobby / minimal viable profile

Clients join with `ip:port` directly. Lobby authentication is off, `flags` is 0, and the
password must be empty (Sec. 18). XMPP is unused.

**Minimal viable profile:** direct-IP, no password, no lobby, no saved games, join
disabled: reject post-start joins with code 5, match already in progress (late-join
policy = deny) and skip game state transfer entirely. Everything in Sec. 7-12 and Sec. 15 is
still required for a playable match.

### 20.7 Suggested hardening (deliberate deviations)

The stock server has **no rate limits** anywhere (chat, flares, commands, auth attempts on
the game socket) and a plaintext transport. A public dedicated server should add
per-session rate caps and auth-attempt throttling, restrict game state transfer requests
(Sec. 22.9), and document the deviations, since clients have no backpressure signalling.

---

## 21. Security Considerations

- Transport is unencrypted and unauthenticated beyond the password hash.
- The hashed password on the wire is vulnerable to offline brute force (Argon2id at
  32 KiB / 2 ops with a username salt, frozen for compatibility).
- The controller secret travels in plaintext (in AUTHENTICATE); anyone who sniffs it
  controls the lobby.
- Lobby mode binds game identity to XMPP identity via the UUID token; the token itself is
  bearer-equivalent until consumed.
- State-hash comparison is trust-based: the server compares hashes but cannot detect two colluding
  clients or a forged "expected" hash from the lowest-host-ID client.
- PLAYER_COMMAND `client`/`turn` fields are unvalidated (Sec. 22.4).

---

## 22. Known Quirks of the Stock Server

Decide per item whether to **preserve** it (for strict parity) or **fix** it. None of the
suggested fixes changes the wire format.

1. **Start while not ready:** if a slotted player is not ready, the stock server sends
   START_SETTINGS but never enters `loading`. Clients then load and send LOADED_GAME, which
   the server cannot accept. *Fix:* reject the start and send nothing (made normative in
   Sec. 11.1). The stock UI normally prevents this case.
2. **Observer lag:** observers block early in the game when an observer lag limit is set:
   until the ready turn reaches the limit, an observer is treated as lagging and blocks
   turn release.
3. **Expected hash:** for each turn, the reference value is the hash reported by the
   client with the lowest client ID.
4. **`client` and `turn` in PLAYER_COMMAND are not validated.** A client can schedule
   commands into already-executed turns, which puts other clients out of sync, or spoof another
   client ID, which only affects ordering.
5. **Controller loss is permanent** (Sec. 20.1).
6. **Join source:** the stock server asks its oldest connection for the snapshot, which
   might be unauthenticated, loading, or the joiner itself (Sec. 20.4). Concurrent
   joiners share a single snapshot slot and may receive each other's (still valid)
   snapshot. *Fix:* source only from `in-game` sessions and refuse the join when none
   exists (made normative in Sec. 13.5 and Sec. 20.4).
7. **Unpause on leave:** a pausing player who disconnects is removed from the paused set
   without a `PLAYER_PAUSE{paused=0}` broadcast. Whether clients clear the pause on
   player slot change is not known; it is worth sending a server-authoritative pause
   message.
8. **16-bit size header** silently breaks messages over 65 535 bytes (Sec. 3.1).
9. **GAMESTATE_REQUEST at any time:** any client can request a cached snapshot or
   saved state at any time, including before authentication. Consider restricting this to
   sessions that were sent JOIN or START_SAVEGAME_SETTINGS.
10. **Empty file response:** answering a request while the cached snapshot or saved state
    is empty produces length 0, which the client rejects, leaving it stuck.
11. **2-byte fields:** `AUTHENTICATE_RESULT.client_id` and `TURN_SEALED.turn_duration_ms`
    are truncated to 16 bits on the wire; client IDs above 65 535 wrap in the client's view.
12. **Lobby-auth token accepted in any session phase** (Sec. 9.1).
13. **Password check uses the raw (unsanitized) name** as salt, while every other check
    uses the sanitized name.
14. **Simulation/mod mismatch code:** send **17**, not 16. On code 16 the client's pretty
    mismatch message never fires; on 17 the client's display layer attaches the
    mismatch details (see also Sec. 8.2).
15. **Empty password == no password:** `hash("", salt) == ""`, so open servers must
    accept an empty password from any client.

---

## 23. Conformance Checklist

A rewrite is wire-compatible when all of these hold against stock clients:

1. ENet: single channel, reliable, MTU 1372, port configurable (default 20595), <=41
   peers, disconnect `data` carries the disconnect code.
2. Framing: `[type u8][size u16 BE incl. header][fields]`, one message per packet,
   `size` exact or the client drops the packet. LE envelope for message 29.
3. Handshake: send challenge `0x5073013F` + game version `0x01010019` + simulation version `"0.28.0"` + your
   mod list (vanilla: `[("0ad","0.28.0")]`); enforce version (code 3) and positional
   simulation/mod equality (code 17); issue a unique 16-hex UUID; flags bit0 iff lobby mode.
4. Auth: all Sec. 9.2 branches in order with the exact disconnect codes; Argon2id
   bit-compatibility (verify against the captured packets, Sec. 18) salted with the raw name;
   controller-secret handling (never accidentally empty).
5. Setup: verbatim `GAME_SETTINGS` relay; player slot broadcast rules (not on PRE_GAME_STATUS);
   ready semantics 0/1/2; `PLAYER_SLOTS` omits disconnected entries.
6. Start/loading: readiness gate; freeze settings; `START_SETTINGS` broadcast; wait for
   **all** sessions' `LOADED_GAME`; `PLAYERS_LOADING` updates; then `LOADED_GAME{0}`
   to everyone.
7. Turns: ready turn starts 3; first release is turn 4; strict one-step sequencing of
   ready turns and simulated turns (codes 12/13); no release timeout; observer-skip rule
   incl. the observer lag limit; controller never observer; echo commands to their
   sender; cheats gate from frozen settings; record commands and turn lengths per turn.
8. State hash: store per-turn hashes; compare when complete; expected = lowest client ID;
   broadcast `WRONG_HASH_PLAYERS` once; latch until out-of-sync clients leave.
9. Join: code 2; session -> `syncing` at auth; snapshot pull from an `in-game` client;
   `JOIN`; serve cached state via game state transfer (1024 B chunks, window 32,
   per-chunk ACK=1, 1 byte..8 MiB); replay from `t+1`; after `LOADED_GAME{R}` expect
   `TURN_SEALED` for turn R+4 and `STATE_HASH` for turn R+1; `JOINED` broadcast;
   pause-set replay.
10. Housekeeping: chat UUID overwrite + targeted delivery with emptied receiver list;
    pause dedup/relay to others; 1 Hz timeout (2000 ms) / RTT (400 ms) warnings to
    *other* sessions; kick/ban semantics incl. controller protection.
11. Optional: Sec. 17 stanzas and connection data for lobby interoperability.

---

## 24. Sequence Diagrams

### 24.1 Direct join into setup (no lobby)

```
Client                                   Server
  |--- ENet connect ---------------------->|  session(await-handshake); banned IP? -> code 7
  |<-- SYN -------------------|
  |--- SYN_ACK ------------------>|  checks version/mods, issues UUID
  |<-- ACK(flags=0) -|  -> await-auth
  |--- AUTHENTICATE(name, "", "") -------->|  checks (Sec. 9.2)
  |<-- AUTHENTICATE_RESULT(OK, client_id, c) -|
  |               (others) <-- PLAYER_SLOTS (add broadcast)
  |<-- PLAYER_SLOTS ------------------|  -> setup
  |   ... controller UI sends GAME_SETTINGS -> server relays to setup sessions ...
```

### 24.2 Lobby join

```
Client                    XMPP server                Host (dedicated server)
  |-- iq get connectiondata(P, name, 0) ------------------->| check ban/password
  |<------------------------ iq result (publicIP, port) ----|
  |== ENet connect =========================================>|
  |<= SYN / SYN_ACK =>                  |
  |<= ACK(flags=1, UUID) ===============|  -> await-lobby-auth
  |-- iq set lobbyauth(token=UUID) ------------------------->| iq result; session lobby name = from.node
  |<= AUTHENTICATE (empty) ===================================|
  |== AUTHENTICATE(name, P, "") =============================>| lobby-name check, password check...
  |<= AUTHENTICATE_RESULT, PLAYER_SLOTS =================|
```

(Connection data is specified in Sec. 17.4.)

### 24.3 Game start, loading, first turns (P1 = controller, P2, O = observer)

```
P1 -> S : START_SETTINGS(json)                          [all slotted players ready]
S      : phase=loading; P1 and P2 are players, O is an observer,
         server now expects TURN_SEALED(4) then STATE_HASH(1) from each
S -> * : PLAYER_SLOTS (connected only), START_SETTINGS(json)    [to setup sessions]
P2 -> S : LOADED_GAME(0)   S -> P2: PLAYERS_LOADING[P1,O]; S -> in-game sessions: same
O  -> S : LOADED_GAME(0)   S -> O : PLAYERS_LOADING[P1];   S -> P2: same
P1 -> S : LOADED_GAME(0)   all loaded -> S -> all: LOADED_GAME(0); phase=in-game
each client (turns run every 200 ms):
    -> TURN_SEALED(4) ; run turn 1 ; -> STATE_HASH(1,h)
    -> TURN_SEALED(5) ; run turn 2 ; -> STATE_HASH(2,h)
    -> TURN_SEALED(6) ; run turn 3 ; -> STATE_HASH(3,h)
    (blocks: needs ready turn >= 4)
S : when P1 and P2 have ready turn >= 4 (O skipped): ready turn = 4 -> all: TURN_SEALED(4,200)
clients: run turn 4, send TURN_SEALED(7), STATE_HASH(4) ...
a command issued at client turn t: -> PLAYER_COMMAND(turn=t+4) -> S echoes to all in-game -> stored
S : after every client reports STATE_HASH(n): compare hashes; mismatch -> WRONG_HASH_PLAYERS to in-game
```

### 24.4 Join (P2 dropped at turn ~500, returns)

```
P2' -> S : connect/handshake/AUTHENTICATE(name "P2")
S       : disconnected player slot named P2 found -> joining; recover the slot by name
S -> P2' : AUTHENTICATE_RESULT(game_stage 2 = joining), PLAYER_SLOTS ; P2' session -> syncing
S -> P1  : GAMESTATE_REQUEST(RUNNING_GAME, id)
P1 -> S  : GAMESTATE_RESPONSE(id, len), DATA... (S acks each)
S -> P2' : JOIN(json)
P2' -> S : GAMESTATE_REQUEST(RUNNING_GAME, id')
S -> P2' : RESPONSE(id', len), DATA... (P2' acks each)
P2'     : loads map, decompresses, deserializes snapshot @turn 503
P2' -> S : LOADED_GAME(503)
S -> P2' : for i=504..max(R+1, last stored command turn): commands stored for turn i,
          then TURN_SEALED(i, len_i) when i <= R      (replay, Sec. 13.3)
S       : records P2' at ready turn R as a player
S -> P2' : LOADED_GAME(R)                          ; P2' -> in-game
P2' -> S : JOINED   S -> in-game: JOINED(uuid P2') ; S -> P2': PLAYER_PAUSE for each pauser
```

---

## 25. Open Questions / To Verify by Capture

1. Whether the joiner receives PLAYER_SLOTS once or twice on join. This depends on
   whether the session phase changes before or after the handler runs. Clients handle
   duplicates idempotently.
2. Latin-1 vs UTF-16 choice for strings emitted by real clients (decoders accept both
   anyway).
3. The exact `players` and `mods` attribute formats in lobby registration, and bot-side
   requirements (MUC presence, listing lifetime, any extra attributes the bot validates).
4. Stock setup UI behaviour when the controller is not the hosting process (Sec. 20.1).
5. Client behaviour when a pausing player disconnects without an unpause broadcast.
6. The ENet minor version bundled with the stock client, and whether its defaults differ.
