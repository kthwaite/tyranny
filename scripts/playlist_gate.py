#!/usr/bin/env python3
"""Exercise saved playlists against real daemons and optional displayed SwiftUI.

Uses isolated state and clock-paced PCM capture, never the user's audio device.
An optional prior protocol-v3 executable creates a genuine schema-2 migration
fixture. No mutation retry or mocked persistence/playback path.
"""
import argparse
import json
import os
from pathlib import Path
import selectors
import sqlite3
import subprocess
import sys
import tempfile
import wave

sys.dont_write_bytecode = True
from server_gate import WebSocket


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', default='target/release/tyranny-server')
    parser.add_argument('--legacy-server', help='Prior protocol-v3/schema-2 executable for real migration coverage')
    parser.add_argument('--app', help='Opt-in native app executable; requires an unlocked desktop and AX permission')
    parser.add_argument('--ui-driver', default='target/native-ui-driver')
    parser.add_argument('--work-dir', default='target/playlist-gate')
    args = parser.parse_args()
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=work))
    state = run / 'state'
    music = run / 'Music – 日本語'
    music.mkdir()
    source = run / 'silence.wav'
    with wave.open(str(source), 'wb') as output:
        output.setparams((2, 2, 48000, 0, 'NONE', 'not compressed'))
        block = bytes(48000 * 4)
        for _ in range(120):
            output.writeframesraw(block)
    for index in range(105):
        os.link(source, music / f'Track {index:03}.wav')
    report = {'run_directory': str(run), 'cases': [], 'launches': [], 'native': [],
              'output': 'clock-paced PCM capture',
              'limitations': ['No device output or listening claim',
                              'Generated real WAVs, not a production-scale playlist benchmark',
                              'Native UI is exercised only when --app is supplied']}
    process = None
    connections = []
    logs = []
    ready = None

    def record(name, **details):
        report['cases'].append({'name': name, 'passed': True, **details})
        print('Passed ' + name, flush=True)

    def connect(version=17):
        client = WebSocket(ready['address'], Path(ready['token_path']).read_text().strip(), expected_version=version)
        connections.append(client)
        return client

    def start(binary, version=17):
        nonlocal process, ready
        index = len(report['launches'])
        command = [str(Path(binary).resolve()), '--state-dir', str(state), '--bind', '127.0.0.1:0',
                   '--capture', str(run / f'capture-{index}.f32')]
        log = (run / f'server-{index}.stderr').open('w')
        logs.append(log)
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=log, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(20), 'Daemon startup timed out'
        line = process.stdout.readline()
        assert line, f'Daemon startup failed; see {log.name}'
        ready = json.loads(line)
        report['launches'].append({'command': command, 'ready': ready})
        return connect(version)

    def stop(crash=False):
        nonlocal process
        for client in connections:
            client.close()
        connections.clear()
        if process is not None:
            process.kill() if crash else process.terminate()
            status = process.wait(timeout=15)
            process.stdout.close()
            process = None
            assert status == (-9 if crash else 0), status

    def library(client):
        return client.request('library_tracks', revision=None, offset=0, limit=500, query='', album_id=None)

    def catalog(client, **overrides):
        return client.request('saved_playlists', **dict(dict(revision=None, offset=0, limit=100, query=''), **overrides))

    def entries(client, playlist, **overrides):
        fields = dict(playlist_id=playlist['id'], revision=playlist['revision'],
                      library_revision=None, offset=0, limit=500, query='')
        fields.update(overrides)
        return client.request('playlist_entries', **fields)

    def queue(client):
        return client.request('queue_page', revision=None, offset=0, limit=500)

    def add(client, playlist, ids, revision):
        return client.request('playlist_add', playlist_id=playlist['id'], expected_revision=playlist['revision'],
                              library_revision=revision, track_ids=ids, before=None)['playlist']

    def enqueue(client, playlist, library_revision, play, error=None):
        return client.request('enqueue_playlist', expect_error=error, playlist_id=playlist['id'],
                              playlist_revision=playlist['revision'], library_revision=library_revision,
                              expected_revision=client.state()['queue_revision'], play=play)

    def scan(client, root_id, count):
        client.request('rescan', root_id=root_id)
        client.state()
        client.wait(lambda s: s['library']['scan']['phase'] == 'complete' and s['library']['tracks'] == count, seconds=30)

    def native(client, relaunch=False, drag=False):
        folder = run / ('native-drag' if drag else 'native-relaunch' if relaunch else 'native-initial')
        command = [str(Path(args.app).resolve()), '--url', 'ws://' + ready['address'] + '/ws',
                   '--token-file', ready['token_path'], '--drag-smoke-output' if drag else '--playlist-smoke-output', str(folder),
                   '--ui-driver', str(Path(args.ui_driver).resolve())]
        if relaunch:
            command.append('--playlist-smoke-relaunch')
        with (run / (folder.name + '.stdout')).open('w') as stdout, (run / (folder.name + '.stderr')).open('w') as stderr:
            result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=180)
        payload = json.loads((folder / 'report.json').read_text())
        report['native'].append(payload)
        assert result.returncode == 0 and payload['passed'], payload.get('error', result.returncode)
        return payload

    try:
        old = None
        if args.legacy_server:
            a = start(args.legacy_server, version=3)
        else:
            a = start(args.server)
        root_id = a.request('add_root', path=str(music))['root']['id']
        a.wait(lambda s: s['library']['scan']['phase'] == 'complete' and s['library']['tracks'] == 105, seconds=30)
        tracks = library(a)
        track_ids = [entry['id'] for entry in tracks['entries']]
        assert len(track_ids) == 105
        if args.legacy_server:
            added = a.request('append_track', expected_revision=a.state()['queue_revision'],
                              library_revision=tracks['revision'], track_id=track_ids[0])
            a.request('play', entry_id=added['entry_id'])
            a.request('pause')
            a.request('seek', entry_id=added['entry_id'], position_frames=24000)
            a.request('set_gain', mode='album')
            old = {'queue': queue(a), 'snapshot': a.state()}
            stop()
            a = start(args.server)
            migrated = a.state()
            assert queue(a) == old['queue']
            for field in ['queue_revision', 'current_id', 'position_frames']:
                assert migrated[field] == old['snapshot'][field], field
            for field, value in old['snapshot']['settings'].items():
                assert migrated['settings'][field] == value, field
            assert not migrated['settings']['equalizer']['enabled']
            assert migrated['transport'] == 'paused' and migrated['library']['tracks'] == 105
            record('real-schema2-migration-preserves-library-queue-selection-settings', snapshot=migrated)
        a.request('clear', expected_revision=a.state()['queue_revision'])
        b = connect()
        initial_catalog_revision = a.state()['playlists_revision']
        playlist = a.request('create_playlist', expected_revision=initial_catalog_revision, name='Échos 一')['playlist']
        assert playlist['name'] == 'Échos 一' and playlist['entry_count'] == 0
        empty_queue = queue(a)
        enqueue(a, playlist, tracks['revision'], False, error='invalid_command')
        assert queue(a) == empty_queue
        b.request('create_playlist', expect_error='stale_playlist_revision', expected_revision=initial_catalog_revision, name='Stale')
        a.request('create_playlist', expect_error='invalid_command', expected_revision=a.state()['playlists_revision'], name='éCHOS 一')
        a.request('create_playlist', expect_error='invalid_command', expected_revision=a.state()['playlists_revision'], name='   ')
        other = a.request('create_playlist', expected_revision=a.state()['playlists_revision'], name='Other')['playlist']
        first_catalog = catalog(a, limit=1)
        second_catalog = catalog(a, revision=first_catalog['revision'], offset=1, limit=1)
        assert first_catalog['total'] == 2 and second_catalog['total'] == 2
        assert first_catalog['entries'][0]['id'] != second_catalog['entries'][0]['id']
        assert [item['id'] for item in catalog(a, query='éCHOS 一')['entries']] == [playlist['id']]
        assert catalog(a, query='%')['total'] == 0
        record('catalog-identity-unique-unicode-names-pages-and-stale-create')

        playlist = add(a, playlist, [track_ids[0], track_ids[1], track_ids[0]], tracks['revision'])
        small = entries(a, playlist)
        assert [entry['track_id'] for entry in small['entries']] == [track_ids[0], track_ids[1], track_ids[0]]
        assert len({entry['id'] for entry in small['entries']}) == 3
        b.request('rename_playlist', expect_error='stale_playlist_revision', playlist_id=playlist['id'], expected_revision=1, name='Lost update')
        playlist = a.request('rename_playlist', playlist_id=playlist['id'], expected_revision=playlist['revision'], name='éCHOS 一')['playlist']
        assert playlist['name'] == 'éCHOS 一'
        a.request('saved_playlists', expect_error='stale_playlist_revision', revision=first_catalog['revision'], offset=0, limit=1, query='')
        playlist = add(a, playlist, track_ids, tracks['revision'])
        first = entries(a, playlist, limit=100)
        rest = entries(a, playlist, offset=100, limit=100, library_revision=first['library_revision'])
        ordered = first['entries'] + rest['entries']
        assert first['total'] == 108 and len(first['entries']) == 100 and len(rest['entries']) == 8
        assert [entry['track_id'] for entry in ordered] == [track_ids[0], track_ids[1], track_ids[0]] + track_ids
        record('duplicate-occurrences-ordered-batches-and-revision-bound-entry-pages')

        playlist = a.request('playlist_move', playlist_id=playlist['id'], expected_revision=playlist['revision'],
                             entry_ids=[ordered[-1]['id']], before=ordered[0]['id'])['playlist']
        ordered = [ordered[-1]] + ordered[:-1]
        assert entries(a, playlist)['entries'] == ordered
        remove_ids = [ordered[1]['id'], ordered[3]['id']]
        playlist = a.request('playlist_remove', playlist_id=playlist['id'], expected_revision=playlist['revision'], entry_ids=remove_ids)['playlist']
        ordered = [entry for entry in ordered if entry['id'] not in remove_ids]
        assert entries(a, playlist)['entries'] == ordered
        other = add(a, other, [track_ids[2]], tracks['revision'])
        foreign_id = entries(a, other)['entries'][0]['id']
        a.request('playlist_move', expect_error='invalid_command', playlist_id=playlist['id'], expected_revision=playlist['revision'], entry_ids=[foreign_id], before=None)
        a.request('playlist_remove', expect_error='invalid_command', playlist_id=playlist['id'], expected_revision=playlist['revision'], entry_ids=[ordered[0]['id'], foreign_id])
        assert entries(a, playlist)['entries'] == ordered
        record('reorder-remove-and-cross-playlist-atomic-rejection')

        before_queue = queue(a)
        with sqlite3.connect(state / 'state.sqlite3') as database:
            database.execute("CREATE TRIGGER reject_playlist_queue BEFORE INSERT ON queue BEGIN SELECT RAISE(ABORT, 'playlist gate commit failure'); END")
        enqueue(a, playlist, tracks['revision'], False, error='persistence')
        assert queue(a) == before_queue
        with sqlite3.connect(state / 'state.sqlite3') as database:
            database.execute('DROP TRIGGER reject_playlist_queue')
        result = enqueue(a, playlist, tracks['revision'], False)
        appended = queue(a)
        assert result['snapshot']['queue_revision'] == before_queue['revision'] + 1
        assert [entry['track_id'] for entry in appended['entries']] == [entry['track_id'] for entry in ordered]
        assert result['snapshot']['transport'] == 'stopped'
        b.request('enqueue_playlist', expect_error='stale_revision', playlist_id=playlist['id'],
                  playlist_revision=playlist['revision'], library_revision=tracks['revision'],
                  expected_revision=before_queue['revision'], play=False)
        stale_playlist = dict(playlist, revision=small['revision'])
        enqueue(a, stale_playlist, tracks['revision'], False, error='stale_playlist_revision')
        assert queue(a) == appended
        played = enqueue(a, playlist, tracks['revision'], True)
        a.wait(lambda s: s['transport'] == 'playing' and s['current_id'] == played['entry_id'])
        a.request('pause')
        a.request('seek', entry_id=played['entry_id'], position_frames=12345)
        preserved_queue, preserved_state = queue(a), a.state()
        assert preserved_queue['total'] == len(ordered) * 2
        assert len({entry['id'] for entry in preserved_queue['entries']}) == preserved_queue['total']
        record('atomic-enqueue-play-and-durable-commit-failure', snapshot=preserved_state)

        bad_path = Path(ordered[0]['path'])
        backup = run / 'temporarily-unavailable.wav'
        bad_path.rename(backup)
        try:
            bad_path.write_bytes(b'not audio')
            enqueue(a, playlist, tracks['revision'], True, error='playback')
            assert queue(a) == preserved_queue
            unchanged = a.state()
            assert (unchanged['current_id'], unchanged['position_frames'], unchanged['transport']) == (preserved_state['current_id'], 12345, 'paused')
        finally:
            bad_path.unlink(missing_ok=True)
            backup.rename(bad_path)
        record('unplayable-first-source-cannot-partially-enqueue-or-change-selection')

        bad_path.rename(backup)
        try:
            scan(a, root_id, 104)
            updated_library = a.state()['library']['revision']
            a.request('playlist_entries', expect_error='stale_library_revision', playlist_id=playlist['id'], revision=playlist['revision'], library_revision=tracks['revision'], offset=0, limit=100, query='')
            unavailable = entries(a, playlist)
            assert unavailable['entries'][0]['available'] is False
            assert unavailable['entries'][0]['id'] == ordered[0]['id']
            enqueue(a, playlist, updated_library, False, error='invalid_command')
            assert queue(a) == preserved_queue
        finally:
            backup.rename(bad_path)
        scan(a, root_id, 105)
        tracks = library(a)
        assert entries(a, playlist)['entries'][0]['available'] is True
        record('unavailable-entry-retained-and-load-rejected-without-partial-queue')

        saved_catalog, saved_entries = catalog(a), entries(a, playlist)
        stop(crash=True)
        a = start(args.server)
        assert catalog(a) == saved_catalog
        assert entries(a, playlist) == saved_entries
        assert queue(a) == preserved_queue
        restored = a.state()
        assert restored['transport'] == 'paused' and restored['current_id'] == preserved_state['current_id'] and restored['position_frames'] == 12345
        record('crash-restart-preserves-playlist-order-identities-and-paused-queue', snapshot=restored)
        for item in catalog(a)['entries']:
            a.request('delete_playlist', playlist_id=item['id'], expected_revision=item['revision'])
        assert catalog(a)['total'] == 0 and queue(a) == preserved_queue
        created = a.request('create_playlist', expected_revision=a.state()['playlists_revision'], name='Temporary')['playlist']
        assert created['id'] > max(playlist['id'], other['id'])
        a.request('delete_playlist', playlist_id=created['id'], expected_revision=created['revision'])
        record('deleting-saved-playlists-does-not-edit-queue-or-recycle-identities')
        a.request('clear', expected_revision=a.state()['queue_revision'])
        large = a.request('create_playlist', expected_revision=a.state()['playlists_revision'], name='Beyond one server page')['playlist']
        large = add(a, large, [track_ids[0]] * 500, tracks['revision'])
        large = add(a, large, [track_ids[1]], tracks['revision'])
        loaded_large = enqueue(a, large, tracks['revision'], False)
        assert loaded_large['snapshot']['queue_length'] == 501
        first_queue_page = queue(a)
        last_queue_page = a.request('queue_page', revision=first_queue_page['revision'], offset=500, limit=100)
        assert len(first_queue_page['entries']) == 500 and len(last_queue_page['entries']) == 1
        assert last_queue_page['entries'][0]['track_id'] == track_ids[1]
        a.request('delete_playlist', playlist_id=large['id'], expected_revision=large['revision'])
        a.request('clear', expected_revision=a.state()['queue_revision'])
        record('entire-501-entry-playlist-loads-without-client-hydration-or-page-size-truncation')

        # Drag commands cross the same real owner/SQLite/engine seam as native drops.
        tracks = library(a)
        track_ids = [track['id'] for track in tracks['entries']]
        inserted = a.request('insert_tracks', expected_revision=a.state()['queue_revision'],
                             library_revision=tracks['revision'], track_ids=track_ids[:6], before=None)
        original = queue(a)['entries']
        before_revision = inserted['snapshot']['queue_revision']
        moved = a.request('move', expected_revision=before_revision,
                          entry_ids=[original[3]['id'], original[1]['id']], before=original[0]['id'])
        ordered = [original[i] for i in [1, 3, 0, 2, 4, 5]]
        assert queue(a)['entries'] == ordered
        assert moved['snapshot']['queue_revision'] == before_revision + 1
        unchanged = queue(a)
        no_op = a.request('move', expected_revision=unchanged['revision'],
                          entry_ids=[ordered[0]['id'], ordered[1]['id']], before=ordered[1]['id'])
        assert no_op['snapshot']['queue_revision'] == unchanged['revision']
        a.request('move', expect_error='stale_revision', expected_revision=before_revision,
                  entry_ids=[ordered[0]['id']], before=None)
        a.request('move', expect_error='invalid_command', expected_revision=unchanged['revision'],
                  entry_ids=[ordered[0]['id'], 9223372036854775807], before=None)
        assert queue(a) == unchanged
        record('batch-queue-move-source-order-noop-and-all-or-nothing-rejection')

        a.request('insert_tracks', expect_error='invalid_command', expected_revision=unchanged['revision'],
                  library_revision=tracks['revision'], track_ids=[track_ids[0]], before=9223372036854775807)
        a.request('insert_tracks', expect_error='stale_library_revision', expected_revision=unchanged['revision'],
                  library_revision=tracks['revision'] - 1, track_ids=[track_ids[0]], before=None)
        with sqlite3.connect(state / 'state.sqlite3') as database:
            database.execute("CREATE TRIGGER reject_drag_insert BEFORE INSERT ON queue BEGIN SELECT RAISE(ABORT,'drag rollback probe'); END")
        try:
            a.request('insert_tracks', expect_error='persistence', expected_revision=unchanged['revision'],
                      library_revision=tracks['revision'], track_ids=track_ids[:2], before=ordered[2]['id'])
        finally:
            with sqlite3.connect(state / 'state.sqlite3') as database:
                database.execute('DROP TRIGGER reject_drag_insert')
        assert queue(a) == unchanged
        added = a.request('insert_tracks', expected_revision=unchanged['revision'],
                          library_revision=tracks['revision'], track_ids=[track_ids[0], track_ids[0]], before=ordered[2]['id'])
        with_duplicates = queue(a)['entries']
        assert with_duplicates[:2] == ordered[:2] and with_duplicates[4:] == ordered[2:]
        assert [row['track_id'] for row in with_duplicates[2:4]] == [track_ids[0], track_ids[0]]
        assert len({row['id'] for row in with_duplicates}) == 8
        assert with_duplicates[2]['id'] == added['entry_id']
        assert added['snapshot']['transport'] != 'playing'
        record('atomic-library-insertion-anchor-duplicates-and-storage-rollback')

        drag_playlist = a.request('create_playlist', expected_revision=a.state()['playlists_revision'], name='Batch drag API')['playlist']
        drag_playlist = add(a, drag_playlist, track_ids, tracks['revision'])
        original_saved = entries(a, drag_playlist)['entries']
        chosen = [original_saved[index]['id'] for index in [1, 3, 101]]
        anchor = original_saved[104]['id']
        drag_playlist = a.request('playlist_move', playlist_id=drag_playlist['id'], expected_revision=drag_playlist['revision'],
                                  entry_ids=list(reversed(chosen)), before=anchor)['playlist']
        expected_saved = [row for row in original_saved if row['id'] not in chosen]
        anchor_index = next(index for index, row in enumerate(expected_saved) if row['id'] == anchor)
        expected_saved[anchor_index:anchor_index] = [row for row in original_saved if row['id'] in chosen]
        assert entries(a, drag_playlist)['entries'] == expected_saved
        unchanged_saved = entries(a, drag_playlist)
        a.request('playlist_move', expect_error='invalid_command', playlist_id=drag_playlist['id'], expected_revision=drag_playlist['revision'],
                  entry_ids=[chosen[0], 9223372036854775807], before=anchor)
        assert entries(a, drag_playlist) == unchanged_saved
        drag_playlist = a.request('playlist_add', playlist_id=drag_playlist['id'], expected_revision=drag_playlist['revision'],
                                  library_revision=tracks['revision'], track_ids=[track_ids[0], track_ids[0]], before=anchor)['playlist']
        after_saved = entries(a, drag_playlist)['entries']
        assert after_saved[:-3] == expected_saved[:-1] and after_saved[-1] == expected_saved[-1]
        assert [row['track_id'] for row in after_saved[-3:-1]] == [track_ids[0], track_ids[0]]
        assert len({row['id'] for row in after_saved}) == 107
        assert queue(a)['entries'] == with_duplicates
        record('batch-saved-move-across-page-boundary-and-anchored-duplicate-insert')
        a.request('delete_playlist', playlist_id=drag_playlist['id'], expected_revision=drag_playlist['revision'])
        a.request('clear', expected_revision=a.state()['queue_revision'])

        if args.app:
            native(a, drag=True)
            paused_drag = a.request('pause')['snapshot']
            drag_queue = queue(a)['entries']
            drag_catalog = catalog(a)
            drag_entries = {item['id']: entries(a, item)['entries'] for item in drag_catalog['entries']}
            stop()
            a = start(args.server)
            assert queue(a)['entries'] == drag_queue
            assert a.state()['current_id'] == paused_drag['current_id'] and a.state()['transport'] == 'paused'
            assert catalog(a) == drag_catalog
            for item in drag_catalog['entries']:
                assert entries(a, item)['entries'] == drag_entries[item['id']]
            record('native-drag-order-and-occurrence-identities-survive-restart')
            a.request('clear', expected_revision=a.state()['queue_revision'])
            for item in catalog(a)['entries']:
                a.request('delete_playlist', playlist_id=item['id'], expected_revision=item['revision'])
            native(a)
            a.request('pause')
            kept = catalog(a)
            assert kept['total'] >= 1
            kept_entries = {item['id']: entries(a, item) for item in kept['entries']}
            assert any(page['total'] > 0 for page in kept_entries.values())
            stop()
            a = start(args.server)
            assert catalog(a) == kept
            for item in kept['entries']:
                assert entries(a, item) == kept_entries[item['id']]
            native(a, relaunch=True)
            record('native-created-playlist-survives-daemon-and-controller-restart', playlists=kept)
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
