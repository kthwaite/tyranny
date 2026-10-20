#!/usr/bin/env python3
"""Exercise structured filters through real indexed audio and optional native UI.

Owns isolated state and clock-paced capture; never touches the user's daemon.
"""
import argparse
import json
import os
from pathlib import Path
import selectors
import subprocess
import sys
import tempfile

sys.dont_write_bytecode = True
from server_gate import WebSocket


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', default='target/release/tyranny-server')
    parser.add_argument('--app')
    parser.add_argument('--ui-driver', default='target/native-ui-driver')
    parser.add_argument('--work-dir', default='target/filter-gate')
    args = parser.parse_args()
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix='run-', dir=work))
    music = run / 'Music – 日本語'
    music.mkdir()
    specifications = {
        'blue': ('So What', 'Miles Davis', 'Kind of Blue', 'Miles Davis'),
        'freedom': ('Freedom', 'Miles Davis', 'Bitches Brew', 'Miles Davis'),
        'ornette': ('Lonely Woman', 'Ornette Coleman', 'Jazz Sampler', 'Various Artists'),
        'compilation': ('Companion', 'Miles Davis', 'Jazz Sampler', 'Various Artists'),
        'joga': ('Jóga', 'Björk', 'Homogenic', 'Björk'),
        'symbols': ('He said "yes" \\ %_', 'Earth, Wind & Fire', 'Symbols', 'Earth, Wind & Fire'),
        'giant': ('Giant Steps', 'John Coltrane', 'Giant Steps', 'John Coltrane'),
    }

    def audio(path, title, artist, album, album_artist):
        command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin',
                   '-f', 'lavfi', '-i', 'anullsrc=r=48000:cl=stereo', '-t', '120',
                   '-c:a', 'flac', '-metadata', 'artist=' + artist,
                   '-metadata', 'album=' + album, '-metadata', 'album_artist=' + album_artist]
        if title is not None:
            command += ['-metadata', 'title=' + title]
        subprocess.run(command + [str(path)], check=True, timeout=30)

    for name, metadata in specifications.items():
        audio(music / (name + '.flac'), *metadata)
    page_source = run / 'page-source.flac'
    audio(page_source, None, 'Miles Davis', 'Sessions', 'Miles Davis')
    for index in range(105):
        os.link(page_source, music / f'Page {index:03}.flac')

    report = {'run_directory': str(run), 'cases': [], 'output': 'clock-paced PCM capture',
              'limitations': ['Generated tagged real FLACs, not a large-collection latency benchmark',
                              'No user database, daemon, audio device or listening involved']}
    process = None
    client = None
    log = (run / 'server.stderr').open('w')

    def record(name):
        report['cases'].append({'name': name, 'passed': True})
        print('Passed ' + name, flush=True)

    try:
        command = [str(Path(args.server).resolve()), '--state-dir', str(run / 'state'),
                   '--bind', '127.0.0.1:0', '--capture', str(run / 'capture.f32')]
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=log, text=True)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(20), 'Server startup timed out'
        ready = json.loads(process.stdout.readline())
        report['launch'] = {'command': command, 'ready': ready}
        client = WebSocket(ready['address'], Path(ready['token_path']).read_text().strip())
        client.request('add_root', path=str(music))
        client.wait(lambda s: s['library']['scan']['phase'] == 'complete' and s['library']['tracks'] == 112, seconds=30)

        def library(query='', command='library_tracks', **overrides):
            fields = dict(revision=None, offset=0, limit=500, query=query)
            if command == 'library_tracks':
                fields['album_id'] = None
            elif command == 'library_albums':
                fields['artist_id'] = None
            fields.update(overrides)
            return client.request(command, **fields)

        all_tracks = library()
        tracks = {Path(entry['path']).stem: entry for entry in all_tracks['entries']}
        assert len(tracks) == 112
        for name, values in specifications.items():
            actual = tracks[name]['metadata']
            assert (actual['title'], actual['artist'], actual['album'], actual['album_artist']) == values
        revision = all_tracks['revision']
        page_ids = [tracks[f'Page {index:03}']['id'] for index in range(105)]

        def ids(query, **overrides):
            return {row['id'] for row in library(query, **overrides)['entries']}

        expected = set(page_ids + [tracks['freedom']['id'], tracks['compilation']['id']])
        query = 'artist:"Miles Davis" & !album:"Kind of Blue"'
        assert ids(query) == expected
        assert ids('artist:"Miles Davis" AND NOT album:"Kind of Blue"') == expected
        record('symbolic-and-not-and-word-equivalence-on-real-metadata')
        alternatives = 'artist:["Miles Davis", "Ornette Coleman"] & !album:["Sessions", "Kind of Blue"]'
        three = {tracks[name]['id'] for name in ['freedom', 'compilation', 'ornette']}
        assert ids(alternatives) == three
        assert ids('(artist:"Miles Davis" | artist:"Ornette Coleman") & !album:["Sessions", "Kind of Blue"]') == three
        assert ids('artist:"Ornette Coleman" | artist:"Miles Davis" & album:"Kind of Blue"') == {tracks['ornette']['id'], tracks['blue']['id']}
        record('list-alternatives-negated-lists-grouping-and-operator-precedence')
        assert ids('artist:"BJÖRK"') == {tracks['joga']['id']}
        assert ids('title:"%_"') == {tracks['symbols']['id']}
        assert ids(r'title:"He said \"yes\" \\ %_"') == {tracks['symbols']['id']}
        assert ids('title:"\' OR 1=1 --"') == set()
        assert ids('Miles Freedom') == {tracks['freedom']['id']}
        record('unicode-substrings-quoted-escapes-implicit-and-and-literal-sql-metacharacters')
        artists = library(query, 'library_artists')
        assert {row['name'] for row in artists['entries']} == {'Miles Davis', 'Various Artists'}
        albums = library(query, 'library_albums')
        assert {row['title']: row['track_count'] for row in albums['entries']} == {'Bitches Brew': 1, 'Jazz Sampler': 1, 'Sessions': 105}
        for command in ['library_tracks', 'library_artists', 'library_albums']:
            assert library('artist:"Miles Davis" & title:"Lonely Woman"', command)['total'] == 0
        record('compilation-artist-projections-use-one-matching-track-per-expression')
        first = library('album:Sessions', limit=100)
        last = library('album:Sessions', revision=revision, offset=100, limit=100)
        assert first['total'] == last['total'] == 105
        assert [row['id'] for row in first['entries'] + last['entries']] == page_ids
        library('album:Sessions', revision=revision - 1, offset=100, expect_error='stale_library_revision')
        record('bounded-filtered-library-pages-and-revision-rejection')
        before = client.state()
        for invalid in ['artist:[]', 'artist:["Miles Davis",]', 'artist:["Miles Davis"',
                        '(artist:Miles | album:Sessions', 'artist:Miles &', 'rating:5',
                        '!' * 80 + 'title:Freedom', 'x' * 1025]:
            for command in ['library_tracks', 'library_artists', 'library_albums']:
                library(invalid, command, expect_error='invalid_filter')
        after = client.state()
        assert (after['queue_revision'], after['playlists_revision'], after['library']['revision']) == (before['queue_revision'], before['playlists_revision'], before['library']['revision'])
        record('malformed-unknown-field-and-bounded-complexity-errors-do-not-mutate-state')

        playlist = client.request('create_playlist', expected_revision=after['playlists_revision'], name='Filter Fixture')['playlist']
        ordered_ids = [page_ids[0], tracks['blue']['id'], page_ids[1], tracks['freedom']['id'], page_ids[2], tracks['ornette']['id']]
        ordered_ids += page_ids[3:] + [tracks[name]['id'] for name in ['compilation', 'joga', 'symbols', 'giant', 'freedom']]
        playlist = client.request('playlist_add', playlist_id=playlist['id'], expected_revision=playlist['revision'], library_revision=revision, track_ids=ordered_ids, before=None)['playlist']

        def entries(query='', **overrides):
            fields = dict(playlist_id=playlist['id'], revision=playlist['revision'], library_revision=revision, offset=0, limit=500, query=query)
            fields.update(overrides)
            return client.request('playlist_entries', **fields)

        full = entries()
        matching = [row for row in full['entries'] if row['track_id'] in page_ids]
        one, two = entries('album:Sessions', limit=100), entries('album:Sessions', offset=100, limit=100)
        assert one['total'] == two['total'] == 105 and one['entries'] + two['entries'] == matching
        duplicates = entries('title:Freedom')['entries']
        assert [row['track_id'] for row in duplicates] == [tracks['freedom']['id']] * 2
        assert duplicates[0]['id'] != duplicates[1]['id']
        entries('artist:[]', expect_error='invalid_filter')
        entries('album:Sessions', offset=100, library_revision=revision - 1, expect_error='stale_library_revision')
        assert entries() == full
        record('saved-filter-pages-retain-duplicate-occurrences-order-and-read-only-semantics')

        if args.app:
            folder = run / 'native'
            command = [str(Path(args.app).resolve()), '--url', 'ws://' + ready['address'] + '/ws',
                       '--token-file', ready['token_path'], '--filter-smoke-output', str(folder),
                       '--ui-driver', str(Path(args.ui_driver).resolve())]
            with (run / 'native.stdout').open('w') as stdout, (run / 'native.stderr').open('w') as stderr:
                result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=180)
            assert result.returncode == 0, f'Native app exited {result.returncode}; see native.stderr'
            payload = json.loads((folder / 'report.json').read_text())
            report['native'] = payload
            assert payload['passed'], payload.get('error')
            assert client.state()['transport'] == 'paused'
            record('native-filter-edits-and-controller-quit-pauses-daemon-playback')
        report['passed'] = True
    except Exception as error:
        report['passed'] = False
        report['failure'] = repr(error)
        raise
    finally:
        if client is not None:
            client.close()
        if process is not None:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=15)
            process.stdout.close()
        log.close()
        (run / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'report': str(run / 'report.json'), 'passed': report.get('passed', False)}))


if __name__ == '__main__':
    main()
