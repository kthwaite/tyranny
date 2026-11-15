#!/usr/bin/env python3
"""Benchmark actual daemon search using an isolated synthetic 50,000-track library.

Example: scripts/search_gate.py --output target/search-before.json
Repeat with the rebuilt --server and --require-typos for the after measurement.
No music files, live app state, playback, or native UI are exercised.
"""
import argparse
import hashlib
import json
import math
import platform
from pathlib import Path
import selectors
import sqlite3
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from server_gate import UnixSocket

TRACKS = 50_000
PAGE = 100
WORDS = "amber autumn black blue broken city crystal dancing distant dream electric falling fire forest golden green harbour heart hidden highway hollow island light midnight moon morning mountain ocean paper quiet rain river rolling shadow silver sky slow snow spring stone summer sun sweet thunder velvet wandering water wild wind winter wooden".split()
QUERIES = [
    ("exact_phrase", '"Paranoid Android"', (1, 1, 1)),
    ("exact_artist", 'artist:"Radiohead"', (100, 10, 1)),
    ("unquoted", "Radiohead", (100, 10, 1)),
    ("literal_needle", '"velvetneedle"', (1, 1, 1)),
    ("miss", "zqxwnevermatched", (0, 0, 0)),
    ("typo", "radiohaed", None),
    ("boolean", 'artist:"Radiohead" AND NOT title:"Paranoid Android"', (99, 10, 1)),
    ("short", "OK", (None, None, None)),
    ("unicode", 'artist:"Björk"', (100, 10, 1)),
    ("broad", '"a"', (None, None, None)),
    ("no_filter", "", (TRACKS, 5000, 500)),
]


def description(query=""):
    return dict(source=dict(kind="library"), browser=dict(genre_ids=[], artist_ids=[],
                compilations=False, album_ids=[]), query=query, sort=[], group="none")


def seed(path, root):
    """Seed only a stopped daemon's own migrated schema; paths intentionally absent."""
    started = time.perf_counter()
    def artist(n):
        return {1: "Radiohead", 2: "Björk"}.get(n, f"{WORDS[n % len(WORDS)].title()} {WORDS[(n * 7 + 3) % len(WORDS)].title()} Ensemble {n:03}")

    def album(n):
        return "OK Computer" if n == 1 else f"{WORDS[n % len(WORDS)].title()} {WORDS[(n * 11 + 5) % len(WORDS)].title()} Sessions {n:04}"

    def tracks():
        for n in range(1, TRACKS + 1):
            a, b = (n - 1) // 100 + 1, (n - 1) // 10 + 1
            title = {1: "Paranoid Android", 3: "Velvetneedle", 101: "Jóga"}.get(n,
                f"{WORDS[n % len(WORDS)].title()} {WORDS[(n * 13 + 9) % len(WORDS)].title()}") + f" {n:05}"
            metadata = dict(title=title, artist=artist(a), album_artist=artist(a), album=album(b),
                track_number=(n - 1) % 10 + 1, track_total=10, disc_number=1, disc_total=1,
                compilation=False, duration_ms=180000, sample_rate=44100, channels=2)
            yield (n, n, a, b, json.dumps(metadata, ensure_ascii=False),
                   f"{title} {artist(a)} {album(b)}".lower(), title.lower(), metadata["track_number"])

    with sqlite3.connect(path) as db:
        db.execute("PRAGMA foreign_keys=ON")
        assert db.execute("SELECT count(*) FROM library_tracks").fetchone()[0] == 0
        db.execute("INSERT INTO library_roots(id,path,search) VALUES(1,?,?)", (str(root), str(root).lower()))
        db.executemany("INSERT INTO library_artists(id,identity,name,search) VALUES(?,?,?,?)",
            ((n, f"search-artist-{n}", artist(n), artist(n).lower()) for n in range(1, 501)))
        db.executemany("INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation) VALUES(?,?,?,?,?,?,?,0)",
            ((n, f"search-album-{n}", (n - 1) // 10 + 1, album(n), artist((n - 1) // 10 + 1),
              f"{album(n)} {artist((n - 1) // 10 + 1)}".lower(), album(n).lower()) for n in range(1, 5001)))
        db.executemany("INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan,metadata_version) VALUES(?,1,?,1,0,2)",
            ((n, str(root / f"Synthetic {n:05}.flac")) for n in range(1, TRACKS + 1)))
        db.executemany("INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number) VALUES(?,?,?,?,?,?,?,1,1,?)", tracks())
        db.execute("UPDATE library_state SET revision=revision+1,root_count=1,track_count=?,pending_upgrade=0 WHERE singleton=1", (TRACKS,))
        assert not db.execute("PRAGMA foreign_key_check").fetchall()
        result = dict(user_version=db.execute("PRAGMA user_version").fetchone()[0],
                      schema_version=db.execute("PRAGMA schema_version").fetchone()[0])
    result.update(seed_ms=(time.perf_counter() - started) * 1000,
                  database_bytes=path.stat().st_size)
    return result


def distribution(samples):
    ordered = sorted(samples)
    return dict(count=len(samples), p50_ms=ordered[math.ceil(len(samples) * .5) - 1],
                p95_ms=ordered[math.ceil(len(samples) * .95) - 1], minimum_ms=ordered[0], maximum_ms=ordered[-1])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", default="target/portable-rust/release/tyranny-likes-smoke-server")
    parser.add_argument("--output", required=True, help="JSON report, including raw samples")
    parser.add_argument("--samples", type=int, default=30, help="Warm round-robin samples per case (minimum 30)")
    parser.add_argument("--protocol-version", type=int, default=29)
    parser.add_argument("--require-typos", action="store_true")
    args = parser.parse_args()
    if args.samples < 30:
        parser.error("--samples must be at least 30")
    server = Path(args.server).resolve()
    output = Path(args.output).resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    report = dict(passed=False, server=str(server), binary_sha256=hashlib.sha256(server.read_bytes()).hexdigest(),
        environment=dict(platform=platform.platform(), python=sys.version),
        fixture=dict(kind="Synthetic offline metadata; no media files or links", tracks=TRACKS, artists=500,
                     albums=5000, generator_version=1, vocabulary=WORDS),
        samples=args.samples, require_typos=args.require_typos, cases=[], launches=[],
        limitations=["Round trip includes framed Unix socket transport and JSON decoding; not SQLite-only time",
                     "Each cold sample uses a fresh daemon, but OS/filesystem caches are uncontrolled",
                     "Warm samples run round-robin on one daemon; synthetic metadata is not a real music collection",
                     "Empty-term find_in_list is omitted because the protocol rejects it",
                     "find_in_list returns at most one match; its result count is presence, not total matches"])
    process = connection = log = None
    with tempfile.TemporaryDirectory(prefix="tyr-search-", dir="/tmp") as temporary:
        run = Path(temporary).resolve()
        state = run / "state"
        state.mkdir(mode=0o700)

        def stop():
            nonlocal process, connection, log
            if connection is not None:
                connection.close()
                connection = None
            if process is not None:
                process.terminate()
                try:
                    code = process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
                    raise
                finally:
                    process.stdout.close()
                    process = None
                assert code == 0, f"daemon shutdown: {code}"
            if log is not None:
                log.close()
                log = None

        def start():
            nonlocal process, connection, log
            log = tempfile.TemporaryFile(mode="w+")
            argv = [str(server), "--state-dir", str(state), "--capture", str(run / "capture.f32")]
            process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=log, text=True)
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                assert selector.select(30), "daemon readiness timeout"
            line = process.stdout.readline()
            if not line:
                log.seek(0)
                raise RuntimeError(f"daemon startup failed: {log.read()}")
            ready = json.loads(line)
            report["launches"].append(ready)
            connection = UnixSocket(ready["socket_path"], expected_version=args.protocol_version)

        def request(case, offset=0):
            query, command = case["query"], case["command"]
            fields = dict(revision=None, offset=offset, limit=PAGE)
            if command == "list_tracks":
                fields.update(list=description(query))
            elif command == "list_albums":
                fields.update(list=description(query), sort="album_artist", descending=False)
            elif command == "list_artists":
                fields.update(query="", list=description(query))
            else:
                fields = dict(list=description(), revision=None, term=query, **{"from": None}, backward=False)
            begin = time.perf_counter_ns()
            result = connection.request(command, **fields)
            elapsed = (time.perf_counter_ns() - begin) / 1e6
            if command == "find_in_list":
                count = int(result["match"] is not None)
                if count:
                    assert 0 <= result["match"]["position"] < TRACKS
                returned = count
            else:
                count, returned = result["total"], len(result["entries"])
                assert result["offset"] == offset
                assert returned == min(PAGE, max(0, count - offset)), result
            expected = case["expected_count"]
            if expected is not None:
                assert count == expected, (case["name"], command, count, expected)
            if "result_count" in case:
                assert count == case["result_count"], (case["name"], "unstable count")
            case.update(result_count=count, returned_count=returned)
            return elapsed, result

        try:
            start()
            stop()
            report["schema"] = seed(state / "state.sqlite3", run / "SYNTHETIC-NOT-IMPORTED")
            for name, query, expected in QUERIES:
                if name == "typo" and args.require_typos:
                    expected = (100, 10, 1)
                for index, command in enumerate(("list_tracks", "list_albums", "list_artists", "find_in_list")):
                    if command == "find_in_list" and not query:
                        continue
                    count = expected[index] if expected and index < 3 else None
                    if index == 3 and expected and expected[0] is not None:
                        count = int(expected[0] > 0)
                    case = dict(name=name, command=command, query=query, expected_count=count, warm_samples_ms=[])
                    report["cases"].append(case)
                    start()
                    case["cold_ms"], _ = request(case)
                    stop()
            start()
            for _ in range(args.samples):
                for case in report["cases"]:
                    elapsed, _ = request(case)
                    case["warm_samples_ms"].append(elapsed)
            # Verify multi-page boundaries and stable, non-overlapping row identities.
            for case in report["cases"]:
                case["warm"] = distribution(case["warm_samples_ms"])
                if case["command"] != "find_in_list":
                    _, first = request(case)
                    _, second = request(case, PAGE)
                    key = "row_id" if case["command"] == "list_tracks" else "id"
                    first_ids = {entry[key] for entry in first["entries"]}
                    second_ids = {entry[key] for entry in second["entries"]}
                    assert len(first_ids) == len(first["entries"])
                    assert len(second_ids) == len(second["entries"])
                    assert first_ids.isdisjoint(second_ids)
                    case["returned_count"] = len(first["entries"])
            report["passed"] = True
        except BaseException as error:
            report["error"] = f"{type(error).__name__}: {error}"
            raise
        finally:
            try:
                stop()
            finally:
                output.write_text(json.dumps(report, indent=2) + "\n")
    print(json.dumps(dict(output=str(output), passed=report["passed"], cases=len(report["cases"]),
                          binary_sha256=report["binary_sha256"], schema=report["schema"])))


if __name__ == "__main__":
    main()
