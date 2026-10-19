#!/usr/bin/env python3
"""Prepare a fixture library on an isolated server and run the debug app's self-render
harness (--render-views). No Accessibility or Screen Recording permission is needed."""
import argparse
import json
from pathlib import Path
import select
import shutil
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
from server_gate import WebSocket, fixture
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
    run = Path(tempfile.mkdtemp(prefix="run-", dir=work))
    source = run / "source.wav"
    fixture(source)
    music = run / "Songs fixture"
    songs_fixture(music, source)
    command = [str(Path(args.server).resolve()), "--state-dir", str(run / "state"), "--bind", "127.0.0.1:0",
               "--capture", str(run / "output.f32")]
    with (run / "server.stderr").open("w") as log:
        server = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=log, text=True)
        try:
            assert select.select([server.stdout], [], [], 15)[0], "server startup timed out"
            ready = json.loads(server.stdout.readline())
            port = int(ready["address"].rsplit(":", 1)[1])
            control = WebSocket(ready["address"], Path(ready["token_path"]).read_text().strip())
            control.request("add_root", path=str(music))
            control.wait(lambda s: s["library"]["scan"]["phase"] == "complete" and s["library"]["tracks"] == 1129, seconds=180)
            prepare(control)
            out = run / "png"
            app = [str(Path(args.app).resolve()), "--url", f"ws://127.0.0.1:{port}/ws",
                   "--token-file", ready["token_path"], "--render-views", str(out)]
            result = subprocess.run(app, timeout=120, capture_output=True, text=True)
            print(result.returncode, result.stderr[-3000:])
            print((out / "render.log").read_text())
            dest = Path(args.out)
            dest.mkdir(parents=True, exist_ok=True)
            for image in sorted(out.glob("*.png")):
                shutil.copy2(image, dest / image.name)
        finally:
            server.terminate()


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
    state = control.state()
    created = control.request("create_playlist", expected_revision=state["playlists_revision"], name="Road Mix")["playlist"]
    control.request("playlist_add", playlist_id=created["id"], expected_revision=created["revision"],
                    library_revision=control.state()["library"]["revision"], track_ids=tagged[14:26], before=None)


if __name__ == "__main__":
    main()
