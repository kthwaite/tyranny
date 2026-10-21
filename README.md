<h1 align="center">
  <img src="macos/Resources/AppIcon.svg" alt="Tyranny app icon" width="128" height="128"><br>
  Tyranny
</h1>

Tyranny is an mpd-style media player with a Rust server and a SwiftUI frontend. With an
interface based loosely on classic iTunes, Tyranny borrows features from a variety of
music players to form a piece of highly-personalised software that satisfies my own
peculiar desires.

## Features

- **Local music library** — folder import with scan progress, artist/album/track
  browsing, live filtering, and compilation-aware album grouping.
- **Playback** — gapless audio, ReplayGain, seeking, a persistent queue; macOS
  system playback controls.
- **Playlists** — editable regular playlists and rule-based smart playlists, with
  Boolean filters, grouping, etc.
- **Artwork** — embedded and album-folder images, with Cover Art Archive lookup
  when a MusicBrainz release ID is available.
- **Internet radio** — direct HTTP(S) MP3/Ogg streams, saved stations, ICY titles,
  station discovery through radio-browser.info.
- **Equalizer** — ten bands, preamp, presets, and persistent server-owned settings.
- **Tracker music** — MOD, XM, IT, and S3M through libopenmpt, alongside the
  FFmpeg-backed audio decoder.

Closing the window leaves playback running. Explicitly quitting the app pauses
playback; the next launch resumes it. The server owns playback and durable state,
so reconnecting the interface does not create a new player.

## Build and run

### Prerequisites

- macOS 14 or later.
- Xcode command-line tools and a Swift 6.2 or newer toolchain.
- A current Rust toolchain with Cargo and Rust 2024 edition support.
- Python 3.9 or newer.
- Optional: [`just`](https://github.com/casey/just) (`brew install just`).

Run these commands from the repository root. The first server build downloads and
compiles pinned FFmpeg and libopenmpt sources; later builds reuse the verified
cache. Homebrew FFmpeg and pkg-config are not required.

To assemble and launch the development app:

```sh
scripts/build_macos.sh
open target/Tyranny.app
```

Stop any existing Tyranny app and daemon before rebuilding that app bundle. The
development bundle combines a debug Swift client with a release Rust server and
bundled native libraries. On launch, the app can bootstrap its local server;
choose your music folder to import your library.

### Development commands

| Command              | Action                                                          |
| -------------------- | --------------------------------------------------------------- |
| `just`               | List available recipes.                                         |
| `just build`         | Build both components in debug mode.                            |
| `just build-server`  | Build the Rust server and command-line tools.                   |
| `just build-client`  | Build the SwiftUI client.                                       |
| `just test`          | Run both components' automated test suites.                     |
| `just test-server`   | Run Rust unit and integration tests; hardware tests are opt-in. |
| `just test-client`   | Run the Swift client test suite.                                |
| `just build-release` | Build both components with release optimizations.               |
| `just dmg`           | Package a self-contained release app in `target/Tyranny.dmg`.   |

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
- **Local protocol:** versioned JSON over authenticated loopback WebSockets.
  Client and server builds must use matching protocol versions.
- **Command-line tools:** `tyrannyctl` controls the server; `audio-probe` supports
  audio-engine diagnostics.

The current output backend and desktop client target macOS. Broader platform,
format, and service support remains tracked in the requirements.

## Further reading

- [Development guide and implementation notes](SPIKE.md)
- [Verification results and known limitations](SPIKE_RESULTS.md)
- [Product requirements](REQUIREMENTS.md)
- [Lyrics provider integration notes](docs/lyrics-providers.md)
