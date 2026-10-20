#!/usr/bin/env python3
"""Launch a copied development app through LaunchServices and test real launchd ownership.

Requires an unlocked macOS GUI login and Accessibility/Screen Recording permissions.
Never installs the default login job or modifies installed decoder libraries.
"""
import argparse
import json
import os
from pathlib import Path
import plistlib
import selectors
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import uuid

sys.dont_write_bytecode = True
from server_gate import WebSocket, fixture


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--app", default="target/Tyranny.app")
    parser.add_argument("--ui-driver", default="target/native-ui-driver")
    parser.add_argument("--work-dir", default="target/lifecycle-gate")
    parser.add_argument("--coreaudio", action="store_true")
    args = parser.parse_args()
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=work))
    os.chmod(run, 0o700)
    app = run / "Tyranny.app"
    source_app = Path(args.app).resolve()
    shutil.copytree(source_app, app, symlinks=True)
    helper = app / "Contents/MacOS/tyranny-server"
    driver = Path(args.ui_driver).resolve()
    environment = {"HOME": str(Path.home()), "PATH": "/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "en_US.UTF-8"}
    domain = f"gui/{os.getuid()}"
    label = "dev.tyranny.spike." + uuid.uuid4().hex
    agents = run / "agents"
    state = run / "state"
    with socket.socket() as reservation:
        reservation.bind(("127.0.0.1", 0))
        port = reservation.getsockname()[1]
    address = f"127.0.0.1:{port}"
    target = f"{domain}/{label}"
    plist = agents / f"{label}.plist"
    report = {"run_directory": str(run), "label": label, "address": address,
              "launch_method": "/usr/bin/open -n -W (LaunchServices)",
              "environment": environment, "output": "coreaudio" if args.coreaudio else "clocked_pcm",
              "cases": [], "commands": [], "native": [],
              "limitations": ["Development per-user LaunchAgent, not SMAppService distribution or sandbox certification.",
                              "Homebrew runtime libraries remain external; only copied Mach-O load commands are mutated.",
                              "Unavailable/permission-denied folders are exercised, not a claim of TCC/FDA grant automation.",
                              "launchctl diagnostic output is retained as evidence; app ownership checks fail closed if its format changes.",
                              "Clocked PCM is real production decoding/playback, not human acoustic verification."]}
    connections = []
    manual = None
    app_pids = set()
    jobs = {target}
    restored_permissions = []

    def terminate_gate(signum, frame):
        raise KeyboardInterrupt(f"Lifecycle gate interrupted by signal {signum}")

    signal.signal(signal.SIGTERM, terminate_gate)

    def command(argv, check=True, timeout=30):
        result = subprocess.run([str(value) for value in argv], env=environment, capture_output=True,
                                text=True, timeout=timeout)
        report["commands"].append({"argv": [str(value) for value in argv], "returncode": result.returncode,
                                   "stdout": "[Unrelated GUI-domain details omitted]" if [str(value) for value in argv] == ["/bin/launchctl", "print", domain] else result.stdout,
                                   "stderr": result.stderr})
        if check:
            assert result.returncode == 0, f"{argv[0]} failed: {result.stderr}"
        return result

    def ctl(*arguments, check=True):
        return command(["/bin/launchctl", *arguments], check=check)

    def record(name, **details):
        report["cases"].append({"name": name, "passed": True, **details})
        print(f"Passed {name}", flush=True)

    def launch(name, bundle=app, action="observe", managed_state=state, managed_label=label, expect_connected=True):
        output = run / f"native-{name}.json"
        argv = ["/usr/bin/open", "-n", "-W",
                "--stdout", str(run / f"native-{name}.stdout"), "--stderr", str(run / f"native-{name}.stderr"),
                str(bundle), "--args",
                "--managed-state-dir", str(managed_state), "--managed-bind", address,
                "--managed-label", managed_label, "--managed-agent-dir", str(agents),
                "--lifecycle-output", str(output), "--lifecycle-action", action, "--ui-driver", str(driver)]
        if not args.coreaudio:
            argv += ["--managed-capture", str(managed_state / "output.f32")]
        request_path = Path(str(output) + ".request.json")
        response_path = Path(str(output) + ".response.json")
        pidfile = Path(str(output) + ".pid")
        deadline = time.monotonic() + 100
        process = subprocess.Popen(argv, env=environment, stdout=subprocess.PIPE,
                                   stderr=subprocess.PIPE, text=True)
        try:
            while process.poll() is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"Native lifecycle launch exceeded 100 seconds: {name}")
                if request_path.is_file():
                    bridge_error = None
                    bridge_argv = None
                    try:
                        request = json.loads(request_path.read_text())
                        request_path.unlink()
                        if request["kind"] == "drive":
                            pid = int(request["pid"])
                            assert pidfile.is_file() and pid == int(pidfile.read_text().strip()), "UI request PID is not this copied app"
                            assert request["action"] in ("activate", "press", "press-title"), "Unsupported lifecycle UI action"
                            bridge_argv = [str(driver), str(pid), request["action"]]
                            if request.get("identifier") is not None:
                                bridge_argv.append(str(request["identifier"]))
                        elif request["kind"] == "capture":
                            capture_path = Path(request["path"])
                            assert capture_path == output.with_suffix(".png"), "Capture must target this launch's screenshot"
                            window_id = int(request["window_id"])
                            assert window_id > 0, "Invalid native window ID"
                            bridge_argv = ["/usr/sbin/screencapture", "-x", "-o", "-l", str(window_id), str(capture_path)]
                        else:
                            raise ValueError(f"Unknown lifecycle request kind: {request['kind']}")
                        remaining = deadline - time.monotonic()
                        if remaining <= 0:
                            raise TimeoutError("Lifecycle deadline reached before UI request")
                        result = command(bridge_argv, check=False, timeout=min(20, remaining))
                        if result.returncode != 0:
                            bridge_error = result.stderr.strip() or result.stdout.strip() or f"UI subprocess exited {result.returncode}"
                    except Exception as error:
                        bridge_error = str(error)
                        request_path.unlink(missing_ok=True)
                        if isinstance(error, subprocess.TimeoutExpired):
                            report["commands"].append({"argv": bridge_argv, "returncode": None,
                                                       "error": bridge_error, "timeout": True})
                    temporary = Path(str(response_path) + ".tmp")
                    temporary.write_text(json.dumps({"error": bridge_error}) + "\n")
                    temporary.replace(response_path)
                time.sleep(min(0.03, max(0, deadline - time.monotonic())))
            assert process.returncode == 0, f"LaunchServices open exited {process.returncode}"
        finally:
            if process.poll() is None:
                process.kill()
            stdout, stderr = process.communicate(timeout=5)
            report["commands"].append({"argv": argv, "returncode": process.returncode,
                                       "stdout": stdout, "stderr": stderr})
            if pidfile.exists():
                app_pids.add(int(pidfile.read_text().strip()))
        assert output.is_file(), f"Native lifecycle report missing: {output}"
        native = json.loads(output.read_text())
        report["native"].append({"name": name, "path": str(output), "report": native})
        assert native["action"] == action and native["managedLabel"] == managed_label, native
        assert native["endpoint"] == f"ws://{address}/ws", native
        assert native["tokenPath"] == str(managed_state / "token"), native
        assert native["connected"] == expect_connected, native
        screenshot = output.with_suffix(".png")
        assert screenshot.is_file() and screenshot.stat().st_size > 0, f"Actual native window capture missing: {screenshot}"
        if expect_connected:
            assert native["error"] is None, native
        try:
            os.kill(native["pid"], 0)
        except ProcessLookupError:
            pass
        else:
            raise AssertionError("LaunchServices returned before the native client exited")
        app_pids.discard(native["pid"])
        return native

    def connect(previous=None, seconds=40):
        deadline = time.monotonic() + seconds
        error = None
        while time.monotonic() < deadline:
            try:
                token = (state / "token").read_text().strip()
                client = WebSocket(address, token)
                assert client.status == 101, "Real daemon did not authenticate"
                if previous is not None and client.hello["instance_id"] == previous:
                    client.close()
                    time.sleep(0.1)
                    continue
                connections.append(client)
                return client
            except (OSError, AssertionError, ConnectionError) as caught:
                error = str(caught)
                time.sleep(0.15)
        raise AssertionError(f"Authenticated daemon readiness timed out: {error}")

    def scan(client, root_id=None):
        if root_id is not None:
            client.request("rescan", root_id=root_id)
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            snapshot = client.state()
            if snapshot["library"]["scan"]["phase"] in ("complete", "failed"):
                return snapshot
            time.sleep(0.05)
        raise AssertionError("Real scanner timed out")

    def tracks(client):
        return client.request("library_tracks", revision=None, offset=0, limit=100, query="", album_id=None)

    try:
        assert driver.is_file(), "Build scripts/native_ui_driver.swift as target/native-ui-driver first"
        ctl("print", domain)
        assert ctl("print", target, check=False).returncode != 0
        launch("absent")
        control = connect()
        initial_instance = control.hello["instance_id"]
        configured = plistlib.loads(plist.read_bytes())
        assert configured["Label"] == label and str(helper) in configured["ProgramArguments"]
        assert configured["KeepAlive"] is True and configured["Umask"] == 63
        assert state.stat().st_mode & 0o777 == 0o700
        assert (state / "token").stat().st_mode & 0o777 == 0o600
        assert (state / "server.log").stat().st_mode & 0o777 == 0o600
        unauthorized = WebSocket(address)
        assert unauthorized.status == 401
        wrong = WebSocket(address, "not-the-token")
        assert wrong.status == 401
        record("absent-daemon-installed-by-launchservices-app-and-authenticated-v4", hello=control.hello,
               launchd=ctl("print", target).stdout, unauthorized_status=401)

        root = run / "Music – 日本語"
        root.mkdir()
        song = root / "continuous.wav"
        fixture(song)
        added = control.request("add_root", path=str(root))
        root_id = added["root"]["id"]
        imported = scan(control)
        assert imported["library"]["scan"]["phase"] == "complete"
        page = tracks(control)
        assert page["total"] == 1
        state_before = control.state()
        control.request("append_track", expected_revision=state_before["queue_revision"], library_revision=page["revision"], track_id=page["entries"][0]["id"])
        control.request("set_resume", enabled=True)
        control.request("play", entry_id=None)
        playing = control.wait(lambda value: value["transport"] == "playing" and value["position_frames"] > 4800)
        native = launch("present-playing")
        assert native["snapshot"]["transport"] == "playing", native
        after = connect()
        assert after.hello["instance_id"] == initial_instance
        paused = after.state()
        after.close()
        assert paused["transport"] == "paused", paused
        assert paused["position_frames"] >= playing["position_frames"], (playing, paused)
        assert paused["current_id"] == playing["current_id"], (playing, paused)
        record("present-daemon-reused-and-client-quit-pauses", before=playing, after=paused)

        # Each check uses a fresh connection, so no idle socket backs up across launches.
        def fresh_state():
            client = connect()
            try:
                return client.state()
            finally:
                client.close()

        # The quit note resumes this server. The relaunch's own quit then pauses it again.
        native = launch("relaunch-resumes")
        resumed = native["snapshot"]
        assert resumed["transport"] == "playing", native
        assert resumed["current_id"] == paused["current_id"], (paused, resumed)
        repaused = fresh_state()
        assert repaused["transport"] == "paused", repaused
        assert repaused["position_frames"] > paused["position_frames"], (paused, repaused)
        record("client-relaunch-resumes-suspended-playback", before=paused, resumed=resumed, after=repaused)

        # That quit left a note. A launch while stopped takes the note and does not resume.
        driver_client = connect()
        driver_client.request("stop")
        driver_client.wait(lambda value: value["transport"] == "stopped")
        driver_client.close()
        native = launch("stopped-takes-note")
        assert native["snapshot"]["transport"] == "stopped", native
        assert fresh_state()["transport"] == "stopped"
        driver_client = connect()
        driver_client.request("play", entry_id=None)
        driver_client.wait(lambda value: value["transport"] == "playing" and value["position_frames"] > 0)
        driver_client.request("pause")
        held = driver_client.wait(lambda value: value["transport"] == "paused")
        driver_client.close()
        native = launch("paused-quit")
        assert native["snapshot"]["transport"] == "paused", native
        native = launch("paused-relaunch")
        assert native["snapshot"]["transport"] == "paused", native
        still = fresh_state()
        assert still["transport"] == "paused" and still["current_id"] == held["current_id"], (held, still)
        record("client-quit-while-paused-does-not-resume", before=held, after=still)

        launch("restart", action="restart")
        control = connect(previous=initial_instance)
        record("native-explicit-clean-restart-preserves-queue-library", hello=control.hello)
        assert control.state()["queue_length"] == 1 and tracks(control)["total"] == 1
        for name, sig in [("clean-exit", "SIGTERM"), ("crash", "SIGKILL")]:
            previous = control.hello["instance_id"]
            ctl("kill", sig, target)
            control = connect(previous=previous)
            snapshot = control.state()
            assert snapshot["queue_length"] == 1 and tracks(control)["total"] == 1
            record(f"launchd-restarts-{name}-without-client", hello=control.hello,
                   launchd=ctl("print", target).stdout)

        control.request("pause")
        indexed = tracks(control)
        offline = run / "Music offline"
        root.rename(offline)
        try:
            unavailable = scan(control, root_id)
            preserved = tracks(control)
            assert unavailable["library"]["scan"]["phase"] == "failed"
            assert unavailable["library"]["scan"]["last_error"]
            assert preserved["entries"] == indexed["entries"]
            launch("unavailable-root")
            record("unavailable-root-reports-error-and-preserves-index", snapshot=unavailable, page=preserved)
        finally:
            offline.rename(root)
        assert os.geteuid() != 0, "Permission-denied root case must run as a normal GUI user"
        restored_permissions.append(root)
        root.chmod(0)
        denied = scan(control, root_id)
        assert denied["library"]["scan"]["phase"] == "failed" and denied["library"]["scan"]["last_error"]
        assert tracks(control)["entries"] == indexed["entries"]
        root.chmod(0o700)
        restored_permissions.remove(root)
        record("permission-denied-root-is-actionable-and-nondestructive", snapshot=denied)
        scan(control, root_id)

        launch("uninstall", action="uninstall", expect_connected=False)
        assert not plist.exists() and ctl("print", target, check=False).returncode != 0
        assert (state / "state.sqlite3").is_file() and (state / "token").is_file() and song.is_file()
        record("native-explicit-uninstall-removes-only-job-and-preserves-data")

        # Start a real manually-owned process; the copied app must reuse, never kill it.
        manual_argv = [str(helper), "--state-dir", str(state), "--bind", address]
        if not args.coreaudio:
            manual_argv += ["--capture", str(state / "manual-output.f32")]
        with (run / "manual.stderr").open("w") as stderr:
            manual = subprocess.Popen(manual_argv, env=environment, stdout=subprocess.PIPE, stderr=stderr, text=True)
            with selectors.DefaultSelector() as selector:
                selector.register(manual.stdout, selectors.EVENT_READ)
                assert selector.select(20), "Manual daemon did not start"
            ready = json.loads(manual.stdout.readline())
            launch("manual-present")
            manual_client = connect()
            assert manual_client.hello["instance_id"] == ready["instance_id"] and not plist.exists()
            rejected = launch("manual-restart-refused", action="restart", expect_connected=False)
            assert rejected["error"] and manual.poll() is None
            still_manual = connect()
            assert still_manual.hello["instance_id"] == ready["instance_id"] and not plist.exists()
            launch("manual-uninstall-noop", action="uninstall", expect_connected=False)
            assert manual.poll() is None and not plist.exists()
            record("manual-daemon-reused-and-never-restarted-or-uninstalled", instance_id=ready["instance_id"], refusal=rejected["error"])
            manual.terminate()
            manual.wait(timeout=20)
            manual.stdout.close()
            manual = None

        broken = run / "MissingDecoder.app"
        shutil.copytree(app, broken, symlinks=True)
        broken_helper = broken / "Contents/MacOS/tyranny-server"
        links = command(["/usr/bin/otool", "-L", broken_helper]).stdout
        dependency = next((line.strip().split(" (compatibility", 1)[0] for line in links.splitlines()
                           if "libavformat." in line), None)
        assert dependency, "The bundled helper must dynamically link the actual FFmpeg decoder"
        missing = "@loader_path/tyranny-missing-avformat.dylib"
        command(["/usr/bin/install_name_tool", "-change", dependency, missing, broken_helper])
        command(["/usr/bin/codesign", "--force", "--sign", "-", broken_helper])
        command(["/usr/bin/codesign", "--force", "--sign", "-", broken])
        broken_label = label + ".missing"
        broken_state = run / "missing-state"
        broken_target = f"{domain}/{broken_label}"
        jobs.add(broken_target)
        missing_result = launch("missing-decoder", bundle=broken, managed_label=broken_label,
                                managed_state=broken_state, expect_connected=False)
        assert missing_result["error"] and "helper-check.log" in missing_result["error"]
        diagnostics = (broken_state / "helper-check.log").read_text()
        assert "Library not loaded" in diagnostics and "tyranny-missing-avformat" in diagnostics
        assert ctl("print", broken_target, check=False).returncode != 0
        assert not (agents / f"{broken_label}.plist").exists(), "Missing dependency registered a restart loop"
        record("missing-decoder-preflight-visible-error-without-launchd-restart-loop", replaced_dependency=dependency,
               missing_dependency=missing, native_error=missing_result["error"], diagnostic=diagnostics)
        report["passed"] = True
    except BaseException as error:
        report["passed"] = False
        report["failure"] = repr(error)
        raise
    finally:
        for root in restored_permissions:
            root.chmod(0o700)
        for connection in connections:
            connection.close()
        if manual is not None and manual.poll() is None:
            manual.terminate()
            try:
                manual.wait(timeout=20)
            except subprocess.TimeoutExpired:
                manual.kill()
                manual.wait(timeout=5)
        for pid in app_pids:
            try:
                identity = subprocess.run(["/bin/ps", "-p", str(pid), "-o", "command="],
                                          capture_output=True, text=True, timeout=5)
                if str(run) in identity.stdout and "--lifecycle-output" in identity.stdout:
                    os.kill(pid, signal.SIGTERM)
            except (OSError, subprocess.TimeoutExpired):
                pass
        # Never use a bare domain bootout: each target is this run's minted label.
        cleanup = []
        for job in sorted(jobs):
            try:
                result = ctl("bootout", job, check=False)
                remaining = ctl("print", job, check=False)
                deadline = time.monotonic() + 20
                while remaining.returncode == 0 and time.monotonic() < deadline:
                    time.sleep(0.1)
                    remaining = ctl("print", job, check=False)
                cleanup.append({"target": job, "bootout_status": result.returncode, "absent": remaining.returncode != 0})
            except Exception as error:
                cleanup.append({"target": job, "error": repr(error), "absent": False})
        report["cleanup"] = cleanup
        if not all(item["absent"] for item in cleanup):
            report["passed"] = False
        (run / "report.json").write_text(json.dumps(report, indent=2) + "\n")
        print(json.dumps({"report": str(run / "report.json"), "passed": report.get("passed", False)}))
    if not report.get("passed"):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
