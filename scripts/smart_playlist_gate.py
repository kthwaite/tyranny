#!/usr/bin/env python3
"""Real smart-rule, migration, snapshot and native workflows in isolated state."""
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

sys.dont_write_bytecode = True
from server_gate import WebSocket


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', default='target/release/tyranny-server')
    parser.add_argument('--legacy-server', help='Unmodified protocol6/schema3 executable')
    parser.add_argument('--app', help='Native debug app executable; requires AX and screen capture permissions')
    parser.add_argument('--ui-driver', default='target/native-ui-driver')
    parser.add_argument('--work-dir', default='target/smart-playlist-gate')
    args = parser.parse_args()
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=work))
    music = run / 'Music – 日本語'
    music.mkdir()

    def audio(path, title, artist, album):
        command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-f', 'lavfi',
                   '-i', 'anullsrc=r=48000:cl=stereo', '-t', '120', '-c:a', 'flac',
                   '-metadata', 'artist=' + artist, '-metadata', 'album_artist=' + artist,
                   '-metadata', 'album=' + album]
        if title is not None:
            command += ['-metadata', 'title=' + title]
        subprocess.run(command + [str(path)], check=True, timeout=30)

    audio(music / 'freedom.flac', 'Freedom', 'Miles Davis', 'Bitches Brew')
    audio(music / 'blue.flac', 'So What', 'Miles Davis', 'Kind of Blue')
    audio(music / 'ornette.flac', 'Lonely Woman', 'Ornette Coleman', 'Jazz Sampler')
    page_source = run / 'page-source.flac'
    audio(page_source, None, 'Miles Davis', 'Sessions')
    for index in range(505):
        os.link(page_source, music / f'Page {index:03}.flac')
    addition = run / 'new-album.flac'
    audio(addition, 'Sparks', 'Miles Davis', 'Nefertiti')
    report = {'run_directory': str(run), 'cases': [], 'launches': [], 'native': [],
              'output': 'clock-paced PCM capture', 'limitations': [
                  '508 generated tagged real FLAC paths, not a production collection benchmark',
                  'No user database, daemon, audio device or acoustic listening involved']}
    process = None
    client = None
    logs = []
    ready = None

    def record(name):
        report['cases'].append({'name': name, 'passed': True})
        print('Passed ' + name, flush=True)

    def start(binary, version=14):
        nonlocal process, client, ready
        index = len(report['launches'])
        command = [str(Path(binary).resolve()), '--state-dir', str(run / 'state'),
                   '--bind', '127.0.0.1:0', '--capture', str(run / f'capture-{index}.f32')]
        log = (run / f'server-{index}.stderr').open('w')
        logs.append(log)
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=log, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(20), 'Daemon startup timed out'
        ready = json.loads(process.stdout.readline())
        report['launches'].append({'command': command, 'ready': ready})
        client = WebSocket(ready['address'], Path(ready['token_path']).read_text().strip(), expected_version=version)

    def stop():
        nonlocal client, process
        if client is not None:
            client.close()
            client = None
        if process is not None:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=15)
            process.stdout.close()
            process = None

    def catalog(kind=None):
        fields = dict(revision=None, offset=0, limit=100, query='')
        if kind is not None:
            fields['kind'] = kind
        return client.request('saved_playlists', **fields)

    def entries(playlist, query='', **overrides):
        fields = dict(playlist_id=playlist['id'], revision=playlist['revision'],
                      library_revision=client.state()['library']['revision'], offset=0, limit=500, query=query)
        fields.update(overrides)
        return client.request('playlist_entries', **fields)

    def all_entries(playlist):
        first = entries(playlist)
        rows = first['entries']
        for offset in range(500, first['total'], 500):
            rows += entries(playlist, offset=offset, library_revision=first['library_revision'])['entries']
        return rows

    def queue():
        first = client.request('queue_page', revision=None, offset=0, limit=500)
        rows = first['entries']
        for offset in range(500, first['total'], 500):
            rows += client.request('queue_page', revision=first['revision'], offset=offset, limit=500)['entries']
        return rows

    def snapshot(playlist, name, **overrides):
        state = client.state()
        fields = dict(playlist_id=playlist['id'], playlist_revision=playlist['revision'],
                      library_revision=state['library']['revision'], expected_revision=state['playlists_revision'], name=name)
        fields.update(overrides)
        return client.request('snapshot_smart_playlist', **fields)

    def rescan(total):
        client.request('rescan', root_id=root['id'])
        client.wait(lambda s: s['library']['scan']['phase'] == 'complete' and s['library']['tracks'] == total, seconds=30)

    def native(mode):
        folder = run / ('native-' + mode)
        command = [str(Path(args.app).resolve()), '--url', 'ws://' + ready['address'] + '/ws',
                   '--token-file', ready['token_path'], '--smart-smoke-output', str(folder),
                   '--smart-smoke-mode', mode, '--smart-addition-source', str(addition),
                   '--smart-music-folder', str(music), '--ui-driver', str(Path(args.ui_driver).resolve())]
        with (run / f'native-{mode}.stdout').open('w') as stdout, (run / f'native-{mode}.stderr').open('w') as stderr:
            completed = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=180)
        assert completed.returncode == 0, f'Native {mode} exited {completed.returncode}'
        payload = json.loads((folder / 'report.json').read_text())
        report['native'].append(payload)
        assert payload['passed'], payload.get('error')

    try:
        start(args.legacy_server or args.server, 6 if args.legacy_server else 9)
        root = client.request('add_root', path=str(music))['root']
        client.wait(lambda s: s['library']['scan']['phase'] == 'complete' and s['library']['tracks'] == 508, seconds=30)
        first = client.request('library_tracks', revision=None, offset=0, limit=500, query='', album_id=None)
        last = client.request('library_tracks', revision=first['revision'], offset=500, limit=500, query='', album_id=None)
        tracks = {Path(row['path']).stem: row for row in first['entries'] + last['entries']}
        expected = [tracks['freedom']['id']] + [tracks[f'Page {n:03}']['id'] for n in range(505)]
        expected_paths = [str(music / 'freedom.flac')] + [str(music / f'Page {n:03}.flac') for n in range(505)]
        regular = client.request('create_playlist', expected_revision=client.state()['playlists_revision'], name='Ordinary Fixture')['playlist']
        regular = client.request('playlist_add', playlist_id=regular['id'], expected_revision=regular['revision'],
                                 library_revision=first['revision'], track_ids=[tracks['freedom']['id'], tracks['blue']['id'], tracks['freedom']['id']], before=None)['playlist']
        kept = all_entries(regular)
        added = client.request('append_track', expected_revision=client.state()['queue_revision'], library_revision=first['revision'], track_id=tracks['freedom']['id'])
        client.request('play', entry_id=added['entry_id'])
        client.wait(lambda s: s['transport'] == 'playing')
        client.request('pause')
        before = client.state()
        kept_queue = queue()
        if args.legacy_server:
            stop()
            start(args.server)
            restored = client.state()
            regular = catalog()['entries'][0]
            assert regular['kind'] == 'regular' and regular['query'] is None and regular['entry_count'] == 3
            assert all_entries(regular) == kept and queue() == kept_queue
            assert (restored['current_id'], restored['position_frames'], restored['queue_revision']) == (before['current_id'], before['position_frames'], before['queue_revision'])
            assert restored['transport'] == 'paused'
            with sqlite3.connect(run / 'state/state.sqlite3') as db:
                assert db.execute('PRAGMA user_version').fetchone()[0] == 7
            record('real-schema3-migration-preserves-ordinary-duplicates-queue-and-checkpoint')
        rule = 'artist:"Miles Davis" & !album:"Kind of Blue"'
        before = client.state()
        client.request('create_smart_playlist', expected_revision=before['playlists_revision'], name='Invalid', query='artist:[]', expect_error='invalid_filter')
        assert client.state()['playlists_revision'] == before['playlists_revision']
        smart = client.request('create_smart_playlist', expected_revision=before['playlists_revision'], name='API Miles', query=rule)['playlist']
        assert smart['kind'] == 'smart' and smart['query'] == rule and smart['entry_count'] is None
        assert [p['id'] for p in catalog('regular')['entries']] == [regular['id']]
        assert [p['id'] for p in catalog('smart')['entries']] == [smart['id']]
        assert [r['track_id'] for r in all_entries(smart)] == expected
        assert entries(smart, 'album:Sessions')['total'] == 505
        assert [r['metadata']['title'] for r in entries(smart, 'title:Freedom | title:"Page 00"')['entries']] == ['Freedom'] + [f'Page {n:03}' for n in range(10)]
        record('durable-rule-catalog-kinds-bounded-results-and-transient-filter-intersection')
        for command, extra in [('playlist_add', dict(library_revision=first['revision'], track_ids=[tracks['blue']['id']], before=None)),
                               ('playlist_remove', dict(entry_ids=[expected[0]])),
                               ('playlist_move', dict(entry_ids=[expected[0]], before=None))]:
            client.request(command, playlist_id=smart['id'], expected_revision=smart['revision'], expect_error='invalid_command', **extra)
        assert [r['track_id'] for r in all_entries(smart)] == expected
        frozen = snapshot(smart, 'API Frozen')['playlist']
        assert frozen['kind'] == 'regular' and frozen['entry_count'] == 506
        frozen_rows = all_entries(frozen)
        assert [r['track_id'] for r in frozen_rows] == expected
        snapshot(smart, 'Ordinary Fixture', expect_error='invalid_command')
        snapshot(smart, 'Stale Catalog', expected_revision=1, expect_error='stale_playlist_revision')
        assert catalog()['total'] == 3
        record('read-only-smart-results-and-atomic-full-result-regular-snapshot-over-500')
        client.request('clear', expected_revision=client.state()['queue_revision'])
        client.request('enqueue_playlist', playlist_id=smart['id'], playlist_revision=smart['revision'], library_revision=first['revision'], expected_revision=client.state()['queue_revision'], play=True)
        client.wait(lambda s: s['transport'] == 'playing' and s['queue_length'] == 506)
        frozen_queue = queue()
        assert [r['path'] for r in frozen_queue] == expected_paths
        state = client.state()
        old_smart = smart
        smart = client.request('update_smart_playlist', playlist_id=smart['id'], expected_revision=smart['revision'], query='artist:"Ornette Coleman"')['playlist']
        assert [r['metadata']['title'] for r in all_entries(smart)] == ['Lonely Woman']
        snapshot(old_smart, 'Stale Rule', expect_error='stale_playlist_revision')
        client.request('update_smart_playlist', playlist_id=smart['id'], expected_revision=smart['revision'], query='album:[', expect_error='invalid_filter')
        assert queue() == frozen_queue and all_entries(frozen) == frozen_rows
        smart = client.request('update_smart_playlist', playlist_id=smart['id'], expected_revision=smart['revision'], query=rule)['playlist']
        catalog_revision = client.state()['playlists_revision']
        shutil.copy2(addition, music / 'api-arrival.flac')
        rescan(509)
        assert entries(smart)['total'] == 507
        assert entries(smart, 'album:Nefertiti')['entries'][0]['metadata']['title'] == 'Sparks'
        assert client.state()['playlists_revision'] == catalog_revision
        assert queue() == frozen_queue and all_entries(frozen) == frozen_rows
        entries(smart, offset=100, library_revision=first['revision'], expect_error='stale_library_revision')
        snapshot(smart, 'Stale Library', library_revision=first['revision'], expect_error='stale_library_revision')
        assert client.state()['queue_revision'] == state['queue_revision']
        record('full-smart-enqueue-and-rule-library-updates-do-not-change-queue-or-regular-snapshot')
        (music / 'api-arrival.flac').unlink()
        removed = music / 'Page 004.flac'
        removed.unlink()
        rescan(507)
        assert entries(smart)['total'] == 505
        assert next(r for r in all_entries(frozen) if r['track_id'] == tracks['Page 004']['id'])['available'] is False
        assert queue() == frozen_queue
        os.link(page_source, removed)
        rescan(508)
        assert [r['track_id'] for r in all_entries(smart)] == expected
        stop()
        start(args.server)
        assert client.state()['transport'] == 'paused'
        assert queue() == frozen_queue and all_entries(frozen) == frozen_rows
        assert next(p for p in catalog()['entries'] if p['id'] == smart['id'])['query'] == rule
        assert [r['track_id'] for r in all_entries(smart)] == expected
        record('missing-files-live-membership-and-restart-preserve-rules-and-independent-copies')
        for playlist in [smart, frozen]:
            client.request('delete_playlist', playlist_id=playlist['id'], expected_revision=playlist['revision'])
        client.request('clear', expected_revision=client.state()['queue_revision'])
        if args.app:
            native('exercise')
            before = client.state()
            native_queue = queue()
            # The smoke left Play All running; the app's quit paused it.
            assert before['transport'] == 'paused' and before['queue_length'] == 1013
            assert all(Path(row['path']).name != 'native-arrival.flac' for row in native_queue)
            named = {p['name']: p for p in catalog()['entries']}
            assert entries(named['Miles without Blue'])['total'] == 507
            assert named['Frozen Miles']['entry_count'] == 506
            assert [r['track_id'] for r in all_entries(named['Frozen Miles'])] == expected
            record('native-import-updates-smart-membership-without-mutating-enqueued-or-frozen-results')
            stop()
            start(args.server)
            assert queue() == native_queue and client.state()['transport'] == 'paused'
            native('restore')
            record('native-and-daemon-relaunch-retain-rules-snapshots-and-paused-queue')
        report['passed'] = True
    except Exception as error:
        report['passed'] = False
        report['failure'] = repr(error)
        raise
    finally:
        stop()
        for log in logs:
            log.close()
        (run / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'report': str(run / 'report.json'), 'passed': report.get('passed', False)}))


if __name__ == '__main__':
    main()
