#!/usr/bin/env python3
"""Drive the displayed SwiftUI controller against a real isolated Rust server.

The debug app uses an external native Accessibility driver. No synthetic
server, Apple Events, or controller-owned playback. Only the stats scenario writes
to the database: it seeds a year of plays, as render_views.py does, because
fifty plays of the two-minute fixture would take too long to play.
"""
import argparse
import json
import os
from pathlib import Path
import random
import select
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from server_gate import WebSocket, fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", default="target/Tyranny.app/Contents/MacOS/Tyranny")
    parser.add_argument("--server", default="target/debug/tyranny-server")
    parser.add_argument("--coreaudio", action="store_true")
    parser.add_argument("--work-dir", default="target/client-gate")
    parser.add_argument("--model-only", action="store_true",
                        help="Run the app's headless model gate (NativeModelGate, no Accessibility driver) without claiming visual verification")
    parser.add_argument("--commands-only", action="store_true",
                        help="Exercise the command-language dispatcher against a real server (no UI input or visual verification)")
    parser.add_argument("--scenario", choices=["core", "shell", "songs", "grouping", "albums", "artists", "history", "upnext", "runs", "stats"], default="core",
                        help="core: folder import, a double-click run, the seek slider, filter, rescan, disconnect and a relaunch (default); shell: the library window shell; "
                             "songs: the continuous Songs table and column browser; "
                             "grouping: Artist › Album blocks, block selection and a rescan that adds an album; "
                             "albums: the album grid, its inline panel, a double-click run and Add to Up Next on two selected tracks; "
                             "artists: Artists with Compilations and Appears On, and Genres; "
                             "history: Recently Added, a played track in History and its Plays count; "
                             "upnext: Up Next sections and Added From labels, Clear, Save as Playlist and the drawer; "
                             "runs: double-click runs under the Up Next policies, Don't ask again, and Select All then Add to Playlist; "
                         "stats: the Stats page at ⌘8 on a server with no history, then Wrapped with a few plays and with a full year")
    parser.add_argument("--screenshots", default="spike-results/library-redesign-ui",
                        help="where a passing shell scenario copies its screenshots")
    args = parser.parse_args()
    if args.model_only and args.commands_only:
        parser.error("--model-only and --commands-only are mutually exclusive")
    if args.app.endswith(".app") or args.app.endswith(".app/"):
        args.app = str(Path(args.app) / "Contents" / "MacOS" / "Tyranny")
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=work))
    root = run / "Music – 日本語"
    root.mkdir()
    source = run / "source.wav"
    fixture(source)
    for index in range(3 if args.commands_only else 105):
        os.link(source, root / f"Track {index:03}.wav")
    state_dir = run / "state"
    server_command = [str(Path(args.server).resolve()), "--state-dir", str(state_dir), "--bind", "127.0.0.1:0"]
    if not args.coreaudio:
        server_command += ["--capture", str(run / "output.f32")]
    report = {"run_directory": str(run), "output": "coreaudio" if args.coreaudio else "clocked_pcm", "cases": [], "server_command": server_command,
              "limitations": ["Native Accessibility automation requires an unlocked desktop and Accessibility/Screen Recording permissions for the launching terminal", "Generated real audio at 105 initial paths; UI rescan adds 1024 hard-linked paths, not production-scale UI certification", "No human listening, LaunchAgent, sandbox or relocatable packaging claim"]}
    server = None
    controller = None
    try:
        with (run / "server.stderr").open("w") as log:
            server = subprocess.Popen(server_command, stdout=subprocess.PIPE, stderr=log, text=True)
            assert select.select([server.stdout], [], [], 15)[0], "server startup timed out"
            ready = json.loads(server.stdout.readline())
            report["ready"] = ready
            port = int(ready["address"].rsplit(":", 1)[1])
            token = Path(ready["token_path"]).read_text().strip()
            controller = WebSocket(ready["address"], token)
            app = str(Path(args.app).resolve())
            if args.commands_only:
                command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
                           "--command-smoke-output", str(run / "commands.json"),
                           "--command-smoke-folder", str(root)]
                with (run / "commands.stdout").open("w") as stdout, (run / "commands.stderr").open("w") as stderr:
                    result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=150)
                report["commands"] = json.loads((run / "commands.json").read_text())
                report["visual_verification"] = False
                report["limitations"] = ["Real command dispatcher and server; no UI input, screenshot, acoustic output, or managed-server verification"]
                assert result.returncode == 0 and report["commands"]["passed"], report["commands"]
                report["passed"] = True
                return
            if args.model_only:
                command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
                           "--model-gate-output", str(run / "native-model.json"), "--model-gate-folder", str(root),
                           "--model-gate-server-pid", str(server.pid)]
                with (run / "model-gate.stdout").open("w") as stdout, (run / "model-gate.stderr").open("w") as stderr:
                    result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=150)
                report["native_model"] = json.loads((run / "native-model.json").read_text())
                report["visual_verification"] = False
                assert result.returncode == 0 and report["native_model"]["passed"], report["native_model"]
                report["passed"] = True
                return
            if args.scenario == "shell":
                shell(report, run, app, port, ready, root, controller, Path(args.screenshots))
                report["passed"] = True
                return
            if args.scenario == "songs":
                songs(report, run, app, port, ready, source, controller, Path(args.screenshots))
                report["passed"] = True
                return
            if args.scenario == "grouping":
                grouping(report, run, app, port, ready, source, controller, Path(args.screenshots))
                report["passed"] = True
                return
            if args.scenario == "albums":
                albums(report, run, app, port, ready, source, controller, Path(args.screenshots))
                report["passed"] = True
                return
            if args.scenario == "artists":
                artists(report, run, app, port, ready, source, controller, Path(args.screenshots))
                report["passed"] = True
                return
            if args.scenario == "history":
                history(report, run, app, port, ready, source, controller, Path(args.screenshots))
                report["passed"] = True
                return
            if args.scenario == "upnext":
                upnext(report, run, app, port, ready, source, controller, Path(args.screenshots))
                report["passed"] = True
                return
            if args.scenario == "runs":
                runs(report, run, app, port, ready, source, controller, Path(args.screenshots))
                report["passed"] = True
                return
            if args.scenario == "stats":
                stats(report, run, app, port, ready, root, controller, Path(args.screenshots))
                report["passed"] = True
                return
            paused_at = None
            for mode in ["initial", "relaunch"]:
                output = run / mode
                command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"], "--ui-smoke-output", str(output)]
                command += ["--ui-driver", str(Path("target/native-ui-driver").resolve())]
                command += ["--ui-smoke-folder", str(root)] if mode == "initial" else ["--ui-smoke-relaunch"]
                with (run / f"{mode}.stdout").open("w") as stdout, (run / f"{mode}.stderr").open("w") as stderr:
                    result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=110)
                assert result.returncode == 0, f"native app exited {result.returncode}"
                native = json.loads((output / "report.json").read_text())
                report[mode] = native
                assert native["passed"], native.get("error")
                state = controller.state()
                # The double-click played the Songs run: all 105 tracks, queued before the rescan added files.
                # Quit paused it. The relaunch resumed it, and its own quit paused it again.
                assert state["transport"] == "paused" and state["queue_length"] == 105, state
                if paused_at is not None:
                    assert (state["current_id"] != paused_at["current_id"]
                            or state["position_frames"] > paused_at["position_frames"]), (paused_at, state)
                time.sleep(0.3)
                state = controller.state()
                assert state["transport"] == "paused" and state["underruns"] == 0, state
                paused_at = state
                report["cases"].append({"name": f"{mode}-client-quit-pauses-server-playback", "snapshot": state})
            report["passed"] = True
    except Exception as error:
        report["passed"] = False
        report["failure"] = repr(error)
        raise
    finally:
        if controller is not None:
            controller.close()
        if server is not None and server.poll() is None:
            server.send_signal(signal.SIGCONT)
            server.terminate()
            server.wait(timeout=15)
        (run / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({"report": str(run / "report.json"), "passed": report.get("passed", False)}))


def shell(report, run, app, port, ready, root, controller, screenshots):
    """Drive the library window shell: source list, player bar transport, status bar, Settings."""
    report["scenario"] = "shell"
    output = run / "shell"
    command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
               "--shell-smoke-output", str(output), "--ui-smoke-folder", str(root),
               "--ui-driver", str(Path("target/native-ui-driver").resolve())]
    report["app_command"] = command
    with (run / "shell.stdout").open("w") as stdout, (run / "shell.stderr").open("w") as stderr:
        result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=240)
    assert result.returncode == 0, f"native app exited {result.returncode}"
    native = json.loads((output / "report.json").read_text())
    report["shell"] = native
    assert native["passed"], native.get("error")
    state = controller.state()
    settings = state["settings"]
    # The server, not the app, must hold the modes the player bar set.
    assert settings["volume"] == 45 and settings["repeat"] == "off" and settings["shuffle"] is False, settings
    # The smoke left playback running; its quit paused it.
    assert state["library"]["tracks"] == 1129 and state["transport"] == "paused", state
    report["cases"].append({"name": "shell-server-holds-player-bar-settings", "settings": settings})
    screenshots.mkdir(parents=True, exist_ok=True)
    for image in sorted(output.glob("shell*.png")):
        shutil.copy2(image, screenshots / image.name)
    report["screenshots"] = [str(screenshots / image.name) for image in sorted(output.glob("shell*.png"))]


SONG_GENRES = ["Ambient", "Folk", "Jazz"]


def tagged_flac(path, title, artist, album, genre, year, track):
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin",
                    "-f", "lavfi", "-i", "anullsrc=r=44100:cl=stereo", "-t", "1", "-c:a", "flac",
                    "-metadata", f"title={title}", "-metadata", f"artist={artist}",
                    "-metadata", f"album_artist={artist}", "-metadata", f"album={album}",
                    "-metadata", f"genre={genre}", "-metadata", f"date={year}",
                    "-metadata", f"track={track}", str(path)], check=True, timeout=30)


def songs_fixture(music, source):
    """1,129 tracks: 105 tagged FLACs (3 genres x 5 albums x 7 tracks, one year per
    album) and 1,024 untagged hard links of one WAV."""
    tagged = music / "Tagged"
    tagged.mkdir(parents=True)
    for genre_index, genre in enumerate(SONG_GENRES):
        for album in range(5):
            year = 1960 + genre_index * 10 + album
            for track in range(1, 8):
                tagged_flac(tagged / f"{genre} {album} {track:02}.flac", f"{genre} {album} Song {track}",
                            f"{genre} Artist {album % 3}", f"{genre} Album {album}", genre, year, track)
    untagged = music / "Untagged"
    untagged.mkdir()
    for index in range(1024):
        os.link(source, untagged / f"Untagged {index:04}.wav")


def songs(report, run, app, port, ready, source, controller, screenshots):
    """Drive the Songs table: scroll to the end, sort, narrow by genre, drag to Up Next."""
    report["scenario"] = "songs"
    music = run / "Songs fixture"
    songs_fixture(music, source)
    controller.request("add_root", path=str(music))
    controller.wait(lambda state: state["library"]["scan"]["phase"] == "complete"
                    and state["library"]["tracks"] == 1129, seconds=180)
    output = run / "songs"
    command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
               "--songs-smoke-output", str(output),
               "--ui-driver", str(Path("target/native-ui-driver").resolve())]
    report["app_command"] = command
    with (run / "songs.stdout").open("w") as stdout, (run / "songs.stderr").open("w") as stderr:
        result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=240)
    assert result.returncode == 0, f"native app exited {result.returncode}"
    native = json.loads((output / "report.json").read_text())
    report["songs"] = native
    assert native["passed"], native.get("error")
    # The server, not the app, holds the dragged entries.
    page = controller.request("queue_page", revision=None, offset=0, limit=500)
    sources = [entry["source"] for entry in page["entries"]]
    assert page["total"] == 5 and all(source == {"kind": "drag", "name": "Songs"} for source in sources), page
    assert [entry["track_id"] for entry in page["entries"]] == native["observations"]["dragged_track_ids"], page
    report["cases"].append({"name": "songs-server-holds-dragged-entries", "total": page["total"]})
    screenshots.mkdir(parents=True, exist_ok=True)
    for image in sorted(output.glob("songs*.png")):
        shutil.copy2(image, screenshots / image.name)
    report["screenshots"] = [str(screenshots / image.name) for image in sorted(output.glob("songs*.png"))]


def grouping(report, run, app, port, ready, source, controller, screenshots):
    """Drive grouping: Artist › Album against the server's spans, a block selection, and a
    rescan that adds an album (staged outside the root; the app moves it in) under the anchor."""
    report["scenario"] = "grouping"
    music = run / "Songs fixture"
    songs_fixture(music, source)
    staged = run / "Staged"
    extra = staged / "Extra"
    extra.mkdir(parents=True)
    # "Aaa" sorts before every tagged album artist, so the new album lands before the anchor.
    for track in range(1, 8):
        tagged_flac(extra / f"Extra {track:02}.flac", f"Extra Song {track}", "Aaa Extra Artist",
                    "Aaa Extra Album", "Jazz", 1999, track)
    controller.request("add_root", path=str(music))
    controller.wait(lambda state: state["library"]["scan"]["phase"] == "complete"
                    and state["library"]["tracks"] == 1129, seconds=180)
    output = run / "grouping"
    command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
               "--songs-smoke-output", str(output), "--songs-smoke-scenario", "grouping",
               "--songs-smoke-extra", str(extra), "--songs-smoke-root", str(music),
               "--ui-driver", str(Path("target/native-ui-driver").resolve())]
    report["app_command"] = command
    with (run / "grouping.stdout").open("w") as stdout, (run / "grouping.stderr").open("w") as stderr:
        result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=300)
    assert result.returncode == 0, f"native app exited {result.returncode}"
    native = json.loads((output / "report.json").read_text())
    report["grouping"] = native
    assert native["passed"], native.get("error")
    # The server, not the app, holds the added album.
    state = controller.state()
    assert state["library"]["tracks"] == 1136 and state["library"]["scan"]["phase"] == "complete", state
    report["cases"].append({"name": "grouping-server-indexed-the-added-album", "tracks": state["library"]["tracks"]})
    screenshots.mkdir(parents=True, exist_ok=True)
    for image in sorted(output.glob("grouped*.png")):
        shutil.copy2(image, screenshots / image.name)
    report["screenshots"] = [str(screenshots / image.name) for image in sorted(output.glob("grouped*.png"))]


def albums(report, run, app, port, ready, source, controller, screenshots):
    """Drive the album grid: scroll to the end, open the last album, check its tracks against
    `album_summaries`, double-click its first track, select its first two tracks and press
    Add to Up Next, and close the panel with Escape."""
    report["scenario"] = "albums"
    music = run / "Songs fixture"
    songs_fixture(music, source)
    controller.request("add_root", path=str(music))
    controller.wait(lambda state: state["library"]["scan"]["phase"] == "complete"
                    and state["library"]["tracks"] == 1129, seconds=180)
    output = run / "albums"
    command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
               "--albums-smoke-output", str(output),
               "--ui-driver", str(Path("target/native-ui-driver").resolve())]
    report["app_command"] = command
    with (run / "albums.stdout").open("w") as stdout, (run / "albums.stderr").open("w") as stderr:
        result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=240)
    assert result.returncode == 0, f"native app exited {result.returncode}"
    native = json.loads((output / "report.json").read_text())
    report["albums"] = native
    assert native["passed"], native.get("error")
    # The server, not the app, holds the album run, then the two added tracks as a run of their own.
    expanded = native["observations"]["expanded"]
    added = native["observations"]["added"]
    summary = controller.request("album_summaries", album_ids=[expanded["id"]], revision=None)["entries"][0]
    count = summary["track_count"]
    assert count == expanded["tracks"], (summary, expanded)
    album_source = {"kind": "album", "name": summary["title"]}
    head = controller.request("queue_page", revision=None, offset=0, limit=500)
    assert head["total"] == count + 2 == added["total"], (head["total"], count, added)
    run = head["entries"][0]["run_id"]
    assert run == head["entries"][0]["id"], head["entries"][0]
    album_entries = head["entries"][:count]
    assert all(entry["source"] == album_source and entry["run_id"] == run for entry in album_entries), head
    # Add to Up Next is its own run: its run ID is the ID of its first entry.
    tail = controller.request("queue_page", revision=None, offset=count, limit=2)
    assert [entry["track_id"] for entry in tail["entries"]] == added["track_ids"], (tail, added)
    assert all(entry["source"] == album_source and entry["run_id"] == tail["entries"][0]["id"]
               for entry in tail["entries"]), tail
    assert tail["entries"][0]["run_id"] != run, tail
    # The album run was playing; the app's quit paused it.
    assert controller.state()["transport"] == "paused"
    report["cases"].append({"name": "albums-server-holds-album-run-and-added-tracks", "total": head["total"]})
    screenshots.mkdir(parents=True, exist_ok=True)
    for image in sorted(output.glob("albums*.png")):
        shutil.copy2(image, screenshots / image.name)
    report["screenshots"] = [str(screenshots / image.name) for image in sorted(output.glob("albums*.png"))]


def compilation_track(path, title, artist, album, track):
    """A compilation track: album artist "Various Artists", the compilation flag set."""
    subprocess.run(["ffmpeg", "-hide_banner", "-loglevel", "error", "-nostdin",
                    "-f", "lavfi", "-i", "anullsrc=r=44100:cl=stereo", "-t", "1", "-c:a", "flac",
                    "-metadata", f"title={title}", "-metadata", f"artist={artist}",
                    "-metadata", "album_artist=Various Artists", "-metadata", f"album={album}",
                    "-metadata", "genre=Folk", "-metadata", "date=2001", "-metadata", "compilation=1",
                    "-metadata", f"track={track}", str(path)], check=True, timeout=30)


def artists(report, run, app, port, ready, source, controller, screenshots):
    """Drive Artists and Genres: the Compilations row lists the compilation album, a track
    artist on it shows the tracks under Appears On, and a genre narrows to its albums.
    The fixture is the songs library plus one four-track compilation album."""
    report["scenario"] = "artists"
    music = run / "Songs fixture"
    songs_fixture(music, source)
    compilation = music / "Compilation"
    compilation.mkdir()
    # Two tracks by a Folk album artist (Appears On) and two by an artist with no albums of their own.
    for track, artist in enumerate(["Folk Artist 1", "Comp Only Artist", "Folk Artist 1", "Comp Only Artist"], start=1):
        compilation_track(compilation / f"Compilation {track:02}.flac", f"Compilation Song {track}", artist,
                          "Smoke Compilation", track)
    controller.request("add_root", path=str(music))
    controller.wait(lambda state: state["library"]["scan"]["phase"] == "complete"
                    and state["library"]["tracks"] == 1133, seconds=180)
    output = run / "artists"
    command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
               "--artists-smoke-output", str(output),
               "--ui-driver", str(Path("target/native-ui-driver").resolve())]
    report["app_command"] = command
    with (run / "artists.stdout").open("w") as stdout, (run / "artists.stderr").open("w") as stderr:
        result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=240)
    assert result.returncode == 0, f"native app exited {result.returncode}"
    native = json.loads((output / "report.json").read_text())
    report["artists"] = native
    assert native["passed"], native.get("error")
    # The server, not the app, holds the expected lists.
    artists_reply = controller.request("list_artists", query="", list=None, revision=None, offset=0, limit=500)
    comp = artists_reply["compilations"]
    assert comp["album_count"] == 1 and comp["track_count"] == 4, comp
    assert "Various Artists" not in [entry["name"] for entry in artists_reply["entries"]], artists_reply
    folk = next(entry for entry in artists_reply["entries"] if entry["name"] == "Folk Artist 1")
    appears = controller.request(
        "list_tracks", list=dict(source=dict(kind="appears_on", artist_id=folk["id"]),
                                 browser=dict(genre_ids=[], artist_ids=[], compilations=False, album_ids=[]),
                                 query="", sort=[], group="none"),
        revision=None, offset=0, limit=500)
    assert appears["total"] == 2, appears
    report["cases"].append({"name": "artists-server-compilations-and-album-artists", "compilations": comp})
    screenshots.mkdir(parents=True, exist_ok=True)
    for image in sorted(output.glob("*.png")):
        shutil.copy2(image, screenshots / ("artists-" + image.name if not image.name.startswith("artists") else image.name))
    report["screenshots"] = [str(screenshots / image.name) for image in sorted(output.glob("*.png"))]


def history(report, run, app, port, ready, source, controller, screenshots):
    """Drive Recently Added and History: the new library lists newest first, one short track
    plays to its end on the clocked capture output, its History row appears, a double-click
    plays it again from Songs named History, and Songs shows Plays 1."""
    report["scenario"] = "history"
    music = run / "Songs fixture"
    songs_fixture(music, source)
    controller.request("add_root", path=str(music))
    controller.wait(lambda state: state["library"]["scan"]["phase"] == "complete"
                    and state["library"]["tracks"] == 1129, seconds=180)
    output = run / "history"
    command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
               "--history-smoke-output", str(output),
               "--ui-driver", str(Path("target/native-ui-driver").resolve())]
    report["app_command"] = command
    with (run / "history.stdout").open("w") as stdout, (run / "history.stderr").open("w") as stderr:
        result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=240)
    assert result.returncode == 0, f"native app exited {result.returncode}"
    native = json.loads((output / "report.json").read_text())
    report["history"] = native
    assert native["passed"], native.get("error")
    # The server, not the app, holds the play and the appended entry.
    played = controller.request("history", revision=None, offset=0, limit=10)
    assert played["total"] == 1 and played["entries"][0]["title"] == "Ambient 0 Song 1", played
    entry = native["observations"]["history_entry"]
    assert played["entries"][0]["track_id"] == entry["track_id"], (played, entry)
    page = controller.request("queue_page", revision=None, offset=0, limit=500)
    appended = page["entries"][-1]
    assert appended["track_id"] == entry["track_id"] and appended["source"] == {"kind": "songs", "name": "History"}, page
    report["cases"].append({"name": "history-server-holds-play-and-appended-entry", "history_total": played["total"]})
    screenshots.mkdir(parents=True, exist_ok=True)
    for image in sorted(output.glob("history*.png")):
        shutil.copy2(image, screenshots / image.name)
    report["screenshots"] = [str(screenshots / image.name) for image in sorted(output.glob("history*.png"))]


def upnext(report, run, app, port, ready, source, controller, screenshots):
    """Drive Up Next: two album runs and a playlist-sourced run show their Added From labels
    behind the playing track, Clear keeps that track playing with nothing upcoming, Save as
    Playlist creates a playlist, and the drawer lists the queue and takes a drop."""
    report["scenario"] = "upnext"
    music = run / "Songs fixture"
    songs_fixture(music, source)
    controller.request("add_root", path=str(music))
    controller.wait(lambda state: state["library"]["scan"]["phase"] == "complete"
                    and state["library"]["tracks"] == 1129, seconds=180)
    output = run / "upnext"
    command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
               "--upnext-smoke-output", str(output),
               "--ui-driver", str(Path("target/native-ui-driver").resolve())]
    report["app_command"] = command
    with (run / "upnext.stdout").open("w") as stdout, (run / "upnext.stderr").open("w") as stderr:
        result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=240)
    assert result.returncode == 0, f"native app exited {result.returncode}"
    native = json.loads((output / "report.json").read_text())
    report["upnext"] = native
    assert native["passed"], native.get("error")
    # The server, not the app, holds the saved playlist and the dropped entries.
    saved = controller.request("saved_playlists", revision=None, offset=0, limit=50, query="", kind=None)
    playlist = next((entry for entry in saved["entries"] if entry["name"] == "Saved From Up Next"), None)
    assert playlist is not None and playlist["kind"] == "regular" and playlist["entry_count"] == 2, saved
    assert playlist["id"] == native["observations"]["saved_playlist"]["id"], (playlist, native["observations"])
    page = controller.request("queue_page", revision=None, offset=0, limit=500)
    assert page["total"] == 6, page
    dropped = page["entries"][-3:]
    assert [entry["track_id"] for entry in dropped] == native["observations"]["dropped_track_ids"], page
    assert all(entry["source"] == {"kind": "drag", "name": "Songs"} for entry in dropped), dropped
    assert page["entries"][0]["id"] == native["observations"]["current_id"], page
    report["cases"].append({"name": "upnext-server-holds-playlist-current-and-drawer-drop",
                            "playlist": playlist["id"], "queue_total": page["total"]})
    screenshots.mkdir(parents=True, exist_ok=True)
    for image in sorted(output.glob("*.png")):
        shutil.copy2(image, screenshots / ("upnext-" + image.name if not image.name.startswith("upnext") else image.name))
    report["screenshots"] = [str(screenshots / image.name) for image in sorted(output.glob("*.png"))]


def runs(report, run, app, port, ready, source, controller, screenshots):
    """Drive runs from the Songs table: a double-click with Up Next empty plays the run from that
    row; with entries upcoming it asks (Keep), or follows the Replace and Keep policies; Don't
    ask again stores the policy; Select All then Add to Playlist adds every row in one playlist
    revision. The native driver reads the queue back in order; this checks the server's copy."""
    report["scenario"] = "runs"
    music = run / "Songs fixture"
    songs_fixture(music, source)
    controller.request("add_root", path=str(music))
    controller.wait(lambda state: state["library"]["scan"]["phase"] == "complete"
                    and state["library"]["tracks"] == 1129, seconds=180)
    output = run / "runs"
    command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
               "--runs-smoke-output", str(output),
               "--ui-driver", str(Path("target/native-ui-driver").resolve())]
    report["app_command"] = command
    with (run / "runs.stdout").open("w") as stdout, (run / "runs.stderr").open("w") as stderr:
        result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=300)
    assert result.returncode == 0, f"native app exited {result.returncode}"
    native = json.loads((output / "report.json").read_text())
    report["runs"] = native
    assert native["passed"], native.get("error")
    # The server, not the app, holds the playlist and the empty queue the scenario ended with.
    saved = controller.request("saved_playlists", revision=None, offset=0, limit=50, query="", kind=None)
    playlist = next((entry for entry in saved["entries"] if entry["name"] == "Runs Playlist"), None)
    assert playlist is not None and playlist["entry_count"] == 1129, saved
    observed = native["observations"]["playlist"]
    assert playlist["revision"] == observed["revision"] == observed["start_revision"] + 1, (playlist, observed)
    page = controller.request("queue_page", revision=None, offset=0, limit=500)
    assert page["total"] == 0, page
    report["cases"].append({"name": "runs-server-holds-full-select-all-add-in-one-revision",
                            "playlist": playlist["id"], "revision": playlist["revision"]})
    screenshots.mkdir(parents=True, exist_ok=True)
    for image in sorted(output.glob("*.png")):
        shutil.copy2(image, screenshots / ("runs-" + image.name if not image.name.startswith("runs") else image.name))
    report["screenshots"] = [str(screenshots / image.name) for image in sorted(output.glob("*.png"))]


def stats(report, run, app, port, ready, root, controller, screenshots):
    """Drive the Stats page and Wrapped, one app launch per history:
    - empty: ⌘8 opens the page, the header shows this week's label, ‹ and › step the period,
      every panel shows its empty state, and Go › Wrapped is disabled;
    - few (12 plays this year): Go › Wrapped opens the "not enough" slide, and esc closes it;
    - full (a seeded year): the slideshow advances, the keys move and pause it, and esc closes it."""
    report["scenario"] = "stats"
    controller.request("add_root", path=str(root))
    controller.wait(lambda state: state["library"]["scan"]["phase"] == "complete"
                    and state["library"]["tracks"] == 105, seconds=60)
    database = run / "state" / "state.sqlite3"
    year = time.localtime().tm_year
    rng = random.Random(12)
    screenshots.mkdir(parents=True, exist_ok=True)
    report["screenshots"] = []
    for phase in ["empty", "few", "full"]:
        if phase == "few":
            seed_year(database, rng, plays=12, skips=0, radio=0)
        elif phase == "full":
            seed_year(database, rng, plays=240, skips=10, radio=4)
        output = run / f"stats-{phase}"
        command = [app, "--url", f"ws://127.0.0.1:{port}/ws", "--token-file", ready["token_path"],
                   "--stats-smoke-output", str(output), "--stats-smoke-phase", phase,
                   "--ui-driver", str(Path("target/native-ui-driver").resolve())]
        report[f"app_command_{phase}"] = command
        with (run / f"stats-{phase}.stdout").open("w") as stdout, (run / f"stats-{phase}.stderr").open("w") as stderr:
            result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=120)
        assert result.returncode == 0, f"native app exited {result.returncode} in {phase}"
        native = json.loads((output / "report.json").read_text())
        report[f"stats_{phase}"] = native
        assert native["passed"], (phase, native.get("error"))
        # The server, not the app, holds the history the page showed.
        years = controller.request("stats_years")["years"]
        plays = controller.request("wrapped", year=year)["report"]["plays"]
        if phase == "empty":
            assert years == [], years
        else:
            assert years == [year] and (plays < 50 if phase == "few" else plays >= 50), (phase, years, plays)
        report["cases"].append({"name": f"stats-server-holds-{phase}-history", "years": years, "plays": plays})
        for image in sorted(output.glob("stats*.png")):
            shutil.copy2(image, screenshots / image.name)
            report["screenshots"].append(str(screenshots / image.name))


def seed_year(database, rng, plays, skips, radio):
    """Writes plays this year straight into play_history: `plays` counted plays over six artists,
    `skips` skips of one track, and `radio` 30-minute plays of one station. They fall between
    1 January and an hour ago."""
    now = int(time.time())
    start = int(time.mktime((time.localtime().tm_year, 1, 1, 0, 0, 0, 0, 0, -1)))
    end = max(start + 1, now - 3600)
    artists = {"Harbour Lights": 9, "Mara Solis": 7, "The Quiet Engines": 5, "Low Orbit": 4,
               "Fennel & Rye": 3, "Cass Vey": 2}
    titles = ["Tidewater", "Paper Moons", "Northbound", "Glasshouse", "Slow Signal", "Afterglow"]
    rows = []
    for _ in range(plays):
        artist = rng.choices(list(artists), weights=list(artists.values()))[0]
        title = rng.choice(titles[:4] if artist == "Harbour Lights" else titles)
        duration = rng.randint(180, 330) * 1000
        rows.append((rng.randrange(start, end), None, f"/fixture/{artist}/{title}", title, artist, f"{artist} LP",
                     duration, "album", f"{artist} LP", duration, "finished", 1))
    for _ in range(skips):
        rows.append((rng.randrange(start, end), None, "/fixture/Low Orbit/Static", "Static", "Low Orbit",
                     "Low Orbit LP", 240_000, "album", "Low Orbit LP", 9_000, "skipped", 0))
    for _ in range(radio):
        rows.append((rng.randrange(start, end), None, "https://example.invalid/Night Owl Radio", "Night Owl",
                     "", "", None, "stream", "Night Owl Radio", 1_800_000, "stopped", 1))
    with sqlite3.connect(database) as connection:
        connection.executemany(
            "INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,duration_ms,"
            "source_kind,source_name,listened_ms,ended_at,end_reason,counted) "
            "VALUES(?1,0,?2,?3,?4,?5,?6,?7,?8,?9,?10,?1+?10/1000,?11,?12)", sorted(rows))
        connection.execute("UPDATE history_control SET writes=writes+1")

if __name__ == "__main__":
    main()
