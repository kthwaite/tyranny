# Library Redesign — Client Implementation Plan (2 of 2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the spike's toolbar-toggle window with the approved shell. That means a source list, toolbar transport, status bar, and continuous virtualized lists. The views are Songs (with column browser and grouping), the album grid, Artists, Genres, Recently Added, Up Next (view and drawer), History and playlists. iTunes-style double-click runs complete it.

**Architecture:** Pure, testable logic moves into a new `TyrannyCore` Swift library target with a test target. That covers wire models, the list window cache, group layout, and selection encoding. The app target keeps AppKit tables (`NSTableView`, `NSCollectionView`), which are needed for native drag and drop, wrapped for SwiftUI. Views read rows through `ListWindowCache`, which fetches bounded windows from the v9 server and never holds a whole list.

**Tech Stack:** Swift 6.3, SwiftUI + AppKit, macOS 14, Swift Testing (`swift test`).

**Spec:** `docs/superpowers/specs/2026-10-18-library-ui-design.md`. Mockups: https://claude.ai/artifact/EyUunVuJrXVq31C1JkWZ29.

**Depends on:** `docs/superpowers/plans/2026-10-18-library-server.md`, which must be complete. The wire types below mirror server Tasks 6–15.

## Global Constraints

- macOS 14 deployment target. Build with `-Xswiftc -warnings-as-errors` in debug and release.
- Gates per task:
  - `swift build --package-path macos --scratch-path target/swift-client -Xswiftc -warnings-as-errors`
  - `swift test --package-path macos --scratch-path target/swift-client`
- Gate per UI task: the named native smoke run through `scripts/native_ui_driver.swift`. It needs the Accessibility and Screen Recording permissions that the spike gates already use.
- Every interactive control gets a stable `accessibilityIdentifier`. Identifiers are kebab-case and listed in each task.
- List window size is 200 rows. At most 10 windows are cached per list. A revision change refetches the visible window at most once per 500 ms.
- Server page limit 500 and selection `Ids` limit 500 (server constants).
- Client-local preferences in `UserDefaults`:
  - `upNextOnPlay` (`ask` | `replace` | `keep`, default `ask`)
  - `group.<destination>` (per destination, default per spec)
  - `columnBrowserVisible`, `upNextDrawerVisible`, `hiddenColumns`, `albumGridCoverSize`
- Copy strings are fixed by the spec, for example "Clear Up Next?", "Don't ask again", "Choose Music Folder", "Drop tracks here", "Added From".
- Selection and the playing indicator use the system accent colour (`Color.accentColor` / `NSColor.controlAccentColor`). The UI follows system light/dark mode, with native controls and the system font.
- Same branch as the server plan (`feature/library-redesign`). Commit messages carry no attribution lines.

## Carried From Server Execution

The server plan was executed first. See `docs/superpowers/plans/2026-10-18-library-server-execution.md` for the rulings. These wire behaviours bind the client. Each one names the task that implements it.

- **Explicit `first_row_id` (Task 1, Task 4).** `TrackSelection.list.first_row_id` must be encoded as an explicit key, `null` when absent. Default `Codable` omits nil keys, so it needs custom encoding. Test it in `trackSelectionListEncodesHalfOpenRanges`.
- **Start row (Task 4, Task 13).** `first_row_id: null` with shuffle on means "play this source": the server starts at a shuffle pick within the new run. The album Shuffle button sends this. Every activation of a clicked row, and Play on a selection, sets `first_row_id` to the first selected row, so playback starts there.
- **Stale queue revision (Task 11, Task 13).** The played-prefix trim bumps the queue revision about once per track once 100 entries have played. Queue edits and run enqueues that get `stale_revision` refetch the snapshot and retry once.
- **Stale library revision (Task 13).** The library revision changes on every indexing batch, including the automatic upgrade rescan. A run enqueue or playlist add that gets `stale_library_revision` must do three things: refetch the clicked row's window, relocate the row by `row_id`, and re-encode. Then retry once. If the row is gone, show "The list changed; try again" in the status bar.
- **Refetch on `current_id` change (Task 11).** Played marks and Repeat All wrap clears do not bump the queue revision. When `current_id` changes, refetch the visible Up Next and drawer windows so HISTORY and NOW PLAYING stay correct.
- **Refetch on `history_revision` change (Task 2, Task 6, Task 10).** Play counts change the Plays and Last Played columns and sorts without a library revision change. When `history_revision` changes, refetch the visible windows and group spans of track lists. Task 2's `serverRevisionChanged` throttle applies.
- **Refresh queue pages on library revision change (Task 11).** Queue display fields come from the library.
- **Artwork invalidation (Task 5, Task 8).** Rescans that change only a cover file do not bump the library revision. Invalidate visible album artwork when a scan reaches `complete`, or by the `Artwork.cache_key` change, not only on revision change.
- **Repeat All Previous (Task 5).** Under Repeat All, `can_previous` is true at a run's first entry, because Previous wraps within the context. Do not disable the Previous button there.
- **Repeat One Next (Task 5).** Under Repeat One, Next moves to the following entry. Only the natural end of the entry replays it.
- **Play All for playlists (Task 12).** Enqueue or Play All may keep using `enqueue_playlist`, which now resolves off the playback owner.
- **Up Next sections (Task 11).** Build the sections from `QueueItem.played` plus `current_id`, not from queue position. They must stay correct under shuffle.

## Review Focus

1. **Scrolling Songs during a scan.** Revisions change every indexing batch. Expected: no row jumps, the selection is kept by ID, placeholders show for at most a refetch, and there is no request storm. The test, `revision_storm_refetches_visible_window_at_most_twice_per_second`, goes in Task 2.
2. **⌘A on 50,000 rows, then drag to a playlist.** Expected: a `List` selection, not 50,000 IDs, and one atomic add. The test, `select_all_encodes_one_full_range`, goes in Task 4.
3. **A fragmented selection, such as every other row of 1,000.** Expected: a selection beyond 64 ranges uses the bounding range plus exclusions when the gaps are loaded. Otherwise it is refused with "Selection is too scattered; select fewer separate ranges." The test goes in Task 4.
4. **A grouped list whose groups change mid-scroll.** For example, a scan adds an album. Expected: the layout rebuilds from fresh spans, and the anchor row stays in view. The test, `layout_rebuild_keeps_anchor_track_position`, goes in Task 3.
5. **Disconnect while a window fetch is in flight.** Expected: the fetch is dropped by session generation, the lists show the disconnected state in the status bar, and no stale rows appear after reconnect. The test, `session_change_discards_in_flight_windows`, goes in Task 2.

---

## File Structure

| File | Responsibility |
|---|---|
| `macos/Package.swift` (modify) | add `TyrannyCore` library and `TyrannyCoreTests` test target |
| `macos/Sources/TyrannyCore/WireModels.swift` (move + extend) | Codable wire types, public, including v9 list/selection types |
| `macos/Sources/TyrannyCore/JSONValue.swift` (create) | request field encoding with nested objects |
| `macos/Sources/TyrannyCore/ListWindowCache.swift` (create) | sparse windowed rows, revision handling, anchors |
| `macos/Sources/TyrannyCore/GroupLayout.swift` (create) | display rows from `GroupSpan`s |
| `macos/Sources/TyrannyCore/SelectionEncoder.swift` (create) | runs, selections, Up Next policy |
| `macos/Tests/TyrannyCoreTests/*.swift` (create) | tests for the above |
| `macos/Sources/Tyranny/Shell/*.swift` (create) | `MainWindowView`, `SourceList`, `TransportDisplay`, `StatusBar`, `SettingsView`, `Destination` |
| `macos/Sources/Tyranny/Lists/*.swift` (create) | `VirtualTrackTable`, `ColumnBrowser`, `AlbumBlockCell`, `TrackListView` |
| `macos/Sources/Tyranny/Albums/*.swift` (create) | `AlbumGridView`, `AlbumExpansionView` |
| `macos/Sources/Tyranny/Views/*.swift` (create) | `ArtistsView`, `GenresView`, `HistoryView`, `UpNextView`, `UpNextDrawer` |
| `macos/Sources/Tyranny/Models/*.swift` (create) | `TransportModel`, `ListModel`, `NavigationModel` split out of `PlayerModel` |
| `macos/Sources/Tyranny/PlayerModel.swift` (modify, shrink) | connection, session, `send`, shared state; legacy paging removed in Task 14 |
| `macos/Sources/Tyranny/ContentView.swift`, `PlaylistView.swift` (delete in Task 14) | spike shell |
| `macos/Sources/Tyranny/Native*.swift`, `scripts/*_gate.py` (modify) | smoke harnesses for the new shell |

---

### Task 1: `TyrannyCore` target and v9 wire models

**Files:**
- Modify: `macos/Package.swift`
- Move: `macos/Sources/Tyranny/WireModels.swift` → `macos/Sources/TyrannyCore/WireModels.swift` (types become `public`)
- Create: `macos/Sources/TyrannyCore/JSONValue.swift`, `macos/Tests/TyrannyCoreTests/WireModelTests.swift`
- Modify: every app file using the moved types (`import TyrannyCore`)

**Interfaces:**
- Package:
  - `.target(name: "TyrannyCore")`
  - `.executableTarget(name: "Tyranny", dependencies: ["TyrannyCore"])`
  - `.testTarget(name: "TyrannyCoreTests", dependencies: ["TyrannyCore"])`
- Produces public Codable types mirroring the server, with snake_case property names as in the existing models:
  - `ListSource` (enum with associated values, custom coding on `kind`), `Browser`, `SortField`, `SortKey`, `Grouping`, `ListDescription`
  - `ListRow`, `TrackSummary`, `ListTotals`, `AlbumSummary`, `NamedCount`, `AlbumSort`, `GroupSpan`
  - `TrackSelection` (`ids` / `list`), `Placement`, `QueueSource`, `SourceKind`, `HistoryEntry`, `RepeatMode`
- Produces `JSONValue` cases `.object([String: JSONValue])` and `.array([JSONValue])`, plus `static func encoding<T: Encodable>(_ value: T) throws -> JSONValue`.

- [ ] **Step 1: Write failing tests**:
  - `listDescriptionEncodesServerShape`: the JSON for `ListDescription(source: .playlist(id: 4, revision: 9), browser: .init(), query: "genre:Jazz", sort: [.init(field: .year, descending: true)], group: .artistAlbum)` equals the literal used in server test `list_description_wire_shape_round_trips`.
  - `listTracksResponseDecodes`, using a fixture JSON with `totals` present and then absent.
  - `trackSelectionListEncodesHalfOpenRanges`, where `UInt64.max` encodes as 18446744073709551615.
- [ ] **Step 2: Run** `swift test --package-path macos --scratch-path target/swift-client` — expect FAIL (no target).
- [ ] **Step 3: Implement** the package change, the move, and the types.
- [ ] **Step 4: Run** the tests, then both builds — expect PASS. The app behaves exactly as after server Task 16.
- [ ] **Step 5: Commit** `git commit -m "Add TyrannyCore with v9 wire models"`.

### Task 2: `ListWindowCache`

**Files:**
- Create: `macos/Sources/TyrannyCore/ListWindowCache.swift`, `macos/Tests/TyrannyCoreTests/ListWindowCacheTests.swift`

**Interfaces:**
```swift
public struct WindowPage<Row: Sendable>: Sendable { public let revision: UInt64; public let total: Int; public let offset: Int; public let rows: [Row] }
public enum WindowFetchError: Error { case stale, failed(String) }
@MainActor @Observable public final class ListWindowCache<Row: Sendable & Identifiable> where Row.ID == UInt64 {
    public typealias Fetch = @Sendable (_ offset: Int, _ limit: Int, _ revision: UInt64?) async throws -> WindowPage<Row>
    public init(windowSize: Int = 200, maxWindows: Int = 10, refreshInterval: Duration = .milliseconds(500), fetch: @escaping Fetch)
    public private(set) var total: Int            // 0 until the first window arrives
    public private(set) var revision: UInt64?
    public private(set) var generation: UInt64    // bumps when rows change; views observe this
    public func row(at position: Int) -> Row?     // nil = placeholder; schedules that window
    public func visible(_ range: Range<Int>)       // prefetch windows covering range ± 1 window; marks LRU
    public func position(of id: Row.ID) -> Int?    // only within loaded windows
    public func serverRevisionChanged(_ revision: UInt64, anchor: Row.ID?) // throttled refetch of visible windows
    public func reset(fetch: Fetch)                // new list description: drop everything
    public func discard(session: UInt64)           // drop in-flight replies from older sessions
}
```
- Behaviour:
  - One in-flight request per window.
  - Least-recently-visible windows are evicted beyond `maxWindows`, never the visible ones.
  - A `.stale` error drops all windows, refetches the visible windows without a revision, and adopts the new revision.
  - After a revision change, the cache restores `anchor` by `position(of:)` in the refetched windows and reports it through `public var anchorPosition: Int?`.

- [ ] **Step 1: Write failing tests** (fake `Fetch` backed by an array, with a request log):
  - `firstAccessLoadsOneWindowAndReportsTotal`
  - `visibleRangePrefetchesNeighbouringWindowsOnly`
  - `evictsLeastRecentlyVisibleBeyondTen`
  - `duplicateRequestsForAWindowCoalesce`
  - `staleErrorRestartsAtNewRevision`
  - `anchorPositionFollowsInsertionAboveIt`
  - `revision_storm_refetches_visible_window_at_most_twice_per_second` (20 changes in 1 s → at most 2 refetches)
  - `session_change_discards_in_flight_windows`
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Add windowed list cache"`.

### Task 3: `GroupLayout`

**Files:**
- Create: `macos/Sources/TyrannyCore/GroupLayout.swift`, `macos/Tests/TyrannyCoreTests/GroupLayoutTests.swift`

**Interfaces:**
```swift
public enum DisplayRow: Equatable, Sendable {
    case artistHeader(artistKey: UInt64, span: Range<Int>)       // span = list positions in this artist group
    case track(position: Int, blockIndex: Int?)                 // blockIndex = row index within its album block (0-based)
    case blockFiller(albumKey: UInt64, blockIndex: Int)          // pads a short album to the cover's height
    case blockGap
}
public struct GroupLayout: Sendable {
    public static let minimumBlockRows = 7    // 104 pt cover + facts at 20 pt rows
    public init(grouping: Grouping, total: Int, spans: [GroupSpan])
    public var count: Int { get }
    public func row(at displayIndex: Int) -> DisplayRow        // O(log n) via prefix sums
    public func displayIndex(ofPosition position: Int) -> Int
    public func albumKey(atDisplayIndex: Int) -> UInt64?
}
```
- Rules:
  - `.none` maps the identity onto `.track(position, nil)`.
  - `.artist` gives a header, then the tracks.
  - `.album` gives each span `max(count, 7)` rows: tracks, then fillers, then one `.blockGap`.
  - `.artistAlbum` gives an artist header before the first album of each run of equal `artist_key`, then album blocks.

- [ ] **Step 1: Write failing tests**: `noneIsIdentity`, `albumBlocksPadToSevenRowsAndEndWithGap`, `artistAlbumEmitsOneHeaderPerArtistRun`, `displayIndexRoundTripsForEveryPosition` (1,000 positions, random spans, fixed seed), `layout_rebuild_keeps_anchor_track_position`.
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Add grouped display layout"`.

### Task 4: `SelectionEncoder` and Up Next policy

**Files:**
- Create: `macos/Sources/TyrannyCore/SelectionEncoder.swift`, `macos/Tests/TyrannyCoreTests/SelectionEncoderTests.swift`

**Interfaces:**
```swift
public enum UpNextPolicy: String, Sendable { case ask, replace, keep }
public enum RunDecision: Equatable, Sendable { case enqueue(Placement), ask }
public enum SelectionEncoder {
    public static func run(list: ListDescription, revision: UInt64, from position: Int, rowID: UInt64) -> TrackSelection
    public static func selection(list: ListDescription, revision: UInt64, positions: IndexSet,
                                 rowID: (Int) -> UInt64?, trackID: (Int) -> UInt64?) throws -> TrackSelection
    public static func decide(policy: UpNextPolicy, hasUpcoming: Bool) -> RunDecision
}
public struct SelectionTooScattered: Error {}   // message: "Selection is too scattered; select fewer separate ranges."
```
- Rules:
  - `run` gives `.list` with `ranges [[position, UInt64.max]]` and `first_row_id = rowID`.
  - `selection` uses `.ids` when there are ≤ 500 positions, all loaded, and the source is not `.playlist` (playlist rows are occurrences). Otherwise `.list`:
    - with contiguous ranges when there are ≤ 64;
    - else with the bounding range and `excluded` gap row IDs, when every gap row is loaded and there are ≤ 500 of them;
    - else `throw SelectionTooScattered()`.
  - `decide`:
    - `hasUpcoming == false` → `.enqueue(.replaceUpcoming)`;
    - `replace` → `.enqueue(.replaceUpcoming)`;
    - `keep` → `.enqueue(.next)`;
    - `ask` → `.ask`.

- [ ] **Step 1: Write failing tests**: `runStartsAtClickedPositionToEnd`, `smallLoadedLibrarySelectionUsesIDs`, `playlistSelectionAlwaysUsesList`, `select_all_encodes_one_full_range`, `fragmentedSelectionUsesBoundingRangeWithExclusions`, `fragmentedSelectionWithUnloadedGapsIsRefused`, `decideCoversEveryPolicy`.
- [ ] **Step 2: Run** — expect FAIL.
- [ ] **Step 3: Implement**.
- [ ] **Step 4: Run** — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Encode runs and selections for the server"`.

### Task 5: Window shell, toolbar transport, status bar, Settings

**Files:**
- Create: `macos/Sources/Tyranny/Shell/{MainWindowView,SourceList,Destination,TransportDisplay,StatusBar,SettingsView}.swift`, `macos/Sources/Tyranny/Models/{TransportModel,NavigationModel}.swift`
- Modify: `TyrannyApp.swift` (window shows `MainWindowView`; add a `Settings` scene), `PlayerModel.swift` (expose `func send<T: Decodable>(_ command: String, _ fields: [String: JSONValue]) async throws -> T`; move transport commands to `TransportModel`), `SystemMediaControls.swift` (enable shuffle and repeat commands → `TransportModel`)

**Interfaces:**
- `enum Destination: Hashable { case songs, albums, artists, genres, recent, upNext, history, playlist(UInt64) }`. `NavigationModel` holds `destination`, `filter`, `columnBrowserVisible`, `upNextDrawerVisible`, and `grouping(for:)`/`setGrouping(_:for:)` (persisted under `group.<destination>`).
- `TransportModel`:
  - read: `snapshot`-derived title, artist, album, elapsed, remaining, `now_playing.format`, a gain label from `now_playing.gain_db` and `settings.gain_mode` (for example "RG album −6.2 dB"), `volume`, `repeat`, `shuffle`;
  - commands: `playPause()`, `next()`, `previous()`, `seek(to:)`, `setVolume(_:)`, `setRepeat(_:)`, `setShuffle(_:)`, which send `set_volume`, `set_repeat`, `set_shuffle`.
- Toolbar content, in order:
  - transport buttons: `transport-previous`, `transport-play-pause`, `transport-next`;
  - volume slider: `transport-volume`;
  - `TransportDisplay` (`now-playing-display`, containing the scrubber `now-playing-scrubber`). It switches to an error state when `snapshot.error` is set, and clicking it shows the full error in a popover (`now-playing-error`);
  - toggles: `transport-shuffle`, `transport-repeat` (cycles off → all → one);
  - filter field: `library-filter`, ⌘F. Syntax errors show as a red border plus a popover (`library-filter-error`).
- `SourceList` sections, with identifiers `source-<destination>`, for example `source-songs`, `source-playlist-12`:
  - LIBRARY: Songs, Albums, Artists, Genres, Recently Added;
  - PLAYING: Up Next, History;
  - PLAYLISTS: filled in Task 12.
- `StatusBar` (`status-bar`):
  - left `status-connection`, which opens Settings → Connection;
  - centre `status-totals`, showing the current view's `ListTotals` or the library totals, or a transient operation message for 4 s;
  - right `status-scan`, showing "Scanning <root> · n of m" with a progress bar while a scan is active.
- `SettingsView` tabs:
  - Connection: the current connection sheet contents and managed server controls;
  - Library: the current roots sheet contents;
  - Playback: gain mode, resume on start, and the `upNextOnPlay` picker (`settings-up-next-on-play`).
- The old `connectionStatus`, `notice` rows, `scanStatus` block and toolbar toggles are not shown. `ContentView.swift` stays compiled but unreferenced until Task 14.
- Onboarding: with `library.roots == 0`, the main area shows the "Choose Music Folder" empty state (`choose-folder`).

- [ ] **Step 1: Build** the shell with placeholder main views ("Songs" text) for each destination.
- [ ] **Step 2: Run** both builds — expect PASS.
- [ ] **Step 3: Add** `NativeShellSmoke.swift` (pattern: `NativeSmoke.startIfRequested`). It drives every `source-*` item, the transport buttons, volume, shuffle and repeat. It asserts the snapshot's `settings.volume`, `repeat` and `shuffle` change, and that the status bar shows scan progress during a rescan. Wire it into `scripts/client_gate.py` as `--scenario shell`.
- [ ] **Step 4: Run** `python3 scripts/client_gate.py --server target/debug/tyranny-server --app target/Tyranny.app --scenario shell` — expect PASS. Capture a screenshot to `spike-results/library-redesign-ui/shell.png`.
- [ ] **Step 5: Commit** `git commit -m "Replace spike shell with source list, toolbar transport and status bar"`.

### Task 6: Songs view with continuous table and column browser

**Files:**
- Create: `macos/Sources/Tyranny/Lists/{VirtualTrackTable,TrackListView,ColumnBrowser,TrackColumns}.swift`, `macos/Sources/Tyranny/Models/ListModel.swift`
- Modify: `TrackDrag.swift` (drag payload carries a `TrackSelection` produced by `SelectionEncoder`; existing queue/playlist reorder payloads unchanged)

**Interfaces:**
- `ListModel` (one per visible track list):
  - `description: ListDescription`, rebuilt from the destination source, browser selection, `NavigationModel.filter`, sort descriptors and grouping;
  - `cache: ListWindowCache<ListRow>`, whose fetch sends `list_tracks`;
  - `totals: ListTotals?`;
  - `selection: Set<UInt64>` (row IDs);
  - `func activate(position:)`, done in Task 13.
- `VirtualTrackTable: NSViewRepresentable` over `NSTableView`:
  - `numberOfRows` is `layout.count`, with a placeholder cell when `row(at:)` is nil;
  - row height 20, alternating backgrounds;
  - `sortDescriptorsDidChange` → `ListModel` sort;
  - a header context menu toggles hidden columns, persisted;
  - selection is kept by row ID across `generation` changes;
  - scrolling reports `visible(_:)`;
  - identifier `track-table-<destination>`.
- `TrackColumns`: visible by default — status, #, Title, Artist, Album, Time, Year, Genre, Format, Plays. Hidden by default — Album Artist, Disc, Date Added, Last Played, Bitrate, Sample Rate.
- `ColumnBrowser` (⌘B, identifier `column-browser`) has three lists:
  - `browser-genres` from `list_genres`;
  - `browser-artists` from `list_artists`, with the Compilations row first when present;
  - `browser-albums` from `list_albums`, with the narrowed `ListDescription`.
  - Each starts with "All (n …)". Multi-select is allowed. Selections set `Browser.genre_ids`, `artist_ids`/`compilations`, `album_ids`.
- The view header shows the title, the count, the group control (`group-control`), the Browser toggle and the Columns menu.

- [ ] **Step 1: Implement** the Songs destination with the group control disabled (wired in Task 7).
- [ ] **Step 2: Run** both builds and `swift test` — expect PASS.
- [ ] **Step 3: Add** `NativeSongsSmoke.swift` and `client_gate.py --scenario songs`. Against the 1,129-track gate fixture:
  - scroll to the end through accessibility;
  - assert the last row's title matches the server's last row;
  - sort by Year descending;
  - narrow with the browser to one genre and assert the count equals the server total;
  - select a range and drag it to Up Next, then assert the queue grew by the range length.
- [ ] **Step 4: Run** the scenario — expect PASS, and capture `songs.png`.
- [ ] **Step 5: Commit** `git commit -m "Add continuous Songs table with column browser"`.

### Task 7: Grouping and album blocks

**Files:**
- Create: `macos/Sources/Tyranny/Lists/{AlbumBlockCell,ArtistHeaderCell}.swift`
- Modify: `ListModel.swift` (fetch `list_groups` when grouping ≠ none; rebuild `GroupLayout` on revision change), `VirtualTrackTable.swift` (row kinds from `GroupLayout.row(at:)`)

**Interfaces:**
- **Artist header row:** 28 pt. Name in semibold 13 pt, then "n albums · m tracks" in secondary text.
- **Album block:**
  - The leftmost gutter column (120 pt) shows the cover (104 pt, `ArtworkStore` album thumbnail) on block row 0, overflowing downward. The row view does not clip.
  - Block rows 5–6 show the facts lines (title; year · genre; tracks · length; format) from `album_summaries`, fetched per visible album keys and cached.
  - Filler rows have no selection, and no drag or activation.
  - Compilation albums show an Artist column inside the block.
- Default grouping per destination: Songs none, Recently Added album, Artists/Genres detail album, playlists none.
- In a regular playlist, reordering is enabled only when grouping is none and the sort is empty. Otherwise a drop shows "Set Group to None and clear sorting to reorder" (`playlist-reorder-hint`).

- [ ] **Step 1: Implement**.
- [ ] **Step 2: Run** both builds and `swift test` — expect PASS.
- [ ] **Step 3: Extend** `NativeSongsSmoke` with `--scenario grouping`:
  - switch to Artist › Album and assert the header and block row counts against `GroupLayout` for the server spans;
  - select one track inside a block and assert the selection is that row ID;
  - trigger a rescan that adds an album and assert the anchor track stays visible.
- [ ] **Step 4: Run** the scenario — expect PASS, and capture `grouped.png`. Compare it with the Grouped mockup.
- [ ] **Step 5: Commit** `git commit -m "Render artist and album grouping"`.

### Task 8: Album grid with inline expansion

**Files:**
- Create: `macos/Sources/Tyranny/Albums/{AlbumGridView,AlbumGridItem,AlbumExpansionView}.swift`

**Interfaces:**
- An `NSCollectionView` flow layout reads a `ListWindowCache<AlbumSummary>`, which fetches `list_albums` with source `.library`, the filter, and the sort `album_artist|year|date_added` (`album-sort`).
- A size slider (`album-size`, 96–200 pt, persisted) sets the cover size. Each cell shows the title, then "artist · year" (`album-cell-<id>`).
- A cell retains its `ArtworkRequest` while visible and releases it on reuse.
- Clicking a cover inserts `AlbumExpansionView` (`album-expansion`) as a full-width item after the last cell of that cover's row, with a caret at the cover. It shows:
  - the cover at 150 pt;
  - title, artist, and the facts line "year · genre · n tracks · length · format · RG album x dB";
  - the tracks in two columns, from `list_tracks` with `source .album(id)`;
  - buttons Play (`album-play`), Shuffle (`album-shuffle`), Add to Up Next (`album-add-up-next`), Add to Playlist… (`album-add-playlist`).
  - Clicking another cover moves the panel. Escape closes it.
- Album actions:
  - Play: `enqueue_tracks` with a `.list` run of the album from position 0, `QueueSource(kind: .album, name: title)`, placement from `decide`, `play: true`.
  - Shuffle: `set_shuffle(true)`, then Play.
  - Return plays the selected album. ⌥Return adds it to Up Next (`.append`).

- [ ] **Step 1: Implement**.
- [ ] **Step 2: Run** both builds — expect PASS.
- [ ] **Step 3: Add** `client_gate.py --scenario albums`:
  - scroll the grid to the end;
  - open an album and assert the track count against `album_summaries`;
  - press Play and assert the queue run source is `album`.
- [ ] **Step 4: Run** the scenario — expect PASS, and capture `albums.png`.
- [ ] **Step 5: Commit** `git commit -m "Add album grid with inline expansion"`.

### Task 9: Artists and Genres

**Files:**
- Create: `macos/Sources/Tyranny/Views/{ArtistsView,GenresView}.swift`

**Interfaces:**
- **Left list.** It is 220 pt wide and continuous through `ListWindowCache<NamedCount>`:
  - Artists: `list_artists`, Compilations row first, identifier `artists-list`;
  - Genres: `list_genres`, identifier `genres-list`.
- **Right side.** A `TrackListView` with source `.artist(id)`, `.compilations` or `.genre(id)`, grouped Album by default.
- **Appears On.** For Artists, a second `TrackListView` section headed "Appears On" (`artist-appears-on`) with source `.appearsOn(artist_id:)`. It is hidden when that list's total is 0.

- [ ] **Step 1: Implement**.
- [ ] **Step 2: Run** both builds — expect PASS.
- [ ] **Step 3: Add** `client_gate.py --scenario artists`. The fixture has a compilation album. Assert:
  - the Compilations row lists it;
  - a track artist on the compilation shows it under Appears On;
  - Genres narrows to the right album count.
- [ ] **Step 4: Run** the scenario — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Add Artists and Genres views"`.

### Task 10: Recently Added and History

**Files:**
- Create: `macos/Sources/Tyranny/Views/HistoryView.swift`
- Modify: `Destination`/`TrackListView` (Recently Added uses source `.recent`, default sort date added descending, grouped Album)

**Interfaces:**
- **Recently Added.** With total 0 it shows the empty state: "Nothing added in the last 30 days" (`recent-empty`).
- **History.** A table (`history-table`) with columns Played At (relative today/yesterday, else date and time), Title, Artist, Album, Time.
  - Its cache fetches `history` and refreshes on `snapshot.history_revision`.
  - Double-click adds that track to Up Next (`.append`), when `track_id` is present.

- [ ] **Step 1: Implement**.
- [ ] **Step 2: Run** both builds — expect PASS.
- [ ] **Step 3: Add** `client_gate.py --scenario history`. Play a short track through the clocked capture to its end, then assert a History row appears and the Plays column shows 1 in Songs.
- [ ] **Step 4: Run** the scenario — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Add Recently Added and History views"`.

### Task 11: Up Next view and drawer

**Files:**
- Create: `macos/Sources/Tyranny/Views/{UpNextView,UpNextDrawer}.swift`
- Modify: `PlayerModel.swift` (queue reads go through `ListWindowCache<QueueItem>`; remove the queue offset paging)

**Interfaces:**
- **Up Next view** (`up-next-table`).
  - Section rows: HISTORY (entries with `played` and not current, dimmed), NOW PLAYING (`current_id`), UP NEXT (the rest), all in queue order.
  - Columns: status, #, Title, Artist, Album, Time, Added From (`QueueSource` rendered as "Album · Spirit of Eden", "Playlist · Late Night", "Smart · Unplayed Jazz", "Songs", "Dragged from Songs", or blank).
  - Title, Artist, Album and Time come from the `QueueItem` display fields (server Task 8).
  - Header actions: Shuffle Upcoming (`up-next-shuffle`), Save as Playlist… (`up-next-save`, a sheet with a name field), Clear (`up-next-clear` → `clear_upcoming`).
  - Reorder and remove keep the existing `move`/`remove` commands and revision checks.
- **Drawer** (⌥⌘U, `up-next-drawer`, 244 pt). It shows the now-playing card (cover 52 pt, title, artist, album · year), then upcoming items (title / artist / time), then a "Drop tracks here" target that appends.

- [ ] **Step 1: Implement**.
- [ ] **Step 2: Run** both builds — expect PASS.
- [ ] **Step 3: Add** `client_gate.py --scenario upnext`. Enqueue two runs, then:
  - assert the Added From labels;
  - Clear and assert the current track keeps playing and upcoming is empty;
  - Save as Playlist and assert the playlist exists;
  - toggle the drawer and drop onto it.
- [ ] **Step 4: Run** the scenario — expect PASS, and capture `upnext.png` and `drawer.png`.
- [ ] **Step 5: Commit** `git commit -m "Add Up Next view and drawer"`.

### Task 12: Playlists in the source list

**Files:**
- Modify: `SourceList.swift`, `TrackListView.swift`
- Move: smart-rule editor and add-to-playlist picker out of `PlaylistView.swift` into `macos/Sources/Tyranny/Views/PlaylistSheets.swift`

**Interfaces:**
- The PLAYLISTS section reads `saved_playlists` through `ListWindowCache<Playlist>`. Regular and smart playlists get distinct icons. Counts are shown for regular playlists only.
- The "+" menu (`source-add`) has New Playlist and New Smart Playlist…. Row context menus have Rename…, Edit Rule… (smart), and Delete (with an in-app confirmation).
- Drop targets:
  - a regular playlist row → `playlist_add_selection` with the drag's `TrackSelection`;
  - Up Next → `enqueue_tracks` `.append` with `source .drag`;
  - smart rows refuse the drop.
- The playlist destination shows `TrackListView` with source `.playlist(id, revision)` or `.smart(id, revision)`, plus the existing Enqueue/Play All and Create Regular Playlist from Results… actions in the view header.

- [ ] **Step 1: Implement**.
- [ ] **Step 2: Run** both builds — expect PASS.
- [ ] **Step 3: Port** the playlist assertions of `NativePlaylistSmoke`, `NativeDragSmoke` and `NativeSmartPlaylistSmoke` to the new identifiers. Run `scripts/playlist_gate.py`, `smart_playlist_gate.py`, and the drag scenario with `--app`. Expect PASS.
- [ ] **Step 4: Commit** `git commit -m "Move playlists into the source list"`.

### Task 13: Double-click runs, selection actions, keyboard

**Files:**
- Create: `macos/Sources/Tyranny/Lists/UpNextPrompt.swift`
- Modify: `ListModel.swift`, `VirtualTrackTable.swift`, `MainWindowView.swift` (commands menu)

**Interfaces:**
- **Activation** (double-click or Return on a track row):
  - Build `SelectionEncoder.run`, then `decide(policy:hasUpcoming:)`. `hasUpcoming` means the queue has an unplayed entry after the current one.
  - `.enqueue(p)` → `enqueue_tracks(selection, placement: p, play: true, source)`. `source` is derived from the destination: Songs → `.songs`, an album → `.album`, a playlist → `.playlist`/`.smart` with its name, Genres → `.genre`, Recently Added → `.recent`, Artists → `.artist`.
  - `.ask` → a sheet (`up-next-prompt`) titled "Clear Up Next?" with buttons Clear (`up-next-prompt-clear` → `.replaceUpcoming`) and Keep (`up-next-prompt-keep` → `.next`), plus the checkbox "Don't ask again" (`up-next-prompt-remember`), which writes `upNextOnPlay`.
- **Context menu** on a selection: Play (the selection as a run, with the same decision), Play Next (`.next`, no play), Add to Up Next (`.append`), Add to Playlist… (picker → `playlist_add_selection`), Show in Album, Show in Artist (navigate and select).
- **Keyboard:** Space play/pause, ⌘→/⌘← next/previous, ⌘F filter, ⌘B browser, ⌥⌘U drawer, ⌘1…⌘7 destinations in source-list order, Return play, ⌥Return add to Up Next, ⌘L reveal the playing row.
- `SelectionTooScattered` shows its message in the status bar.

- [ ] **Step 1: Implement**.
- [ ] **Step 2: Run** both builds and `swift test` — expect PASS.
- [ ] **Step 3: Add** `client_gate.py --scenario runs`. It covers spec §8:
  - double-click with Up Next empty;
  - double-click with Up Next non-empty under Ask (choose Keep, then assert the order), under Replace, and under Keep;
  - "Don't ask again" persists;
  - ⌘A then Add to Playlist adds every row in one revision.
- [ ] **Step 4: Run** the scenario — expect PASS.
- [ ] **Step 5: Commit** `git commit -m "Play runs from track lists with Up Next policy"`.

### Task 14: Remove the spike shell; port remaining harnesses

**Files:**
- Delete: `ContentView.swift`, `PlaylistView.swift` (after Task 12 moved what remains)
- Modify: `PlayerModel.swift` (remove `artists/albums/tracks/queue` pages, offsets, `page*` functions and the 100-row page size), `NativeSmoke.swift`, `NativeFilterSmoke.swift`, `NativeLifecycle.swift`, `NativeScale.swift`, `NativeArtworkSmoke.swift`, matching `scripts/*_gate.py`

**Interfaces:**
- No new types. Every native gate drives the new identifiers.
- `NativeScale` measures:
  - the first Songs window visible after warm launch (target < 1 s);
  - end-to-end filter latency;
  - a top-to-bottom scroll of 50,000 rows, asserting `ListWindowCache` never holds more than 10 windows (2,000 rows) and resident memory stays within 1.5× of its post-launch value.

- [ ] **Step 1: Delete and port**.
- [ ] **Step 2: Run** both builds and `swift test` — expect PASS.
- [ ] **Step 3: Run** every gate with `--app`: `client_gate.py` (all scenarios), `filter_gate.py`, `lifecycle_gate.py`, `artwork_gate.py`, `playlist_gate.py`, `smart_playlist_gate.py`, `scale_gate.py`. Expect PASS. Record the scale results.
- [ ] **Step 4: Commit** `git commit -m "Remove spike shell and port native gates"`.

### Task 15: Final verification and merge

- [ ] **Step 1: Run** the full server gates (server plan Task 18, Step 1) and the client gates (Task 14, Step 3). Expect PASS.
- [ ] **Step 2: Build** `scripts/build_macos.sh`, launch `target/Tyranny.app` outside the build shell, and walk the spec §8 native workflow by hand. Capture screenshots for each view into `spike-results/library-redesign-ui/`, and compare them with the mockups.
- [ ] **Step 3: Append** a "Library redesign — client" section to `SPIKE_RESULTS.md` with the gate counts, scale numbers and screenshots.
- [ ] **Step 4: Request** a whole-branch review (superpowers:requesting-code-review). Address the findings.
- [ ] **Step 5: Merge** `feature/library-redesign` into `main` (superpowers:finishing-a-development-branch).
