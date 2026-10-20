#!/usr/bin/env python3
"""Real audio import/rescan/restart gate over the actual server WebSocket.

No metadata-only substitute: FLAC files are encoded by FFmpeg, then read by the
native indexer. Bulk entries are real filesystem hard links, with path identities.
"""
import argparse
import json
import os
from pathlib import Path
import selectors
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import wave

sys.dont_write_bytecode = True
from server_gate import WebSocket, fixture


FINISHED = {"complete", "failed", "interrupted"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", default="target/debug/tyranny-server")
    parser.add_argument("--work-dir", default="target/library-gate")
    parser.add_argument("--coreaudio", action="store_true")
    parser.add_argument("--bulk-tracks", type=int, default=2048)
    args = parser.parse_args()
    assert args.bulk_tracks >= 128
    server = str(Path(args.server).resolve())
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=work))
    root = run / "Musique – 日本語"
    root.mkdir()
    state_dir = run / "state"
    state_dir.mkdir(mode=0o700)
    playing = run / "continuous.wav"
    fixture(playing)
    short = run / "source.wav"
    with wave.open(str(playing), "rb") as source:
        samples = source.readframes(48000)
    with wave.open(str(short), "wb") as output:
        output.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
        output.writeframes(samples)
    report = dict(run_directory=str(run), output="coreaudio" if args.coreaudio else "clocked_pcm", cases=[], launches=[], encoder_commands=[], bulk_tracks=args.bulk_tracks, limitations=["No SwiftUI or folder picker exercised", "Path identity; automatic move detection and CUE parsing deferred", "Generated real audio fixtures, not a commercial music collection", "No 50000-track production scale certification or human listening claim"])

    def encode(path, tags):
        path.parent.mkdir(parents=True, exist_ok=True)
        command = ["ffmpeg", "-v", "error", "-y", "-i", str(short), "-map_metadata", "-1", "-c:a", "flac"]
        for key, value in tags.items():
            command += ["-metadata", f"{key}={value}"]
        command += [str(path)]
        result = subprocess.run(command, capture_output=True, text=True, timeout=15)
        report["encoder_commands"].append(dict(argv=command, returncode=result.returncode, stderr=result.stderr))
        assert result.returncode == 0, result.stderr

    unicode_file = root / "Ångström" / "Échos" / "01 – Étude 一.flac"
    unicode_tags = dict(title="Étude 100% 一", artist="Ångström", album_artist="Ångström", album="Échos", track="1/2", disc="1/1", replaygain_track_gain="-7.50 dB", replaygain_album_gain="-5.25 dB", replaygain_track_peak="0.91", replaygain_album_peak="0.97", MUSICBRAINZ_ALBUMID="11111111-1111-4111-8111-111111111111", MUSICBRAINZ_RELEASEGROUPID="11111111-1111-4111-8111-222222222222", MUSICBRAINZ_TRACKID="11111111-1111-4111-8111-333333333333", MUSICBRAINZ_RELEASETRACKID="11111111-1111-4111-8111-444444444444", MUSICBRAINZ_ARTISTID="11111111-1111-4111-8111-555555555555", MUSICBRAINZ_ALBUMARTISTID="11111111-1111-4111-8111-555555555555")
    encode(unicode_file, unicode_tags)
    compilation = []
    for number, artist in enumerate(["Alice", "Bob"], 1):
        path = root / "Compilation" / f"{number:02}.flac"
        encode(path, dict(title=f"Song {number}", artist=artist, album="Sampler", compilation="1", track=f"{number}/2"))
        compilation.append(path)
    discs = []
    for disc in [1, 2]:
        path = root / "Box" / f"Disc {disc}" / "01.flac"
        encode(path, dict(title=f"Movement {disc}", artist="Ensemble", album_artist="Ensemble", album="Box", disc=f"{disc}/2", track="1/1", MUSICBRAINZ_ALBUMID="22222222-2222-4222-8222-222222222222"))
        discs.append(path)
    editions = []
    for label in ["Original", "Alternate"]:
        path = root / "Same Artist" / label / "01.flac"
        encode(path, dict(title=label, artist="Same Artist", album_artist="Same Artist", album="Same Name", track="1"))
        editions.append(path)
    untagged = root / "Sans tags.WAV"
    shutil.copyfile(short, untagged)
    (root / "broken.flac").write_bytes(b"not a media file")
    unreadable = root / "sealed.flac"
    shutil.copyfile(unicode_file, unreadable)
    unreadable.chmod(0)
    (root / "cover.txt").write_text("not an audio candidate\n")
    (root / "cycle").symlink_to(root, target_is_directory=True)
    bulk_source = run / "bulk-source.flac"
    encode(bulk_source, dict(title="Stress Track", artist="Load Artist", album="Working Set"))
    bulk = root / "Bulk"
    bulk.mkdir()
    for number in range(args.bulk_tracks):
        os.link(bulk_source, bulk / f"{number:05}.flac")
    expected_tracks = args.bulk_tracks + 8 + int(os.geteuid() == 0)
    minimum_failures = 1 + int(os.geteuid() != 0)

    # Exact old v1 schema: prove migration preserves queue identities/checkpoint.
    database_path = state_dir / "state.sqlite3"
    with sqlite3.connect(database_path) as database:
        database.executescript("""
            PRAGMA application_id=1954116206;
            PRAGMA user_version=1;
            CREATE TABLE queue(id INTEGER PRIMARY KEY CHECK(id>0),ordinal INTEGER NOT NULL UNIQUE,path TEXT NOT NULL);
            CREATE TABLE control(singleton INTEGER PRIMARY KEY CHECK(singleton=1),revision INTEGER NOT NULL CHECK(revision>0),next_id INTEGER NOT NULL CHECK(next_id>0),current_id INTEGER REFERENCES queue(id) ON DELETE SET NULL,position_frames INTEGER NOT NULL CHECK(position_frames>=0),playing INTEGER NOT NULL CHECK(playing IN(0,1)),settings TEXT NOT NULL);
        """)
        database.execute("INSERT INTO queue VALUES(7,0,?)", (str(playing),))
        database.execute("INSERT INTO control VALUES(1,11,42,7,24000,0,?)", (json.dumps(dict(gain_mode="off", resume_on_start=False)),))
    database_path.chmod(0o600)
    process = None
    connections = []
    logs = []
    launches = 0
    blocked = None
    offline = None

    def record(name, **details):
        report["cases"].append(dict(name=name, passed=True, **details))
        print(f"Passed {name}", flush=True)

    def start():
        nonlocal process, launches
        launches += 1
        argv = [server, "--state-dir", str(state_dir), "--bind", "127.0.0.1:0"]
        if not args.coreaudio:
            argv += ["--capture", str(run / f"capture-{launches}.f32")]
        log = (run / f"server-{launches}.stderr").open("w")
        logs.append(log)
        process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=log, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(15), "server startup timed out"
        line = process.stdout.readline()
        assert line, f"server failed; see {log.name}"
        ready = json.loads(line)
        report["launches"].append(dict(argv=argv, ready=ready))
        token = Path(ready["token_path"]).read_text()
        control = WebSocket(ready["address"], token)
        connections.append(control)
        return control

    def stop(crash=False):
        nonlocal process
        if process is not None:
            process.kill() if crash else process.terminate()
            code = process.wait(timeout=15)
            assert code == (-9 if crash else 0), code
            process.stdout.close()
            process = None
        for connection in connections:
            connection.close()
        connections.clear()

    def page(control, kind, revision=None, offset=0, limit=137, query="", **filters):
        return control.request("library_" + kind, revision=revision, offset=offset, limit=limit, query=query, **filters)

    def tracks(control):
        first = page(control, "tracks", album_id=None)
        entries = list(first["entries"])
        while len(entries) < first["total"]:
            following = page(control, "tracks", revision=first["revision"], offset=len(entries), album_id=None)
            assert following["entries"]
            entries.extend(following["entries"])
        assert len(entries) == first["total"] and len({entry["id"] for entry in entries}) == len(entries)
        return {entry["path"]: entry for entry in entries}, first["revision"]

    def scan(control, root_id=None, start_new=True, watch_audio=False):
        if start_new:
            control.request("rescan", root_id=root_id)
        deadline = time.monotonic() + 90
        observations = []
        latencies = []
        before = control.state()["position_frames"]
        while time.monotonic() < deadline:
            begin = time.monotonic()
            state = control.state()
            latencies.append(time.monotonic() - begin)
            status = state["library"]["scan"]
            observations.append(status)
            if status["phase"] == "discovering":
                assert status["total"] is None
            if status["phase"] == "indexing":
                assert status["total"] == status["discovered"] and status["processed"] <= status["total"]
                result = page(control, "tracks", limit=7, album_id=None)
                assert len(result["entries"]) <= 7
            if watch_audio:
                assert state["underruns"] == 0 and state["current_id"] == 7
            if status["phase"] in FINISHED:
                if watch_audio:
                    assert state["position_frames"] > before + 4800, "fixture scan too brief to measure playback progress"
                return state, observations, max(latencies)
            time.sleep(0.02)
        raise AssertionError(f"scan timed out: {observations[-1:]}")

    try:
        control = start()
        initial = control.snapshot
        assert initial["queue_revision"] == 11 and initial["current_id"] == 7
        assert initial["position_frames"] == 24000 and initial["transport"] == "paused"
        assert initial["library"]["tracks"] == 0 and initial["library"]["scan"]["phase"] == "idle"
        with sqlite3.connect(database_path) as database:
            assert database.execute("PRAGMA user_version").fetchone()[0] == 7
        record("v1-migration-preserves-queue-identity-and-checkpoint", snapshot=initial)
        control.request("play")
        control.wait(lambda state: state["transport"] == "playing")
        added = control.request("add_root", path=str(root))
        root_id = added["root"]["id"]
        state, progress, latency = scan(control, start_new=False, watch_audio=True)
        assert state["library"]["scan"]["phase"] == "complete", state
        assert state["library"]["scan"]["failures"] >= minimum_failures
        assert state["library"]["tracks"] == expected_tracks, state
        assert latency < 1, latency
        record("native-import-progress-and-playback-isolation", tracks=expected_tracks, progress=progress, maximum_control_latency_seconds=latency, underruns=state["underruns"])

        indexed, revision = tracks(control)
        original_ids = {path: entry["id"] for path, entry in indexed.items()}
        metadata = indexed[str(unicode_file)]["metadata"]
        assert metadata["title"] == unicode_tags["title"] and metadata["artist"] == "Ångström"
        assert (metadata["track_number"], metadata["track_total"], metadata["disc_number"], metadata["disc_total"]) == (1, 2, 1, 1)
        assert abs(metadata["track_gain_db"] + 7.5) < 0.001 and abs(metadata["album_peak"] - 0.97) < 0.001
        assert metadata["musicbrainz_release_id"] == unicode_tags["MUSICBRAINZ_ALBUMID"]
        assert metadata["musicbrainz_recording_id"] == unicode_tags["MUSICBRAINZ_TRACKID"]
        assert metadata["musicbrainz_track_id"] == unicode_tags["MUSICBRAINZ_RELEASETRACKID"]
        assert metadata["duration_ms"] is not None and 990 <= metadata["duration_ms"] <= 1010
        assert metadata["sample_rate"] == 48000 and metadata["channels"] == 2
        artists = page(control, "artists", query="ÅNG")
        assert [entry["name"] for entry in artists["entries"]] == ["Ångström"]
        albums = page(control, "albums", artist_id=artists["entries"][0]["id"])
        assert [entry["title"] for entry in albums["entries"]] == ["Échos"]
        percent = page(control, "tracks", query="100%", album_id=None)
        assert [entry["id"] for entry in percent["entries"]] == [indexed[str(unicode_file)]["id"]]
        assert page(control, "tracks", query="100_", album_id=None)["total"] == 0
        fallback = indexed[str(untagged)]["metadata"]
        assert (fallback["title"], fallback["artist"], fallback["album"]) == ("Sans tags", "Unknown Artist", "Unknown Album")
        record("metadata-unicode-gain-musicbrainz-and-literal-filtering", metadata=metadata)

        compilation_tracks = [indexed[str(path)] for path in compilation]
        assert len({entry["album_id"] for entry in compilation_tracks}) == 1
        assert {entry["metadata"]["artist"] for entry in compilation_tracks} == {"Alice", "Bob"}
        assert all(entry["metadata"]["album_artist"] == "Various Artists" and entry["metadata"]["compilation"] for entry in compilation_tracks)
        box_id = indexed[str(discs[0])]["album_id"]
        assert indexed[str(discs[1])]["album_id"] == box_id
        ordered = page(control, "tracks", album_id=box_id)
        assert [entry["metadata"]["disc_number"] for entry in ordered["entries"]] == [1, 2]
        box = page(control, "albums", query="Box", artist_id=None)["entries"]
        assert len(box) == 1 and box[0]["disc_count"] == 2 and box[0]["track_count"] == 2
        assert indexed[str(editions[0])]["album_id"] != indexed[str(editions[1])]["album_id"]
        record("compilation-multidisc-and-distinct-release-grouping", box=box[0])
        control.request("library_tracks", expect_error="invalid_command", revision=None, offset=0, limit=501, query="", album_id=None)
        control.request("add_root", expect_error="invalid_command", path=str(root / "Bulk"))
        control.request("add_root", expect_error="invalid_command", path=str(root))
        queue = control.state()
        appended = control.request("append_track", expected_revision=queue["queue_revision"], library_revision=revision, track_id=indexed[str(unicode_file)]["id"])
        assert appended["entry_id"] == 42
        queued = control.request("queue_page", revision=None, offset=0, limit=100)
        assert queued["entries"][-1]["track_id"] == indexed[str(unicode_file)]["id"]
        control.request("append_track", expect_error="stale_library_revision", expected_revision=queued["revision"], library_revision=revision-1, track_id=indexed[str(unicode_file)]["id"])
        record("bounded-pages-overlap-rejection-and-transactional-track-enqueue", queue=queued)

        state, _, _ = scan(control, root_id)
        assert state["library"]["scan"]["updated"] == 0 and state["library"]["scan"]["unchanged"] == expected_tracks
        unchanged, old_revision = tracks(control)
        assert {path: entry["id"] for path, entry in unchanged.items()} == original_ids
        record("unchanged-rescan-preserves-ids-and-skips-probes", scan=state["library"]["scan"])
        unicode_tags["title"] = "Révisé 一"
        encode(unicode_file, unicode_tags)
        new_file = root / "New" / "01.flac"
        encode(new_file, dict(title="New track", artist="New Artist", album="New Album"))
        editions[0].unlink()
        state, _, _ = scan(control, root_id)
        current, revision = tracks(control)
        assert len(current) == expected_tracks and str(editions[0]) not in current
        assert current[str(unicode_file)]["id"] == original_ids[str(unicode_file)]
        assert current[str(unicode_file)]["metadata"]["title"] == "Révisé 一"
        assert str(new_file) in current
        control.request("library_tracks", expect_error="stale_library_revision", revision=old_revision, offset=0, limit=100, query="", album_id=None)
        encode(editions[0], dict(title="Original", artist="Same Artist", album_artist="Same Artist", album="Same Name", track="1"))
        scan(control, root_id)
        restored, _ = tracks(control)
        expected_tracks += 1
        assert len(restored) == expected_tracks and restored[str(editions[0])]["id"] == original_ids[str(editions[0])]
        record("add-retag-remove-and-reappear-with-stable-identities")

        offline = root.with_name(root.name + " offline")
        root.rename(offline)
        state, _, _ = scan(control, root_id)
        assert state["library"]["scan"]["phase"] == "failed" and state["library"]["scan"]["last_error"]
        preserved, _ = tracks(control)
        assert {path: entry["id"] for path, entry in preserved.items()} == {path: entry["id"] for path, entry in restored.items()}
        offline.rename(root)
        offline = None
        record("unavailable-root-retains-last-known-index")
        if os.geteuid() != 0:
            blocked = editions[1].parent
            blocked.chmod(0)
            state, _, _ = scan(control, root_id)
            assert state["library"]["scan"]["phase"] == "failed"
            preserved, _ = tracks(control)
            assert len(preserved) == expected_tracks and str(editions[1]) in preserved
            blocked.chmod(0o755)
            blocked = None
            record("incomplete-directory-traversal-never-prunes")
        scan(control, root_id)
        unicode_file.write_bytes(b"corrupted replacement")
        state, _, _ = scan(control, root_id)
        damaged, _ = tracks(control)
        assert damaged[str(unicode_file)]["id"] == original_ids[str(unicode_file)]
        assert damaged[str(unicode_file)]["metadata"]["title"] == "Révisé 一" and damaged[str(unicode_file)]["error"]
        encode(unicode_file, unicode_tags)
        scan(control, root_id)
        repaired, _ = tracks(control)
        assert repaired[str(unicode_file)]["error"] is None
        record("failed-probe-retains-good-metadata-and-retries-on-rescan")

        stop()
        unicode_tags["title"] = "Changed while stopped"
        encode(unicode_file, unicode_tags)
        control = start()
        before_scan, _ = tracks(control)
        assert before_scan[str(unicode_file)]["metadata"]["title"] == "Révisé 一"
        assert before_scan[str(unicode_file)]["id"] == original_ids[str(unicode_file)]
        assert control.state()["library"]["scan"]["phase"] == "complete"
        record("restart-browses-durable-index-without-rescan")
        scan(control, root_id)
        updated, _ = tracks(control)
        assert updated[str(unicode_file)]["metadata"]["title"] == "Changed while stopped"

        # Force genuine metadata work for every hard-linked file, then kill while
        # indexing, before completion can reconcile the deliberately missing file.
        stamp = bulk_source.stat()
        os.utime(bulk_source, ns=(stamp.st_atime_ns, stamp.st_mtime_ns + 2_000_000_000))
        editions[1].unlink()
        control.request("rescan", root_id=root_id)
        deadline = time.monotonic() + 20
        while True:
            status = control.state()["library"]["scan"]
            if status["phase"] == "indexing" and status["processed"] < status["total"] // 2:
                break
            assert time.monotonic() < deadline and status["phase"] not in FINISHED, status
            time.sleep(0.001)
        stop(crash=True)
        control = start()
        interrupted = control.state()["library"]["scan"]
        assert interrupted["phase"] == "interrupted", interrupted
        retained, _ = tracks(control)
        assert str(editions[1]) in retained and retained[str(editions[1])]["id"] == original_ids[str(editions[1])]
        scan(control, root_id)
        completed, revision = tracks(control)
        assert str(editions[1]) not in completed and len(completed) == expected_tracks - 1
        record("crash-interrupted-scan-retains-files-until-complete-rescan", interrupted=interrupted)

        current = control.state()
        queued = control.request("append_track", expected_revision=current["queue_revision"], library_revision=revision, track_id=completed[str(untagged)]["id"])
        control.request("play", entry_id=queued["entry_id"])
        heard = control.wait(lambda state: state["transport"] == "playing" and state["current_id"] == queued["entry_id"])
        assert heard["underruns"] == 0
        ended = control.wait(lambda state: state["transport"] == "ended")
        assert ended["underruns"] == 0
        record("indexed-track-plays-through-real-output", snapshot=ended)
        stop()
        report["passed"] = True
    except Exception as error:
        report["passed"] = False
        report["failure"] = repr(error)
        raise
    finally:
        if blocked is not None:
            blocked.chmod(0o755)
        if offline is not None and offline.exists():
            offline.rename(root)
        unreadable.chmod(0o644)
        for connection in connections:
            connection.close()
        if process is not None:
            process.kill()
            process.wait(timeout=15)
        for log in logs:
            log.close()
        (run / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(dict(report=str(run / "report.json"), passed=report.get("passed", False), cases=len(report["cases"]))))


if __name__ == "__main__":
    main()
