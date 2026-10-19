#!/usr/bin/env python3
"""Drive the displayed SwiftUI controller against a real isolated Rust server.

The debug app uses an external native Accessibility driver. No synthetic
server, direct database mutation, Apple Events, or controller-owned playback.
"""
import argparse
import json
import os
from pathlib import Path
import select
import shutil
import signal
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
    parser.add_argument("--model-only", action="store_true", help="Exercise native transport/model without claiming visual verification")
    parser.add_argument("--scenario", choices=["spike", "shell", "songs", "grouping", "albums", "artists", "history"], default="spike",
                        help="spike: the original three-pane smoke and relaunch (default); shell: the library window shell; "
                             "songs: the continuous Songs table and column browser; "
                             "grouping: Artist › Album blocks, block selection and a rescan that adds an album; "
                             "albums: the album grid, its inline panel and album Play; "
                             "artists: Artists with Compilations and Appears On, and Genres; "
                             "history: Recently Added, a played track in History and its Plays count")
    parser.add_argument("--screenshots", default="spike-results/library-redesign-ui",
                        help="where a passing shell scenario copies its screenshots")
    args = parser.parse_args()
    if args.app.endswith(".app") or args.app.endswith(".app/"):
        args.app = str(Path(args.app) / "Contents" / "MacOS" / "Tyranny")
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=work))
    root = run / "Music – 日本語"
    root.mkdir()
    source = run / "source.wav"
    fixture(source)
    for index in range(105):
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
            if args.model_only:
                command = [str(Path("target/native-model-gate").resolve()), "--url", f"ws://127.0.0.1:{port}/ws",
                           "--token-file", ready["token_path"], "--folder", str(root), "--server-pid", str(server.pid),
                           "--output", str(run / "native-model.json")]
                result = subprocess.run(command, capture_output=True, text=True, timeout=100)
                report["native_model"] = json.loads((run / "native-model.json").read_text())
                report["stdout"] = result.stdout
                report["stderr"] = result.stderr
                report["visual_verification"] = False
                assert result.returncode == 0 and report["native_model"]["passed"], report["native_model"]
                report["passed"] = True
                return
            app = str(Path(args.app).resolve())
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
                assert state["transport"] == "playing" and state["queue_length"] == 1, state
                before = state["position_frames"]
                time.sleep(0.3)
                state = controller.state()
                assert state["position_frames"] > before and state["underruns"] == 0, state
                report["cases"].append({"name": f"{mode}-client-exit-preserves-server-playback", "snapshot": state})
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
    """Drive the library window shell: source list, toolbar transport, status bar, Settings."""
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
    # The server, not the app, must hold the modes the toolbar set.
    assert settings["volume"] == 45 and settings["repeat"] == "off" and settings["shuffle"] is False, settings
    assert state["library"]["tracks"] == 1129 and state["transport"] == "playing", state
    report["cases"].append({"name": "shell-server-holds-toolbar-settings", "settings": settings})
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
    `album_summaries`, press Play, and close the panel with Escape."""
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
    # The server, not the app, holds the album run.
    expanded = native["observations"]["expanded"]
    summary = controller.request("album_summaries", album_ids=[expanded["id"]], revision=None)["entries"][0]
    page = controller.request("queue_page", revision=None, offset=0, limit=500)
    assert page["total"] == summary["track_count"] == expanded["tracks"], (page["total"], summary, expanded)
    assert all(entry["source"] == {"kind": "album", "name": summary["title"]} for entry in page["entries"]), page
    assert len({entry["run_id"] for entry in page["entries"]}) == 1, page
    assert controller.state()["transport"] == "playing"
    report["cases"].append({"name": "albums-server-holds-album-run", "total": page["total"]})
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
    appends it to Up Next, and Songs shows Plays 1."""
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


if __name__ == "__main__":
    main()
