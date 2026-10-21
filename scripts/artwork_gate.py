#!/usr/bin/env python3
"""Artwork through real tagged files, isolated daemon, public provider and native controls."""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path
import selectors
import struct
import subprocess
import sys
import time
import zlib

sys.dont_write_bytecode = True
from server_gate import UnixSocket, run_directory


def png(path, colour):
    def chunk(kind, payload):
        return struct.pack('>I', len(payload)) + kind + payload + struct.pack('>I', zlib.crc32(kind + payload))
    pixels = (b'\0' + bytes(colour) * 1200) * 800
    path.write_bytes(b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', 1200, 800, 8, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(pixels)) + chunk(b'IEND', b''))


def audio(path, title, album, image=None, release=None):
    command = ['ffmpeg', '-hide_banner', '-loglevel', 'error', '-nostdin', '-f', 'lavfi', '-i', 'anullsrc=r=48000:cl=stereo']
    if image:
        command += ['-i', str(image), '-map', '0:a', '-map', '1:v', '-c:v', 'copy', '-disposition:v', 'attached_pic']
    command += ['-t', '120', '-c:a', 'flac', '-metadata', 'artist=Artwork Fixture', '-metadata', 'album_artist=Artwork Fixture', '-metadata', 'album=' + album, '-metadata', 'title=' + title]
    if release:
        command += ['-metadata', 'MUSICBRAINZ_ALBUMID=' + release]
    subprocess.run(command + [str(path)], check=True, timeout=30)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', default='target/release/tyranny-server')
    parser.add_argument('--app')
    parser.add_argument('--ui-driver', default='target/native-ui-driver')
    parser.add_argument('--release-id', default='76df3287-6cda-33eb-8e9a-044b5e15ffdd')
    parser.add_argument('--work-dir', default='target/artwork/gate')
    args = parser.parse_args()
    work = Path(args.work_dir).resolve()
    work.mkdir(parents=True, exist_ok=True)
    run = run_directory(work)
    music = run / 'Music'
    music.mkdir()
    colours = {}
    for name, rgb in [('red', (255, 0, 0)), ('green', (0, 255, 0)), ('blue', (0, 0, 255))]:
        colours[name] = run / (name + '.png')
        png(colours[name], rgb)
    folders = {}
    for name in ['Embedded', 'Folder', 'Missing', 'Broken Folder', 'Online']:
        folder = music / name
        folder.mkdir()
        folders[name] = folder
        audio(folder / 'a.flac', name + ' Track', name + ' Album', release=args.release_id if name == 'Online' else None)
    audio(folders['Embedded'] / 'z.flac', 'Embedded Later Track', 'Embedded Album', image=colours['red'])
    (folders['Embedded'] / 'cover.jpg').write_bytes(colours['blue'].read_bytes())
    (folders['Folder'] / 'CoVeR.PNG').write_bytes(colours['blue'].read_bytes())
    (folders['Broken Folder'] / 'cover.jpg').write_bytes(b'not an image')
    (folders['Broken Folder'] / 'folder.png').write_bytes(colours['green'].read_bytes())
    direct = run / 'direct.flac'
    audio(direct, 'Direct Track', 'Unindexed Album', image=colours['red'])
    report = {'run_directory': str(run), 'cases': [], 'launches': [], 'native': [], 'artwork': {}, 'limitations': ['Generated silent FLAC; isolated clock-paced PCM capture, not user audio or production-library performance.']}
    process = client = ready = None
    logs = []

    def record(name):
        report['cases'].append(name)
        print('Passed ' + name, flush=True)

    def start(offline=False):
        nonlocal process, client, ready
        index = len(report['launches'])
        command = [str(Path(args.server).resolve()), '--state-dir', str(run / 'state'), '--capture', str(run / f'capture-{index}.f32')]
        environment = os.environ.copy()
        if offline:
            for key in ['HTTP_PROXY', 'HTTPS_PROXY', 'ALL_PROXY', 'http_proxy', 'https_proxy', 'all_proxy']:
                environment[key] = 'http://127.0.0.1:9'
            environment.update(NO_PROXY='', no_proxy='')
        log = (run / f'server-{index}.stderr').open('w')
        logs.append(log)
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=log, text=True, env=environment)
        with selectors.DefaultSelector() as selector:
            selector.register(process.stdout, selectors.EVENT_READ)
            assert selector.select(20), 'Daemon startup timed out'
        ready = json.loads(process.stdout.readline())
        report['launches'].append({'command': command, 'ready': ready, 'offline_proxy': offline})
        client = UnixSocket(ready['socket_path'])

    def stop():
        nonlocal client, process
        if client:
            client.close()
            client = None
        if process:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=15)
            process.stdout.close()
            process = None

    def albums():
        result = client.request('library_albums', revision=None, offset=0, limit=100, query='', artist_id=None)
        return {item['title']: item for item in result['entries']}

    def art(album=None, entry=None, size='display', **overrides):
        fields = dict(album_id=album, entry_id=entry, library_revision=client.state()['library']['revision'], size=size)
        fields.update(overrides)
        return client.request('artwork', **fields)

    def verify(name, response, source, channel=None, size=512):
        image = response['artwork']
        assert image['status'] == 'ready' and image['source'] == source, image
        data = base64.b64decode(image['data'], validate=True)
        assert data[:2] == b'\xff\xd8' and image['mime_type'] == 'image/jpeg' and len(data) < 2 * 1024 * 1024
        assert 0 < image['width'] <= size and 0 < image['height'] <= size
        probe = subprocess.run(['ffprobe', '-v', 'error', '-show_entries', 'stream=width,height,codec_name', '-of', 'json', '-i', 'pipe:0'], input=data, capture_output=True, check=True, timeout=10)
        stream = json.loads(probe.stdout)['streams'][0]
        assert stream['width'] == image['width'] and stream['height'] == image['height'] and stream['codec_name'] == 'mjpeg'
        if channel is not None:
            pixel = subprocess.run(['ffmpeg', '-v', 'error', '-i', 'pipe:0', '-vf', 'scale=1:1:flags=area', '-frames:v', '1', '-pix_fmt', 'rgb24', '-f', 'rawvideo', 'pipe:1'], input=data, capture_output=True, check=True, timeout=10).stdout
            assert len(pixel) == 3 and pixel[channel] > 220 and all(value < 30 for index, value in enumerate(pixel) if index != channel), list(pixel)
        report['artwork'][name] = {key: value for key, value in image.items() if key != 'data'} | {'sha256': hashlib.sha256(data).hexdigest(), 'encoded_bytes': len(data)}
        return image['cache_key']

    def rescan(total):
        before = client.state()['library']
        client.request('rescan', root_id=root['id'])
        if before['tracks'] == total:
            # v9: the revision is the content revision and does not move when only cover files change.
            client.wait(lambda state: state['library']['scan']['phase'] == 'complete' and state['library']['tracks'] == total, seconds=30)
            time.sleep(2)
            return
        client.wait(lambda state: state['library']['revision'] > before['revision'] and state['library']['scan']['phase'] == 'complete' and state['library']['tracks'] == total, seconds=30)

    def native(mode):
        folder = run / ('native-' + mode)
        command = [str(Path(args.app).resolve()), '--socket', ready['socket_path'], '--artwork-smoke-output', str(folder), '--artwork-smoke-mode', mode, '--artwork-music-folder', str(music), '--artwork-green-source', str(colours['green']), '--ui-driver', str(Path(args.ui_driver).resolve())]
        with (run / f'native-{mode}.stdout').open('w') as stdout, (run / f'native-{mode}.stderr').open('w') as stderr:
            result = subprocess.run(command, stdout=stdout, stderr=stderr, timeout=180)
        assert result.returncode == 0, result.returncode
        payload = json.loads((folder / 'report.json').read_text())
        report['native'].append(payload)
        assert payload['passed'], payload.get('error')

    try:
        start()
        root = client.request('add_root', path=str(music))['root']
        client.wait(lambda state: state['library']['scan']['phase'] == 'complete' and state['library']['tracks'] == 6, seconds=30)
        catalog = albums()
        embedded = catalog['Embedded Album']['id']
        folder = catalog['Folder Album']['id']
        online = catalog['Online Album']['id']
        verify('embedded', art(embedded), 'embedded', 0)
        verify('thumbnail', art(embedded, size='thumbnail'), 'embedded', 0, 128)
        verify('folder', art(folder), 'folder', 2)
        verify('corrupt-folder-fallback', art(catalog['Broken Folder Album']['id']), 'folder', 1)
        assert art(catalog['Missing Album']['id'])['artwork']['status'] == 'missing'
        record('all-embedded-sources-before-folder-case-insensitive-names-corruption-fallback-bounded-jpeg-and-missing')
        online_key = verify('online', art(online), 'cover_art_archive')
        record('real-release-id-only-cover-art-archive-fetch')
        client.request('append', path=str(direct), expected_revision=client.state()['queue_revision'])
        entry = client.request('queue_page', revision=None, offset=0, limit=100)['entries'][0]
        assert entry['track_id'] is None
        verify('unindexed-entry', art(entry=entry['id']), 'embedded', 0)
        record('unindexed-durable-queue-occurrence-discovers-local-artwork')
        before = client.state()
        art(expect_error='invalid_command')
        art(album=embedded, entry=entry['id'], expect_error='invalid_command')
        art(album=embedded, library_revision=2**64 - 1, expect_error='stale_library_revision')
        after = client.state()
        assert (after['queue_revision'], after['current_id'], after['error']) == (before['queue_revision'], before['current_id'], before['error'])
        client.request('clear', expected_revision=client.state()['queue_revision'])
        art(entry=entry['id'], expect_error='invalid_command')
        record('invalid-stale-and-removed-artwork-targets-do-not-mutate-playback')
        if args.app:
            native('exercise')
        else:
            (folders['Folder'] / 'CoVeR.PNG').write_bytes(colours['green'].read_bytes())
            (folders['Missing'] / 'folder.png').write_bytes(colours['green'].read_bytes())
            rescan(6)
        verify('folder-refreshed', art(folder), 'folder', 1)
        verify('missing-refreshed', art(catalog['Missing Album']['id']), 'folder', 1)
        record('rescan-refreshes-folder-only-change-and-cached-local-miss')
        state = client.state()
        stop()
        probe_folder = music / 'Offline Probe'
        probe_folder.mkdir()
        audio(probe_folder / 'a.flac', 'Offline Probe Track', 'Offline Probe Album', release='99b09d02-9cc9-3fed-8431-f162165a9371')
        start(offline=True)
        assert client.state()['queue_length'] == state['queue_length']
        rescan(7)
        catalog = albums()
        assert verify('online-offline-restart', art(catalog['Online Album']['id']), 'cover_art_archive') == online_key
        assert art(catalog['Offline Probe Album']['id'])['artwork']['status'] == 'unavailable'
        record('persistent-positive-online-cache-survives-offline-restart-and-rescan-uncached-network-fails')
        if args.app:
            native('restore')
            record('native-offline-album-and-now-playing-images-with-independent-transport')
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
