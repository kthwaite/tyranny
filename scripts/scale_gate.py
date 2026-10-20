#!/usr/bin/env python3
"""Measure the real daemon/native client with a declared offline 50k/10k fixture.

Library rows are seeded metadata, NOT scanned media; their asset paths are hard
links to one short generated WAV, so run enqueue can canonicalize every source.
Queue occurrences point at a real generated WAV; a separate root scans real WAV
hard links during native use.
No builds, cache-purge claims, default user state, or fabricated timing results.
"""
import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import selectors
import sqlite3
import subprocess
import sys
import tempfile
import time
import wave

sys.dont_write_bytecode = True
from server_gate import WebSocket, fixture

TRACKS = 50_000
QUEUE = 10_000
PAGE = 100  # the legacy library_* page cases below
WINDOW = 200  # the client's list and Up Next window size


class MeasuredWebSocket(WebSocket):
    frame_bytes = 0

    def take(self, count):
        result = super().take(count)
        self.frame_bytes += len(result)
        return result


def distribution(samples):
    ordered = sorted(samples)
    if not ordered:
        return None
    def percentile(fraction):
        return ordered[max(0, math.ceil(len(ordered) * fraction) - 1)]
    return dict(count=len(ordered), minimum_ms=ordered[0], p50_ms=percentile(.5),
                p95_ms=percentile(.95), p99_ms=percentile(.99), maximum_ms=ordered[-1])


GENRES = ["Rock", "Jazz", "Classical", "Electronic", "Hip Hop", "Folk", "Blues", "Metal", "Pop", "Soul",
          "Reggae", "Country", "Ambient", "Punk", "Funk", "Latin", "World", "Soundtrack", "Gospel", "Disco"]
NOW = int(time.time())


def audio_profile(n):
    """Deterministic format mix: FLAC 16/24-bit and MP3 320/VBR."""
    kind = n % 10
    if kind < 4:
        return "flac", 16, 900 + n % 400, 30_000_000
    if kind < 6:
        return "flac", 24, 1800 + n % 900, 60_000_000
    if kind < 8:
        return "mp3", None, 320, 9_000_000
    return "mp3", None, 190 + n % 60, 5_500_000


def seed(database_path, root, scan_root, playing):
    """Only call while the daemon is stopped; use its own migrated schema.

    Asset paths are real hard links to one tiny WAV so run enqueue can
    canonicalize them; link creation time is returned separately."""
    link_started = time.perf_counter()
    audio = root.parent / "link-source.wav"
    with wave.open(str(playing), "rb") as wav:
        frames = wav.readframes(4800)
    with wave.open(str(audio), "wb") as wav:
        wav.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
        wav.writeframes(frames)
    for n in range(1, TRACKS + 1):
        os.link(audio, root / f"Metadata only {n:05}.flac")
    link_seconds = time.perf_counter() - link_started
    seed_started = time.perf_counter()
    with sqlite3.connect(database_path) as db:
        db.execute("PRAGMA foreign_keys=ON")
        assert db.execute("PRAGMA user_version").fetchone()[0] == 5
        assert db.execute("SELECT count(*) FROM library_tracks").fetchone()[0] == 0
        for identity, path in [(1, root), (2, scan_root)]:
            db.execute("INSERT INTO library_roots(id,path,search) VALUES(?,?,?)",
                       (identity, str(path), str(path).lower()))
        db.executemany("INSERT INTO library_artists(id,identity,name,search) VALUES(?,?,?,?)",
                       ((n, f"scale-artist-{n}", f"Scale Artist {n:04}", f"scale artist {n:04}")
                        for n in range(1, 501)))
        db.executemany("INSERT INTO library_albums(id,identity,artist_id,title,album_artist,search,title_sort,compilation) VALUES(?,?,?,?,?,?,?,0)",
                       ((n, f"scale-album-{n}", (n - 1) // 10 + 1, f"Scale Album {n:04}",
                         f"Scale Artist {(n - 1) // 10 + 1:04}",
                         f"scale album {n:04} scale artist {(n - 1) // 10 + 1:04}", f"scale album {n:04}")
                        for n in range(1, 5001)))
        db.executemany("INSERT INTO library_assets(id,root_id,path,indexed,last_seen_scan,metadata_version) VALUES(?,1,?,1,0,2)",
                       ((n, str(root / f"Metadata only {n:05}.flac")) for n in range(1, TRACKS + 1)))
        def tracks():
            for n in range(1, TRACKS + 1):
                artist, album, track = (n - 1) // 100 + 1, (n - 1) // 10 + 1, (n - 1) % 10 + 1
                title = f"Scale Track {n:05}"
                metadata = dict(title=title, artist=f"Scale Artist {artist:04}",
                                album_artist=f"Scale Artist {artist:04}", album=f"Scale Album {album:04}",
                                track_number=track, track_total=10, disc_number=1, disc_total=1,
                                compilation=False, duration_ms=120000, sample_rate=48000, channels=2)
                search = f"{title} {metadata['artist']} {metadata['album']}".lower()
                yield (n, n, artist, album, json.dumps(metadata), search, title.lower(), track)
        def full_tracks():
            for n, *rest in tracks():
                codec, depth, bitrate, size = audio_profile(n)
                year = 1950 + (n * 7919) % 76
                # One in ten tracks was added in the last 30 days; the rest over five years.
                added = NOW - (n * 104729) % 2_000_000 if n % 10 == 0 else NOW - 2_592_000 - (n * 104729) % 155_000_000
                yield (n, *rest, year, codec, depth, bitrate, size, added)
        db.executemany("INSERT INTO library_tracks(id,asset_id,artist_id,album_id,metadata,search,title_sort,disc_number,disc_total,track_number,year,codec,bit_depth,bitrate_kbps,file_size,date_added) VALUES(?,?,?,?,?,?,?,1,1,?,?,?,?,?,?,?)", full_tracks())
        db.executemany("INSERT INTO library_genres(id,name,search) VALUES(?,?,?)",
                       ((i, name, name.lower()) for i, name in enumerate(GENRES, 1)))
        def track_genres():
            for n in range(1, TRACKS + 1):
                first = n % len(GENRES) + 1
                yield (n, first, 0)
                if n % 3 == 0:
                    yield (n, (first + 6) % len(GENRES) + 1, 1)
        db.executemany("INSERT INTO library_track_genres(track_id,genre_id,position) VALUES(?,?,?)", track_genres())
        db.executemany("INSERT INTO track_stats(track_id,play_count,last_played) VALUES(?,?,?)",
                       ((n, 1 + n % 40, NOW - n * 600) for n in range(1, TRACKS + 1, 5)))
        db.execute("UPDATE library_state SET revision=revision+1,root_count=2,track_count=?,pending_upgrade=0 WHERE singleton=1", (TRACKS,))
        db.executemany("INSERT INTO queue(id,ordinal,path,track_id) VALUES(?,?,?,NULL)",
                       ((n, n - 1, str(playing)) for n in range(1, QUEUE + 1)))
        db.execute("UPDATE control SET revision=revision+1,next_id=?,current_id=1,position_frames=0,playing=0 WHERE singleton=1", (QUEUE + 1,))
        assert not db.execute("PRAGMA foreign_key_check").fetchall()
        counts = {table: db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0]
                  for table in ["library_artists", "library_albums", "library_tracks", "library_genres",
                                "library_track_genres", "track_stats", "queue"]}
    counts["asset_hard_link_creation_seconds"] = link_seconds
    counts["sql_seed_seconds"] = time.perf_counter() - seed_started
    return counts


def binary_identity(path, declared_profile):
    digest = hashlib.sha256()
    with path.open("rb") as binary:
        for chunk in iter(lambda: binary.read(1024 * 1024), b""):
            digest.update(chunk)
    return dict(path=str(path), sha256=digest.hexdigest(), size_bytes=path.stat().st_size,
                declared_build_profile=declared_profile,
                profile_provenance="Harness argument; defaults match scripts/build_macos.sh, not inferred from machine code")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", default="target/Tyranny.app/Contents/MacOS/tyranny-server")
    parser.add_argument("--app", default="target/Tyranny.app")
    parser.add_argument("--ui-driver", default="target/native-ui-driver")
    parser.add_argument("--work-dir", default="target/scale-gate")
    parser.add_argument("--server-profile", choices=["debug", "release", "unknown"], default="release")
    parser.add_argument("--native-profile", choices=["debug", "release", "unknown"], default="debug",
                        help="Native instrumentation requires DEBUG even when compiling optimized code")
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--native-runs", type=int, default=3)
    parser.add_argument("--scan-files", type=int, default=16384)
    parser.add_argument("--coreaudio", action="store_true")
    parser.add_argument("--record", help="Merge the v9 list/enqueue results into this JSON file (for example spike-results/scale-gate.json)")
    parser.add_argument("--queries-only", action="store_true", help="Explicitly incomplete native gate; report marks native coverage absent")
    args = parser.parse_args()
    if args.samples < 20 or args.native_runs < 2 or args.scan_files < 1:
        parser.error("require samples >=20, native-runs >=2, scan-files >=1")
    server = Path(args.server).resolve()
    app = Path(args.app).resolve()
    executable = app / "Contents/MacOS/Tyranny" if app.suffix == ".app" else app
    driver = Path(args.ui_driver).resolve()
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=work))
    state_dir = run / "state"
    state_dir.mkdir(mode=0o700)
    root, scan_root = run / "METADATA-ONLY-NOT-IMPORTED", run / "Real scan WAVs"
    root.mkdir()
    scan_root.mkdir()
    report = dict(run_directory=str(run), passed=False, targets_met=None,
                  fixture=dict(library_tracks=TRACKS, queue_entries=QUEUE,
                               kind="Explicit offline metadata-only library and persistent queue fixture",
                               media="Queue entries all reference one real generated 120-second WAV; seeded library asset paths are hard links to one generated 0.1-second WAV (metadata seeded, not scanned)",
                               scan_files=args.scan_files),
                  environment=dict(platform=platform.platform(), python=sys.version, server=str(server), app=str(app)),
                  output="coreaudio" if args.coreaudio else "clocked_pcm",
                  targets=dict(query_p95_ms=dict(operator="<", value=100), warm_native_first_usable_ms=dict(operator="<=", value=1000)),
                  launches=[], query_samples=[], query_distributions={}, native_runs=[],
                  limitations=["Process-cold means newly started daemon and client before library query warmup; OS/filesystem caches remain uncontrolled, with no purge",
                               "Query latency is end-to-end authenticated WebSocket round trip, including JSON decoding, not SQLite-only time",
                               "Native filter timing includes external Accessibility process overhead, debounce, queries and observed UI completion",
                               "Bounded native/model and reply pages checked; not a heap profiler or proof of server-internal constant memory",
                               "Server currently loads its persistent queue on startup; no full-library or full-queue hydration is requested by this client",
                               "Generated WAV/consumer counters do not establish acoustic output quality"])
    process = None
    native = None
    control = None
    logs = []

    def start():
        nonlocal process, control
        number = len(report["launches"]) + 1
        argv = [str(server), "--state-dir", str(state_dir), "--bind", "127.0.0.1:0"]
        if not args.coreaudio:
            argv += ["--capture", str(run / f"capture-{number}.f32")]
        log = (run / f"server-{number}.stderr").open("w")
        logs.append(log)
        started = time.monotonic()
        process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=log, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(30), "server readiness timed out"
        line = process.stdout.readline()
        assert line, f"server failed; inspect {log.name}"
        ready = json.loads(line)
        control = MeasuredWebSocket(ready["address"], Path(ready["token_path"]).read_text())
        assert control.status == 101
        report["launches"].append(dict(argv=argv, ready=ready, process_launch_uptime=started,
                                       authenticated_ready_ms=(time.monotonic()-started)*1000))
        return ready

    def stop():
        nonlocal process, control
        if control is not None:
            control.close()
            control = None
        if process is not None:
            process.terminate()
            try:
                code = process.wait(timeout=15)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise AssertionError("daemon failed to terminate cleanly")
            finally:
                process.stdout.close()
                process = None
            assert code == 0, f"daemon exit {code}"

    def native_run(index, ready, server_launch_uptime=None, scan=False):
        nonlocal control, native
        # Avoid accumulating unread events while the real native client owns
        # this stage. The next query stage gets a fresh authenticated snapshot.
        control.close()
        control = None
        output = run / f"native-{index}"
        output.mkdir()
        label = "process-cold-cache-uncontrolled" if server_launch_uptime is not None else f"warm-{index}"
        argv = [str(executable), "--url", f"ws://{ready['address']}/ws", "--token-file", ready["token_path"],
                "--scale-output", str(output), "--ui-driver", str(driver), "--scale-run-label", label]
        if server_launch_uptime is not None:
            argv += ["--scale-server-launch-uptime", str(server_launch_uptime)]
        if scan:
            argv += ["--scale-scan-root", str(scan_root)]
        log = (output / "app.log").open("w")
        logs.append(log)
        launched = time.monotonic()
        argv += ["--scale-launch-uptime", str(launched)]
        native = subprocess.Popen(argv, stdout=log, stderr=subprocess.STDOUT)
        try:
            code = native.wait(timeout=240)
        except subprocess.TimeoutExpired:
            native.kill()
            native.wait()
            raise AssertionError(f"native run timed out: {output}")
        native = None
        result_path = output / "report.json"
        assert result_path.exists(), f"native report missing (exit {code}): {output}"
        result = json.loads(result_path.read_text())
        result["process_exit_code"] = code
        result["process_lifetime_ms"] = (time.monotonic() - launched) * 1000
        result["report_path"] = str(result_path)
        report["native_runs"].append(result)
        assert code == 0 and result["passed"], f"native correctness failure: {result_path}"
        control = MeasuredWebSocket(ready["address"], Path(ready["token_path"]).read_text())
        assert control.status == 101
        # The client's quit paused playback. Resume it, so later stages still measure under playback.
        quit_state = control.state()
        result["quit_transport"] = quit_state["transport"]
        assert quit_state["transport"] == "paused", f"client quit did not pause playback: {quit_state['transport']}"
        control.request("play")
        control.wait(lambda state: state["transport"] == "playing")

    def measure(name, command, iteration, **fields):
        before = control.frame_bytes
        started = time.perf_counter_ns()
        result = control.request(command, **fields)
        elapsed = (time.perf_counter_ns() - started) / 1_000_000
        entries = result["entries"]
        assert len(entries) <= fields["limit"] and fields["limit"] in (PAGE, WINDOW)
        assert result["offset"] == fields["offset"]
        assert len({entry["id"] for entry in entries}) == len(entries)
        report["query_samples"].append(dict(name=name, iteration=iteration, command=command,
            fields=fields, elapsed_ms=elapsed, revision=result["revision"], total=result["total"],
            returned=len(entries), first_id=entries[0]["id"] if entries else None,
            last_id=entries[-1]["id"] if entries else None,
            received_frame_bytes_including_state_events=control.frame_bytes-before))
        return result

    try:
        assert server.is_file(), f"Build server first: {server}"
        report["binaries"] = dict(server=binary_identity(server, args.server_profile))
        if not args.queries_only:
            assert executable.is_file() and driver.is_file(), "Build debug native app and Accessibility driver first"
            report["binaries"]["native"] = binary_identity(executable, args.native_profile)
            report["binaries"]["accessibility_driver"] = binary_identity(driver, "unknown")
            bundled_helper = app / "Contents/MacOS/tyranny-server"
            if bundled_helper.is_file():
                report["binaries"]["bundled_helper"] = binary_identity(bundled_helper, "unknown")
                report["binaries"]["measured_server_matches_bundle"] = (
                    report["binaries"]["server"]["sha256"] == report["binaries"]["bundled_helper"]["sha256"])
        playing = run / "continuous.wav"
        fixture(playing)
        source = run / "short-scan.wav"
        with wave.open(str(playing), "rb") as wav:
            samples = wav.readframes(4800)
        with wave.open(str(source), "wb") as wav:
            wav.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
            wav.writeframes(samples)
        for index in range(args.scan_files):
            os.link(source, scan_root / f"Scan WAV {index:05}.wav")
        start()  # Let the production server create/migrate its database.
        stop()
        report["fixture"]["persisted_counts"] = seed(state_dir / "state.sqlite3", root, scan_root, playing)
        ready = start()
        initial = control.state()
        assert initial["library"]["tracks"] == TRACKS and initial["queue_length"] == QUEUE
        report["initial_snapshot"] = initial
        control.request("play")
        control.wait(lambda state: state["transport"] == "playing")
        if not args.queries_only:
            # Fresh daemon + first native client, before any library query
            # distributions. State/play readiness is part of this combined time.
            native_run(0, ready, server_launch_uptime=report["launches"][-1]["process_launch_uptime"])
            report["process_cold_startup"] = report["native_runs"][0]["startup"]

        smart_all = control.request("create_smart_playlist",
                                    expected_revision=control.state()["playlists_revision"],
                                    name="All scale tracks", query="")["playlist"]
        smart_selected = control.request("create_smart_playlist",
                                         expected_revision=control.state()["playlists_revision"],
                                         name="Selected scale artists",
                                         query='artist:["Scale Artist 0001", "Scale Artist 0002"] & !album:"Scale Album 0001"')["playlist"]
        before_queries = control.state()
        assert before_queries["transport"] == "playing"
        cases = [
            ("artists-first", "library_artists", dict(query="", offset=0)),
            ("artists-filter", "library_artists", dict(query="Artist 0001", offset=0)),
            ("albums-first", "library_albums", dict(query="", offset=0, artist_id=None)),
            ("albums-deep", "library_albums", dict(query="", offset=4900, artist_id=None)),
            ("albums-by-artist", "library_albums", dict(query="", offset=0, artist_id=250)),
            ("tracks-first", "library_tracks", dict(query="", offset=0, album_id=None)),
            ("tracks-deep", "library_tracks", dict(query="", offset=49900, album_id=None)),
            ("tracks-selective", "library_tracks", dict(query="Scale Track 00001", offset=0, album_id=None)),
            ("tracks-broad", "library_tracks", dict(query="Scale Track", offset=0, album_id=None)),
            ("tracks-no-match", "library_tracks", dict(query="scale-no-match", offset=0, album_id=None)),
            ("tracks-by-album", "library_tracks", dict(query="", offset=0, album_id=2500)),
            ("tracks-structured-list-negation", "library_tracks", dict(
                query='artist:["Scale Artist 0001", "Scale Artist 0002"] & !album:"Scale Album 0001"',
                offset=0, album_id=None)),
            ("artists-structured-track-projection", "library_artists", dict(
                query='artist:"Scale Artist 0001" & (title:"Scale Track 00001" | title:"Scale Track 00011")',
                offset=0)),
            # Up Next reads the queue in client-sized windows.
            ("queue-first", "queue_page", dict(offset=0, limit=WINDOW)),
            ("queue-deep", "queue_page", dict(offset=QUEUE - WINDOW, limit=WINDOW)),
            ("smart-first", "playlist_entries", dict(playlist_id=smart_all["id"],
                revision=smart_all["revision"], library_revision=before_queries["library"]["revision"], query="", offset=0)),
            ("smart-deep", "playlist_entries", dict(playlist_id=smart_all["id"],
                revision=smart_all["revision"], library_revision=before_queries["library"]["revision"], query="", offset=49900)),
            ("smart-rule-and-view-filter", "playlist_entries", dict(playlist_id=smart_selected["id"],
                revision=smart_selected["revision"], library_revision=before_queries["library"]["revision"], query="title:0001", offset=0)),
            ("smart-catalog", "saved_playlists", dict(kind="smart", query="", offset=0)),
        ]
        # Rotate order to avoid assigning every case the same place in each round.
        for iteration in range(args.samples):
            shift = iteration % len(cases)
            for name, command, fields in cases[shift:] + cases[:shift]:
                request_fields = {"revision": None, "limit": PAGE, **fields}
                result = measure(name, command, iteration, **request_fields)
                if name == "tracks-selective":
                    assert result["total"] == 1 and result["entries"][0]["id"] == 1
                if name == "tracks-broad":
                    assert result["total"] == TRACKS
                if name == "tracks-no-match":
                    assert result["total"] == 0
                if name == "tracks-structured-list-negation":
                    # Library order is disc, track number, then title—not album grouping.
                    expected = [track + album * 10 for track in range(1, 11) for album in range(1, 20)][:PAGE]
                    assert result["total"] == 190 and [row["id"] for row in result["entries"]] == expected
                if name == "artists-structured-track-projection":
                    assert result["total"] == 1 and result["entries"][0]["id"] == 1 and result["entries"][0]["album_count"] == 2
                if name == "smart-first":
                    assert result["total"] == TRACKS and [row["track_id"] for row in result["entries"]] == list(range(1, 1001, 10))
                if name == "smart-deep":
                    assert result["total"] == TRACKS and [row["track_id"] for row in result["entries"]] == list(range(49010, 50001, 10))
                if name == "smart-rule-and-view-filter":
                    assert result["total"] == 9 and [row["track_id"] for row in result["entries"]] == list(range(11, 20))
                if name == "smart-catalog":
                    assert [row["id"] for row in result["entries"]] == [smart_all["id"], smart_selected["id"]]
                    assert all(row["kind"] == "smart" and row["entry_count"] is None for row in result["entries"])
                if command == "queue_page":
                    assert result["total"] == QUEUE and len(result["entries"]) == WINDOW
        for name, _, _ in cases:
            summary = distribution([sample["elapsed_ms"] for sample in report["query_samples"] if sample["name"] == name])
            summary["target_p95_ms"] = 100
            summary["target_met"] = summary["p95_ms"] < 100
            report["query_distributions"][name] = summary
        # v9 list families: every wire field explicit, nulls included.
        snapshot = control.state()
        library_revision = snapshot["library"]["revision"]
        underruns = dict(before_legacy_queries=before_queries["underruns"], after_legacy_queries=snapshot["underruns"])
        everything = dict(source=dict(kind="library"), browser=dict(genre_ids=[], artist_ids=[], compilations=False, album_ids=[]),
                          query="", sort=[], group="none")
        def described(**changes):
            return {**everything, **changes}
        list_families = [
            ("list_tracks_first_window", "list_tracks", dict(list=described(), revision=None, offset=0, limit=200), TRACKS),
            ("list_tracks_deep_window", "list_tracks", dict(list=described(), revision=None, offset=TRACKS - 200, limit=200), TRACKS),
            ("list_tracks_sorted_year_desc", "list_tracks", dict(list=described(sort=[dict(field="year", descending=True)]),
                                                               revision=None, offset=0, limit=200), TRACKS),
            ("list_tracks_grouped_artist_album", "list_tracks", dict(list=described(group="artist_album"),
                                                                    revision=None, offset=0, limit=200), TRACKS),
            ("list_tracks_filtered_genre", "list_tracks", dict(list=described(browser=dict(genre_ids=[2], artist_ids=[], compilations=False, album_ids=[])),
                                                              revision=None, offset=0, limit=200), None),
            ("list_tracks_filtered_genre_query", "list_tracks", dict(list=described(query="genre:Jazz"),
                                                                    revision=None, offset=0, limit=200), None),
            ("list_albums_by_year", "list_albums", dict(list=described(), sort="year", descending=True,
                                                       revision=None, offset=0, limit=200), None),
        ]
        families = {}
        for name, command, fields, expected_total in list_families:
            samples = []
            for iteration in range(args.samples):
                started = time.perf_counter_ns()
                result = control.request(command, **fields)
                samples.append((time.perf_counter_ns() - started) / 1_000_000)
                assert result["offset"] == fields["offset"] and 0 < len(result["entries"]) <= 200, (name, result["total"])
                if expected_total is not None:
                    assert result["total"] == expected_total, (name, result["total"])
            summary = distribution(samples)
            summary.update(target_p95_ms=100, target_met=summary["p95_ms"] < 100, total=result["total"],
                           returned=len(result["entries"]), samples_ms=samples)
            families[name] = summary
            print(f"{name}: p50 {summary['p50_ms']:.1f} p95 {summary['p95_ms']:.1f} max {summary['maximum_ms']:.1f} ms", flush=True)
        report["v9_list_distributions"] = families
        underruns["after_list_families"] = control.state()["underruns"]
        report["v9_library_revision"] = library_revision
        # 50k run enqueue: ReplaceUpcoming of the whole library, queue length asserted.
        enqueue_samples = []
        for iteration in range(3):
            snapshot = control.state()
            current = 1 if snapshot["current_id"] is not None else 0
            started = time.perf_counter_ns()
            result = control.request("enqueue_tracks",
                selection=dict(kind="list", list=described(), library_revision=snapshot["library"]["revision"],
                               ranges=[[0, 18446744073709551615]], excluded=[], first_row_id=None),
                expected_revision=snapshot["queue_revision"], placement="replace_upcoming", play=False,
                source=dict(kind="songs", name="Songs"))
            elapsed = (time.perf_counter_ns() - started) / 1_000_000
            after = control.state()
            assert after["queue_length"] == current + TRACKS, (after["queue_length"], current)
            enqueue_samples.append(elapsed)
            underruns[f"after_enqueue_{iteration}"] = after["underruns"]
            print(f"enqueue_run_50k #{iteration}: {elapsed:.1f} ms queue_length {after['queue_length']}", flush=True)
        enqueue = distribution(enqueue_samples)
        enqueue.update(target_p95_ms=None, target_met=None, samples_ms=enqueue_samples,
                       expected_queue_length=current + TRACKS, queue_length_asserted=True)
        report["v9_enqueue_run_50k"] = enqueue
        report["v9_underrun_counters"] = underruns
        report["v9_list_targets_met"] = all(family["target_met"] for family in families.values())
        # Longer than the output prebuffer, so a stall the enqueues caused is counted.
        time.sleep(1)
        underruns["after_enqueues_settled"] = control.state()["underruns"]

        # 50k queue and playlist edits while playing. Each family runs three
        # times; its underrun delta includes a settle longer than the prebuffer.
        whole_library = dict(kind="list", list=described(), library_revision=library_revision,
                             ranges=[[0, 18446744073709551615]], excluded=[], first_row_id=None)
        upcoming = TRACKS
        queue_families = {}
        def timed(command, **fields):
            started = time.perf_counter_ns()
            result = control.request(command, **fields)
            return result, (time.perf_counter_ns() - started) / 1_000_000
        def family(name, step):
            before = control.state()["underruns"]
            samples = [step(iteration) for iteration in range(3)]
            time.sleep(1)
            after = control.state()
            assert after["transport"] == "playing", (name, after["transport"])
            summary = distribution(samples)
            summary.update(samples_ms=samples, underrun_delta=after["underruns"] - before)
            queue_families[name] = summary
            print(f"{name}: {', '.join(f'{sample:.1f}' for sample in samples)} ms, "
                  f"underruns {summary['underrun_delta']}", flush=True)
        def shuffle_upcoming(iteration):
            snapshot = control.state()
            _, elapsed = timed("shuffle_upcoming", expected_revision=snapshot["queue_revision"])
            assert control.state()["queue_length"] == 1 + upcoming
            return elapsed
        saved = []
        def save_upcoming(iteration):
            snapshot = control.state()
            result, elapsed = timed("save_upcoming", expected_revision=snapshot["queue_revision"],
                                    expected_playlists_revision=snapshot["playlists_revision"],
                                    name=f"Saved upcoming {iteration}")
            assert result["playlist"]["entry_count"] == upcoming, result["playlist"]
            saved.append(result["playlist"])
            return elapsed
        def playlist_add_selection(iteration):
            playlist = control.request("create_playlist", expected_revision=control.state()["playlists_revision"],
                                       name=f"Selection {iteration}")["playlist"]
            result, elapsed = timed("playlist_add_selection", playlist_id=playlist["id"],
                                    expected_revision=playlist["revision"], selection=whole_library, before=None)
            assert result["playlist"]["entry_count"] == TRACKS, result["playlist"]
            return elapsed
        def clear_upcoming(iteration):
            snapshot = control.state()
            assert snapshot["queue_length"] == 1 + upcoming
            _, elapsed = timed("clear_upcoming", expected_revision=snapshot["queue_revision"])
            assert control.state()["queue_length"] == 1
            # Refill outside the measurement for the next sample.
            if iteration < 2:
                snapshot = control.state()
                control.request("enqueue_playlist", playlist_id=saved[0]["id"],
                                playlist_revision=saved[0]["revision"], library_revision=library_revision,
                                expected_revision=snapshot["queue_revision"], play=False)
            return elapsed
        def enqueue_playlist(iteration):
            snapshot = control.state()
            _, elapsed = timed("enqueue_playlist", playlist_id=saved[0]["id"],
                               playlist_revision=saved[0]["revision"], library_revision=library_revision,
                               expected_revision=snapshot["queue_revision"], play=False)
            assert control.state()["queue_length"] == 1 + upcoming
            # Empty the upcoming entries outside the measurement for the next sample.
            if iteration < 2:
                control.request("clear_upcoming", expected_revision=control.state()["queue_revision"])
            return elapsed
        family("shuffle_upcoming_50k", shuffle_upcoming)
        family("save_upcoming_50k", save_upcoming)
        family("playlist_add_selection_50k", playlist_add_selection)
        family("clear_upcoming_50k", clear_upcoming)
        family("enqueue_playlist_50k", enqueue_playlist)
        report["v9_queue_families_50k"] = queue_families
        underruns["after_queue_families"] = control.state()["underruns"]

        final = control.state()
        report["final_snapshot"] = final
        report["playback_observation"] = dict(before=before_queries, after=final,
            advanced=final["current_id"] != before_queries["current_id"] or final["position_frames"] > before_queries["position_frames"],
            underrun_delta=final["underruns"] - before_queries["underruns"])
        assert final["transport"] == "playing" and report["playback_observation"]["advanced"]
        # Neither queries nor 50k run enqueues may disturb playback: run
        # selections resolve off the playback owner.
        query_underruns = underruns["after_list_families"] - before_queries["underruns"]
        enqueue_underruns = underruns["after_enqueues_settled"] - underruns["after_list_families"]
        report["playback_observation"]["query_phase_underrun_delta"] = query_underruns
        report["playback_observation"]["enqueue_phase_underrun_delta"] = enqueue_underruns
        assert query_underruns == 0, "playback underruns during query measurements"
        assert enqueue_underruns == 0, f"{enqueue_underruns} playback underruns during 50k run enqueues"
        stalled = {name: summary["underrun_delta"] for name, summary in queue_families.items()
                   if summary["underrun_delta"] != 0}
        assert not stalled, f"playback underruns during 50k queue edits: {stalled}"
        if not args.queries_only:
            for index in range(1, args.native_runs):
                native_run(index, ready, scan=index == args.native_runs - 1)
        else:
            report["limitations"].append("--queries-only: native startup, Accessibility, screenshots and native concurrent scan NOT exercised")

        # Every startup and query distribution uses exactly 50k/10k; scanning
        # adds real tracks only after the final warm native startup measurement.
        report["limitations"].append("Warm native repeats follow the first native run and query distributions, reusing the same daemon")
        warm = [r["startup"]["process_to_first_usable_ms"] for r in report["native_runs"] if r["run_label"].startswith("warm-")]
        report["warm_startup_distribution"] = distribution(warm)
        report["targets_met"] = (all(s["target_met"] for s in report["query_distributions"].values())
                                 and all(sample <= 1000 for sample in warm)) if warm else None
        report["native_filter_distributions"] = {
            label: distribution([sample["elapsed_ms"] for native_run in report["native_runs"]
                                 for sample in native_run["filter_samples"] if sample["label"] == label])
            for label in sorted({sample["label"] for native_run in report["native_runs"]
                                 for sample in native_run["filter_samples"]})}
        report["final_snapshot"] = control.state()
        report["concurrent_scan_filter_observed"] = any(
            native_run.get("concurrent_scan", {}).get("filter_spanned_active_scan", False)
            for native_run in report["native_runs"])
        report["native_coverage_complete"] = bool(report["native_runs"]) and report["concurrent_scan_filter_observed"]
        if report["native_runs"]:
            library = report["final_snapshot"]["library"]
            assert library["tracks"] == TRACKS + args.scan_files, "real scan did not add every WAV fixture"
            assert library["scan"]["phase"] == "complete" and library["scan"]["failures"] == 0
        if report["native_runs"] and not report["concurrent_scan_filter_observed"]:
            raise AssertionError("No native filter completed during an active scan; concurrency coverage is unproven. Increase --scan-files for faster machines.")
        report["passed"] = True  # Functional gate; performance-target misses remain explicit.
    except Exception as error:
        report["error"] = f"{type(error).__name__}: {error}"
    finally:
        if native is not None:
            native.terminate()
            try:
                native.wait(timeout=5)
            except subprocess.TimeoutExpired:
                native.kill()
                native.wait()
        try:
            stop()
        except Exception as error:
            report["cleanup_error"] = str(error)
            report["passed"] = False
        for log in logs:
            log.close()
        (run / "report.json").write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
        if args.record and "v9_list_distributions" in report:
            record = Path(args.record)
            merged = json.loads(record.read_text()) if record.exists() else {}
            # Hand-written keys (baselines, findings) survive a rerun.
            merged["library_server_v9_scale"] = merged.get("library_server_v9_scale", {})
            merged["library_server_v9_scale"].update(
                run_report=str(run / "report.json"), passed=report["passed"], error=report.get("error"),
                native_coverage="ABSENT: --queries-only; native UI, Accessibility and Screen Recording runs not exercised" if args.queries_only else "present",
                fixture=report["fixture"], binaries=report["binaries"], environment=report["environment"],
                server_profile=args.server_profile, samples_per_family=args.samples, library_revision=report["v9_library_revision"],
                list_distributions=report["v9_list_distributions"], list_targets_met=report["v9_list_targets_met"],
                enqueue_run_50k=report["v9_enqueue_run_50k"], underrun_counters=report["v9_underrun_counters"],
                playback_observation={k: v for k, v in report.get("playback_observation", {}).items() if k not in ("before", "after")},
                queue_edit_families_50k=report.get("v9_queue_families_50k"),
                note="Latency is end-to-end authenticated WebSocket round trip on this machine; seeding time (hard links and SQL) is reported in fixture.persisted_counts and excluded from enqueue timing")
            record.write_text(json.dumps(merged, indent=2, sort_keys=True) + "\n")
        print(json.dumps(dict(report=str(run / "report.json"), passed=report["passed"], targets_met=report["targets_met"])))
    return 0 if report["passed"] else 1


if __name__ == "__main__":
    sys.exit(main())
