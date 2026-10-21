#!/usr/bin/env python3
"""Prepare a fixture library on an isolated server and run the debug app's self-render
harness (--render-views). No Accessibility or Screen Recording permission is needed."""
import argparse
import json
from pathlib import Path
import random
import select
import shutil
import sqlite3
import subprocess
import sys
import time

sys.dont_write_bytecode = True
from server_gate import UnixSocket, fixture, run_directory
from client_gate import songs_fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", default="target/Tyranny.app/Contents/MacOS/Tyranny")
    parser.add_argument("--server", default="target/debug/tyranny-server")
    parser.add_argument("--work-dir", default="target/render-views")
    parser.add_argument("--out", default="spike-results/library-redesign-ui")
    args = parser.parse_args()
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = run_directory(work)
    source = run / "source.wav"
    fixture(source)
    music = run / "Songs fixture"
    songs_fixture(music, source)
    command = [str(Path(args.server).resolve()), "--state-dir", str(run / "state"),
               "--capture", str(run / "output.f32")]
    with (run / "server.stderr").open("w") as log:
        server = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=log, text=True)
        control = None
        try:
            assert select.select([server.stdout], [], [], 15)[0], "server startup timed out"
            ready = json.loads(server.stdout.readline())
            control = UnixSocket(ready["socket_path"])
            control.request("add_root", path=str(music))
            control.wait(lambda s: s["library"]["scan"]["phase"] == "complete" and s["library"]["tracks"] == 1129, seconds=180)
            prepare(control)
            seed_history(control, run / "state" / "state.sqlite3")
            # Outside the library root. The harness queues and plays it to show a playback error.
            undecodable = run / "Undecodable.wav"
            undecodable.write_bytes(b"not audio " * 64)
            out = run / "png"
            app = [str(Path(args.app).resolve()), "--socket", ready["socket_path"], "--render-views", str(out),
                   "--render-error-source", str(undecodable)]
            result = subprocess.run(app, timeout=120, capture_output=True, text=True)
            print(result.returncode, result.stderr[-3000:])
            print((out / "render.log").read_text())
            dest = Path(args.out)
            dest.mkdir(parents=True, exist_ok=True)
            for image in sorted(out.glob("*.png")):
                shutil.copy2(image, dest / image.name)
        finally:
            if control is not None:
                control.close()
            server.terminate()
            server.wait(timeout=15)
            server.stdout.close()


def prepare(control):
    listing = control.request("list_tracks", list=dict(source=dict(kind="library"),
        browser=dict(genre_ids=[], artist_ids=[], compilations=False, album_ids=[]), query="", sort=[], group="none"),
        revision=None, offset=0, limit=500)
    revision = listing["revision"]
    tagged = [e["track"]["id"] for e in listing["entries"] if not e["track"]["title"].startswith("Untagged")]
    untagged = [e["track"]["id"] for e in listing["entries"] if e["track"]["title"].startswith("Untagged")]
    assert len(tagged) >= 20 and untagged, (len(tagged), len(untagged))

    def enqueue(ids, placement, play, kind, name):
        state = control.state()
        control.request("enqueue_tracks",
            selection=dict(kind="ids", library_revision=state["library"]["revision"], track_ids=ids),
            expected_revision=state["queue_revision"], placement=placement, play=play,
            source=dict(kind=kind, name=name))
    enqueue(untagged[:1], "replace_upcoming", True, "songs", "Songs")
    control.request("pause")
    enqueue(tagged[:7], "append", False, "album", "Jazz Album 0")
    enqueue(tagged[10:13], "append", False, "drag", "Songs")
    # Play the second entry, so Up Next shows all three sections: HISTORY, NOW PLAYING, UP NEXT.
    queue = control.request("queue_page", revision=None, offset=0, limit=20)["entries"]
    control.request("play", entry_id=queue[1]["id"])
    control.request("pause")
    state = control.state()
    created = control.request("create_playlist", expected_revision=state["playlists_revision"], name="Road Mix")["playlist"]
    control.request("playlist_add", playlist_id=created["id"], expected_revision=created["revision"],
                    library_revision=control.state()["library"]["revision"], track_ids=tagged[14:26], before=None)


def seed_history(control, database):
    """Writes 150 days of plays straight into play_history, so the Stats renders have data.
    Playing that much through the server would take days. The plays favour evenings and
    weekends. Folk Artist 2 stops 60 days ago (a lapsed love); Jazz Artist 1 plays mostly two
    tracks (a shallow dive); Ambient Artist 2 is first heard this week (a new artist)."""
    listing = control.request("list_tracks", list=dict(source=dict(kind="library"),
        browser=dict(genre_ids=[], artist_ids=[], compilations=False, album_ids=[]), query="", sort=[], group="none"),
        revision=None, offset=0, limit=500)
    tracks = [e["track"] for e in listing["entries"] if not e["track"]["title"].startswith("Untagged")]
    by_artist = {}
    for track in tracks:
        by_artist.setdefault(track["artist"], []).append(track)
    weights = {"Jazz Artist 0": 9, "Folk Artist 0": 7, "Ambient Artist 0": 6, "Jazz Artist 1": 5,
               "Folk Artist 1": 4, "Ambient Artist 1": 3, "Jazz Artist 2": 2, "Folk Artist 2": 6, "Ambient Artist 2": 3}
    hours = [1, 1, 0, 0, 0, 0, 1, 4, 5, 3, 2, 2, 4, 3, 2, 2, 3, 5, 8, 10, 10, 8, 5, 3]
    stations = [("Radio Paradise", "Nils Frahm", "Says"), ("BBC Radio 3", "Arvo Pärt", "Spiegel im Spiegel"),
                ("FIP", "Ahmad Jamal", "Poinciana")]
    rng = random.Random(7)
    now = int(time.time())
    today = time.localtime(now)
    midnight = now - (today.tm_hour * 3600 + today.tm_min * 60 + today.tm_sec)
    rows = []
    for day in range(150, -1, -1):
        start = midnight - day * 86400
        weekday = time.localtime(start).tm_wday
        for _ in range(rng.randint(2, 7) + (3 if weekday >= 5 else 0)):
            played_at = start + rng.choices(range(24), weights=hours)[0] * 3600 + rng.randrange(3600)
            if played_at > now - 60:
                continue
            if rng.random() < 0.06:
                station, artist, title = rng.choice(stations)
                listened = rng.randint(4, 50) * 60_000
                rows.append((played_at, None, f"https://example.invalid/{station}", title, artist, "", None,
                             "stream", station, listened, "stopped", int(listened >= 60_000)))
                continue
            pool = [a for a in weights if not (a == "Folk Artist 2" and day < 60)
                    and not (a == "Ambient Artist 2" and day > weekday)]
            artist = rng.choices(pool, weights=[weights[a] for a in pool])[0]
            choices = by_artist[artist]
            track = rng.choice(choices[:2]) if artist == "Jazz Artist 1" and rng.random() < 0.8 else rng.choice(choices)
            duration = rng.randint(150, 420) * 1000
            skipped = rng.random() < (0.45 if track["title"].endswith("Song 7") else 0.08)
            listened = rng.randint(8, 40) * 1000 if skipped else duration
            rows.append((played_at, track["id"], f"/fixture/{track['id']}", track["title"], track["artist"],
                         track["album"], duration, "album", track["album"], listened,
                         "skipped" if skipped else "finished", int(not skipped)))
    with sqlite3.connect(database) as connection:
        connection.executemany(
            "INSERT INTO play_history(played_at,entry_id,track_id,path,title,artist,album,duration_ms,"
            "source_kind,source_name,listened_ms,ended_at,end_reason,counted) "
            "VALUES(?1,0,?2,?3,?4,?5,?6,?7,?8,?9,?10,?1+?10/1000,?11,?12)", sorted(rows))
        connection.execute("UPDATE history_control SET writes=writes+1")
    print(f"seeded {len(rows)} plays")


if __name__ == "__main__":
    main()
