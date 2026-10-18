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
    args = parser.parse_args()
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


if __name__ == "__main__":
    main()
