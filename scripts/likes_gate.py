#!/usr/bin/env python3
"""Measure native heart updates on an isolated three-track fixture, never live likes."""
import argparse
import hashlib
import json
from pathlib import Path
import selectors
import subprocess
import tempfile
import time

from server_gate import UnixSocket
from search_gate import description, distribution, seed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--app', default='target/swift-client/debug/tyranny likes smoke')
    parser.add_argument('--server', default='target/portable-rust/release/tyranny-likes-smoke-server')
    parser.add_argument('--output', required=True)
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--server-only', action='store_true',
                        help='Measure writes on 50,000 synthetic tracks without launching the UI')
    args = parser.parse_args()
    output = Path(args.output).resolve()
    output.mkdir(parents=True, exist_ok=True)
    server = Path(args.server).resolve()
    app = Path(args.app).resolve()
    with tempfile.TemporaryDirectory(prefix='tyr-likes-', dir='/tmp') as temporary:
        run = Path(temporary).resolve()
        music = run / 'music'
        music.mkdir()
        if not args.server_only:
            for index, title in enumerate(['Alpha', 'Beta', 'Gamma']):
                subprocess.run(['ffmpeg', '-v', 'error', '-f', 'lavfi', '-i',
                    'anullsrc=r=44100:cl=stereo', '-t', '1', '-metadata', f'title={title}',
                    '-metadata', 'artist=Likes Fixture', '-metadata', 'album=Likes Fixture',
                    '-metadata', f'track={index + 1}', str(music / f'{title}.flac')], check=True)
        with (output / 'server.log').open('w') as log:
            def launch():
                process = subprocess.Popen([str(server), '--state-dir', str(run / 'state'),
                    '--capture', str(run / 'capture.f32')], stdout=subprocess.PIPE, stderr=log, text=True)
                return process
            daemon = launch()
            controller = None
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(daemon.stdout, selectors.EVENT_READ)
                    assert selector.select(30), 'Daemon readiness timeout'
                ready = json.loads(daemon.stdout.readline())
                controller = UnixSocket(ready['socket_path'])
                schema = None
                if args.server_only:
                    controller.close()
                    controller = None
                    daemon.terminate()
                    daemon.wait(timeout=15)
                    daemon.stdout.close()
                    schema = seed(run / 'state/state.sqlite3', music)
                    daemon = launch()
                    with selectors.DefaultSelector() as selector:
                        selector.register(daemon.stdout, selectors.EVENT_READ)
                        assert selector.select(30), 'Daemon readiness timeout'
                    ready = json.loads(daemon.stdout.readline())
                    controller = UnixSocket(ready['socket_path'])
                else:
                    controller.request('add_root', path=str(music))
                    controller.wait(lambda s: s['library']['tracks'] == 3
                                    and s['library']['scan']['phase'] == 'complete', seconds=30)
                rows = controller.request('list_tracks', list=description(), revision=None, offset=0, limit=100)
                track = rows['entries'][0]['track']['id']
                revision = rows['revision']
                timings = []
                for n in range(30):
                    start = time.perf_counter()
                    reply = controller.request('set_track_liked', track_id=track,
                                               liked=n % 2 == 0, library_revision=revision)
                    timings.append((time.perf_counter() - start) * 1000)
                    revision = reply['library_revision']
                if args.server_only:
                    report = dict(passed=True, tracks=50_000, schema=schema)
                else:
                    command = [str(app), '--socket', ready['socket_path'], '--likes-smoke-output',
                               str(output), '--ui-driver', str(Path('target/native-ui-driver').resolve())]
                    if args.check:
                        command.append('--likes-performance-check')
                    with (output / 'client.log').open('w') as client_log:
                        subprocess.run(command, stdout=client_log, stderr=client_log, check=True, timeout=180)
                    report = json.loads((output / 'report.json').read_text())
                report['server_roundtrip'] = distribution(timings)
                report['server_samples_ms'] = timings
                binaries = [server] if args.server_only else [server, app]
                report['binary_sha256'] = {str(p): hashlib.sha256(p.read_bytes()).hexdigest() for p in binaries}
                for key, samples in report.get('observations', {}).items():
                    if key.endswith('_timings'):
                        report[key + '_summary'] = distribution([s['visible_ms'] for s in samples])
                (output / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
                print(json.dumps({k: v for k, v in report.items() if k.endswith('_summary') or k in ['passed', 'error', 'server_roundtrip']}, indent=2))
                assert report['passed'], report.get('error')
            finally:
                if controller:
                    controller.close()
                daemon.terminate()
                daemon.wait(timeout=15)
                daemon.stdout.close()


if __name__ == '__main__':
    main()
