# Quit Pauses Playback, and a Full-Width Player Bar

Date: 2026-10-20. Scope: macOS client (`macos/`), with one small server addition. Builds on the
library redesign (`2026-10-18-library-ui-design.md`).

## Intent

Two UX fixes the user asked for after using the redesigned client.

1. **Quitting the UI must stop the music.** Today ⌘Q leaves the launchd-owned server playing
   with no visible way to stop it. Quitting a music app and still hearing music is bad UX. The
   server stays an independent daemon; only the *client's quit* changes behaviour.
2. **The now-playing display is crowded.** The iTunes-style toolbar packs transport buttons,
   volume, a 46 pt display (art, title, artist — album, scrubber, format, gain), shuffle, repeat,
   the drawer toggle and the filter into one strip. Replace it with a Spotify/Feishin-style
   full-width player bar at the bottom of the window, with album art that can expand into the
   foot of the sidebar.

## Goals

- ⌘Q while playing pauses the server, and the next app launch resumes from the same spot.
- Closing the window (app keeps running) does *not* pause — unchanged.
- A full-width player bar above the status bar: now playing (left), transport and scrubber
  (centre), format/gain, Up Next drawer toggle and volume (right).
- Album art toggles between a bar thumbnail and a sidebar-width square at the foot of the
  sidebar.

## Non-goals

- No change to the server's lifetime: it remains a KeepAlive LaunchAgent that outlives the client.
- No change to the existing **Resume playback when the server starts** setting (server
  restarts/`resume_on_start`); it is unrelated to UI quit.
- No new transport features (no stop button, ratings, or "more" menu in the bar).
- No change to menu commands or keyboard shortcuts; only button placement moves.

## Anti-goals

- Quit must never hang. A slow or unreachable server cannot block termination beyond a short
  timeout.
- Relaunch must not clobber playback another client started in the meantime.

## Reversal of a recorded decision

`SPIKE.md` (phase gates) records "explicit Quit leaves daemon audio running" and gates on
"quit/relaunch without interrupting server playback". `ManagedServer.swift:4` says "The app never
owns playback", and `TyrannyAppDelegate.applicationWillTerminate` says "The daemon owns audio and
outlives this controller." This design deliberately reverses the *audio* half of that decision:
the daemon still outlives the client, but the client pauses it on quit. Update those comments,
`SPIKE.md`'s description, and the gates that assert the old behaviour (see Verification).

## Part 1 — Quit pauses, relaunch resumes

### Behaviour (decided with the user)

| Situation | Result |
|---|---|
| ⌘Q (or Quit menu / Dock Quit) while `transport.isPlaying` | Send `pause`; wait for the reply, at most ~1 s; record a local "suspended" note; terminate. |
| ⌘Q while paused or stopped | Terminate. No note. Next launch does not auto-play. |
| Close the window (app stays running) | No change: playback continues. |
| Server unreachable or reply times out at quit | Terminate anyway. Record no note (we don't know the state). |
| Connected to a server the app did not launch (`--url`) | Same as managed: pause on quit. *(Claude's call.)* |
| Next launch, first authoritative snapshot after connecting | If a note exists **and** the server is `paused` **and** `current_id` equals the noted entry id: send `play` (resume, `entry_id: null`). Clear the note either way. |

"Suspended" note: `UserDefaults` key, value = the `current_id` (queue entry id) at the moment of
pause, plus the endpoint it applies to so a different `--url`/managed endpoint never resumes the
wrong server. Clearing it on first decision means a crash between launch and decision at worst
skips one resume. Gate, smoke, render and isolated-management launches keep the note in the
`dev.tyranny.controller.harness` defaults suite instead of the standard defaults
(`QuitSuspensionStore.suiteName(forArguments:)`), so they never touch the user's note; all
launches of one harness run share that suite.

### Mechanism

- `TyrannyAppDelegate.applicationShouldTerminate(_:)` returns `.terminateLater` when connected
  and playing; it awaits `TransportModel.pause()`'s reply via `WireClient.request("pause")`
  (wrap in a 1 s timeout), writes the note on success, then calls
  `NSApp.reply(toApplicationShouldTerminate: true)`. Otherwise returns `.terminateNow`.
  `applicationWillTerminate` keeps calling `model?.shutdown()`.
- The resume decision lives in a small, unit-testable TyrannyCore type (e.g. `QuitSuspension`)
  with pure functions: `noteForQuit(state) -> Note?` and `shouldResume(note, state, endpoint) ->
  Bool`. `PlayerModel` applies it on the first snapshot after a launch-time connect (not on later
  reconnects within the same run).
- `SystemMediaControls` teardown order is unchanged.
- The lifecycle harness (`NativeLifecycle`, which calls `NSApp.terminate`) must go through the same
  path, so the gates observe the new behaviour.

## Part 2 — Full-width player bar

Approved mockup: `.superpowers/brainstorm/*/content/player-bar.html` (git-ignored; layout
summarised here).

### Layout

```
┌ toolbar: [sidebar toggle] ……………………………………………………………… [Filter      ] ┐
├ sidebar ──────┬ main area (+ optional Up Next drawer) ─────────────────────┤
│ LIBRARY …     │                                                             │
│ PLAYLISTS …   │                                                             │
│ [large art]*  │                                                             │
├───────────────┴─────────────────────────────────────────────────────────────┤
│ [art] Title ⚠      │   ⤮  ⏮  (❚❚)  ⏭  ↻            │ FLAC 16/44.1   ▤  🔈 ━━━ │  player bar
│       Artist       │ 1:12 ━━━━━━━━━━━○──────── −1:55 │ RG album −6.2 dB         │  (~74 pt)
│       Album        │                                 │                          │
├─────────────────────────────────────────────────────────────────────────────┤
│ ● Connected              1,129 songs · 3.4 days                Scanning 412/… │  status bar
└─────────────────────────────────────────────────────────────────────────────┘
* only when art is expanded
```

- **Toolbar** keeps only the sidebar toggle and the filter field (with its error border, help and
  popover). Previous/Play/Next, volume, the `TransportDisplay`, shuffle, repeat and the Up Next
  drawer toggle leave the toolbar.
- **Player bar** spans the full window width (under both sidebar and detail columns), directly
  above the existing `StatusBar`, height ≈ 74 pt.
  - **Left (fixed width, ~250 pt):** 52 pt art thumbnail (collapsed state only), then title
    (semibold; metadata-warning ⚠ after it), artist, album — one line each, truncating.
  - **Centre (flexible):** row of shuffle, previous, a prominent circular play/pause, next,
    repeat; below it elapsed — scrubber (`NativeSeekSlider`, regular size) — remaining, max
    width ~520 pt. Seek preview behaviour as today.
  - **Right (fixed width, ~250 pt):** format line and gain line (two lines, as today), Up Next
    drawer toggle, volume icon + `VolumeSlider`.
- Existing accessibility identifiers (`transport-*`, `now-playing-*`, `up-next-drawer-toggle`,
  `library-filter`) move with their controls, so native smokes keep finding them.

### Art expansion

- Hovering the thumbnail shows a ⤢ badge; clicking the thumbnail expands the art.
- Expanded: the thumbnail leaves the bar (text shifts left); a square at sidebar width appears
  pinned to the foot of the sidebar, below the scrolling source list. Hover shows ⤡; clicking
  the large art collapses it.
- The state persists across launches (`NavigationModel`, `UserDefaults`, like
  `upNextDrawerVisible`).
- If the sidebar is collapsed while expanded, the bar shows the thumbnail until the sidebar
  returns; the persisted preference is unchanged.

### Interactions *(Claude's calls, approved)*

- Click title → reveal the playing row (same as ⌘L, `navigation.revealPlayingRequest`).
- Click artist → open the album artist (`RevealTarget.artist`, from
  `NowPlaying.album_artist_id`), as "Show in Artist" does; click album → open that album
  (`RevealTarget.album`, from `NowPlaying.album_id`). The Artists browser lists only album
  artists of non-compilation albums, so the artist text is not a link for a compilation.
- `NowPlaying` has `album_id` but no artist id. Add optional `album_artist_id` and `compilation`
  to `NowPlaying` in `src/protocol.rs`, mirroring `TrackSummary`, filled by the owner from the
  library alongside `album_id`. Both are absent for an unindexed entry. The artist text is a link
  only when `album_artist_id` is present and `compilation` is not true; the album text is a link
  only when `album_id` is present.

### States

- Nothing playing or disconnected: the bar keeps its size and layout; blank art tile, empty text,
  controls disabled, scrubber at 0:00.
- Playback error: replaces the left section with "Playback Error" + one-line summary; click opens
  the full-message popover (today's `errorSummary` behaviour).
- Window minimum stays 940 × 560. Left and right sections are fixed; the centre (scrubber) shrinks
  first.

## Left to the builder

- Exact spacing, font sizes and button glyph sizes, within the mockup's proportions.
- Whether the bar reuses `TransportDisplay` pieces or replaces the file outright.
- How the large art view is attached to the sidebar (`safeAreaInset(edge: .bottom)` on the
  `List` is a reasonable start).
- Protocol version handling for the additive `album_artist_id`/`compilation` fields (bump only if the matching-version
  rule requires it).

## Verification

- TyrannyCore unit tests for `QuitSuspension` (note on playing; none on paused/stopped/unknown;
  resume only when paused on the same entry and endpoint; note cleared after decision).
- Rust test that `NowPlaying.album_artist_id`/`compilation` are filled for indexed entries and
  `None` otherwise.
- Update `scripts/lifecycle_gate.py` (~lines 244–252) and the `NativeSmoke` relaunch case
  (`native-relaunch-receives-authoritative-playing-state`) to assert: playing → quit → server
  paused at the same position → relaunch → playing again. Add a case: paused → quit → relaunch →
  still paused.
- Render the bar (collapsed, expanded, disconnected, error) with `scripts/render_views.py`.
- Standard gates: `cargo fmt --check`, clippy `-D warnings`, `cargo test`, Swift debug and release
  builds with `-warnings-as-errors`, `swift test`, `client_gate.py --model-only`. Native gates
  remain blocked on Accessibility/Screen Recording permissions.
