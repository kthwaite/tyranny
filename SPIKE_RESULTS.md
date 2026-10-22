# Spike results: native audio, persistent server, durable library, and SwiftUI controller

Generated reports and screenshots under `spike-results/` are generally local, ignored artifacts. The five onboarding captures preserved during worktree integration are versioned exceptions. References below describe historical verification runs; other artifact paths may be absent in a fresh checkout. Keep the written findings here, and regenerate current evidence with the gate scripts in `scripts/` (create `spike-results/` first when using `--record`). Per-run fixtures, logs, databases, and build products stay under ignored `target/` or `.build/` directories. Unix-socket gates use short canonical temporary directories, with discovery symlinks beneath the requested work directory, to respect socket-path length limits.

## Application rename: tyranny likes smoke (2026-10-22)

The visible app, bundle/executable, DMG, Swift modules, Rust package, command-line
tools, helper, scripts, and maintained documentation use the new name. The feature
branch retained protocol 23; merging into `dev` advances it to 24 without changing
the wire shape. Existing bundle/LaunchAgent IDs, state paths, Keychain identity,
and Walkman upload paths/journals remain stable by explicit user choice. Historical
artifact paths, repository URLs, and byte-stable tracker fixtures retain their
original names.

The renamed debug bundle built successfully. The native app menu opened **About
tyranny likes smoke**, its composited screenshot showed the exact new name and
credits, and **Quit tyranny likes smoke** exited cleanly. Evidence:
`/tmp/claude-screenshots/screenshot-20261022-010118.png`.

Verification: 451 Swift tests in 62 suites, the Go helper suite, 19 Python
build/packaging tests, and 13 real server/CLI scenarios passed. Nine isolated
lifecycle scenarios passed, including replacement of a real running helper from
the old installed bundle, reuse/resume, moved/deleted-app recovery, and rejection
of foreign LaunchAgents. The installed app, its running daemon, and library were
not replaced. Reports: `/private/tmp/tyr-h287ehb4/report.json` (server) and
`/private/tmp/tyr-6yn3amok/report.json` (lifecycle).

The broader shell UI gate imported 105 tracks but timed out waiting for Albums
navigation; it is **not** a passing full-UI result. Its failure report and captures
are under `/private/tmp/tyr-a3rcly3a/`. The separate native About/menu check above
verified the changed branding surface.

Final source-built verification also passed all 611 Rust tests (two opt-in tests
ignored), including cross-language constants. Release packaging rebuilt the pinned
native dependencies, assembled and strictly verified the signed bundle, and
created `target/tyranny likes smoke.dmg`; `hdiutil verify` passed. Signing remains
ad-hoc, not notarized.

Protocol-24 merge verification passed 20 Rust protocol tests, all three
cross-language constant checks, 451 Swift tests, 13 real server/CLI scenarios,
and 19 native client-model scenarios. Reports:
`/private/tmp/tyr-h_v210nx/report.json` and
`/private/tmp/tyr-0j3omkcr/report.json`. The development app and release DMG were
rebuilt with matching protocol-24 binaries; strict bundle signature and DMG
checksum verification passed. The earlier shell UI navigation timeout remains
documented above; it was not rerun for the version-only integration.

## Worktree integration (protocol 23)

Integrated the outstanding song-likes/scroll-memory history and pending worktree
snapshots for app upgrades, source-download mirrors, album ordering, artwork
providers, onboarding captures, and Unix-domain transport on top of Walkman sync.
Separate merges advance `dev` from protocol 18 through 23; matching client and
server builds are required. Control transport is now socket-only, while USB
management remains independent.

The real legacy-app migration exposed Foundation rewriting `/private/tmp` to
`/tmp` during ownership validation. Lexical validation now preserves canonical
paths without weakening the loaded-job/plist checks. A rapid native-app relaunch
also exposed a stale USB discovery error after contention had cleared; discovery
and operation errors are now separate so recovery cannot erase a failed transfer.
Native acceptance drivers retain saved column layouts, scroll hearts into view,
and use one real pointer click for destructive confirmations. Restart checks
require a new authoritative server instance.

Verification on 2026-10-22:

| Check | Observed result |
| --- | --- |
| Rust all-targets suite | 611 passed; 2 opt-in tests ignored |
| Swift suite | 451 passed in 62 suites |
| MTP transport engine | 9 passed |
| Source-build and Mach-O packaging regressions | 19 passed |
| Real daemon/CLI and native client-model gates | 13 and 19 passed |
| Managed lifecycle, including running WebSocket-helper upgrade | 17 passed; all temporary LaunchAgents removed |
| Library / filter / regular-playlist / smart-playlist process gates | 13 / 7 / 13 / 5 passed |
| Artwork process gate | 6 passed, including live Cover Art Archive and offline cache reuse |
| Native UI | Likes 6, Songs scroll restoration 3, read-only Walkman 3; artwork settings inspected |
| Packaging and static checks | Debug app assembled; strict deep signature, Rust formatting/Clippy, Python compilation passed |

Reports are discoverable under `target/all-branches-{server-gate,model-gate,
lifecycle,library,filter,playlist,smart-playlist,artwork,scroll}/`. Successful
lifecycle evidence is `tyr-joqwgavl/report.json`; scroll evidence is
`tyr-7i_tlr9b/report.json`, with inspected screenshots in
`target/all-branches-scroll-ui/`.

The likes/device reports under
`target/all-branches-features/tyr-6kdc5vvt/{likes,device}/report.json` passed.
Their combined launcher then hit the screenshot script's missing execute bit;
the separate settings run invoked it through Bash and passed at
`target/all-branches-features/tyr-hp_deyed/report.json`. Its actual Artwork
settings screenshot and the liked-only and SD-card screenshots were inspected.
The device exposed 7,779 songs across both storages; this integration rerun
performed no device writes. Earlier authorized upload/deduplication/deletion
evidence remains in [device sync](docs/device-sync.md).

No installed app or user daemon was replaced. This is not a Linux, acoustic,
50,000-track performance, or live authenticated Discogs/Deezer verification claim.

## Song likes (2026-10-21)

Implemented on `feature/song-likes`, branched from `dev` at `88c1ede`, in
`../tyranny-song-likes`. SQLite schema 9 stores per-track likes independently
of file tags. Feature commit `d3f3b69` retained protocol 17; the merge into `dev`
bumps it to **18** and requires matching client/server builds.

Integration verification in `../tyranny-likes-integration`: **584 Rust tests
passed, 2 ignored**, **435 Swift tests passed in 59 suites**, debug binaries
built and formatting passed. The protocol-18 daemon/client passed the six
native likes scenarios without reconnects or surfaced errors; its liked-only
screenshot was inspected. The protocol-18 CLI connected, while the previous
protocol-17 CLI rejected the daemon. Evidence:
`target/likes-integration-3xf_7y7y/report.json` and `native/report.json`.
The existing structured-filter gate also passed all seven cases:
`target/filter-gate/run-dr8z97cp/report.json`.

Verification:

- Rust: **584 passed, 2 ignored** across all targets; debug binaries built.
  Formatting and Clippy with warnings denied passed. Used the existing pinned
  FFmpeg 9.0.2/libopenmpt prefix.
- Swift: **435 tests passed in 59 suites**; debug native executable built.
- Isolated real daemon with three tagged FLACs: default-unliked state,
  desired-state/no-op semantics, stale-revision rejection, invalid filters,
  Boolean composition, duplicate playlist occurrences, live smart-rule
  membership, rescan preservation and daemon restart preservation passed.
  Backend cases are recorded in `target/run-jzl01gbi/report.json`; its initial
  combined verdict was interrupted by a native automation failure.
- Final native smoke passed all six scenarios: heart clicks without selecting
  or playing; Liked only AND-composed with OR text; removal on unlike; visible
  malformed-filter errors and retained text; hide/show with persisted column
  preferences; grouped-table and album-expansion updates. Connection generation
  stayed unchanged and no client error remained.
  Evidence: `target/run-jzl01gbi/native-final/report.json`.
  Final liked-only and album screenshots were inspected.
- Existing real-file structured-filter gate passed all seven cases:
  `target/filter-gate/run-1if9qm8b/report.json`.

Native verification caught an omitted `track_liked` reply whitelist entry:
the server committed but the client disconnected on the reply. Both reply
validation paths were updated, and the smoke now rejects connection resets
or surfaced errors. Its Columns interaction also now selects the menu item,
not the identically titled table header.

The retained DEBUG hook is `--likes-smoke-output <directory>` with an explicit
isolated server URL/token and `--ui-driver <path>`. Supply three initially
unliked tracks titled Alpha, Beta and Gamma in Likes Album, and an empty queue.
The run used a separate `local.tyranny.likes-smoke` bundle identifier, private
state and clock-paced capture. No installed app, user library, running daemon,
release package or hardware audio device was changed or tested.

## Parametric equalizer (2026-10-21)

Implemented `TSK-knobby-iris-fg8hk` in `../tyranny-knobby-iris`, branch
`feature/knobby-iris-parametric-eq`, from `dev` at `48ba232`, commit `71d3709`.
The feature retained protocol **14**; integration into `dev` bumps it to **15**.
The changed wire shape requires matching client/server builds.

Integration verification on `dev`: **343 Rust tests passed, 2 ignored** and
**391 Swift tests passed in 52 suites**, with warnings denied. Rust binary
builds, Clippy and formatting checks passed. The rebuilt protocol-15 client
and daemon passed the complete native EQ smoke and a separate restart/restore
pass; screenshots of the restored curve and narrow notch were inspected.
The previous protocol-14 CLI correctly rejected the protocol-15 daemon.
Integration evidence: `target/eq-integration-ui/run-ea6em3c0/` in the main
worktree. Private build/app/state paths were used; the running app/daemon and
pre-existing uncommitted files were not replaced or included in the merge.

- Six filter types, up to 16 independently bypassable bands, 12–48 dB/octave
  pass slopes, frequency/gain/Q/shelf-slope editing, global preamp and bypass.
  Pass Q scales only the highest-Q Butterworth section; Q = 1/√2 gives
  Butterworth response at every slope.
- Three preallocated banks hold current, incoming and latest pending curves.
  At most two process audio; 10 ms warm-up plus 20 ms crossfade gives a bounded
  60 ms final-settle delay under rapid edits, excluding existing PCM buffering.
  Settled flat/bypass is bit-exact. No coefficient work or allocation occurs
  in the sample loop; gapless histories and explicit discontinuity resets remain.
- Storage-only migration preserves the old graphic response using
  frequency-specific Q, including its 16 kHz bandwidth correction. Factory
  presets retain their responses; the active curve and named user presets are
  server-owned. Preset CRUD persists before publication without changing audio.
- Native response graph, selectable/bypassable band rows, numeric controls,
  graph dragging, saved-preset CRUD, and manual headroom compensation.
  Graph sampling includes exact centers and refined maxima/minima, including
  narrow notches. Headroom remains an estimate, not a limiter.

Verification:

- Rust full suite: **343 passed, 2 ignored**. Clippy with warnings denied and
  formatting checks passed. The corrected storage fixture also passed its
  focused regression after the lint cleanup.
- Swift full suite: **391 tests passed in 52 suites**, warnings treated as
  errors. The native executable built with warnings denied.
- Real isolated daemon and production capture consumer: all six filter types
  and every pass slope matched independent response calculations at 1 kHz and
  250 Hz. A +6 dB bell measured **+6.00000001 dB** at its center; each Butterworth
  pass slope measured approximately **−3.0103 dB** at cutoff. Enabled flat and
  disabled nonflat curves each preserved **96,000 frames** bit-for-bit.
- Actual live editing retained all **288,000 frames**, preserved early buffered
  PCM at unity, and reached **+6.00000001 dB** late in the capture, with zero
  reported underruns. Real-daemon smoke also verified legacy database migration,
  preset save/rename/delete/restart, invalid input, and injected persistence
  failures leaving active and published settings unchanged.
  Evidence: `target/parametric-smoke/run-6w9nqeb9/report.json`.
- Native Accessibility smoke and daemon/app restart passed: graph mouse drag,
  all filter controls/slopes, band rows and independent bypass, 16-band limit,
  remove/reset, exact untouched numeric values, compensation, preset overwrite/
  rename/recall/delete, disconnect cancellation and authoritative restoration.
  Two byte-distinct NFC/NFD preset names recalled their distinct curves.
  Final evidence: `target/parametric-ui/run-rzppxip4/`; initial/restored windows
  and a 19.5 kHz, Q=18 notch were captured and visually inspected. Axis labels
  follow their logarithmic positions without overlapping the gain labels.
- Independent standards/spec reviews found and then confirmed fixes for
  canonical-Unicode preset identity and undersampled narrow graph extrema.
  Native smoke caught a SwiftUI stale-index crash when resetting populated
  presets; picker/list rendering now captures its input collection, and the
  full native sequence passes.
  Throwaway orchestration scripts were removed; the native smoke hook and
  regression tests remain reproducible.

Evidence paths are local to the feature worktree. Capture output is not acoustic
listening or hardware-loopback proof; no release-package/DMG claim. The user's
running app, daemon, database and original working-tree changes were untouched.

## Tracker modules (2026-10-21)

Implemented `TSK-blithe-vat-1f8b1` on `feature/blithe-vat-trackers`, branched from
`dev`, in `../tyranny-blithe-vat`. MOD/XM/IT/S3M scanning and playback use pinned
libopenmpt 0.8.9 through FFmpeg 9.0.2. Module titles are preserved; one file plays
all subsongs once, with libopenmpt's normal end fade. Queue repeats are separate.
The feature kept protocol 13; integration into `dev` bumps it to **14** under
the merge-time version policy. Client and server must be upgraded together.
No wire-shape or database-schema migration was needed.
Integration verification on `dev`: **332 Rust tests passed, 2 ignored**, and
**384 Swift tests passed**. The rebuilt protocol-14 server accepted the updated
gate, indexed all four tracker formats with their titles and no failures, and
the rebuilt audio probe played entries [1, 2, 3, 4] to EOF without errors or
underruns. Feature commit: `345661f`.

Verification against the newly source-built runtime:

- `cargo test --locked --target-dir target/portable-rust --all-targets` with
  `FFMPEG_DIR` set to the new prefix: **332 passed, 2 ignored**.
- Final focused format suite: **6 passed**. Clippy with warnings denied, Rust
  formatting checks, and `just --list` passed.
- Self-authored fixtures contain backward song jumps and looping samples; IT
  has two distinct-note subsongs. Tests cover exact independently measured EOF
  frame counts, audible finite PCM, persistent EOF, restart, interior and
  cross-subsong seeks, embedded titles/durations, and queue advancement.
- Independent libopenmpt C API references at 48 kHz: MOD/XM/S3M each **50,880
  frames**, two-subsong IT **96,960 frames**. Reported song durations are 960 ms
  and 1,920 ms respectively; the extra 100 ms is the normal end fade.
- Actual isolated server/WebSocket import from a Unicode directory with
  uppercase extensions: **4 discovered, 4 indexed, 0 failures, 0 skipped**;
  all four embedded titles and `Tracker` labels returned correctly.
- Actual `audio-probe` queue: entries **[1, 2, 3, 4]**, EOF true, zero errors,
  zero underruns, **249,600 captured stereo frames**, peak approximately
  **0.28093**. The capture contained only finite samples.
- Copied the real probe into a temporary app-shaped bundle, ran the native
  dependency packager, moved it to a path containing spaces/Unicode, signed and
  executed it: all four modules reached EOF again with no errors/underruns.
  Dependency inventory contained the four FFmpeg dylibs plus libopenmpt, with
  no unresolved edges or non-system dependencies outside the bundle.
- Build safeguards: **7 Python configuration/cache/checksum tests passed**;
  dependency packaging: **8 tests passed**, including libopenmpt relocation and
  rejection of an external C++ runtime.

All runtime state/captures were temporary and removed. The user's running app,
daemon, database, and other worktrees were not replaced. No native UI or acoustic
listening claim; no full DMG build in this verification. Seeking/duration are
approximate, and compressed containers or MP3/Vorbis-compressed module samples
are outside the minimal build's support claim. See `SPIKE.md` for build and
fixture-regeneration commands.

## Compilation inference (2026-10-20)

`TSK-jolly-dingo-6x70y` adds explicit album-artist/compilation provenance and
bounded post-probe reconciliation. Same-folder/title and shared-release
compilations group without losing per-track artists or durable track IDs.
Explicit non-VA album artists, explicit false flags, conflicting release IDs,
and unknown metadata fallbacks prevent speculative merging. Partial release IDs
join only an unambiguous folder peer; raw MusicBrainz metadata remains unchanged.

Final verification: **288 Rust tests passed, one ignored**; Clippy with warnings
denied and formatting checks passed. The full run exposed two playback race
fixtures that assumed current-track identity implied next-track prebuffering.
Their setup now fills the buffer while paused, then waits for audible entry 1
before preparing the edit; the transition/PCM assertions remain unchanged.

Real-daemon verification in `../tyranny-jolly-dingo`:

- Twenty generated FLAC fixtures passed untagged/mixed-tag grouping, VA aliases,
  explicit-tag protection, distinct-release isolation, cross-folder release
  grouping, partial MusicBrainz association, unchanged rescans, removal/re-add,
  retag reversal, and restart persistence. Every surviving/restored track kept
  its ID.
- A separate 512-directory/512-track import completed in **0.367 s**; its
  unchanged rescan completed in **0.198 s**. These are local fixture timings,
  not production-library latency guarantees.
- The existing library process gate passed all **13 scenarios** with **2,056
  tracks**, including import during clocked PCM playback, migrations, bounded
  queries, retags, missing/unreadable roots, crash recovery and indexed playback.
  Maximum observed control-request latency during import was **1.57 ms** with
  **zero underruns**.

Reports: `target/compilations-smoke-1jop8gkk/report.json` and
`target/compilation-library-gate/run-2buyt_2v/report.json` in that worktree.
Existing roots need a manual rescan to extract provenance (metadata version 3);
no database-schema or protocol-version change. Verification used isolated state,
not the user's daemon/library. No new native UI or acoustic-listening claim.

## Audio equalizer (2026-10-20)

Implemented `TSK-sunlit-egret-jywzq` in the isolated
`../tyranny-sunlit-egret` worktree on `feature/sunlit-egret-equalizer`, branched
from `dev`. Playback → Equalizer… opens the native ten-band/preamp window.
The allocation-free stereo biquad bank handles both local and streaming PCM;
Flat, Bass Boost, Treble Boost and Vocal presets, retained bypass curves,
server-owned persistence, and reconnect-authoritative edits are implemented.
Protocol **v11**, SQLite **schema 6 unchanged**. Existing databases start with
disabled EQ; client and server must be upgraded together.

Integration verification with the decoder and lyrics changes: **279 Rust tests
passed, one ignored; 296 Swift tests passed in 44 suites** with warnings as
errors. Clippy, formatting, and binary builds passed. Independent standards and
specification reviews found no actionable issues. Native controls passed again;
the rebuilt integrated client/server restored the persisted Bass Boost curve
after daemon restart. Screenshots were inspected. Evidence:
`../tyranny-sunlit-egret/target/equalizer-ui/integration-cf4w4vdz/`.

### Exercised evidence

- `cargo test --locked --all-targets`: **263 passed, one ignored**. Coverage
  includes exact flat/bypass sample bits, frequency/bandwidth response, stereo
  isolation, chunk invariance, bounded finite decay, pending-buffer retries,
  gapless history continuity, explicit discontinuity resets, stream-reconnect
  alignment, malformed/range/nonfinite settings, persistence failure, existing
  database defaults, durable restart and two-controller state publication.
- `cargo clippy --locked --all-targets -- -D warnings` passed; Rust formatting
  and `cargo build --locked --bins` completed.
- `swift test --package-path macos --scratch-path target/swift-client
  -Xswiftc -warnings-as-errors`: **290 passed in 43 suites**, including EQ
  decoding, validation, preset and bypass behavior; the native executable built.
- A throwaway real-daemon smoke rendered a three-second stereo fixture through
  the production PCM consumer. Enabled flat and disabled nonflat EQ produced
  **1,152,000 identical bytes / 144,000 frames**. A +6 dB 1 kHz band measured
  **+5.9999998 dB** at 1 kHz and **+0.2215682 dB** at 250 Hz. A −6 dB preamp
  measured **−6.0000001 dB**. A live edit preserved all frames, left the early
  buffered samples unchanged, and reached the requested gain after the ring
  delay. There were no reported underruns. Restart restored the curve; rejected
  out-of-range input left settings unchanged.
- Native Accessibility exercised all four presets, all ten bands, preamp,
  Custom recognition, retained bypass, enabled/disabled editing, queued-edit
  cancellation on disconnect, authoritative reconnect and daemon/app restart.
  Both initial and restored windows were captured and visually inspected.
  Four consecutive complete edit/restart runs passed after fixing the native
  driver's menu-dismissal race: selecting an already-selected preset does not
  change model state, so the driver now waits for the actual menu to close before
  allowing another invocation to press it.

PCM evidence: `target/equalizer-smoke/run-5flchrsq/report.json`. Native evidence:
`target/equalizer-ui/run-4nt3y0wd/`, with three additional successful race checks
at `run-ak8gz63s`, `run-u4eeqcc4` and `run-uerj_bpm`. Paths are local to the
equalizer worktree. The native debug smoke hook and regression tests remain
reproducible; throwaway orchestration/diagnostic scripts were removed.

Limitations: capture output, not acoustic listening or hardware loopback. No
release packaging claim. Settings edits retain buffered audio (about 1.4 seconds
plus decoder/device buffering), reset changed-filter histories, and do not add
a limiter or transition smoothing. Boosts require preamp headroom. The user's
running app, daemon and database were not replaced or edited.

## Lyrics retrieval and display (2026-10-20)

TSK-smart-gale-rqmwc implements embedded-first lyrics, cached LRCLIB lookup,
and the native Lyrics pane (**⌥⌘L**). Protocol v10 gains an additive `lyrics`
request; the main database schema remains unchanged. Source/access decisions
for all eight requested providers are documented in
[docs/lyrics-providers.md](docs/lyrics-providers.md). Only LRCLIB is enabled
remotely; no third-party lyrics scrapers or credentials were added.

Verification in the isolated `../tyranny-smart-gale` worktree:

- Full Rust suite: **256 passed, one ignored**. Clippy with warnings denied and
  formatting checks passed. The 13 lyrics-focused tests cover real embedded
  tags, exact metadata matching, instrumental/missing/error outcomes, bounded
  responses/cache, expiry, offline persistence, rescan/duplicate-file cache
  reuse, cancellation, and removal during slow lookup.
- Swift: **291 tests in 43 suites passed** with warnings as errors; the final
  native build also passed with warnings as errors. LRC tests cover fractions,
  multiple timestamps, offsets, duplicate-time lines, invalid input, backward
  seeks and provider-link restrictions.
- Ten real-daemon smoke scenarios passed, including genuine ID3v2.4 **USLT**
  and FLAC **LYRICS**, a live LRCLIB HTTPS request, embedded precedence,
  invalid/stale/removed entries, stream exclusion, persisted online lyrics
  after offline restart, and unavailable—not missing—on an uncached outage.
  Deterministic loopback tests separately verify HTTP 404 negative caching.
- Nine native scenarios passed using actual Accessibility toggle, pause,
  scrubber and close controls. Fixture selection and disconnect use the normal
  model commands. The mounted pane followed playback, paused at the second
  line, sought backward to the opening line, released hidden demand, rendered
  plain lyrics, rejected superseded occurrences, refreshed on explicit rescan,
  recovered after reconnect, and displayed missing/online states and attribution.
  Screen-window captures were visually inspected; no model-driven fallback was
  used.

The native smoke caught two integration defects before delivery: the existing
wire reply-kind allowlist needed `lyrics`, and a tiny no-op rescan could complete
without an observable state change. Accepted explicit rescans now refresh
visible lyrics directly; observed scan completion still invalidates them.
Remote cache identity excludes unrelated library revisions and file paths, so
rescans and duplicate files do not defeat offline reuse.

Local evidence: `target/lyrics-smoke/report.json` and
`target/lyrics-smoke/native/{report.json,*.png}`. The orchestration script was
a throwaway probe; `NativeLyricsSmoke` remains the opt-in native entrypoint.
Generated silent audio and clock-paced capture do not constitute an acoustic
listening test. The user's daemon, database and installed app were not changed.
An exploratory unknown-track request received LRCLIB's temporary backoff;
the integration retained `unavailable` and honored the delay rather than
converting it into a permanent miss or bypassing throttling.
## Configurable artwork providers (2026-10-20)

Implemented on `feature/extra-stream-artwork` in `../tyranny-extra-stream`, from
`dev`, for `TSK-extra-stream-f7xrq`. Protocol **v11** requires matching client/server;
main SQLite schema remains **6**. The user's running app, daemon and database were
not replaced. Last.fm was explicitly deferred by the user after reviewing its
artwork exclusion; the other four requested providers were assessed, not implemented.

Settings → Artwork persists provider order/enablement locally and stores the
Discogs token only in Keychain. Embedded/folder precedence remains fixed. Remote
lookup uses CAA release IDs or conservative Deezer/Discogs artist/title matching.
Ambiguous editions are skipped. The server retains bounded HTTP/image work,
allowlisted HTTPS destinations, credential-redacted diagnostics and a 26-second
active-load budget; each provider has a six-second deadline.

Deezer images bypass server image persistence and client reusable caching once
demand ends. Discogs images carry their original six-hour expiry through caches
and active views; stale images disappear before refresh. Adjacent provider links
and authentic Deezer branding appear in album grids, expanded albums, grouped
lists and Now Playing. Neither new provider's artwork enters notification
attachments. Disabling a source revokes its currently held image handles.

Verification:

- `cargo test --locked --all-targets`: **259 passed, one ignored**. After replacing
  the large private cache enum with `Result<Option<Images>, String>` and fixing
  lint issues, `cargo test --locked artwork`: **44 passed**. Clippy with warnings
  denied passed; Rust formatting and server binary build passed.
- `swift test --package-path macos --scratch-path target/swift-client -Xswiftc
  -warnings-as-errors`: **291 passed in 43 suites**. Subsequent native presentation
  fixes rebuilt with warnings denied and passed the actual native workflow.
- Existing server artwork gate: **six scenarios passed**, including real CAA
  retrieval, local precedence, unindexed entries, stale/removed targets, rescan
  refresh and offline persistent-cache reuse.
  Report: `target/extra-stream/artwork-gate/run-i1o7xi2n/report.json`.
- Isolated real-daemon Deezer smoke: unique Daft Punk / Discovery match (album
  302127), decoded 500×500 JPEG, CAA miss → Deezer success, disabled-provider
  non-reuse, missing-token/duplicate rejection, and no Deezer image after offline
  daemon restart. Report: `target/extra-stream-b9xs4u7l/report.json`.
- Native AX smoke passed settings enable/reorder/persistence/local-only mode,
  missing/empty-token rejection, and actual Keychain save/reload/delete using an
  isolated synthetic token with every provider disabled. The token was removed.
  Live Deezer covers rendered with linked official branding in the grid and
  expanded album, then disappeared on disable without cached redisplay.
  Report and inspected screenshots:
  `target/extra-stream/native-5lg3nc2p/{report.json,settings/,providers/}`.

The native run used a separate bundle/preferences/Keychain domain and an isolated
clocked-PCM daemon. It exposed and fixed an undersized Artwork settings pane and
missing image accessibility semantics in the expanded cover. A wording-dependent
smoke assertion was removed in favor of checking rejected credential state.

**Limits:** no real Discogs token was supplied; its authentication, header
isolation, primary-image parsing, matching, fallback, rate limits and expiry are
exercised through local HTTP fixtures, not a live account. Native Discogs image
display and a six-hour wall-clock soak were not exercised. No release DMG,
clean-Mac, acoustic-output or notification-delivery claim is made by this work.

Native reproduction: build a debug bundle with `DeezerLogo.png`, use a fresh
isolated bundle ID and an isolated daemon, then launch with `--url`,
`--token-file`, `--ui-driver target/native-ui-driver`, and
`--artwork-smoke-output <directory> --artwork-smoke-mode settings` against an
empty library. Mode `providers` needs a scanned track tagged album artist
`Daft Punk`, album `Discovery`, title `One More Time`, with no local cover or MBID.

## Development integration (2026-10-20)

Notifications and HTTP streaming were integrated with the current command-language, decoder-component coverage and packet-duration changes. Conflict resolution preserves both native queue-command methods, the expanded required decoder/demuxer sets, and the network/TLS restrictions. Configuration tests exercise rejection of missing audio components instead of copying component lists or inspecting source text.

Combined verification: **243 Rust tests passed, one ignored; 267 Swift tests passed** with warnings as errors; Clippy and formatting passed. The merged portable FFmpeg configuration rebuilt successfully, its four configuration-rejection tests passed, and **46 format/transport tests** passed against that prefix, including HTTP/ICY stall recovery. The real native command-dispatch smoke passed six scenarios against an isolated daemon (`target/merge-command-gate/run-ov15z8zc/`); that run did not claim UI or acoustic verification. Native notification banner/artwork delivery remains blocked on macOS authorization.

## Native track notifications

Settings → Playback now offers opt-in track-change notifications and an option to show them while tyranny likes smoke is frontmost. Notification authorization is requested only on opt-in; permission status and delivery errors remain visible in Settings. Notifications carry title, artist, album and available current artwork. Artwork waits at most one second on the existing artwork demand and never blocks playback.

Initial/reconnected snapshots establish a quiet baseline. Buffering waits for actual playback; position ticks, pause/resume and preference/focus changes do not replay an already-considered occurrence. Pending deliveries are cancelled when their occurrence changes or the connection ends. Generation-specific request identifiers prevent a late obsolete delivery from removing a newer notification.

Verification: `swift test --package-path macos -Xswiftc -warnings-as-errors` passed, including three notification-transition regressions. A throwaway bundled native probe connected to a real isolated server with clocked PCM output, reached the actual settings controls through Accessibility, and self-rendered the settings pane. Its notification authorization status was denied, with no delivered notifications. **Banner delivery and artwork attachment require an authorized native run; they are not claimed verified.** The probe used a separate bundle identifier and never changed the user's daemon, database or normal notification preferences.

## HTTP stream playback

**Up Next → Add Stream…** accepts a direct HTTP(S) MP3 or Ogg URL and an optional station title. **Add to Up Next** persists it without starting playback; **Add & Play** starts its new queue occurrence. The queue displays the station, URL and **Live** duration; Now Playing displays changing ICY titles. Live sources have no seek range or library-navigation target. URL credentials, non-HTTP schemes, missing hosts, control characters and oversized fields are rejected without changing the queue. Playlist-file imports and a saved-station catalog remain the separate radio task.

Protocol **v10**, SQLite **schema 6**. Queue items have an optional `stream` descriptor; `append_stream` uses the existing queue-revision contract. Stream identity survives restart independently of local paths, and a restored stream resumes from the live endpoint rather than seeking a saved frame offset. Upgrade the client and server together; older servers cannot open schema 6. Verification used isolated state and capture output, not the user's daemon/database or audio device.

### Decode ownership and bounds

- Local playback retains its synchronous decoder and file-only protocol whitelist. Explicit stream entries alone may open HTTP/HTTPS/TCP/TLS, and only MP3/Ogg demuxers. HTTPS certificate verification is enabled.
- Network open, probing and decoding run off the playback owner. Four recycled PCM blocks bound each worker's handoff; the owner polls without waiting on network I/O, joining a thread or acquiring a blocking metadata lock. Temporary starvation pauses output and resumes after half a second of prebuffer without changing queue identity.
- Each connection has a ten-second open/probe/prime deadline and five-second read deadlines. Four consecutive failed or short connections exhaust recovery, with 250/500/1,000 ms backoff; ten seconds of decoded audio resets that failure count. Pause, stop and replacement can cancel a worker without joining it. A platform DNS call may outlive cancellation; the cap of four active/retiring workers bounds this limitation.
- ICY metadata is bounded and change-driven. Missing titles retain the station fallback. Artwork/library inspection never treats a stream URL as a local file.
- The pinned FFmpeg build enables Secure Transport and exactly `file,http,https,tcp,tls`. RTP/RTSP/SAP/SDP demuxers are explicitly disabled because they otherwise pull in RTP/UDP protocols.

### Exercised evidence

- Rust: **243 passed, one ignored** across the full suite; Clippy with warnings denied and formatting checks passed. Three build-configuration rejection tests cover missing TLS and unwanted protocols.
- Swift: **248 tests passed** with warnings as errors.
- The local Icecast-style HTTP integration uses real MP3/Ogg bytes and ICY framing, verifies a transient 503, title changes without replacing queue identity, non-silent PCM, restart persistence, rejected live seeks, and responsive snapshot/pause/stop commands while a server stalls. An established stream also hits its read deadline, reconnects under the same occurrence, and resumes advancing PCM.
- The native Accessibility smoke entered an invalid URL without queue mutation, added and played a valid stream, observed a changing ICY title, checked disabled live seeking/library navigation, and exercised pause/resume. Window captures were visually inspected. This exposed and fixed inherited sheet Accessibility identifiers; the existing playlist-sheet containment pattern now preserves each field's identity.
- The source-built portable FFmpeg passed the local stream integration and a real HTTPS Icecast smoke using [SomaFM's published MP3 endpoint](https://somafm.com/groovesalad.pls). The observed server was `Icecast 2.4.0-kh22-Soma1.7`; the client published `Bluetech - Unidentified Flying Octopus` and captured 124,928 non-silent scalar PCM samples. A separate self-signed TLS endpoint was rejected before an HTTP request reached it.

Local reports/captures are under `target/stream-smoke-zrbnpu96/` and `target/https-stream-g0aprz93/` in the streaming worktree. The permanent local integration is reproducible without external network access:

```sh
cargo test --test server_transport live_streams_recover_publish_icy_persist_and_keep_controls_responsive
```

`scripts/make_test_fixtures.py` regenerates `tests/fixtures/streams/live.mp3` and `live.ogg` alongside the existing fixtures. Native and real-station smoke scripts were throwaway probes; their captured evidence is local, not a claimed fresh-checkout gate.

## Saved radio stations

The sidebar's PLAYING section has a **Radio** source (`go radio`, Go menu ⌘8). It lists the server's saved stations by name, with Name and URL columns. Double-click or Return plays a station. The context menu offers Play, Add to Up Next, Edit… and Remove… (with a confirmation alert). **Add Station…** takes a stream URL and an optional name; **Import…** reads one `.pls`, `.m3u` or `.m3u8` file on the client and sends its stations in consecutive `add_stations` batches. Each batch holds at most 500 stations and its request frame fits the 16 KiB request limit; each names the revision of the reply before it. URLs that are already saved are skipped by the server, and the status bar counts the stations newly added. If a batch fails, earlier batches stay added and the error says how many stations they added. Playing or adding a station appends a stream entry with the station's URL and name (`append_stream`, then `play`), so later station edits never touch the queue.

`StationCatalog` (TyrannyLikesSmokeCore) holds the rows and builds every request behind a send seam; the app's `StationsModel` sends them on the shared connection. It refetches `stations` only when `ControlState.stations_revision` is newer than the shown revision. No fetch starts while an edit is in flight: WireClient applies the reply's snapshot before the edit resumes, and the `stations_applied` reply then updates the rows and revision. When the edit ends, a snapshot newer than the reply (another client's edit) fetches once. A `stale_stations_revision` reply refetches and shows the server's message; the edit is not retried. A station file with no HTTP(S) entries shows an error naming the file and sends nothing.

Verification (2026-10-20, after review fixes): 305 Swift tests in 44 suites and 255 Rust tests (one ignored) passed; Swift built without warnings; Clippy and formatting passed.

```sh
swift test --package-path macos --scratch-path target/swift-client --filter "StationsModel|CommandLanguage|StationFile"
cargo test --locked --test server_transport stations
```

**The live UI check was not done.** Driving the app (add, play, edit, remove a station; import a `.pls`) needs a visible window and Accessibility control, which this run could not use without a human. The Radio view, its sheet, the table's double-click/Return action and the open panel are compiled but not exercised in a running app.

## Station directory (radio-browser.info)

Radio's **Find Stations…** searches radio-browser.info by station name (protocol v13 `search_directory`). The server looks up `all.api.radio-browser.info`, then reverse-resolves each address with `getnameinfo`. It keeps only `*.api.radio-browser.info` names, in random order. A search is HTTPS only with no redirects. It tries at most 3 mirrors within 8 s, and a failed mirror moves to the back of the list. The list is resolved again after an hour or once every mirror has failed. A search returns at most 50 stations, the most-clicked first. Broken stations, HLS streams, bad stream URLs, unsafe IDs and repeated URLs are dropped. Each reply is capped at 512 KiB, and a failure returns `unavailable` with short text. Playing or adding a result sends `count_directory_click`, which the server sends on as `/json/url/{uuid}` from its own click worker. The app sends searches on their own connection, so a slow mirror never stalls the transport connection. Saves go through `StationCatalog`.

Verification (2026-10-21): 253 Rust lib tests (one ignored), 49 server-transport tests and 341 Swift tests in 47 suites passed. Clippy and formatting passed. The tests never call the live API: they use loopback HTTP origins and `tests/fixtures/radio-browser/search.json`. One `#[ignore]` test resolves the real mirrors.

```sh
cargo test --locked --lib directory
swift test --package-path macos --scratch-path target/swift-client --filter StationDirectory
```

**The live UI check was not done.** The Find Stations sheet is compiled but has not been run in the app.

## Unix-domain control transport — 2026-10-20

Replaced loopback WebSocket control with protocol-v9 JSON over Unix-domain streams across the Rust server, CLI, Swift client, managed LaunchAgent, connection settings, artwork connections, and Python gates. Frames are a four-byte big-endian length plus UTF-8 JSON; request/reply limits remain 16 KiB/8 MiB. Hello, request IDs, authoritative state ordering, deadlines, disconnect behavior, and no-mutation-replay semantics are unchanged. There is no TCP/WebSocket compatibility listener or bearer-token machinery.

The socket is `<state-dir>/control.sock`, mode `0600`, beneath an owned `0700` directory with trusted canonical ancestors. Rust checks same-effective-UID peers; Swift uses filesystem ownership and permission checks. The instance lock protects stale-socket recovery and inode-checked shutdown cleanup. Live listeners and unsafe filesystem entries are never silently replaced. Existing token files remain inert.

Integration exposed and fixed three transport-specific issues:

- Darwin's default `SO_SNDLOWAT=2048` prevented writable readiness for a peer with a 1024-byte receive buffer, timing out before Hello. Setting the send low-water mark to 1 on macOS fixed a failing real-server regression and the unread-controller gate.
- Foundation canonicalization rewrote `/private/tmp` back to the `/tmp` symlink. POSIX `realpath` and lexical managed-path normalization preserve the actual socket path.
- A missing socket initially shared the unsafe-path error, bypassing managed first-launch bootstrap. Missing parent/socket now reports `socket_unavailable`; insecure existing directories and socket entries remain `invalid_socket` and do not authorize replacement.

### Exercised verification

| Check | Result |
| --- | --- |
| Rust unit/integration tests | 231 passed; 1 hardware test ignored |
| Rust binaries and Clippy with warnings denied | Passed |
| Swift tests and debug build with warnings as errors | 243 passed; build passed |
| Real daemon/CLI transport gate | 13 passed |
| Native model/transport gate | 19 passed |
| Packaged app/LaunchAgent lifecycle gate | 12 passed |
| Library / filter / regular-playlist / smart-playlist process gates | 13 / 7 / 13 / 5 passed |

The lifecycle run used an isolated, ad-hoc-signed `target/Tyranny-Unix.app`: first-launch bootstrap, reuse, quit/resume, clean restart, crash recovery, safe uninstall, manual-server protection, and missing-decoder preflight all passed. Its temporary LaunchAgents were removed. The Connection settings window was exercised through external Accessibility and captured at `target/unix-settings.png`; it shows the Unix socket field and a connected local server.

Local reports: `/private/tmp/tyr-cb821_m5/report.json` (server), `/private/tmp/tyr-wokhy74d/report.json` (native model), `/private/tmp/tyr-1k7bpzyw/report.json` (lifecycle), `/private/tmp/tyr-lqilopfk/report.json` (library), `/private/tmp/tyr-zt3b_0ln/report.json` (filter), `/private/tmp/tyr-9yv3wxft/report.json` (playlists), and `/private/tmp/tyr-hsxywjp5/report.json` (smart playlists). These are local artifacts, not repository prerequisites.

**Limits:** verification ran on macOS with clock-paced PCM capture, not Linux or human listening. The broader core playback UI smoke did not complete: one run timed out on AX increment; a diagnostic run preserved paused seeks through that step but its mouse drag targeted the track's last frame instead of the intended position. No duplicate or unexpected play command appeared in that diagnostic trace. Production slider/playback behavior was left unchanged, and temporary tracing was removed. This is not a claim that the full playback UI gate passes.

Historical WebSocket measurements below remain historical; they are not re-labelled as Unix-socket performance results. See **Run the app** for the old-LaunchAgent cutover instructions.

## Album artwork

Artwork is implemented and verified in Rust and SwiftUI. The matching protocol-v8 development bundle is installed at `target/Tyranny.app`; the prior protocol-v7 bundle is preserved at `target/artwork/Tyranny.app`. The directories were atomically exchanged, preserving the old executable inodes; both bundle signatures verified. The user's running daemon/database were not changed. To load matching processes: **Connection Settings → Stop…, quit the client, then reopen `target/Tyranny.app`**.

### Sources, ownership and bounds

- Usable embedded artwork across an album's indexed files takes precedence over album-folder `cover`/`folder` images (`.jpg`, `.jpeg`, `.png`, case-insensitive). Malformed images fall through; incomplete/timed-out discovery reports unavailable rather than silently skipping a higher-priority source.
- Only an existing valid MusicBrainz **release** ID enables [Cover Art Archive `front-500`](https://musicbrainz.org/doc/Cover_Art_Archive/API). No artist/title guessing, MusicBrainz search, release-group substitution, arbitrary client path or provider URL. Production requests follow only trusted CAA/Archive HTTPS redirects, upgrade trusted legacy HTTP redirects before transmission, and carry an identifying tyranny-likes-smoke/version User-Agent. The development User-Agent has no invented maintainer contact.
- Two workers and a 16-request queue isolate filesystem/image/network work from playback. Workers use independent read-only library connections. A shared provider gate rechecks the cache so concurrent thumbnail/display requests do not duplicate a cold download. Network requests have a six-second total deadline, no retries, an 8 MiB streamed-body cap, and conservative shared one-second start spacing. This spacing is our choice, not a claimed CAA numeric quota; [MusicBrainz's policy](https://musicbrainz.org/doc/MusicBrainz_API/Rate_Limiting) describes its own service.
- Native FFmpeg extracts attached pictures with file-only protocols and bounded probing; production does not shell out to a decoder. Source images are limited to 8 MiB, 16 megapixels and 8,192 pixels per dimension. Returned JPEG variants preserve aspect ratio and are bounded to 128/512 pixels and 2 MiB each. Album discovery is capped at 1,024 tracks and directory enumeration at 100,000 entries; exceeding a bound reports unavailable.
- The separate private `state.artwork.sqlite` cache stores up to 128 MiB of payload/4,096 records, with SQLite overhead and a hard page ceiling. Online positives survive restart/rescan/offline use until eviction; provider misses expire after 24 hours and transient failures after 60 seconds. Completed-scan watermarks invalidate local discovery, including changed folder images with unchanged audio files. New local artwork still outranks cached online artwork. Cache failures do not prevent playback startup.
- The native client has two reusable artwork-only connections: visible album thumbnails and current-occurrence display artwork. Its decoded cache is bounded to 128 entries/16 MiB; image headers are checked before decompression. Session/revision/occurrence changes reject stale replies. Artwork requests allow 30 seconds for bounded worker/discovery/provider waits; main control requests retain their ten-second deadline. In-app Now Playing artwork is independent of the browser filter/page. System Now Playing artwork, artwork-derived themes and additional providers are not part of this slice.

Protocol **v8**, main SQLite **schema 4 unchanged**. Artwork reads do not mutate queue revisions, current selection or playback faults. Direct-path/unindexed durable queue entries are supported.

### Exercised evidence

**99 Rust tests passed, 1 ignored**; Clippy passed with warnings denied. Swift debug/release, native AX driver and standalone model builds passed with warnings as errors. Tests cover malformed embedded/folder sources, image limits, source precedence, cache expiry/persistence/coalescing, removed/stale targets, delayed provider versus live main controls, unusable cache paths and artwork wire pipelining. HTTP fixtures explicitly restore blocking mode on accepted Darwin sockets; inherited nonblocking sockets previously truncated the boundary fixture.

The full artwork gate passed **seven scenarios including five native workflows**, using a live CAA fetch for release `76df3287-6cda-33eb-8e9a-044b5e15ffdd`, decoded JPEG dimensions/pixels, later-track embedded precedence, unindexed occurrences, cover-only rescans, and offline restart/rescan. The offline daemon used an unreachable standard HTTPS proxy: its previously cached cover remained available while a new uncached release returned unavailable. Actual native controls verified browser-independent Now Playing artwork, refreshed decoded pixels during playback, neutral failure placeholders, and offline relaunch. Existing gates passed **12 protocol**, **19 native model**, **eight native filter**, **14 regular-playlist checks / 21 native playlist-and-drag cases**, and **six smart-playlist checks / eight native cases**. The earlier separate five-case smart API run also exercised migration using the old protocol-v6 executable. All use isolated state and clock-paced PCM capture, not the user's audio device.

Query-only scale runs used 50,000 metadata-only tracks, 10,000 queue occurrences and 30 samples per family. The initial artwork run missed the 100 ms p95 target for deep track/smart queries (138.48/136.35 ms). A subsequent sequential comparison found the untouched v7 deep-track baseline at 88.30 ms and v8 at 80.32 ms; all 19 families met the target in that v8 comparison. OS/filesystem caches are uncontrolled; retain both runs rather than treating a single passing run as a production latency guarantee. No large-scale native or artwork-memory profiling claim.

Reports and source/binary hashes are retained in `spike-results/artwork.json`; five native captures are in `spike-results/artwork-ui/`. Initial native verification was blocked by macOS permissions; after permissions were enabled, it exposed a missing image accessibility trait. Adding `.isImage` made the decoded artwork state available to native accessibility clients. The smoke harness now selects the exact titled row among substring-filter matches rather than assuming one result. The complete native artwork and regression runs passed afterward.

```sh
python3 scripts/artwork_gate.py \
  --server "target/tyranny likes smoke.app/Contents/MacOS/tyranny-likes-smoke-server" \
  --app "target/tyranny likes smoke.app/Contents/MacOS/tyranny likes smoke" \
  --work-dir target/artwork/gate
```

Omit `--app` for server-only checks. Native checks require Accessibility and Screen Recording permissions for the launching app. The installed development bundle remains ad-hoc signed and Homebrew-dependent, not relocatable distribution.

## Smart playlists

**Save as Smart Playlist…** beside the library filter stores a named rule such as `artist:"Miles Davis" & !album:"Kind of Blue"`. The editor previews the exact rule against the **entire available library**, independent of the selected artist/album pane. Preview and browsing use 100-row pages; invalid syntax disables Save and appears inline. Empty rules include every available track; valid rules with no matches may still be saved.

The mixed catalog distinguishes regular playlists from **Smart playlist · Live results**. **Edit Rule…** changes membership; rename/delete retain the existing playlist controls. Results refresh on library revisions, including rescans that import new matching albums or mark sources missing. Source changes clear stale selections. Smart result rows cannot be manually added, removed, reordered or used as drag destinations/sources; double-click still plays only the clicked track. The Add to Playlist picker requests regular destinations from the server, rather than filtering a partially loaded mixed catalog locally.

**Enqueue / Play** captures the full rule result, including matches hidden by the temporary entry filter. Play appends and starts the first new occurrence; it does not replace the queue. **Create Regular Playlist from Results…** atomically creates an editable snapshot of every rule match, even beyond 500 tracks, with new persisted occurrence IDs. Empty snapshots are allowed. Later rule edits, imports, missing-file reconciliation and smart-playlist deletion cannot rewrite the queue or regular snapshots.

Smart results use the library's deterministic disc/track/title/path/ID order. The server evaluates stored rule AND transient entry query in one revision-bound transaction and materializes only the requested metadata page. Catalog records have `kind`, nullable `query`, and nullable `entry_count`: regular playlists carry a count and no rule; smart playlists carry a rule and no catalog count. Opening the catalog does not evaluate every rule just to display counts. Smart row `id` equals `track_id`, not an editable persisted occurrence ID.

That slice introduced protocol **v7**, SQLite **schema 4**. Its transactional migration added a nullable rule column while retaining ordinary playlist identities, duplicate occurrence order, revisions, queue selection and checkpoints. The historical run exercised v6/schema-3 state through the old executable before migration, not by rewriting a new schema. The current Unix-socket gate exercises restart preservation instead; it no longer connects to historical WebSocket executables. Older executables cannot reopen schema 4: preserving the prior app bundle is not a database downgrade.

Reproduce with a separately built matching debug bundle:

```sh
python3 scripts/smart_playlist_gate.py \
  --server "target/tyranny likes smoke.app/Contents/MacOS/tyranny-likes-smoke-server" \
  --app "target/tyranny likes smoke.app/Contents/MacOS/tyranny likes smoke"
```

Omit `--app` for server-only checks. The gate uses 508 generated tagged real FLAC paths, then imports a new matching album through native Rescan. It verifies 506-result snapshots/enqueue, rule changes and missing files, stale authorities, read-only enforcement, independent copies, and native/daemon relaunch. Verification uses isolated clock-paced PCM capture, never the user's daemon, database or output device. Dynamic queue autofill and numeric/date rule fields remain separate work.

Verification: **72 Rust tests passed, 1 ignored**, Clippy with warnings denied, and Swift debug/release/model-driver builds with warnings as errors. The smart gate passed **7 end-to-end checks including 8 native workflows**; existing coverage passed **14 regular-playlist checks / 21 native cases**, **8 native filter cases**, and **19 transport/model cases**. Native screenshots verify stable editor positioning on syntax errors and an unclipped catalog after clearing its filter. Reports, source hashes and before/after captures are retained in `spike-results/smart-playlists.json` and `spike-results/smart-playlists-ui/`.

The query-only scale run used **50,000 metadata-only tracks**, **10,000 queue occurrences**, and **30 samples per family**. All 19 query families met the 100 ms p95 target. These are authenticated WebSocket round-trip measurements, not SQLite-only timings; this run does not claim large-scale native startup or concurrent-scan coverage.

| Smart query | p95 |
| --- | ---: |
| Two-rule catalog | 1.33 ms |
| First result page | 6.19 ms |
| Rule AND temporary entry filter | 7.82 ms |
| Result page at offset 49,900 | 69.64 ms |

## Structured live filters

Library search and the separate saved-playlist **entry filter** share the Rust parser/compiler in `src/filter.rs`. Playlist-name and root-path search remain literal.

```text
artist:"Miles Davis" & !album:"Kind of Blue"
(artist:"Miles Davis" | artist:"Ornette Coleman") & !album:"Kind of Blue"
artist:["Miles Davis", "Ornette Coleman"]
```

- `!` / `NOT` binds before `&` / `AND`, which binds before `|` / `OR`. Parentheses override precedence. Word operators must be uppercase whole words; lowercase prose remains searchable.
- Field names are lowercase `title`, `artist`, and `album`. `artist` means the track artist, including on compilations—not the album artist.
- Lists are OR alternatives for their field. Negating a list excludes all its alternatives. Lists must contain nonblank values, separated by commas; trailing commas and nested lists are errors.
- Values match Unicode-lowercased literal substrings. Quotes preserve contiguous phrases and accept `\"` and `\\` escapes. This is not regex, SQL wildcard matching, canonical Unicode normalization or full case folding.
- Bare text retains title, track artist, album, album artist and path search. Whitespace-separated predicates implicitly AND together; quote multiword text when contiguous phrase matching is intended.
- Empty/whitespace input means all tracks. Unknown fields, missing operands, malformed lists/quotes and excessive complexity return `invalid_filter` with a zero-based UTF-8 byte offset. Bounds are 1,024 input bytes, 128 expression nodes and 32 nesting/unary levels.

Every artist/album projection evaluates the same expression against an individual track; terms cannot accidentally match different tracks in a compilation. SQL parameters carry values, existing normalized columns supply field text, and narrow matching pages are materialized before decoding metadata. Counts, revisions and page rows share a read transaction. Saved results retain distinct duplicate occurrences and unavailable tracks.

Native queries debounce for 250 ms, invalidate old rows/selection immediately, and reject superseded replies. Invalid syntax stays visible beside the relevant field. Library playback and drag actions require a current valid page. Filter changes invalidate captured source drags; cross-page neighbor reads bind the destination query as well as revisions.

Filtered saved-playlist moves and copies insert before the exact matching occurrence; hidden entries retain their relative order. The next-page anchor is the next **matching** occurrence. Dropping after the final match appends to the **full** playlist, which the UI states explicitly. Whole-playlist controls say **Enqueue All Tracks / Play All Tracks** while filtering; double-click and removal still address only the clicked/selected visible occurrences. Copies that the filter hides are acknowledged explicitly.

The structured-filter slice introduced protocol **v6** with SQLite **schema 3**: `playlist_entries` requires `query` (empty for all), and its total/offset refer to matches. The current v7/schema-4 upgrade is described above and under Build, reproduce and upgrade below.

Reproduce against a separately built matching bundle:

```sh
python3 scripts/filter_gate.py \
  --server "target/tyranny likes smoke.app/Contents/MacOS/tyranny-likes-smoke-server" \
  --app "target/tyranny likes smoke.app/Contents/MacOS/tyranny likes smoke"
```

The gate uses real tagged FLACs, isolated state, real native text input and mouse drags, and clock-paced PCM capture. Saved smart-playlist rules and additional numeric/date fields are not part of this slice.

Verification retained in `spike-results/structured-filters.json` and `spike-results/structured-filters-ui/`:

- Rust: **65 passed, 1 ignored**; Clippy with warnings denied. Swift debug/release and the native driver built with warnings as errors.
- **7 real-server filter API scenarios**, **8 native filter workflows**, and continued daemon playback after controller exit. Native checks cover syntax recovery, superseded queries, Unicode input, filtered double-click, cross-page reorder, hidden insertions and visible-only removal.
- Existing saved-playlist regression gate: **14 API/persistence checks** and **21 native cases**. Native transport/model gate: **19 cases**, including immediate source-drag invalidation when filtering changes.
- Query-only scale run: **50,000 metadata-only tracks**, **10,000 queued occurrences**, **30 samples per query family**. All 15 query families met the 100 ms p95 target; this is authenticated WebSocket round-trip time, not SQLite-only time. Native startup and concurrent-scan scale scenarios were not rerun.

| Structured scale query | p95 |
| --- | ---: |
| Track artist list with album negation | 37.03 ms |
| Artist projection with grouped title alternatives | 19.61 ms |

The UI driver transports Unicode values as base64 because Foundation process arguments decompose accented text, and targets inter-row gaps for insertion gestures. These are harness corrections; matching remains literal Unicode-lowercase substring matching. Verification uses isolated clock-paced capture, not the user's database or audio device. This remains a development bundle with non-system dynamic dependencies, not a relocatable release.

## Native multi-row drag and drop

Select one or more rows and drag:

- **Within the queue or a saved playlist:** reorder the selected occurrences as one ordered block. Noncontiguous selections keep their source order; duplicate tracks retain distinct occurrence IDs.
- **From the library:** copy tracks into the queue or a saved playlist at the native insertion line. Empty destinations accept drops. Copying does not start or stop playback.
- **Across panes:** hold over the Queue or Playlists toolbar control for 650 ms, then continue into the destination without releasing. Dropping on a playlist catalog row appends; hovering that row selects it so the entries table can accept a positioned drop.
- **Across pages:** hover Next/Previous while holding the drag. Each hover advances one bounded page; leave and re-enter to advance again. Dropping after a nonfinal page resolves the next occurrence with a one-row revision-bound read, not a global append.

The drag slice introduced protocol **v5** with SQLite **schema 3**; the current v7 contract retains these batch edits for regular playlists. Batch moves and inserts are single durable transactions, not loops of client mutations. Moves take 1–500 distinct occurrence IDs and preserve server-side source order; inserts preserve supplied track order and duplicates. Selected-anchor and already-positioned moves are validated no-ops without revision increments. Invalid members/anchors, stale revisions and failed persistence cannot partially edit a collection. Reconnect invalidates captured drags; uncertain commands are never replayed.

The shared `TrackDrag.swift` module owns public `NSTableView` instances, native pasteboard writers, insertion feedback, double-click activation, and public native hover destinations. Source tables stay alive until their drag session ends, including spring navigation and cancellation. Hover timers run in common run-loop modes; ordinary mouse clicks pass through the hover regions. SwiftUI drop delegates did not receive these native table drags in the observed run, so the cutover uses AppKit destinations rather than retaining an ineffective second path. Native tables reuse cells/rows and keep displayed data bounded to 100 rows.

Verification passed: **59 Rust tests (1 ignored)**, Clippy with warnings denied, Swift debug/release builds with warnings as errors, **12 real-server compatibility scenarios**, **12 playlist/batch API scenarios plus two post-native restart checks**, **nine native drag scenarios**, the **11 existing playlist/double-click scenarios and relaunch browsing**, and **18 real-client model scenarios** including stale/reconnected drag rejection. The existing modal regression caught a SwiftUI exclusivity crash during hover-region teardown; releasing the binding before stopping the native region fixed it, and the complete native gate then passed.

The actual insertion-line screenshots and ordered occurrence snapshots are retained in [`spike-results/drag-drop.json`](spike-results/drag-drop.json), [queue insertion](spike-results/drag-drop-ui/queue-insertion-indicator.png), [playlist insertion](spike-results/drag-drop-ui/playlist-insertion-indicator.png), and [saved order after dragging](spike-results/drag-drop-ui/saved-playlist-after-drag.png). Verification used isolated state and clock-paced real PCM capture; this is not a new CoreAudio listening or large-playlist latency certification.

That slice installed a matching v5 bundle with the previous executable inodes retained under `target/drag-drop/Tyranny.app`. Its user processes and database were left untouched. Follow the current matching-version upgrade instructions below.

## Saved playlists: implemented and exercised

Protocol v7 and SQLite schema 4 retain saved playlists independently of the playback queue. Regular playlists support create/rename/delete, literal catalog filtering, structured entry filtering, ordered entry pages, multiselect removal, native batch drag reordering, and single-entry Move Up/Down across page boundaries. Select tracks in the library and choose **Add to Playlist…** to use a paged regular-destination picker or enter a new name and choose **Create & Add Tracks** without leaving the library. Native pages contain at most 100 rows; the protocol permits 1–500. Smart rule workflows are described above.

**Enqueue** appends the entire saved playlist. **Play** also appends, then starts its first newly queued occurrence; neither action replaces the existing queue. Editing or deleting a saved playlist never edits already-enqueued occurrences. Regular playlists preserve duplicates with distinct, non-recycled occurrence IDs. Names are trimmed, limited to 256 UTF-8 bytes, and unique under Unicode lowercase matching across both kinds. Missing indexed sources stay visible as unavailable in regular playlists after rescan; smart results instead reflect the available library.

The owner caches the catalog revision; position events do not query playlist tables. Bounded SQL pages join current indexed metadata. Batch reordering computes the ordinal permutation inside SQLite without hydrating the whole playlist into the client or owner. Queue edits prepare permutation indices, capacity and tokens before the durable transaction; optional whole-playlist Play also prepares its first decoder. The matching engine commit does not reopen sources or allocate. Invalid sources, stale revisions and persistence errors cannot leave a partial queue edit. Filesystem changes after preflight and subsequent decode failures follow normal playback policy.

### Playlist protocol

| Command | Authority and fields |
| --- | --- |
| `saved_playlists` | Catalog `revision` (null initially), `offset`, `limit`, literal substring `query`, optional `kind` (`regular`, `smart`, or null/omitted for both). Returns ID, name, individual revision, kind, nullable rule query and nullable entry count. |
| `playlist_entries` | `playlist_id`, individual `revision`, `library_revision`, `offset`, `limit`, and required structured `query` (empty for all). Both revisions are required after offset zero. Total/offset refer to matches; rows contain ID, indexed track, path, metadata, availability and error. Regular row IDs identify occurrences; smart row IDs identify tracks and are read-only. |
| `create_playlist` | `expected_revision` binds snapshot `playlists_revision`; `name`. |
| `create_smart_playlist` | Catalog `expected_revision`, unique `name`, and structured rule `query`. |
| `update_smart_playlist` | Smart `playlist_id`, individual `expected_revision`, and replacement `query`. |
| `snapshot_smart_playlist` | Source `playlist_id`, `playlist_revision`, `library_revision`, catalog `expected_revision`, and new regular-playlist `name`. Captures all rule matches in one transaction; returns the new regular playlist. |
| `rename_playlist`, `delete_playlist` | `playlist_id`, `expected_revision` binds the individual playlist; rename also supplies `name`. |
| `playlist_add` | Regular playlist only: individual revision, `library_revision`, 1–500 `track_ids`, and `before` occurrence ID or null for the end; preserves supplied order and duplicates, all-or-nothing. |
| `playlist_remove` | Regular playlist only: individual revision and 1–500 distinct `entry_ids` belonging to that playlist; all-or-nothing. |
| `playlist_move` | Regular playlist only: individual revision, 1–500 distinct `entry_ids`, and `before` occurrence ID or null for the end. Preserves source order; selected-anchor and already-positioned moves are no-ops. |
| `enqueue_playlist` | Either kind: `playlist_id`, `playlist_revision`, `library_revision`, queue `expected_revision`, and `play`. Captures every occurrence/rule match, independent of entry filtering. One queue revision increment for the entire append. |

Edits return `playlist_applied` with the authoritative snapshot and resulting playlist header (null after deletion). Queue loading returns ordinary `applied` with the first newly queued ID. `stale_playlist_revision`, `stale_library_revision` and `stale_revision` require reviewing refreshed state, never automatic mutation replay.

### Verification and regression

- 56 Rust tests passed; the opt-in hardware test remains ignored. Coverage includes schema 1/2 migration, restart, identity non-reuse, multi-client conflicts, duplicate occurrences, rollback, capture-source protection, bulk loading beyond a server page, and gap-safe moves at SQLite's signed-integer boundary.
- The real-process playlist gate passed 11 scenarios. A preserved protocol-v3 executable created the actual schema-2 fixture; migration retained its library, queue, selected occurrence, position and settings. Injected SQL failure, an unplayable first source and a disappeared library source left queue/selection intact. An entire 501-entry playlist loaded without client hydration or page truncation.
- Actual external Accessibility input passed seven initial native scenarios plus restart browsing. It added 105 tracks, moved across both page boundaries, renamed, removed two, enqueued/played, and confirmed deletion of a second playlist without changing playback. The retained 103-entry playlist and its exact occurrence order survived daemon and controller restart; the queue contained 206 independent occurrences.
- The existing native-model gate passed 16 cases. The library gate passed 13 cases with 2,048 bulk fixture paths, including schema-1 migration and scanning during playback. An initial 128-path run was too brief for its existing playback-progress assertion; the workload was restored, not the assertion weakened.
- Swift debug/release builds with warnings as errors, Clippy, Rust formatting and bundle signature checks passed. All new runtime verification used isolated state, generated media and clock-paced PCM capture. Native playlist snapshots reported zero consumer underruns. This slice makes no new device-output, listening, large-playlist performance or clean-machine distribution claim.

The first native cross-page move exposed a query-ordering bug: the client requested offset 100 with a null individual revision while its catalog refresh was pending. The server correctly rejected it. A focused real-model probe failed before the fix and passed afterward; catalog completion now establishes the individual revision before resuming the desired entry page. The unchanged native regression then passed in both directions.

Evidence: [`spike-results/saved-playlists.json`](spike-results/saved-playlists.json), [saved playlist](spike-results/saved-playlists-ui/saved-playlist.png), [cross-page move](spike-results/saved-playlists-ui/page-boundary-move.png), and [restart persistence](spike-results/saved-playlists-ui/restart-persistence.png).

### Create directly from the destination picker

The picker now creates and populates a new playlist using the captured library selection. It uses the creation reply's identity directly, so filtering or a new playlist sorting beyond the first 100 catalog rows cannot redirect the add. Successful completion dismisses the modal; errors stay inside it with selection preserved. Creation and addition are two acknowledged commands, not one transaction: if creation succeeds but addition fails or its outcome is uncertain, the playlist is retained and the modal explains the partial outcome. Commands are never automatically replayed.

The updated real native gate passed eight initial scenarios plus restart browsing, including creation with 100 selected tracks, duplicate-name rejection without closing the picker, and subsequent addition to an existing playlist. Separate real-model probes used a 102-playlist catalog to verify off-page creation/addition and injected an SQL add failure to verify the partial-outcome warning, retained empty playlist, unchanged queue, and preserved selection. Debug and release builds passed with warnings as errors. Verification used isolated clock-paced capture, not the user's audio device.

The original picker slice was client-only, protocol v4/schema 3, with its previous executables preserved under `target/playlist-picker/Tyranny.app`. That historical update left the server binary unchanged; subsequent protocol changes require both processes to match. Evidence: [`spike-results/playlist-picker.json`](spike-results/playlist-picker.json), [inline creation](spike-results/playlist-picker-ui/create-from-picker.png), and [visible modal error](spike-results/playlist-picker-ui/picker-create-error.png).

### Double-click a track to play it

Double-clicking a library or saved-playlist row appends **only that track** and immediately plays the acknowledged new queue occurrence. Double-clicking a queue row plays the existing occurrence without appending; double-clicking the current row restarts it. The clicked row is used even when several rows are selected. Single-click selection stays native.

The shared native tables now use `NSTableView.doubleAction` and `clickedRow`, resolving against displayed stable IDs rather than treating the whole selection as the clicked track. The earlier event-monitor adapter was removed during the native drag-table cutover. Library/playlist append and play remain separate acknowledged commands: a successful append is retained if play fails, with the partial outcome reported rather than commands replayed.

Debug and release builds passed with warnings as errors. The full isolated gate passed all 10 backend/persistence cases, 11 initial native scenarios, and restart browsing. Actual CGEvent mouse clicks verified single-click selection without playback, exact clicked-track activation after multiselection, queue resume/restart without duplication, and unchanged saved-playlist contents. Verification used clock-paced PCM capture, not the user's output device. Evidence: [`spike-results/double-click.json`](spike-results/double-click.json) and [native playback with two selected rows](spike-results/double-click-ui/double-click-playback.png).

The original double-click slice was client-only, protocol v4/schema 3; its prior executable inodes remain under `target/double-click/Tyranny.app`. That historical bundle left the server unchanged. Follow the current matching-version upgrade instructions below for drag-and-drop support.

### Build, reproduce and upgrade

Client and daemon must both use **protocol v7**. Existing SQLite schema 3 migrates transactionally to schema 4 without a library reimport; schemas 1 and 2 migrate through the prior steps. The schema-4 database is not backward-compatible with older executables. Use **Connection Settings → Stop…**, quit the old controller, then reopen `target/Tyranny.app`. Normal bootstrap starts the matching daemon with the existing library, queue and resume policy.

Development bundles are installed through an atomic directory swap so running executable inodes remain intact. Stop the app/daemon before rebuilding the normal bundle in place with the command below; verification uses separately staged bundles and isolated databases.

```sh
scripts/build_macos.sh
swiftc -swift-version 6 -warnings-as-errors scripts/native_ui_driver.swift \
  -o target/native-ui-driver
python3 scripts/playlist_gate.py \
  --server "target/tyranny likes smoke.app/Contents/MacOS/tyranny-likes-smoke-server" \
  --app "target/tyranny likes smoke.app/Contents/MacOS/tyranny likes smoke"
```

Omit `--app` for API-only verification. The current gate exercises persistence through current-server restarts; historical old-executable migration runs are recorded above, not supported by a WebSocket compatibility path. Native verification includes real multi-row dragging, spring navigation, page-boundary moves, cancellation and the existing playlist/double-click workflows; it requires an unlocked desktop and Accessibility/Screen Recording permissions. Dynamic queue autofill, import/export and production-scale playlist-reorder benchmarking remain separate work.

## Playback controls: implemented and exercised

Protocol v3 adds server-owned Next/Previous and atomic play/pause toggle, occurrence-bound seeking, and authoritative Now Playing metadata. Next follows current queue order without wrapping. Previous restarts at or after three seconds, otherwise chooses the predecessor (or restarts the first entry). Paused navigation stays paused. Destination preparation and persistence complete before audible selection changes; failed selection leaves the current occurrence intact.

The server caches display metadata from the already-open decoder input, associates it with the consumed occurrence rather than prefetched audio, and shares it without copying strings on each state publication. Pause, end and reconnect retain metadata; stop clears it. Malformed display tags produce a visible, nonfatal warning rather than rejecting valid audio. Queue/library paging does not supply the now-playing display.

The native seek bar previews mouse dragging and commits once on release, supports keyboard and Accessibility adjustment, clamps to the last valid frame, and cancels a gesture when its occurrence/session changes. The server independently rejects a stale occurrence ID. Unknown duration disables the slider instead of inventing a range.

[`SystemMediaControls.swift`](macos/Sources/TyrannyLikesSmoke/SystemMediaControls.swift) uses Apple's public [MPRemoteCommandCenter](https://developer.apple.com/documentation/mediaplayer/mpremotecommandcenter) and [MPNowPlayingInfoCenter](https://developer.apple.com/documentation/mediaplayer/mpnowplayinginfocenter). All actions go through the same native model and Rust commands; there is no client audio engine. Callback receipts are fenced across disconnect/reconnect, seek captures the occurrence at receipt, and cleanup unregisters handlers without clearing another application's information. The app stays running after its last window closes; Dock/LaunchServices reopening restores the window. Explicit Quit removes its system controls but leaves the daemon playing.

### Verification

- 48 ordinary Rust tests passed; the opt-in hardware regression remains ignored in that run. New authenticated-server tests cover queue order and the exact three-second boundary, paused/ended behavior, stale seek, prefetch/natural transition/reconnect metadata, malformed display metadata, and preservation of the current selection when the next source cannot open.
- The real native model passed 16 cases. The displayed SwiftUI app passed 12 initial cases plus relaunch, with two independent observations of daemon playback surviving client exit. Actual mouse, keyboard and Accessibility input exercised the seek bar, including its duration boundary. Filtering removed every visible library row without losing now-playing information.
- The native run used a release Rust helper, debug SwiftUI controller, real CoreAudio output and silent 120-second WAV fixtures. Reported consumer underruns stayed zero. A separate real FLAC with an oversized display tag retained its valid −6 dB ReplayGain.
- Fourteen additional scenarios exercised actual Control Center play/pause/next/previous, published title/artist/album, disabled Next at queue end, closed-window control, window reopening, disconnect cleanup, single-handler reconnect, and quitting without stopping Rust playback. A real mouse drag overlapping another controller's Next did not seek the successor.
- Synthetic system media-key events exercised toggle/next/previous, **not physical keyboard or headphone hardware**. This macOS Control Center card did not expose a position slider, so there is no end-to-end system seek-slider claim. macOS chooses the global Now Playing application and may show a fallback app after this controller disconnects.

The first keyboard smoke failed because synthesized events were posted before AppKit's key-window focus settled; adding focus-settle/key-hold timing to the external driver exercised the unchanged native slider successfully. Closing the window also exposed a missing reopen action; the app delegate now uses SwiftUI's `openWindow`, verified through LaunchServices. Debug and production Swift builds with warnings as errors, Clippy, formatting, and app signing checks passed.

Evidence: [`spike-results/playback-controls.json`](spike-results/playback-controls.json), [filtered library with retained now playing](spike-results/playback-controls-ui/filtered-library.png), and [actual Control Center card](spike-results/playback-controls-ui/now-playing.png). Older scale/audio/lifecycle matrices below remain historical, not newly rerun certifications.

### Build and upgrade

The client now requires **Swift 6.2 or newer** for actor-isolated teardown; the deployment target remains macOS 14. Verification used Swift 6.4 on macOS 26.6.2.

That controls slice used protocol v3 and SQLite schema 2. The current matching-version upgrade procedure is in the saved-playlists section above. `scripts/build_macos.sh` remains the normal build command; stop the app/daemon before rebuilding that in-use bundle.

That slice staged its updated bundle with an atomic directory swap, retaining the old bundle's in-use files. Neither existing user process was restarted and no user state was written. At that staging, the still-running v2 daemon reported `audio output: CoreAudio output previously failed`, unchanged across the swap. The controls slice did not claim device-failure/hotplug recovery.

## Default-output compatibility correction

A normal app launch exposed a missing device case: the Sony WF-1000XM5 default output ran at 44.1 kHz and advertised only 16/44.1 kHz. Our 48 kHz hardware check rejected it. Removing that check alone was insufficient: a direct CPAL 0.18.2 stream probe also failed because that backend tried to set the hardware nominal rate to 48 kHz.

The output adapter now uses Apple's **DefaultOutput AudioUnit** directly, configuring only its client input as stereo F32/48 kHz. CoreAudio converts to the device's native format; neither the device's physical format nor nominal clock is set. Decoder behavior, frame-based positions, persistent checkpoints, and protocol semantics remain unchanged. This follows Apple's [Default I/O unit conversion contract](https://developer.apple.com/library/archive/documentation/MusicAudio/Conceptual/CoreAudioOverview/ARoadmaptoCommonTasks/ARoadmaptoCommonTasks.html) and [AudioUnit scope/bus model](https://developer.apple.com/library/archive/technotes/tn2091/_index.html). CPAL and its obsolete error adapter have been removed rather than retained as a second output path.

Verification on that same 44.1 kHz headphone route:
- The opt-in public `Output` regression failed with the original error before the fix, then passed: 48,000 frames consumed, final seek-relative position 72,000, ordered occurrence starts, stable pause/resume, eventual drain, and zero reported underruns. The default device and rate remained unchanged through open, playback, and teardown.
- All 44 ordinary Rust tests and Clippy passed. The hardware regression is ignored by default so ordinary tests do not require or activate audio hardware.
- The rebuilt release-helper app passed all ten LaunchServices/lifecycle cases using real decoding/output with **silent WAV fixtures**. Temporary jobs were removed.
- The user's existing job was stopped during helper replacement and restored without replacing its state. Its authenticated v2 endpoint became ready with no playback error; the reopened native app displayed **Connected**. The headphones remained at 44.1 kHz.

```sh
cargo test --locked --test playback \
  coreaudio_hardware::hardware_output_preserves_device_rate_and_48khz_timeline \
  -- --ignored --exact --nocapture
```

[`spike-results/coreaudio-output-fix.json`](spike-results/coreaudio-output-fix.json) retains the compatibility evidence and current source/binary hashes. The older scale, audio-format and lifecycle reports below remain historical measurements, not freshly rerun full matrices for this adapter. Silent device verification establishes initialization, frame accounting and lifecycle—not acoustic quality, Bluetooth latency accuracy, or arbitrary route/hotplug behavior. Drain remains a conservative native timestamp/latency estimate; device-frame latency is converted using the native rate, not the 48 kHz application rate. Native callback storage is released after disposal; a failed disposal aborts rather than risking a callback use-after-free.

## Library and queue scale

The reproducible gate uses a **release Rust helper and instrumented debug SwiftUI client** on an Apple M4 Max / 64 GiB / macOS 26.6.2 arm64, with Swift 6.4. It creates the database through the real server, stops it, and explicitly seeds 50,000 metadata-only tracks, 5,000 albums, 500 artists, and 10,000 queue occurrences. The library paths are deliberately nonexistent; queue entries reference one real generated WAV. This measures an existing index, not import or decoding of 50,000 real music files.

Each output mode measures 30 authenticated WebSocket round trips for each of 13 query families, including deep pages, selective/broad/empty filters, and queue pages. Timings include serialization, transport, and Python JSON decoding. The native observer launches one process-cold client before query warmup, then two warm clients against the same daemon. First usable means an actual native row can be selected through external Accessibility, not merely that the model received JSON. Process-cold includes fresh daemon/client processes; OS/filesystem caches are uncontrolled and are not purged.

Native filtering is measured separately from RPC latency and includes the 250 ms debounce and external Accessibility overhead. The observer requires a changed result total and actual row selection; it does not wait for unrelated background refreshes to become idle. Accessibility lookup waits for the actual control and reacquires focus, but never retries a mutation after an uncertain result.

The final warm run starts a genuine scan of 16,384 real WAV hard links while the existing queue plays. The full native gate requires a filter to start **and complete** during active scanning, successful import of every fixture, advancing playback, and no increase in reported consumer underruns. Client model/reply pages remain bounded at 100 rows; this is not heap profiling. The server still loads its persistent queue at startup.

```sh
scripts/build_macos.sh
swiftc -swift-version 6 -warnings-as-errors scripts/native_ui_driver.swift \
  -o target/native-ui-driver
python3 scripts/scale_gate.py
python3 scripts/scale_gate.py --coreaudio
```

The initial debug-server deep-track query missed the target at 203.91 ms p95. Track-first ordering and late metadata materialization reduced that to 152.20 ms in the debug build, preserving path/ID tie order, filtering, availability, errors, and revision checks. The same query in the release-server comparison measured 62.43 ms. The bundle now builds its Rust helper with `--release`; do not attribute the entire improvement to the SQL change or substitute debug measurements for deployment-profile results.

Both functional correctness and performance-target verdicts are retained independently. Generated media, private database/token files, and raw working directories stay under ignored `target/`.

### Final scale measurements

Both output modes pass the full functional gate and both proposed performance targets. Values below are milliseconds; warm startup has only two samples per mode, process-cold startup one, and each RPC family thirty. These are observed development-machine results, not statistical deployment guarantees.

| Measurement | Clock-paced PCM | CoreAudio |
| --- | ---: | ---: |
| Warm client first usable, maximum | 934.93 | 936.45 |
| Process-cold client first usable | 1,240.34 | 907.39 |
| Process-cold daemon + client first usable | 1,305.37 | 1,006.93 |
| Worst RPC p95: deep track page | 61.37 | 63.17 |
| First track page RPC p95 | 7.20 | 7.23 |
| Worst queue page RPC p95 | 1.53 | 1.48 |
| Worst native filter p95 during playback | 599.16 | 482.24 |
| Native filter during active scan, maximum of two | 1,054.24 | 1,017.03 |

All four scan/filter samples started and completed while indexing was active. Each final index contains 66,384 tracks. All six native runs and both query sweeps show advancing playback with zero reported consumer underruns. Cold startup is deliberately separate from the one-second **warm** target.

[`spike-results/scale-gate.json`](spike-results/scale-gate.json) retains final reports, raw samples, binary/source hashes, native model compatibility results, and earlier performance/automation failures. Inspected native screenshots: [50,000-track library](spike-results/scale-ui/library.png), [10,000-entry queue](spike-results/scale-ui/queue.png), and [filtering during the real scan](spike-results/scale-ui/scanning.png).

## Managed macOS process: implemented and exercised

Both real lifecycle runs passed **10/10 cases**: clock-paced PCM and CoreAudio. They launch a copied `.app` through `/usr/bin/open -n -W` with a minimal environment, rather than executing it from the build shell. Each run uses its own state, label, and agent directory; all temporary jobs are removed afterward. The user's normal login job and library are not used.

- [`ManagedServer.swift`](macos/Sources/TyrannyLikesSmoke/ManagedServer.swift) owns the small launchd interface: authenticated readiness, install/start, graceful restart, and explicit removal. The client does not become a playback-process parent.
- Only literal local addresses are managed. State/log/plist ownership and permissions are checked, symlinks and incompatible jobs are rejected, and subprocesses use absolute executable paths with bounded waits. No shell interpolation or `sudo`.
- The helper runs with a cleared environment, private logs, `KeepAlive`, a ten-second restart throttle, and a graceful-exit window. Authentication failures do not trigger automatic replacement. Bootstrap failures are visible and require an explicit Retry, rather than an unbounded installation loop.
- Existing authenticated manual servers are reused. Restart refuses a manual owner; removal leaves it running. Registered-job ownership checks fail closed unless both the owned plist and loaded origin/arguments agree.
- The helper's actual `--help` execution is checked before registration. The missing-decoder case rewrites a copied helper's FFmpeg load command: dyld fails, the native app shows an actionable error/log path, and no restart-loop job is installed. Installed Homebrew libraries are not modified.

The native gate exercises actual Start-up, Restart and Stop control paths, client quit/relaunch during playback, launchd recovery after clean exit and SIGKILL without a controller, persisted library/queue state, unavailable and permission-denied roots, manual-owner protection, and missing-library diagnostics. Restarts intentionally interrupt playback; controller exit does not.

```sh
scripts/build_macos.sh
swiftc -swift-version 6 -warnings-as-errors scripts/native_ui_driver.swift \
  -o target/native-ui-driver
python3 scripts/lifecycle_gate.py
python3 scripts/lifecycle_gate.py --coreaudio
```

LaunchServices gives the app its own privacy identity. The debug observer therefore asks the terminal-owned harness to perform public Accessibility actions and composited-window captures under the terminal's existing grants. No Apple Events permission or app-owned screen-recording grant is required by the normal player.

[`spike-results/lifecycle-gate.json`](spike-results/lifecycle-gate.json) retains both final release-helper lifecycle runs, cleanup evidence, source/binary hashes, and the native dependency inventory. Inspected native screenshots: [server controls](spike-results/lifecycle-ui/managed-server.png), [unavailable root with index retained](spike-results/lifecycle-ui/unavailable-root.png), and [missing decoder diagnostic](spike-results/lifecycle-ui/missing-decoder.png).

### Development packaging limits

[`scripts/macos_dependencies.py`](scripts/macos_dependencies.py) inventories transitive Mach-O dependencies into `Tyranny.app/Contents/Resources/development-dependencies.json`. The observed bundle has two executables and fifteen external non-system dylibs: FFmpeg's `avformat`, `avcodec`, `avutil`, `swresample`, plus dav1d, LAME, libvpx, mpg123, OpenSSL crypto/SSL, Opus, SVT-AV1, x264, x265, and liblzma. No unresolved load-command dependency was found on this Mac.

This is still an **ad-hoc-signed, non-sandboxed, Homebrew-linked development bundle**, not a relocatable installer. The inventory records install names, resolved paths and rpaths, including the Swift executable's Xcode toolchain rpath. Running with a clean environment does not establish operation on a Mac without Xcode/Homebrew. Optional runtime plugins, licensing/redistribution, notarization, minimum-OS deployment, sandbox bookmark transfer, TCC/Full Disk Access grants, and login/logout across a real user session remain separate checks.

The launchctl ownership guard intentionally depends on diagnostic `print` output and refuses changes if it cannot recognize that output. Apple does not promise that format is stable; this is a development adapter, not an SMAppService distribution claim. See Apple's [launchd guide](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingLaunchdJobs.html) and the installed `launchctl(1)`, `launchd.plist(5)`, and `open(1)` manuals for the platform contracts.

## Native SwiftUI controller

The development app is a real macOS SwiftUI controller of the Rust server—not another playback engine. Its three panes are **album artists | albums | tracks**, with a separate queue and persistent transport. Normal launches now bootstrap a per-user development LaunchAgent when the local server is absent; an already authenticated server is reused.

### Run the app

With the FFmpeg prerequisites below installed:

```sh
scripts/build_macos.sh
open "target/tyranny likes smoke.app"
```

The build includes the real Rust helper. On first normal launch, the app installs `~/Library/LaunchAgents/dev.tyranny.server.plist` and starts the helper through `launchd`. The job persists independently of the client and is configured to start at login. Keep the development app at its installed path; stop its background job before moving or deleting the bundle.

Choose a music folder, wait for tracks to appear, select an artist and album or browse all tracks, then select tracks and use **Enqueue** or **Play Selection**. Play Selection appends the selection in displayed order and starts its first new queue occurrence; it does not replace the existing queue. Queue controls play, remove, move, and clear occurrences. Transport supports play/pause, stop, and a seek position in seconds, plus ReplayGain and the server's persisted resume-on-start preference.

The native default is `~/Library/Application Support/tyranny/control.sock`. Connection Settings can select another private Unix socket for this session. An isolated server can also be selected at launch:

```sh
open -n "target/tyranny likes smoke.app" --args \
  --socket /absolute/private/state/control.sock
```

Providing `--socket PATH` selects an **unmanaged** connection: the app will not install or restart a server for it. Connection Settings on a normal managed launch exposes Start, Restart, and Stop. Restart/Stop require confirmation. Stop removes the owned job and plist, not your music/database; opening the app again enables bootstrap. Manually started servers are never stopped or replaced by those controls. For isolated managed launches, supply `--managed-state-dir`, `--managed-label`, and `--managed-agent-dir` together.

Upgrading from a WebSocket build automatically replaces a recognized tyranny likes smoke LaunchAgent, including one whose old app moved or was deleted. Recognition requires the same label, state paths, launch policy and environment, a bundled `tyranny-likes-smoke-server` path, and the exact retired loopback-bind shape; loaded-job arguments must still match the saved plist. Foreign jobs and manually owned servers remain untouched. Old `--url`, `--token-file`, and `--managed-bind` options are rejected; existing token files are ignored. The cutover does not migrate or delete the music database.

This is an ad-hoc-signed, non-sandboxed development `.app`, built with Swift 6 for macOS 14+. Choosing a folder does not grant the separate daemon sandbox or TCC access. The server must independently be able to read it. Closing the client disconnects control only; it does not stop playback.

### Client boundaries and invariants

- [`macos/Sources/TyrannyLikesSmoke/WireClient.swift`](macos/Sources/TyrannyLikesSmoke/WireClient.swift): protocol-v24 framed JSON over a Unix stream, private-socket validation, finite request/message bounds and deadlines, generation guards, monotonic authoritative snapshots, and explicit uncertain outcomes. No automatic mutation replay.
- [`PlayerModel.swift`](macos/Sources/TyrannyLikesSmoke/PlayerModel.swift): connection/reconnect ownership, serialized mutations, 100-row replacement pages for every collection, stable selection IDs, superseded-query rejection, debounced filtering, and scan-refresh coalescing. It never hydrates the whole queue or library. Moving an entry across a page boundary reads at most one additional neighbor under the same queue revision.
- [`ContentView.swift`](macos/Sources/TyrannyLikesSmoke/ContentView.swift): native lists/tables, a nonblocking folder-picker sheet, roots/rescan controls, discovery/indexing progress, explicit disconnected/empty/error states, queue edits, and transport/settings. Unknown duration remains unknown rather than showing a fabricated seek range.
- [`TyrannyLikesSmokeApp.swift`](macos/Sources/TyrannyLikesSmoke/TyrannyLikesSmokeApp.swift), [`macos/Package.swift`](macos/Package.swift), and [`scripts/build_macos.sh`](scripts/build_macos.sh): the standalone native application and reproducible development bundle.

Reconnect clears cached pages and selections, fetches fresh state, invalidates old drag snapshots, and never resends prior mutations. Unsafe socket paths and unavailable servers remain visible while the connection loop backs off.

### Reproduce the native gates

```sh
swiftc -swift-version 6 -warnings-as-errors scripts/native_ui_driver.swift \
  -o target/native-ui-driver
scripts/build_macos.sh
python3 scripts/client_gate.py
python3 scripts/client_gate.py --coreaudio

python3 scripts/client_gate.py --model-only
python3 scripts/client_gate.py --model-only --coreaudio

swift build --package-path macos --scratch-path target/swift-client \
  -c release -Xswiftc -warnings-as-errors
```

The UI gate needs an **unlocked graphical session** and Accessibility/Screen Recording permission for the launching terminal. It drives actual SwiftUI controls with an external public Accessibility client, confirms the actual native folder picker, and captures composited windows. No Apple Events permission, synthetic server, direct database mutation, or model-call substitute for UI actions is used. Debug-only smoke hooks choose the isolated fixture's initial picker directory and observe state; release builds exclude them.

The separate model gate makes no visual-verification claim. It covers unsafe socket-path/type rejection, bounded pages, selection, transport/settings, queue edits, debounced queries, reconnect, controller-independent playback, stale revisions, cross-page queue moves, and a deliberately suspended server's lost-reply timeout without replay. The cross-page move regression failed before the neighbor lookup fix and passed afterward.

### Native verification results

| Output | UI/lifetime cases | Native model cases | Reported playback underruns |
| --- | --- | --- | --- |
| Clock-paced PCM | 13/13 | 12/12 | 0 |
| CoreAudio | 13/13 | 12/12 | 0 |

UI totals include initial interaction, relaunch, and independent server-lifetime checks. The native surface imports 105 generated real WAV paths, then rescans 1,024 additional hard-linked paths while playing. Live filtering completes within the two-second smoke threshold while indexing is still active; this is not a p95 or production-scale benchmark. The completed index contains 1,129 tracks, with 100-row client pages.

The phase-4 debug bundle, optimized release build with warnings treated as errors, and standalone native drivers compiled on macOS 26.6.2 arm64 / Swift 6.3.3. That slice changed no Rust implementation. Phase 5 subsequently optimized the library query and reran all 44 Rust tests, formatting, Clippy with warnings denied, and native model compatibility (12/12 in each output mode). The current optimized Swift build also passes warnings-as-errors under Swift 6.4. Earlier audio/library reports below remain prior-stage evidence, not newly rerun full matrices.

[`spike-results/client-gate.json`](spike-results/client-gate.json) retains both output-mode reports, model cases, regression evidence, and source/binary/screenshot hashes. Inspected native screenshots: [library and transport](spike-results/client-ui/playing.png), [queue](spike-results/client-ui/queue.png), [filtering during indexing](spike-results/client-ui/scanning.png), [disconnected](spike-results/client-ui/disconnected.png), and [relaunch](spike-results/client-ui/relaunch.png). Generated media, private state, and raw run logs remain under ignored `target/client-gate/`.

## Durable library: implemented and exercised

The server now imports folders through native FFmpeg, indexes them on a dedicated SQLite worker, and serves album-artist/album/track pages without loading the collection into the playback owner. Both real-process library runs passed; the existing server gate also passed 12/12 cases in each output mode. **43 Rust tests**, formatting, Clippy with warnings denied, and binary builds passed.

| Library gate | Cases passed | Candidate files | Imported tracks | Maximum observed snapshot latency during import | Reported underruns |
| --- | --- | --- | --- | --- | --- |
| Clock-paced PCM | 13/13 | 2,058 | 2,056 | 1.54 ms | 0 |
| CoreAudio | 13/13 | 2,058 | 2,056 | 1.51 ms | 0 |

These are generated real FLAC/WAV fixtures, including 2,048 hard-linked paths to one FLAC—not a commercial collection or a cold-disk/50,000-track benchmark. Two candidates deliberately fail: corrupt audio and a permission-denied file. A symlink cycle and non-audio file are skipped. Latencies are individual run maxima, not p95 certification. Actual CoreAudio execution does not establish human listening.

The refreshed native audio gate also passed **22/22** numerical/device cases after the shared FFmpeg initialization and gain-metadata changes; thresholds were unchanged.

### Library implementation and invariants

- [`src/metadata.rs`](src/metadata.rs): native FFmpeg stream probing, Unicode tags, duration/audio properties, track/disc numbering and totals, compilation, ReplayGain/peaks, and MusicBrainz release/release-group/recording/release-track/artist IDs. No production encoder/decoder subprocess or full-track PCM extraction during indexing.
- [`src/scanner.rs`](src/scanner.rs): depth-limited traversal without symlink following, common audio-extension filtering, size/nanosecond-mtime fingerprints checked before and after probing, bounded batches/channels, and cancellation-aware shutdown.
- [`src/library/`](src/library/): dedicated coordinator and SQLite connection, persisted worklists, incremental updates, grouping, stable asset/track/artist/album IDs, and revision-consistent pages. [`src/library_types.rs`](src/library_types.rs) contains the plain serializable models.
- Discovery has an unknown total; indexing exposes processed/total. State is published nominally at 4 Hz and on transitions. One scan runs at a time. Adding a root starts its first scan; later rescans are explicit.
- Only a completed enumeration/indexing run advances the root's reconciliation watermark. Missing files then disappear from available queries without deleting their IDs. Missing roots, unreadable directories, cancellation, and crashes never advance that watermark. Reappearing paths recover their IDs.
- A corrupt individual file does not make traversal incomplete: the scan may finish `complete` with failures. Previously good metadata and its successful fingerprint survive probe failure; the error remains visible and the next scan retries. New unreadable/corrupt files do not become playable indexed tracks.
- Release IDs group albums across disc directories. Without one, album artist, album title, and source directory distinguish releases. Compilations retain track artists and use `Various Artists` when album artist is absent. Album tracks sort by disc/track number. Display text stays intact; filtering uses Unicode lowercase and literal substring matching, not SQL wildcard syntax, normalization, or full case folding. Track search includes the source path.
- SQLite schema **2** migrates schema-1 queues in place, preserving occurrence IDs, next ID, settings, revision, and checkpoint. Indexed queue entries retain nullable track provenance. Track enqueue validates both queue and library revisions again inside an immediate write transaction; a failed durable edit does not append to playback.
- Restart browses the persisted index without scanning. An unfinished scan becomes `interrupted`; rescan is manual. Paths, not inode/content hashes, define asset identity; moves do not automatically preserve identity.

The request queue holds at most 32 operations; discovery batches hold at most 128 paths, probes at most 64, with one probe batch in flight. Directory depth is capped at 64. FFmpeg probing uses a 2 MiB probe size, 256-packet limit, 2-second analysis window, and 10-second interrupt deadline. Tags are limited to 1,024 entries/256 KiB total/64 KiB per value. These are controlled application/probing bounds, not a hard FFmpeg allocator sandbox; blocked OS I/O cannot be forcibly interrupted. Size/mtime fingerprints cannot detect replacements preserving both values.

### Try the library

Start the server using the instructions below, then:

```sh
target/debug/tyranny-likes-smoke-ctl '{"id":10,"command":"add_root","path":"/absolute/path/to/Music"}'
target/debug/tyranny-likes-smoke-ctl --watch '{"id":11,"command":"snapshot"}'
target/debug/tyranny-likes-smoke-ctl '{"id":12,"command":"library_artists","revision":null,"offset":0,"limit":100,"query":""}'
target/debug/tyranny-likes-smoke-ctl '{"id":13,"command":"library_albums","revision":null,"offset":0,"limit":100,"query":"","artist_id":null}'
target/debug/tyranny-likes-smoke-ctl '{"id":14,"command":"library_tracks","revision":null,"offset":0,"limit":100,"query":"","album_id":null}'
```

Protocol **6** retains the library contract introduced in v2: snapshots include `library` revision/counts/scan status, and queue pages include nullable `track_id`. It requires authoritative Now Playing metadata/capabilities, an occurrence ID on every seek, and `playlists_revision`. Upgrade clients and server together; use returned IDs and revisions rather than assuming fresh-database values.

| Commands | Fields and semantics |
| --- | --- |
| `add_root` | Absolute UTF-8 directory `path`; canonicalizes and rejects duplicate/overlapping roots. Registers the root and starts scanning. |
| `rescan` | `root_id`; rejects another scan while one is active. |
| `library_roots` | `revision` (null for first page), `offset`, `limit` (1–500), literal `query` (at most 1,024 UTF-8 bytes). |
| `library_artists` | Same paging fields; structured track `query`. Returns album-artist groups containing matching available tracks. |
| `library_albums` | Same paging fields and structured `query`, plus optional `artist_id`; group counts reflect matching tracks. |
| `library_tracks` | Same paging fields and structured `query`, plus optional `album_id`; returns stable track/asset IDs, source, metadata, and last probe error. |
| `append_track` | `expected_revision` for the queue, `library_revision`, and `track_id`; returns a distinct queue-occurrence ID. |
| `insert_tracks` | Queue `expected_revision`, `library_revision`, 1–500 ordered `track_ids`, and `before` queue-occurrence ID or null for the end. Atomic insertion returns the first new occurrence ID and does not change play intent. |

Page revision/count/rows share one read transaction. Subsequent pages must use the returned revision; `stale_library_revision` means restart the query, not merge incompatible pages. Scanning may invalidate pages. Existing queue occurrences remain independent of later library availability.

### Reproduce the library gate

```sh
python3 scripts/library_gate.py
python3 scripts/library_gate.py --coreaudio
```

[`scripts/library_gate.py`](scripts/library_gate.py) exercises actual processes, native imports, v1 migration, Unicode and metadata, compilations/multi-disc/distinct releases, stale and bounded pages, overlap rejection, track enqueue, unchanged scans, add/retag/remove/reappearance, permission failures, corrupt-file recovery, restart without scan, and SIGKILL during indexing. It also plays an indexed track through the selected output. FFmpeg CLI is used only to generate tagged fixtures. [`spike-results/library-gate.json`](spike-results/library-gate.json) retains reports, regression checks, and source/binary hashes; generated media and private databases remain under ignored `target/`.

The native folder picker, managed process, and scale fixture are exercised in the newer sections above. Watchers, root removal, CUE expansion, automatic move detection, and relocatable native-library packaging remain unimplemented; the metadata-only scale fixture is not a 50,000-file import test.

## Server authority: implemented and exercised

The Rust server owns playback independently of its controllers, with a durable SQLite queue. **The original WebSocket-era real-process server runs passed 12/12 acceptance cases:** one with clock-paced PCM capture and one with actual CoreAudio output. The numerical/device audio gate passed **22/22** after the playback extensions. At completion of that server-only slice, formatting, Clippy with warnings denied, and **21 Rust tests** passed. These are historical results, not Unix-transport performance or device certification.

Implemented files:

- [`src/owner.rs`](src/owner.rs): dedicated playback/SQLite thread, bounded command ingress, atomic snapshot/subscription, ordered state publication, checkpoints, and restart policy.
- [`src/store.rs`](src/store.rs): versioned SQLite schema, WAL with full synchronous commits, stable occurrence IDs/order/revisions, settings, and selected-position checkpoints. Failed transactions leave the engine unchanged.
- [`src/protocol.rs`](src/protocol.rs): version-9 JSON commands, replies, errors, state, and bounded revision-bound pages.
- [`src/server.rs`](src/server.rs) and [`src/transport.rs`](src/transport.rs): Tokio Unix transport; private state/socket files, exclusive instance lock, same-UID peers, bounded cancellation-safe framing, finite connection/send limits, and signal-driven shutdown.
- [`src/bin/tyranny-likes-smoke-server.rs`](src/bin/tyranny-likes-smoke-server.rs) and [`src/bin/tyranny-likes-smoke-ctl.rs`](src/bin/tyranny-likes-smoke-ctl.rs): actual server and development controller executables.
- [`scripts/server_gate.py`](scripts/server_gate.py): stdlib-only process/Unix-socket harness. Uses the real playback owner and SQLite database, not mocks.

### Run the server

Use the FFmpeg build prerequisites below, then:

```sh
cargo build --locked --bins
target/debug/tyranny-likes-smoke-server
```

On macOS, state defaults to `$HOME/Library/Application Support/tyranny`, with real CoreAudio output. On Linux, state defaults to `$XDG_STATE_HOME/tyranny` when that variable is absolute, otherwise `$HOME/.local/state/tyranny`. The state directory is `0700`; `control.sock`, `server.lock`, and `state.sqlite3` are `0600`. A second server cannot own that directory. Ready JSON reports `socket_path` and a fresh `instance_id`; there is no address or token. The socket path must fit the platform byte limit (under 104 bytes on macOS, under 108 on Linux).

In another terminal:

```sh
target/debug/tyranny-likes-smoke-ctl '{"id":1,"command":"snapshot"}'

# For a fresh database only: otherwise use the revision and ID returned by the server.
target/debug/tyranny-likes-smoke-ctl '{"id":2,"command":"append","expected_revision":1,"path":"/absolute/path/to/music.flac"}'
target/debug/tyranny-likes-smoke-ctl --watch '{"id":3,"command":"play","entry_id":1}'
```

Quitting `tyranny-likes-smoke-ctl` does not stop playback. SIGINT/SIGTERM to the server checkpoints and shuts it down, including connected controllers, then removes its socket. `--state-dir PATH` selects another private directory; match it with the controller's `--socket PATH/control.sock`. A crash can leave a stale socket: restart removes it only while holding the directory lock and after checking its ownership, type, permissions, lack of a live listener, and unchanged inode. There is no TCP listener or WebSocket fallback.

For silent verification, server `--capture /absolute/output.f32` selects the same clock-paced PCM consumer instead of CoreAudio. Existing capture files are overwritten, but server state files and queued sources—including hard-link aliases—are rejected as capture destinations. Capture output cannot subsequently be enqueued as an input.

### Implemented command contract

Every request has a numeric `id` and a snake_case `command`; the reply echoes that ID. The authoritative type definitions are in `src/protocol.rs`.

| Commands | Fields and semantics |
| --- | --- |
| `snapshot` | Current control state; does not copy the queue. |
| `queue_page` | `revision` (null for first read), `offset`, `limit` (1–500). Subsequent pages must use the returned revision. |
| `append` | `expected_revision`, absolute UTF-8 local-file `path`; returns a new occurrence ID, even for a duplicate source. |
| `remove`, `clear` | Require `expected_revision`. Remove addresses one `entry_id`; removing current stops playback. |
| `move` | `expected_revision`, 1–500 distinct `entry_ids`, and `before` occurrence ID or null for the end. One atomic source-order-preserving move; current playback is retained and future preparation rebuilt. Validated no-ops keep the revision unchanged. |
| `play` | Optional `entry_id`: select it from frame zero. Without an ID, resume current; after stop/end, restart at the queue's beginning. |
| `pause`, `stop` | Pause retains selection/metadata; stop clears both. |
| `toggle_play_pause` | Atomically pause Playing/Buffering, otherwise play; no client-side transport prediction. |
| `next`, `previous` | Owner resolves current queue order. Next never wraps and rejects the last/no-current boundary. Previous restarts at or after 3 seconds, otherwise selects the predecessor; first entry restarts. Preserve active playback or pause intent; ended/error selection remains paused. |
| `seek` | Required `entry_id` and `position_frames` in the reported 48 kHz application PCM timeline. Reject a stale occurrence or position at/past known duration. Selection is prepared and checkpointed before audible commit. |
| `set_gain` | `mode`: `off`, `track`, or `album`. Reprepares the selected source at its observed position; not a click-free live-gain transition claim. |
| `set_resume` | `enabled`: persist explicit auto-resume preference. Default false. |

Snapshots include `can_next`, `can_previous`, and nullable `now_playing` (`entry_id`, title, artist, album, `duration_frames`, nonfatal `metadata_error`). Metadata belongs to the consumed/selected occurrence, never the prefetched source or a browsed page. Unknown duration stays unknown. Stop clears metadata; pause, end, and reconnect retain it.

`GET /ws` requires `Authorization: Bearer TOKEN`; all browser `Origin` headers are rejected. Hello includes protocol version, server-instance ID, and a fresh control snapshot. Snapshot plus subscription are captured in one owner operation; subsequent state sequence numbers never go backward, including after newer command replies. Reconnect discards cached control state and stale queue pages. There is no durable event replay, request deduplication, or automatic command retry after a missing reply.

Queue edits commit before engine application and acknowledgment. A failed persistence operation is visible; an uncertain/lost reply must be resolved by querying current state, not blindly repeating an append. State distinguishes stopped, paused, buffering, playing, ended, and error; acknowledgment alone does not imply that samples have reached the output.

Restart preserves queue IDs/order/revision, the monotonic next ID, selected occurrence/position, and gain/resume settings. It restores paused by default. With auto-resume enabled, only previously playing/buffering state resumes; an explicit pause/stop remains paused/stopped. Checkpoints run nominally every second and on control mutations/clean shutdown. Crash position loss includes owner/I/O scheduling and in-flight output; this is not a hard real-time one-second guarantee.

Resource bounds: 32 connections including handshakes, 16 KiB requests, 8 MiB reply ceiling, 500-entry pages, 64 queued owner operations, and 32 state broadcasts. Handshake/send deadlines are five seconds; unread or lagged controllers are disconnected. Decoding and queue SQLite stay on the playback owner; library SQL/probing use separate workers, never network tasks or the PCM callback. Playback diagnostic history is drained instead of growing for the daemon's lifetime.

### Server verification

```sh
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
python3 scripts/server_gate.py
python3 scripts/server_gate.py --coreaudio
```

The harness checks two controllers, duplicate identity, stale mutations/pages, shared queue ordering, reconnect during playback, pause/seek/gain, the real CLI, and continued playback with no controllers. It injects an SQLite abort to prove a failed durable edit does not mutate the engine. An unread TCP client receives queued large-page requests until its send window stalls; it is disconnected while another controller and playback remain responsive.

It also kills and restarts the actual server process: 200 entries, revision 202, next ID 201, settings, and the selected checkpoint survive. Paused-default, explicit auto-resume, explicit pause despite auto-resume, and non-recycled IDs after clear all pass. Both output modes report zero PCM-consumer underruns. In the CoreAudio slow-client run, the other client's observed snapshot latency was 0.80 ms; this is one observation, not a p95 or scale certification.

Integration found and fixed capture destinations overlapping protected state or queued media. A failing-before/passing-after regression covers credentials, database, source, and hard-link alias. Other regressions cover authentication, unsafe paths, ownership lock, ordering, prepared selection, active-entry reorder, replay, and mutation overflow.

[`spike-results/server-gate.json`](spike-results/server-gate.json) retains the final capture/device reports, refreshed audio-gate report, verification output, and source/binary hashes. Per-run media, logs, and private test databases stay under ignored `target/server-gate/`.

This earlier gate proves the manually launched server's independence from controller lifetime. The newer lifecycle gate above covers LaunchAgent installation and app bootstrap separately. Volume and replace-and-play remain subsequent work; Next/Previous are implemented in protocol v3.

## Native audio-engine baseline

**Decision: retain the native FFmpeg → bounded PCM → continuous CoreAudio engine.** It passes the unchanged numerical thresholds that rejected the original GStreamer pipeline. The full fast-capture/device run passes **22/22 cases**, including prepared-next queue mutations and pause/seek/resume, with zero reported PCM-consumer underruns.

The separate clock-paced capture run also passes **14/14 cases**, with zero reported PCM-consumer underruns. It exercises the same numerical, transport, error, and queue-mutation paths without using a fast file sink as a substitute for real-time consumption.

This is a working audio-engine, persistent-server, durable-library, native-controller, and managed-process spike, not a completed music-player application. The process and scale experiments in [SPIKE.md](SPIKE.md) now have measured evidence; human listening remains unconfirmed. No requirements were removed.

## Implemented

- [`src/decoder.rs`](src/decoder.rs): native libavformat/libavcodec/libswresample through `ffmpeg-next`; fresh demuxer, decoder, and resampler per track. Honors decoder skip/padding and frame-duration trimming, supports forward/backward seek with preroll, and converts to 48 kHz stereo F32. Mono is duplicated at unity gain. Track/album gain, album fallback, peak protection, and untagged unity fallback are resolved independently for each source.
- [`src/output.rs`](src/output.rs): one continuous output and a capture sink sharing the same PCM consumer. [`src/output/coreaudio.rs`](src/output/coreaudio.rs) supplies stereo F32/48 kHz client PCM to a native DefaultOutput AudioUnit, leaving hardware format conversion to CoreAudio. A fixed pool of 64 × 1,024 stereo frames holds **512 KiB of PCM**, or 1.365 seconds at 48 kHz. Ready/recycle/event rings are bounded. Invalidated blocks and their tokens are reclaimed on the producer side. Device completion waits for the predicted scheduled tail, not just an empty software queue.
- [`src/playback.rs`](src/playback.rs): a single playback owner with stable queue-occurrence IDs, revisions, stale-edit rejection, append/remove/reorder, stop, pause/resume, seek, and prepared-next invalidation. Removing the active entry stops playback. Future edits preserve current-track PCM. Drive `pump` on a worker: native decode and disk access may block there, never in the audio callback.
- [`src/bin/audio-probe.rs`](src/bin/audio-probe.rs): runnable headless playback and capture, checked transport and queue exercises, and structured results. The obsolete hello-world entrypoint and GStreamer playback dependencies are removed.
- [`scripts/audio_gate.py`](scripts/audio_gate.py): the original numerical acceptance suite, extended with real playback-owner queue mutations and zero-reported-underrun checks. Thresholds were not relaxed.
- [`tests/playback.rs`](tests/playback.rs): actual decoder/output integration checks for duplicate queue identity, stale revisions, precise WAV seeking, active-entry removal, and replacement prebuffering after seek. Four decoder unit tests cover gain and trimming/seek calculations.

The PCM callback performs no filesystem access, decoding, logging, blocking locks, or dynamic allocation/free in this implementation. Native render-error notifications latch atomic fault state; native property queries and error formatting run on the owner thread. Callback storage is released only after AudioUnit disposal. This is not a blanket hard-real-time certification of Apple's internal converter. Native decoder allocations are outside the output callback. The 512 KiB bound covers the PCM pool, not total process memory.

## Reproduce

The original audio matrix was measured on arm64 macOS with Rust 1.99.1, FFmpeg 9.0.2, CPAL 0.18.2, ffmpeg-next 9.0.0, Python 3.14.7, GStreamer 1.28.8, and LAME 4.0. CPAL has since been replaced by direct native CoreAudio bindings (`objc2` audio crates 0.3.2); the default-output compatibility evidence above distinguishes that fix from the earlier matrix. Building requires Xcode command-line tools/Clang, `pkg-config`, and the FFmpeg development libraries.

```sh
brew install pkg-config ffmpeg
cargo build --locked --bin audio-probe
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings

# Fixture encoders only; neither is a playback dependency.
brew install gstreamer lame

# Emits modest-amplitude audio through the default CoreAudio device.
python3 scripts/audio_gate.py --probe target/debug/audio-probe --coreaudio

# Separate real-time-paced captures; no speaker output by default.
python3 scripts/audio_gate.py --probe target/debug/audio-probe --clocked
```

Each gate invocation creates an isolated `target/audio-gate/run-*` directory with sources, encoded fixtures, captured PCM, commands/stdout/stderr, and `report.json`. FFmpeg and LAME generate fixtures; GStreamer's libvorbis encoder is used solely to preserve the original Vorbis baseline. **The player does not spawn any decoder or depend on GStreamer.**

Play actual files using absolute paths:

```sh
cargo run --locked --bin audio-probe -- \
  --output coreaudio --gain album \
  /absolute/path/to/first.flac /absolute/path/to/second.flac
```

`--gain` accepts `off`, `track`, or `album`. A path in `--output` writes stereo F32LE at 48 kHz; `--clocked` paces capture. `--exercise-transport` needs a seekable fixture at least six seconds long. `--exercise-queue remove-next` or `move-next` takes three files and mutates the second entry only after it is prepared while the first plays; use roughly one-second first/second fixtures so preparation fits in the bounded pool. The CLI has a 60-second deadline: it is an experiment, not an arbitrary-length player interface.

## Measurements

Same-rate fixtures retain the original 48,137 / 52,979 / 33,611 frame split: **134,727 frames total**. No sample offsets are fitted and no captured samples are trimmed to turn a failure into a pass. Lossy quality is compared separately from source-length and independently decoded handoff correctness.

| Case | Native engine result |
| --- | --- |
| WAV, FLAC | Exact source samples and length across both transitions. |
| FFmpeg/libmp3lame and direct-LAME MP3 | Correct source lengths; zero sample difference from independent per-track decodes. |
| Vorbis, Opus, AAC-in-MP4 | Correct source lengths; zero sample difference from independent per-track decodes. Lossy source-error checks also pass. |
| ReplayGain | Off/track/album modes, album fallback, peak protection, and tagged → untagged unity reset pass measured PCM checks. |
| Mixed rates/channels | 44.1/48 kHz mono/stereo sequence: 156,187 frames, exact length and samples against the independently resampled reference. Separate from the same-rate result. |
| 137-frame next track | Exact samples and length. |
| Corrupt next track | Visible bounded error; valid preceding audio drains, without silent skip or loop. |
| Remove prepared next | Occurrences `[1, 3]`; captured PCM exactly matches the corresponding source sequence. |
| Move prepared next | Occurrences `[1, 3, 2]`; captured PCM exactly matches that order. |
| Pause/seek/resume | Stable paused position, seek to frame 48,000, and subsequent advancement in clocked capture and actual CoreAudio. |
| CoreAudio formats | All seven format/encoder variants complete with expected occurrence order, no reported backend errors, and no reported PCM underruns. |

Additional smoke checks:

- Prepared-next removal and reordering also ran against actual CoreAudio, producing `[1, 3]` and `[1, 3, 2]`, respectively, with zero reported underruns.
- A temporary direct-decoder smoke compared complete post-seek output against full-decode suffixes at frames 24,017, 1,337, and 47,000 for all seven variants plus 44.1 kHz mono FLAC. All 24 checks preserved the exact remaining length. Samples matched exactly except AAC at frame 1,337, whose maximum absolute difference was 0.000021033, below the smoke's 0.0001 bound. This checks PCM, not merely a reported seek position; the temporary executable source was removed after measurement.
- A FLAC capture launched from `/tmp` with only `HOME` and `/usr/bin:/bin` in `PATH` completed successfully. `otool -L` shows direct Homebrew FFmpeg dylib dependencies and system CoreAudio frameworks, not GStreamer. This proves independence from the build shell, **not** relocatable packaging.
- `cargo test --all-targets`: eight tests passed. Formatting and Clippy with warnings denied passed.

### Seek/prebuffer race found during integration

The first native-device run passed all numerical cases but recorded one underrun during CoreAudio pause/seek/resume. Invalidated pre-seek blocks were still included in the output's physical occupancy count until the consumer reclaimed them; the owner could therefore resume before any replacement PCM was ready.

The owner now tracks valid pre-start submissions separately, including per-entry adjustments when preparation is invalidated. Physical occupancy remains an honest pool statistic, not a readiness signal. The regression reproduced the premature resume before the fix; afterward it passed 12 consecutive runs, and the complete CoreAudio gate passed with zero reported underruns. No threshold or underrun counter was suppressed.

## Retained evidence and historical rejection

[`spike-results/native-audio-gate.json`](spike-results/native-audio-gate.json) retains the replacement's reports, source hashes, additional device/launch checks, seek measurements, and the initial native-run failure. Generated media and full per-run working files remain under ignored `target/native-audio-gate/` directories.

[`spike-results/audio-gate.json`](spike-results/audio-gate.json) remains the **unchanged historical GStreamer rejection**, not a report of the current executable. That pipeline passed 15/20 fast/device cases and 7/12 clocked cases. It added 4,665 frames for FFmpeg-written MP3, retained ReplayGain on an untagged transition, changed Opus samples when reusing the pipeline, and failed strict Vorbis/mixed-rate checks. The native engine passes those same checks without wrapping MPD, launching FFmpeg as a player, or retaining a second playback backend.

## Remaining limits and next decision

- **The user reports real-world playback testing is going well.** This is positive listening feedback, not a completed format/device/long-duration listening matrix. Historical reports intentionally retain `full_spike_audio_gate_passed: false`; there is no acoustic loopback, hardware-gaplessness, bit-perfect output, long-duration stress, or hotplug certification. Zero reported consumer underruns is a measured result, not proof that the OS/device never glitched.
- Application PCM and positions remain stereo F32 at 48 kHz. CoreAudio converts to the default device's native format without changing its nominal rate; a 44.1 kHz headphone route is exercised separately. Position reflects samples consumed by the application output callback, not a measured DAC playhead. Pause/seek cannot retract samples already handed to the device.
- The tested formats are a capability matrix, not a claim of support for every FFmpeg container/codec, module format, network source, or unusual channel layout. Custom/ambisonic layouts and intra-source format changes fail explicitly.
- Homebrew-linked server binaries still require a native-library distribution/licensing review, signing, and clean-machine testing. The controller has an ad-hoc-signed development `.app`; it is not a relocatable distribution of the server and decoder libraries.
- Queue state, settings, checkpoints, roots, library metadata and saved playlists are durable. Local protocol/reconnect, library import, SwiftUI, and the independent managed-process lifecycle are implemented as described above.

Continue real-collection listening and longer-duration operation alongside playback controls, regular playlists and smart rules. Treat dynamic queue autofill and relocatable distribution as separate gates; do not mistake the development LaunchAgent for a finished installer.

## Library redesign — server

The server half of the library-window redesign is implemented on `feature/library-redesign`: **protocol v9, SQLite schema 5**. The existing SwiftUI client was only moved to v9 on the wire so its gates still run. **The new client UI is not built here**; it is a separate plan.

### Added

- **List descriptions and queries.** Tracks, albums, artists, genres, playlists, smart playlists and Recent are described by one list description with source, filter, sort and grouping. Pages, totals, album/artist/genre browsers and the artist→album grouping come from one SQL compiler. Windows are revision-bound (`stale_library_revision`).
- **Content revision.** The library revision changes only when rows visible to list queries change. Scan progress, and cover-file-only rescans, do not move it, so clients may cache windows by revision.
- **Run enqueue.** An enqueue carries a selection (ids, or a list description plus optional first row) and a placement. The selection is resolved and paths are canonicalized **off the playback owner**; the owner only rechecks revisions and commits.
- **Queue sources, played flag, trim.** Each queue entry records its source and run, and a `played` flag. After 100 played entries the played prefix is trimmed (one queue-revision bump per trim; skipped inside a Repeat context).
- **Shuffle, repeat, volume.** Uniform shuffle per run, Repeat Off/All/One with Previous that walks history, and a ramped volume setting. All persist across restart.
- **History and counts.** A play is counted once per pass at the start-time stamp and threshold. Failed history writes are kept pending, retried, and never pause playback (bounded to 1000, oldest dropped and logged).
- **Filter fields.** `year` (single year or ascending `a..b` range) joins title, artist, album, albumartist and genre. Genre extraction is capped at 32 genres per track.
- **Metadata.** `now_playing` gains `track_id`, `album_id`, `format` and `gain_db` (present when a gain mode is active).

### Verification

| Gate | Result |
| --- | --- |
| `cargo fmt --check` | pass |
| `cargo clippy --locked --all-targets -- -D warnings` | pass |
| `cargo test --locked --all-targets` | **210 passed**, 1 ignored (the pre-existing CoreAudio hardware test): lib 155, `playback` 15 (+1 ignored), `server_transport` 40 |
| `server_gate.py` (capture) | pass, 12 cases |
| `library_gate.py` | pass, 13 cases |
| `playlist_gate.py` | pass |
| `smart_playlist_gate.py` | pass |
| `filter_gate.py` | pass |
| `artwork_gate.py` | pass, including the live Cover Art Archive fetch |
| `client_gate.py --model-only` (builds `scripts/native_model_gate.swift`) | pass; headless real server and `PlayerModel`, no UI permissions |
| Scale gate | recorded in `spike-results/scale-gate.json` (see below) |
| `lifecycle_gate.py`, `--app` modes, `client_gate.py --coreaudio`, `server_gate.py --coreaudio` | **not run**: need an unlocked GUI session with Accessibility/Screen Recording, or the real audio device |

Gate scripts were changed only for documented v9 drift:

- `library_gate.py`: expected `user_version` 4 → 5 (schema 5).
- `filter_gate.py`: `year:1959` is now a valid filter; the unknown-field probe uses `rating:5`.
- `artwork_gate.py`: a cover-file-only rescan no longer moves the content revision; the helper waits for scan `complete` plus a 2 s settle instead of a revision bump when the track count is unchanged. The refresh assertions are unchanged.
- `native_model_gate.swift`: `now_playing.gain_db` is `nil` until a gain mode is set, so the post-gain metadata checks compare every other field and ignore `gain_db`. This case had not been run since Task 16.

Product code was not changed.

### Scale (50,000 tracks, release server, 30 samples per list family)

| Family | p50 ms | p95 ms | 100 ms target |
| --- | --- | --- | --- |
| `list_tracks_first_window` | 93.1 | 98.1 | met |
| `list_tracks_deep_window` | 71.0 | 74.4 | met |
| `list_albums_by_year` | 70.8 | 77.2 | met |
| `list_tracks_filtered_genre` | 17.3 | 18.7 | met |
| `list_tracks_filtered_genre_query` | 50.1 | 51.2 | met |
| `list_tracks_sorted_year_desc` | 99.0 | 105.6 | **unmet** |
| `list_tracks_grouped_artist_album` | 138.1 | 141.5 | **unmet** |

Unmet targets are accepted and recorded: **`sorted_year_desc` (about 105 ms) and `grouped_artist_album` (about 140 ms; 145.1 ms in an earlier run)**. Each new window still needs one 50k-row sort; no index serves a multi-table total order. Fixing it needs a stored or cached order, which is a plan change. A client window cache masks it. Tuning cut the first window from 162 to 98 ms p95 and grouped from 279 to 141 ms.

`enqueue_run_50k` (50,001-entry queue, three enqueues while playing): **996.6, 1094.0 and 1214.0 ms, with 0 underruns** across all three (underrun delta 0, playback advanced). Before the off-owner resolution (Task 17b) the same gate gave 983 / 1077 / 1073 ms and **2 underruns**. Total time is similar; the owner thread is now blocked only 53–116 ms.

### Limitations

- The native SwiftUI UI gates were **not run** (no GUI session). Only the model-level gate ran.
- The client UI is not rebuilt for the new library window; that is a separate plan. The existing client only speaks v9 and passes its model gate.
- A run enqueue is checked for a stale queue revision after off-thread resolution (about 0.9 s at 50k), and a trim bumps the queue revision. Clients must retry a run enqueue once on `stale_revision`.
- CoreAudio-output gate variants were not re-run for this slice.

## Library redesign — client

Verified on branch `feature/library-redesign` after the final-review fix wave (base f89a89e).

### Gates

| Gate | Result |
| --- | --- |
| `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` | pass |
| `cargo test --locked --all-targets` | pass: 166 unit, 15 (1 ignored), 42 integration; 0 failed |
| `server_gate.py`, `playlist_gate.py` (server only) | pass (server: 12 cases) |
| `library_gate.py` / `smart_playlist_gate.py` / `filter_gate.py` / `artwork_gate.py` (server only) | passed before the fix wave; not rerun (their paths did not change) |
| Swift debug and release builds, `-warnings-as-errors` | pass |
| `swift test` | pass: 181 tests in 32 suites |
| `client_gate.py --model-only` | pass (16 cases) |
| `client_gate.py --app` scenarios core, shell, songs, grouping, albums, artists, history, upnext, runs | not run (permissions): every scenario fails at "Native UI driver requires a PID/action and Accessibility permission for its launching terminal." |
| `filter_gate.py`, `artwork_gate.py`, `playlist_gate.py`, `smart_playlist_gate.py`, `scale_gate.py` with `--app` | same Accessibility error (server-side steps passed first) |
| `lifecycle_gate.py --app` | not run (permissions): native window capture missing (Screen Recording) |

### Scale

Native scale (first Songs window within 1 s of warm launch, native scroll, memory): **not run (permissions)**. `scale_gate.py --app` stopped at the Accessibility error. Server list-window and run-enqueue numbers are in the server section above.

### Self-rendered views

`--render-views <dir>` (DEBUG only, `NativeRenderViews.swift`, driven by `scripts/render_views.py`) renders each destination with `cacheDisplay`. No Screen Recording or Accessibility is needed. The images are self-rendered, not composited screenshots: Settings is rendered off-screen without the tab bar. While rendering, track tables skip column autosave, so the images show the default column widths. Fixture: the 1,129-track songs library, a regular playlist "Road Mix" (12 tracks), a queue of 11 entries with the second one current (paused), so Up Next shows all three sections.

Screenshots in `spike-results/library-redesign-ui/`: 01-songs-ungrouped, 02-songs-grouped-by-album, 03-songs-with-up-next-drawer, 03b-songs-column-browser, 04-albums-grid, 05-albums-grid-expanded, 06-artists, 07-genres, 08-recently-added, 09-history, 10-up-next, 11-playlist-regular, 12/13/14-settings (connection, library, playback).

Fixed in the final-review fix wave (seen in the re-render):

- Up Next (10) shows HISTORY, NOW PLAYING and UP NEXT beside the numbers; other columns have no indent.
- "Upcoming" is one shuffle-aware rule on the server and the client (`ControlState.upcoming_count`, `current_index`): sidebar 9, header "9 upcoming", and the drawer (03) lists the 9 entries after the current one.
- The toolbar filter field shows at 1180 pt (transport display ideal width 380 pt).
- Songs at 1180 pt shows every default column through Plays, with no horizontal scroller (01, 03b, 11).
- The grouped lists' album gutter has the header "Album" (02, 08).
- No NSCollectionViewFlowLayout warning on Albums (04, 05).
- Loading and missing art use the same note placeholder (04: Unknown Album).

Still visible (none hand-walked; the native workflow in spec section 8 was not run):

- Grouped lists (02, 08) add the 120 pt album gutter, so Format and Plays sit behind the horizontal scroller at 1180 pt.
- The status column left of "#" has an empty header (tooltip "Playing").
- Artists and Genres have no view header (title, count, group) and no Compilations entry (the fixture has no compilation album, so that may be correct).
- The format text has no RG value (the fixture has no ReplayGain tags).
- Settings 12 (connection) renders nearly blank and 13 (library) half light, half dark: the off-screen window does not follow the dark appearance. A self-render limitation, unchanged by this wave.
