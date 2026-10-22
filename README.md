<h1 align="center">
  <img src="macos/Resources/AppIcon.svg" alt="tyranny likes smoke app icon" width="128" height="128"><br>
  tyranny likes smoke
</h1>

_tyranny likes smoke_ is an mpd-style media player with a Rust server and (currently only) a SwiftUI frontend.
With an interface based loosely on classic iTunes, _tyranny likes smoke_ borrows features from a variety of
music players to form a piece of highly-personalised software that satisfies my own peculiar desires.

## Features

- **Local music library** — folder import with scan progress, artist/album/track
  browsing, live filtering, and compilation-aware album grouping.
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
