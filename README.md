<h1 align="center">
  <img src="macos/Resources/AppIcon.svg" alt="tyranny likes smoke app icon" width="128" height="128"><br>
  tyranny likes smoke
</h1>

_tyranny likes smoke_ is an mpd-style media player with a Rust server and (currently only) a SwiftUI frontend.
With an interface based loosely on classic iTunes, _tyranny likes smoke_ borrows features from a variety of
music players to form a piece of highly-personalised software that satisfies my own peculiar desires.

## Features

- **Local music library** — folder import with scan progress, artist/album/track
  browsing, live filtering, compilation-aware album grouping, and persistent
  removal of selected songs without deleting audio files.
- **Playback** — gapless audio, ReplayGain, seeking, a persistent queue; macOS
  system playback controls.
- **Playlists** — editable regular playlists and rule-based smart playlists, with
  Boolean filters, grouping, etc.
- **Artwork** — embedded and album-folder images, with configurable Cover Art
  Archive, Deezer, and Discogs lookup, retention, and attribution.
- **Internet radio** — direct HTTP(S) MP3/Ogg streams, saved stations, ICY titles,
  station discovery through radio-browser.info.
- **Parametric equalizer** — up to sixteen configurable bands, preamp, presets,
  and persistent server-owned settings.
- **Tracker music** — MOD, XM, IT, and S3M through libopenmpt, alongside the
  FFmpeg-backed audio decoder.
- **Likes and listening history** — liked-only filtering, listening statistics,
  and yearly Wrapped views.
- **Sony Walkman sync** — browse the NW-A306 over USB, drag songs to copy without
  duplicates, remove selected device copies, and see storage usage and synced
  status in library columns.

### Likes

Hearts show server-confirmed values, not optimistic toggles. The client fetches the
new snapshot after the reply instead of waiting for the 250 ms state broadcast.
Outside a scan, list edits refresh immediately rather than waiting for the scan's
500 ms batching timer.
Heart changes preserve native row views and selection; unchanged group layouts no
longer rebuild the table. Album actions use the revision of their displayed rows.
The server reuses duration/size totals for a like only when no unpublished library
work exists.

Reproduce native timing and flicker checks on an isolated three-track fixture
(requires `ffmpeg`, a debug client, and Accessibility/Screen Recording permission):

```sh
python3 scripts/likes_gate.py --output target/likes-after --check
python3 scripts/likes_gate.py --server-only --output target/likes-server-after
```

The native report measures ten repeated toggles per ungrouped, grouped, and expanded
album table, including AppKit's `performClick` animation; surrounding checks use
actual pointer clicks and verify liked-only removal, filtering, selection, and
column visibility. The server-only report uses 30 writes against the synthetic
50,000-track search fixture. Neither mode touches the user's library.

Measured on Apple Silicon, 2026-11-14 (median milliseconds):

| Measurement | Before | After |
| --- | ---: | ---: |
| Ungrouped heart visible, including click animation | 366.23 | 125.59 |
| Grouped heart visible, including click animation | 365.92 | 130.52 |
| Server like write, 50,000 tracks | 37.03 | 0.11 |

Grouped toggles went from one whole-table reload each to zero. Repeated album
toggles previously failed with a stale revision; all ten now succeed, at a
132.15 ms median. These are synthetic-fixture measurements, not a benchmark of
the user's collection. Raw reports are under `target/likes-before`,
`target/likes-after`, `target/likes-server-after`, and `target/likes-server-scale.json`
(the pre-optimization server measurement).

### Audio output recovery

If CoreAudio fails, playback pauses and retains the original error. Restore the
desired macOS output device, then press Play to rebuild the failed audio unit.
Recovery retains queued PCM, position and volume; it does not retry in the
background or resume playback by itself. Audio already handed to the old device
cannot be recalled. If reopening fails, playback stays paused with that error.

If macOS switches devices while the server is reading output timing, the server
keeps its last latency estimate until the next refresh. A failed timing read
from a device that is still active remains an error.

### Removing music

Select songs in a track list or expanded album, right-click, and choose
**Remove from Library…**. Confirm to remove the selected songs from the library;
**Cancel** leaves it unchanged. This is separate from **Remove from Playlist**.

Audio files stay on disk. Removed paths remain excluded after rescans and
restarts. Existing queue entries and listening history are retained; regular
playlists keep unavailable entries, while smart playlists exclude removed songs.
Removal is revision-checked: if the library changes while confirmation is open,
refresh and select again rather than automatically replaying the removal.

### Search

Library filters, saved-playlist rules, and find-in-list share case-insensitive
matching. Unquoted alphabetic words of 4–64 Unicode characters allow one insertion,
deletion, substitution, or adjacent transposition (`radiohaed` finds Radiohead).
Existing substring matches still count. Quoted text stays literal; short words,
numbers, and punctuation-containing terms do not expand to typo matches.

Use `title:`, `artist:`, `album:`, `albumartist:`, or `genre:` to restrict a term.
Boolean operators, `year:` and `liked:` retain their existing meaning. In particular,
`NOT radiohaed` excludes typo matches too; `NOT "radiohaed"` excludes only the
literal text. The circled × clears the toolbar filter and keeps keyboard focus.

SQLite schema 10 builds persistent substring/word indexes once on upgrade, without
rescanning audio. Transactional triggers maintain them through metadata edits and
deletion. The indexes cost disk space and import work; queries do not cache stale
library membership. Filtering does not change the selected sort order.

Reproduce end-to-end daemon latency on the declared synthetic 50,000-track fixture:

```sh
python3 scripts/search_gate.py --server /path/to/baseline-server --protocol-version 25 --output target/search-before.json
just build-server-release
python3 scripts/search_gate.py --output target/search-after.json --require-typos
```

Reports include binary hashes, fixture/schema identity, index-inclusive database
size and seeding time, process-cold samples, and 30 warm round-trip samples per
query with median/p95 latency. OS caches are uncontrolled; this is not a benchmark
of your own collection or native rendering. Search properties compare compiled SQL
against an independent string/Boolean model, including update/delete/reinsert,
rollback, populated migration, and reopening.

Measured on Apple Silicon, 2026-11-12, with the fixture above (milliseconds;
before binary `dad46b214219`, after `fa747dd6e451`):

| Track-page query | Before median | After median | Before p95 | After p95 |
| --- | ---: | ---: | ---: | ---: |
| `Radiohead` | 34.27 | 4.90 | 42.49 | 7.10 |
| `"Paranoid Android"` | 31.04 | 4.39 | 32.14 | 8.12 |
| No matching term | 26.45 | 0.48 | 34.71 | 0.82 |

`radiohaed` now returns 100 tracks (previously zero), with a 4.76 ms median.
Broad/short queries and find-in-list still spend most of their time scanning or
ordering rows; their median latency is largely unchanged. The indexed fixture
occupies 58.8 MB and takes 4.75 seconds to seed. Raw samples are in
`target/search-before.json` and `target/search-after.json`.

Run the search properties independently (change `PROPTEST_RNG_SEED` for a fresh
reproducible run; minimized failures are persisted by proptest):

```sh
FFMPEG_DIR="$(python3 scripts/build_ffmpeg.py)" MACOSX_DEPLOYMENT_TARGET=14.0 \
  PROPTEST_RNG_SEED=1222026 cargo test --locked --release \
  --target-dir target/portable-rust --lib filter::tests
```

## Build and run

### Prerequisites

- Apple Silicon Mac running macOS 14 or later.
- Xcode command-line tools and a Swift 6.2 or newer toolchain.
- A current Rust toolchain with Cargo and Rust 2024 edition support.
- Python 3.9 or newer.
- Go 1.23 or newer and `pkg-config` for the USB helper.
- Optional: [`just`](https://github.com/casey/just) (`brew install just`).

The first build downloads and compiles pinned FFmpeg, libopenmpt, and libusb sources; later builds reuse the
verified cache.

To assemble and launch the development app:

```sh
scripts/build_macos.sh
open "target/tyranny likes smoke.app"
```

Stop any existing _tyranny likes smoke_ app and daemon before rebuilding that app bundle. The
development bundle combines a debug Swift client with a release Rust server and
bundled native libraries. On launch, the app can bootstrap its local server. Onboarding flow
prompts you to choose a local music folder to import your library.

### Development commands

| Command              | Action                                                                    |
| -------------------- | ------------------------------------------------------------------------- |
| `just`               | List available recipes.                                                   |
| `just build`         | Build both components in debug mode.                                      |
| `just build-server`  | Build the Rust server and command-line tools.                             |
| `just build-client`  | Build the SwiftUI client.                                                 |
| `just test`          | Run both components' automated test suites.                               |
| `just test-server`   | Run Rust unit and integration tests; hardware tests are opt-in.           |
| `just test-client`   | Run the Swift client test suite.                                          |
| `just build-release` | Build both components with release optimizations.                         |
| `just dmg`           | Package a self-contained release app in `target/tyranny likes smoke.dmg`. |

Component builds do **not** replace the app bundle. Rust artifacts go into
`target/portable-rust`; Swift artifacts go into `target/swift-client`.
Unbundled Rust binaries still depend on the native build cache—use the app bundle
or DMG when moving the player to another location.

Release packaging currently uses **ad-hoc signing**, not Developer ID signing or
notarization, and does not produce a universal binary. Gatekeeper may block
downloaded copies.

## Architecture

- **Rust server:** owns decoding, CoreAudio output, library indexing, queue,
  playlists, settings, and SQLite persistence.
- **SwiftUI client:** provides the native library browser, playback controls, and
  server connection management.
- **Local protocol:** versioned, length-prefixed JSON over a private Unix-domain
  socket. Client and server builds must use matching protocol versions; there is
  no TCP listener or bearer token.
- **Command-line tools:** `tyranny-likes-smoke-ctl` controls the server; `audio-probe` supports
  audio-engine diagnostics.
- **USB helper:** a bundled native MTP process manages the Walkman independently
  of the playback server.

The current output backend and desktop client target macOS. Broader platform,
format, and service support remains tracked in the requirements.
