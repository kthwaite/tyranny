#!/usr/bin/env python3
"""Exercise real daemon processes and independent RFC 6455 clients; stdlib only.

Capture mode uses the production PCM consumer, paced in real time. --coreaudio
instead emits modest-amplitude audio. No mock owner, database, or playback path.
"""
import argparse
import array
import base64
import json
import math
import os
from pathlib import Path
import selectors
import socket
import sqlite3
import struct
import subprocess
import tempfile
import threading
import time
import wave


class WebSocket:
    def __init__(self, address, token=None, origin=None, receive_buffer=None, expected_version=17):
        host, port = address.rsplit(":", 1)
        self.socket = socket.create_connection((host, int(port)), timeout=8)
        if receive_buffer:
            self.socket.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, receive_buffer)
        self.buffer = bytearray()
        self.sequence = -1
        self.snapshot = None
        self.counter = 0
        key = base64.b64encode(os.urandom(16)).decode()
        headers = ["GET /ws HTTP/1.1", f"Host: {address}", "Upgrade: websocket", "Connection: Upgrade", f"Sec-WebSocket-Key: {key}", "Sec-WebSocket-Version: 13"]
        if token is not None:
            headers.append(f"Authorization: Bearer {token}")
        if origin is not None:
            headers.append(f"Origin: {origin}")
        self.socket.sendall(("\r\n".join(headers) + "\r\n\r\n").encode())
        while b"\r\n\r\n" not in self.buffer:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise ConnectionError("closed during handshake")
            self.buffer.extend(chunk)
            assert len(self.buffer) < 65536
        header, rest = bytes(self.buffer).split(b"\r\n\r\n", 1)
        self.buffer = bytearray(rest)
        self.status = int(header.split(b" ", 2)[1])
        if self.status != 101:
            self.hello = None
            self.close()
            return
        self.hello = self.receive()
        assert self.hello["type"] == "hello" and self.hello["protocol_version"] == expected_version

    def take(self, count):
        while len(self.buffer) < count:
            chunk = self.socket.recv(max(4096, count - len(self.buffer)))
            if not chunk:
                raise ConnectionError("socket closed")
            self.buffer.extend(chunk)
        result = bytes(self.buffer[:count])
        del self.buffer[:count]
        return result

    def send_frame(self, opcode, payload):
        mask = os.urandom(4)
        length = len(payload)
        prefix = bytes([0x80 | opcode])
        if length < 126:
            prefix += bytes([0x80 | length])
        elif length < 65536:
            prefix += bytes([0xFE]) + struct.pack("!H", length)
        else:
            prefix += bytes([0xFF]) + struct.pack("!Q", length)
        masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
        self.socket.sendall(prefix + mask + masked)

    def send(self, request):
        self.send_frame(1, json.dumps(request, separators=(",", ":")).encode())

    def receive(self, timeout=8):
        self.socket.settimeout(timeout)
        while True:
            first, second = self.take(2)
            assert first & 0x80 and not second & 0x80, "unexpected fragmented or masked server frame"
            length = second & 0x7F
            if length == 126:
                length = struct.unpack("!H", self.take(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self.take(8))[0]
            assert length <= 8 * 1024 * 1024
            payload = self.take(length)
            opcode = first & 0xF
            if opcode == 8:
                raise ConnectionError("WebSocket closed")
            if opcode == 9:
                self.send_frame(10, payload)
                continue
            if opcode == 10:
                continue
            assert opcode == 1
            message = json.loads(payload)
            snapshot = message.get("snapshot")
            if message["type"] == "reply" and message.get("status") == "ok":
                snapshot = message["result"].get("snapshot")
            if snapshot:
                sequence = snapshot["sequence"]
                assert sequence >= self.sequence, (sequence, self.sequence, message)
                if message["type"] == "state":
                    assert sequence > self.sequence, "superseded state event arrived after newer reply"
                self.sequence = sequence
                self.snapshot = snapshot
            return message

    def request(self, command, expect_error=None, **fields):
        self.counter += 1
        self.send(dict(id=self.counter, command=command, **fields))
        while True:
            message = self.receive()
            if message["type"] == "reply" and message["id"] == self.counter:
                if expect_error:
                    assert message["status"] == "error" and message["code"] == expect_error, message
                    return message
                assert message["status"] == "ok", message
                return message["result"]

    def state(self):
        return self.request("snapshot")["snapshot"]

    def wait(self, predicate, seconds=8):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if self.snapshot is not None and predicate(self.snapshot):
                return self.snapshot
            self.receive(max(0.05, deadline - time.monotonic()))
        raise AssertionError(f"state condition timed out: {self.snapshot}")

    def close(self):
        self.socket.close()


def fixture(path):
    samples = array.array("h")
    for frame in range(480):
        value = round(900 * math.sin(2 * math.pi * 3 * frame / 480))
        samples.extend((value, value))
    if os.sys.byteorder != "little":
        samples.byteswap()
    with wave.open(str(path), "wb") as output:
        output.setparams((2, 2, 48000, 0, "NONE", "not compressed"))
        for _ in range(12000):
            output.writeframesraw(samples.tobytes())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--server", default="target/debug/tyranny-server")
    parser.add_argument("--client", default="target/debug/tyrannyctl")
    parser.add_argument("--work-dir", default="target/server-gate")
    parser.add_argument("--coreaudio", action="store_true")
    args = parser.parse_args()
    server = str(Path(args.server).resolve())
    client = str(Path(args.client).resolve())
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=work))
    state_dir = run / "state"
    state_dir.mkdir(mode=0o700)
    source = run / "continuous.wav"
    fixture(source)
    report = dict(run_directory=str(run), output="coreaudio" if args.coreaudio else "clocked_pcm", cases=[], launches=[], limitations=["No library scanner, SwiftUI surface, or packaged app exercised", "Consumer underrun counters are not acoustic-loopback proof"])
    process = None
    logs = []
    clients = []
    launch_count = 0

    def record(name, **details):
        report["cases"].append(dict(name=name, passed=True, **details))
        print(f"Passed {name}", flush=True)

    def start():
        nonlocal process, launch_count
        launch_count += 1
        argv = [server, "--state-dir", str(state_dir), "--bind", "127.0.0.1:0"]
        if not args.coreaudio:
            argv += ["--capture", str(run / f"output-{launch_count}.f32")]
        log = (run / f"server-{launch_count}.stderr").open("w")
        logs.append(log)
        process = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=log, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(15), "server did not announce readiness"
        line = process.stdout.readline()
        assert line, f"server exited at startup; see {log.name}"
        ready = json.loads(line)
        report["launches"].append(dict(argv=argv, ready=ready))
        return ready

    def stop(crash=False):
        nonlocal process
        if process is not None:
            if crash:
                process.kill()
            else:
                process.terminate()
            code = process.wait(timeout=12)
            assert code == (-9 if crash else 0), f"unexpected shutdown: {code}"
            process.stdout.close()
            process = None
        for connection in clients:
            connection.close()
        clients.clear()

    def connect(ready, **kwargs):
        connection = WebSocket(ready["address"], token, **kwargs)
        assert connection.status == 101
        clients.append(connection)
        return connection

    def persisted():
        with sqlite3.connect(state_dir / "state.sqlite3") as database:
            row = database.execute("SELECT current_id,position_frames,playing,revision,next_id,settings FROM control").fetchone()
        return dict(current_id=row[0], position_frames=row[1], playing=bool(row[2]), revision=row[3], next_id=row[4], settings=json.loads(row[5]))

    try:
        ready = start()
        token_path = Path(ready["token_path"])
        token = token_path.read_text().strip()
        assert token_path.stat().st_mode & 0o777 == 0o600
        assert state_dir.stat().st_mode & 0o777 == 0o700
        unauthorized = WebSocket(ready["address"])
        incorrect = WebSocket(ready["address"], "wrong-token")
        browser = WebSocket(ready["address"], token, origin="http://localhost")
        assert unauthorized.status == 401 and incorrect.status == 401 and browser.status == 403
        record("local-authentication-and-origin-rejection", statuses=[unauthorized.status, incorrect.status, browser.status])
        duplicate = subprocess.run([server, "--state-dir", str(state_dir), "--bind", "127.0.0.1:0", "--capture", str(run / "duplicate.f32")], capture_output=True, text=True, timeout=10)
        assert duplicate.returncode != 0
        nonlocal_bind = subprocess.run([server, "--state-dir", str(run / "bad-bind"), "--bind", "0.0.0.0:0"], capture_output=True, text=True, timeout=10)
        assert nonlocal_bind.returncode != 0
        record("single-owner-lock-and-loopback-only", duplicate_stderr=duplicate.stderr, nonlocal_stderr=nonlocal_bind.stderr)

        a, b = connect(ready), connect(ready)
        assert a.hello["instance_id"] == b.hello["instance_id"]
        revision = a.snapshot["queue_revision"]
        first = a.request("append", expected_revision=revision, path=str(source))
        one = first["entry_id"]
        second = a.request("append", expected_revision=first["snapshot"]["queue_revision"], path=str(source))
        two = second["entry_id"]
        assert one != two
        b.wait(lambda state: state["queue_length"] == 2)
        a.request("append", expect_error="stale_revision", expected_revision=revision, path=str(source))
        before_move = second["snapshot"]["queue_revision"]
        moved = a.request("move", expected_revision=before_move, entry_ids=[two], before=one)
        revision = moved["snapshot"]["queue_revision"]
        a.request("queue_page", expect_error="stale_revision", revision=before_move, offset=0, limit=100)
        page_a = a.request("queue_page", revision=revision, offset=0, limit=100)
        page_b = b.request("queue_page", revision=revision, offset=0, limit=100)
        assert page_a == page_b and [item["id"] for item in page_a["entries"]] == [two, one]
        record("duplicate-identities-stale-mutations-and-consistent-pages", order=[two, one], revision=revision)

        with sqlite3.connect(state_dir / "state.sqlite3") as database:
            database.execute("CREATE TRIGGER reject_append BEFORE INSERT ON queue BEGIN SELECT RAISE(ABORT, 'gate rejects append'); END")
        a.request("append", expect_error="persistence", expected_revision=revision, path=str(source))
        assert a.request("queue_page", revision=revision, offset=0, limit=100) == page_a
        with sqlite3.connect(state_dir / "state.sqlite3") as database:
            database.execute("DROP TRIGGER reject_append")
        record("failed-durable-edit-does-not-mutate-engine")

        a.request("play", entry_id=two)
        a.wait(lambda state: state["transport"] == "playing" and state["current_id"] == two)
        b.wait(lambda state: state["transport"] == "playing" and state["current_id"] == two)
        old_position = b.snapshot["position_frames"]
        b.close()
        time.sleep(0.1)
        b = connect(ready)
        assert b.snapshot["current_id"] == two and b.snapshot["position_frames"] >= old_position
        record("two-controllers-and-authoritative-reconnect", reconnect=b.snapshot)

        a.request("seek", entry_id=two, position_frames=96000)
        a.wait(lambda state: state["transport"] == "playing" and state["position_frames"] >= 96000)
        a.request("pause")
        time.sleep(0.08)
        paused = a.state()
        time.sleep(0.15)
        assert a.state()["position_frames"] == paused["position_frames"]
        assert paused["transport"] == "paused"
        a.request("set_gain", mode="track")
        record("real-pause-seek-resume-and-gain-setting", paused_frame=paused["position_frames"])

        cli_command = [client, "--url", "ws://" + ready["address"] + "/ws", "--token-file", str(token_path), '{"id":900,"command":"snapshot"}']
        cli = subprocess.run(cli_command, capture_output=True, text=True, timeout=10)
        assert cli.returncode == 0, cli.stderr
        cli_messages = [json.loads(line) for line in cli.stdout.splitlines()]
        assert any(message.get("id") == 900 and message.get("status") == "ok" for message in cli_messages)
        record("development-client-binary", argv=cli_command, stdout=cli.stdout)

        # A large bounded page makes a genuinely unread TCP connection exhaust
        # its send window quickly, without adding a fake slow-consumer hook.
        for _ in range(198):
            result = a.request("append", expected_revision=revision, path=str(source))
            revision = result["snapshot"]["queue_revision"]
        a.request("play")
        a.wait(lambda state: state["transport"] == "playing")
        slow = connect(ready, receive_buffer=1024)
        slow.socket.settimeout(3)
        def flood():
            try:
                for index in range(4096):
                    slow.send(dict(id=10000+index, command="queue_page", revision=revision, offset=0, limit=500))
            except (OSError, ConnectionError):
                pass
        flood_thread = threading.Thread(target=flood, daemon=True)
        flood_thread.start()
        previous = a.state()["position_frames"]
        time.sleep(1.5)
        started = time.monotonic()
        live = a.state()
        latency = time.monotonic() - started
        assert live["position_frames"] > previous + 48000 and live["underruns"] == 0
        assert latency < 1, latency
        # Drain only after leaving the peer unread beyond the server's deadline.
        time.sleep(4.5)
        closed = False
        deadline = time.monotonic() + 8
        try:
            while time.monotonic() < deadline:
                slow.receive(timeout=max(0.1, deadline-time.monotonic()))
        except (ConnectionError, OSError):
            closed = True
        assert closed, "unread controller did not disconnect"
        slow.close()
        flood_thread.join(timeout=4)
        assert a.state()["underruns"] == 0
        record("unread-controller-bounded-disconnect-and-uninterrupted-audio", responsive_client_latency_seconds=latency)

        a.close()
        b.close()
        time.sleep(1.2)
        c = connect(ready)
        assert c.snapshot["transport"] == "playing" and c.snapshot["underruns"] == 0
        record("playback-independent-of-controller-lifetime")
        first_instance = ready["instance_id"]
        observed = c.state()["position_frames"]
        stop(crash=True)
        checkpoint = persisted()
        assert checkpoint["playing"] and checkpoint["current_id"] == two
        assert 0 <= observed - checkpoint["position_frames"] <= 72000
        ready = start()
        assert ready["instance_id"] != first_instance and token_path.read_text().strip() == token
        a = connect(ready)
        restored = a.snapshot
        assert restored["transport"] == "paused" and restored["current_id"] == two
        assert restored["position_frames"] == checkpoint["position_frames"]
        assert restored["queue_revision"] == revision and restored["queue_length"] == 200
        assert restored["settings"]["gain_mode"] == "track"
        page = a.request("queue_page", revision=revision, offset=0, limit=500)
        assert [entry["id"] for entry in page["entries"][:2]] == [two, one]
        assert len({entry["id"] for entry in page["entries"]}) == 200
        record("crash-restart-durable-queue-and-paused-default", checkpoint=checkpoint, restored=restored)

        a.request("set_resume", enabled=True)
        a.request("play")
        a.wait(lambda state: state["transport"] == "playing")
        stop()
        checkpoint = persisted()
        assert checkpoint["playing"]
        ready = start()
        a = connect(ready)
        resumed = a.wait(lambda state: state["transport"] == "playing")
        assert resumed["current_id"] == two and resumed["position_frames"] >= checkpoint["position_frames"]
        record("explicit-auto-resume-after-clean-restart", resumed=resumed)
        a.request("pause")
        stop()
        ready = start()
        a = connect(ready)
        assert a.snapshot["transport"] == "paused" and a.snapshot["settings"]["resume_on_start"]
        removed = a.request("remove", expected_revision=revision, entry_id=two)
        assert removed["snapshot"]["transport"] == "stopped" and removed["snapshot"]["current_id"] is None
        cleared = a.request("clear", expected_revision=removed["snapshot"]["queue_revision"])
        added = a.request("append", expected_revision=cleared["snapshot"]["queue_revision"], path=str(source))
        assert added["entry_id"] >= checkpoint["next_id"] and added["snapshot"]["queue_length"] == 1
        record("explicit-pause-survives-auto-resume-and-ids-never-recycle", new_entry_id=added["entry_id"])
        stop()
        report["passed"] = True
    except Exception as error:
        report["passed"] = False
        report["failure"] = repr(error)
        raise
    finally:
        for connection in clients:
            connection.close()
        if process is not None:
            process.kill()
            process.wait(timeout=10)
        for log in logs:
            log.close()
        (run / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps(dict(report=str(run / "report.json"), passed=report.get("passed", False), cases=len(report["cases"]))))


if __name__ == "__main__":
    main()
