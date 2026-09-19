#!/usr/bin/env python3
# Extracts hex dumps from either of two log sources into plain-hex fixture
# files under tests/fixtures/real_messages, one per distinct (message type,
# body length) shape -- see the dedup comment in main() for why length
# instead of exact bytes:
#   - "raw packet received dump=" from our relay (RUST_LOG with
#     server::relay::net_server=trace) -- client-to-relay messages.
#   - "raw packet sent dump=" from an instrumented vanilla engine build (see
#     the LogRawPacketDump hook added to NetHost.cpp in the 0ad28vanilla
#     worktree) -- covers message types the relay only ever sends and so can
#     never observe on its own inbound trace (Syn,
#     PlayerSlots, WrongHashPlayers, etc).
# Re-run against a fresh log to add fixtures for message types
# tests/real_messages.rs still has no coverage for.

import hashlib
import re
import sys
from pathlib import Path

ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
DUMP_LINE_RE = re.compile(r"^[0-9a-f]{8}  .*\|")
HEX_BYTE_RE = re.compile(r"[0-9a-f]{2}")
DUMP_MARKERS = ("raw packet received dump=", "raw packet sent dump=")

# Mirrors WireMessage::id() in src/relay/messages/mod.rs.
MESSAGE_NAMES = {
    1: "Syn",
    2: "SynAck",
    3: "Ack",
    4: "Authenticate",
    5: "AuthenticateResult",
    6: "Chat",
    7: "PreGameStatus",
    8: "ResetPregameStatus",
    9: "GameSettings",
    10: "MapPlayerIdToSlot",
    11: "PlayerSlots",
    12: "GamestateRequest",
    13: "GamestateResponse",
    14: "GamestateChunk",
    15: "GamestateChunkAck",
    16: "Join",
    17: "Joined",
    18: "Kicked",
    19: "LastSeen",
    20: "LaggingClients",
    21: "PlayersLoading",
    22: "PlayerPause",
    23: "LoadedGame",
    24: "StartSettings",
    25: "StartSavegameSettings",
    26: "TurnSealed",
    27: "StateHash",
    28: "WrongHashPlayers",
    29: "PlayerCommand",
    30: "Flare",
}


def parse_dump_line(line):
    if not DUMP_LINE_RE.match(line):
        return None
    # Layout is "{offset:08x}  {hex bytes, space padded}  |{ascii}|" -- strip
    # the offset first so its own hex-looking digits are not read as data.
    prefix = line.split("|", 1)[0][8:]
    return bytes(int(b, 16) for b in HEX_BYTE_RE.findall(prefix))


def extract_messages(log_path):
    messages = []
    with open(log_path, "r", errors="replace") as handle:
        lines = [ANSI_RE.sub("", line).rstrip("\n") for line in handle]

    i = 0
    n = len(lines)
    while i < n:
        line = lines[i]
        idx = -1
        marker_len = 0
        for marker in DUMP_MARKERS:
            idx = line.find(marker)
            if idx != -1:
                marker_len = len(marker)
                break
        if idx == -1:
            i += 1
            continue

        first_dump_part = line[idx + marker_len:]
        chunks = [first_dump_part]
        i += 1
        while i < n and DUMP_LINE_RE.match(lines[i]):
            chunks.append(lines[i])
            i += 1

        data = bytearray()
        for chunk in chunks:
            parsed = parse_dump_line(chunk)
            if parsed is None:
                break
            data.extend(parsed)
        if data:
            messages.append(bytes(data))

    return messages


def load_existing_fixtures(out_dir):
    """Seed dedup state from fixtures already in out_dir, so re-running the
    script against a new log (e.g. once for the relay's inbound trace, once
    for an instrumented vanilla engine's outbound trace) adds new shapes
    without duplicating or renumbering what's already there."""
    seen_shapes = set()
    per_type_count = {}
    for path in out_dir.glob("*.hex"):
        parts = path.stem.split("_")
        if len(parts) != 3:
            continue
        type_name, index_str, _digest = parts
        try:
            index = int(index_str)
        except ValueError:
            continue
        per_type_count[type_name] = max(per_type_count.get(type_name, 0), index)

        try:
            data = bytes.fromhex(path.read_text().strip())
        except ValueError:
            continue
        if data:
            seen_shapes.add((data[0], len(data)))

    return seen_shapes, per_type_count


def main():
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <log_path> <out_dir>", file=sys.stderr)
        sys.exit(1)

    log_path = Path(sys.argv[1])
    out_dir = Path(sys.argv[2])
    out_dir.mkdir(parents=True, exist_ok=True)

    messages = extract_messages(log_path)

    # High-volume types (TurnSealed, StateHash, PlayerCommand, ...) embed a
    # turn counter or hash, so every instance is byte-distinct even though
    # the wire shape repeats. Dedup on (type, body length) instead of exact
    # bytes so the corpus keeps one example per distinct shape, not one per
    # counter tick. Seeded from out_dir so re-running against another log
    # auto-dedupes against fixtures already saved there.
    seen_shapes, per_type_count = load_existing_fixtures(out_dir)
    already_present = len(seen_shapes)
    saved = 0
    for data in messages:
        type_id = data[0] if data else 255
        shape_key = (type_id, len(data))
        if shape_key in seen_shapes:
            continue
        seen_shapes.add(shape_key)

        digest = hashlib.sha256(data).hexdigest()
        type_name = MESSAGE_NAMES.get(type_id, f"unknown_{type_id}")
        per_type_count[type_name] = per_type_count.get(type_name, 0) + 1
        index = per_type_count[type_name]

        # Plain continuous lowercase hex, no separators: readable as text and
        # decodes directly with the `hex` crate already in Cargo.toml, e.g.
        # hex::decode(fs::read_to_string(path)?.trim()).
        out_path = out_dir / f"{type_name}_{index:03d}_{digest[:8]}.hex"
        out_path.write_text(data.hex() + "\n")
        saved += 1

    print(f"total raw packets found: {len(messages)}")
    print(f"fixtures already in {out_dir}: {already_present}")
    print(f"new unique messages saved: {saved}")
    print("by type (including pre-existing):")
    for name, count in sorted(per_type_count.items()):
        print(f"  {name}: {count}")


if __name__ == "__main__":
    main()
