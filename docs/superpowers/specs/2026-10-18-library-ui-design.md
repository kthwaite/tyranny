# Library window redesign: source list, continuous lists, grouping, album grid

Status: approved · 2026-10-18
Mockups: [Tyranny Window Studies](https://claude.ai/artifact/EyUunVuJrXVq31C1JkWZ29) (private artifact; approved by the user as the target)

## 1. Intent

The spike proved the architecture. Its window is a test harness. It does not work as a daily music player. The user's complaints after real use:

- Getting from the main view to playlists takes too many clicks.
- The artist → album → track tiers do not show the whole library at once.
- The connection row and the music-folders row take vertical space and add nothing.
- The transport strip along the bottom is too large.
- Playlists and the queue open through toolbar *toggles*. That feels rough.
- There is no sidebar for choosing between views.
- Lists are paged. Paging feels unnatural.

The target is a window that shows as much information and control as possible while staying clean and minimal. It takes its structure from early iTunes (source list, toolbar display, column browser, status bar). It takes grouping and density from Clementine and foobar-style album lists. It takes the album grid from Feishin and Euphonica.

Success: the user leaves the window open all day, and every view is one click away.

## 2. Goals, non-goals, anti-goals

### Goals

1. A single window: source list on the left, one main view, toolbar transport, one-line status bar.
2. A **Songs** view: one continuous, sortable table over the whole library, with an optional column browser.
3. **Grouping** on any track list: None, Artist, Album, Artist › Album.
4. An **album grid** with inline expansion.
5. **Artists** and **Genres** views.
6. **Recently Added**, **Up Next**, and **History** views.
7. Regular and smart playlists in the source list.
8. Continuous scrolling everywhere. No visible pages.
9. iTunes-style double-click: play from the clicked track to the end of the visible list.
10. The Rust server and schema work that these views need (Section 6).
11. Shuffle, repeat and volume as server playback modes (Section 6.7).

### Non-goals (this pass)

- **Folders** view (directory browser) and **Radio**. The spike deferred both. Neither appears in the source list this pass.
- User-chosen table columns beyond the fixed set in Section 4.2. A "Columns…" menu may come later.
- Artwork-derived colours, custom themes, user-chosen accent colours.
- Vi search mode, command mode, configurable keybindings.
- Ratings or "loved" flags. The mockup's "Loved, Not Lately" playlist is only an example.
- Tag editing, MusicBrainz lookup, lyrics, scrobbling.
- Dynamic queue autofill.
- Any non-macOS client.

### Anti-goals (failure even if it technically works)

- The client loads the whole library into memory to scroll or sort it. Lists must stay bounded underneath, at the 50,000-track scale the spike measured.
- The client holds its own authoritative queue or play order. The server stays the only authority. (Unchanged from SPIKE.md.)
- Status, connection, or folder information comes back as permanent rows above the content.
- Modal toggles return for switching between views.
- Audio stutters while the user scrolls, sorts, groups, or expands an album, or while a scan runs.

## 3. Decided by the user

- All four mocked views as shown: Songs with column browser, Grouped with the Up Next drawer, Album grid, Up Next.
- Transport in the toolbar (option A in the mockups) is the default.
- Rust server and schema work is in scope.
- The destination cut in Section 4.1, with Folders and Radio out.
- Double-click plays from the clicked track to the end of the visible list (Section 5.1). Ask / Replace / Keep handling of the existing Up Next.
- Artists and Genres views use a narrow list plus album blocks (Section 4.5).
- Shuffle is a server play-order mode behind a trait, with a naive first implementation. Repeat is Off / All / One, where All repeats the context: playlist, album, or queue. Volume is a server output volume (Section 6.7).
- The spike is committed as-is to `main` as the baseline. `main` is the integration branch.

## 4. The window

### 4.1 Shell

```
┌ toolbar: ● ● ●  ⏮ ⏯ ⏭  volume  [ art | title / artist — album | scrubber | format, RG ]  shuffle repeat  [filter] ┐
├ source list ───────┬ view header (title · count · group · view controls) ─────────────┬ Up Next drawer (optional) ┤
│ LIBRARY            │                                                                  │                           │
│  Songs        n    │  main view                                                       │                           │
│  Albums       n    │                                                                  │                           │
│  Artists      n    │                                                                  │                           │
│  Genres       n    │                                                                  │                           │
│  Recently Added n  │                                                                  │                           │
│ PLAYING            │                                                                  │                           │
│  Up Next      n    │                                                                  │                           │
│  History           │                                                                  │                           │
│ PLAYLISTS          │                                                                  │                           │
│  regular + smart   │                                                                  │                           │
├ status bar: server state · totals for the current view · scan progress ──────────────────────────────────────────┤
```

**Toolbar** (about 52 pt, the window's title bar row):
- Previous, Play/Pause, Next. Volume slider.
- A centred display, as in iTunes. It shows a thumbnail, the title, "artist — album", and a scrubber with elapsed and remaining time. Small text on the right shows format and the active ReplayGain value, for example "FLAC 16/44.1 · RG album −6.2 dB".
- Shuffle and repeat toggles.
- The filter field. It uses the existing query syntax and filters the current view live.
- The 40 pt bottom bar (option B in the mockups) is not built this pass.

**Source list**: native macOS sidebar style, with three sections.
- LIBRARY: Songs, Albums, Artists, Genres, Recently Added.
- PLAYING: Up Next, History.
- PLAYLISTS: regular and smart playlists in one list, told apart by icon.
- Each row shows a count where the server can supply one cheaply. Smart playlists show no count (the catalog does not evaluate rules, as today).
- A "+" control creates a playlist or smart playlist.
- Tracks dropped on a regular playlist row are added to it. Dropped on Up Next, they are appended to the queue. This replaces the toolbar hover destinations.

**Status bar** (about 24 pt):
- Left: connection state as a dot and a short label, for example "Server · local · protocol v9". Clicking it opens Connection settings.
- Centre: totals for the current view, for example "1,129 songs · 3.2 days · 41.7 GB" or "22 of 1,129 songs · 2.6 hours" while filtered.
- Right: scan progress while a scan runs ("Scanning ~/Music · 412 of 1,129" with a small bar), otherwise nothing.

**Settings window** (⌘,): Connection (the current connection sheet and server start/stop/restart controls), Library (the current Music Folders sheet: add, rescan, remove), Playback (gain mode, resume on start, the double-click Up Next setting).

**Onboarding**: with no roots, the main area keeps the prominent "Choose Music Folder" empty state (REQUIREMENTS.md). Scan progress then appears in the status bar.

**Notices**: the spike's stacked notice rows are removed.
- Playback errors show in the toolbar display, which takes an error state. Clicking it shows details.
- Filter syntax errors show inline under the filter field, as a small popover or a red field border with a tooltip. They do not take a full-width row.
- Operation messages (for example "Added 12 tracks to Late Night") show briefly in the status bar centre.

**Up Next drawer**: an optional right-hand panel (⌥⌘U). It shows now playing and the next entries. It accepts drops. It stays open across views. It is a second view of the server queue, not a second queue.

### 4.2 Songs

- One table over all tracks in the library. It scrolls continuously.
- Fixed columns: status (playing indicator), #, Title, Artist, Album, Time, Year, Genre, Format, Plays. Also available but hidden by default: Album Artist, Disc, Date Added, Last Played, Bitrate, Sample Rate.
  - The builder decides how to hide and show these. A header context menu is enough.
- Clicking a column header sorts by it. Clicking again reverses. Ties break by album artist, album, disc, track, then ID.
- **Column browser** (⌘B), above the table: three lists, Genre | Artist | Album. Each starts with "All (n …)". Selecting in one narrows the lists to its right and the table. The browser selection combines with the filter field by AND.
- Group control in the view header (Section 4.3).

### 4.3 Grouping

- Grouping is a setting on any track list: Songs, Recently Added, a regular playlist, a smart playlist, Genres detail. Values: None, Artist, Album, Artist › Album.
- **Album** grouping: each album is a block. The cover is on the left (about 104 pt). Under the cover: album title, year · genre, track count · total time, format. The album's tracks are on the right as compact rows.
  - For compilations, the rows add an Artist column.
- **Artist** grouping: a header row per artist (name, album and track counts), then that artist's tracks.
- **Artist › Album**: an artist header, then album blocks.
- With **Album** grouping, column sorting orders album blocks only. Rows inside each album always use ascending disc then track number, regardless of column or direction. Unnumbered tracks follow numbered tracks on the same disc; ties and missing numbers use title, path, then row ID for a deterministic order. Other grouping modes retain the list's row sort.
- Groups follow the list's sort, using the group's own value for the sort column: the highest value for a descending sort, the lowest for an ascending one. So Recently Added, grouped by Album, puts the album with the newest track first. Sorting by Plays puts the album with the most-played track first. When the sort column is the group key itself (Artist, Album), this reduces to ordering by that key. *(Builder's call recorded here.)*
- **Regular playlists**: grouping changes the display only. With grouping on, drag-reordering inside the playlist is disabled. Reorder needs grouping set to None, and the UI says so.
- The group setting is remembered per destination, in client-local preferences.

### 4.4 Albums (grid)

- A grid of covers, sized by a slider. The title, then artist · year, appear under each cover.
- Sort: Album Artist, Year, Date Added.
- The filter field and the group setting do not apply to the grid's layout. The filter narrows which albums appear.
- Clicking a cover opens a panel below its row, with a caret pointing at the cover. The panel shows a large cover, the title and artist, then the facts line: year · genre · tracks · length · format · album ReplayGain. The tracklist follows, then the actions: Add to Up Next, Add to Playlist….
  - The tracklist is a table: one column list, no header. Columns: # (disc-track on multi-disc albums), Title, Artist (compilations only), Time. Unavailable tracks are dimmed. At most 12 rows show; a longer album scrolls inside the table. *(Builder's call recorded here: the 12-row height.)*
  - It allows multiple selection. The selection survives a reload of the tracks and is cleared when the panel moves to another album.
  - Double-click on a track plays the album from that track to its end, as a run (Section 5.1).
  - Return, ⌥Return and the context menu act as in track lists (Section 5.2): Return on one selected track plays from it to the album's end; on several, it plays exactly those tracks. The context menu offers Play, Play Next, Add to Up Next and Add to Playlist. Space plays and pauses; Escape closes the panel. *(Builder's call recorded here: Return on several tracks plays only those.)*
  - Add to Up Next and Add to Playlist… act on the selected tracks, or on the whole album when none are selected.
- Clicking another cover moves the panel. Escape closes it.
- In the grid, Return plays the selected album. ⌥Return adds it to Up Next.
- Covers come from the existing artwork pipeline at thumbnail size. Only visible cells request artwork. Requests for cells scrolled out of view are cancelled or ignored.

### 4.5 Artists and Genres

- **Artists**: a narrow list of album artists, with counts, inside the main area. Selecting an artist shows their albums on the right as album blocks (Section 4.3). A "Compilations" entry collects compilation albums.
- **Genres**: the same layout with a genre list. Selecting a genre shows its albums as album blocks.
- Both lists scroll continuously and follow the filter field.

**Which artist.** Browsing uses the **album artist**. That covers the Artists view, the column browser's Artist list, Artist grouping, and `source: artist(id)`.
- Compilation albums group under **Compilations**, not under each track artist and not under "Various Artists". Compilations is its own source value (`source: compilations`), since it has no artist ID.
- In the Artists view, an artist's detail also lists an **Appears On** section: compilation albums that contain tracks by that track artist.
- The Songs **Artist** column shows the track artist. `artist:` filters by track artist, as today. `albumartist:` filters by album artist.

*(Builder's call recorded here.)*

### 4.6 Recently Added

- A track list of tracks added in the last 30 days, sorted by date added, newest first, grouped by Album by default. *(Builder's call recorded here: the 30-day window.)*
- It is a fixed source-list entry, not a user-editable smart playlist.
- When nothing was added in the window, it shows an empty state that says so.

### 4.7 Up Next and History

- **Up Next** at full width shows one list with three labelled sections: HISTORY (the last few played entries, dimmed), NOW PLAYING, UP NEXT.
- Columns: status, #, Title, Artist, Album, Time, Added From.
- **Added From** records where each entry came from, for example "Album · Spirit of Eden", "Playlist · Late Night", "Smart · Unplayed Jazz", "Songs", "Dragged from Songs". The server stores it with the queue entry (Section 6.4).
- View-header actions: Shuffle Upcoming, Save as Playlist…, Clear.
  - Shuffle Upcoming is one server command that reorders the entries after the current one.
  - Save as Playlist… creates a regular playlist from the upcoming entries.
  - **Clear** removes the upcoming entries only. The current track keeps playing. This needs a new server command. The existing whole-queue clear stays available to the protocol but has no button.
- The HISTORY section shows the queue's played entries before the current one. The server trims that played prefix to the most recent 100 entries as playback advances, so the queue does not grow all day. Previous can step back through those 100. *(Builder's call recorded here: Clear semantics and the 100-entry trim.)*
- Drag reorder and remove keep their current semantics: entry IDs, revisions, insertion anchors.
- **History** is a separate view: a continuous list of plays, newest first. Columns: Played At, Title, Artist, Album, Time. Grouping by day is a builder option. History comes from the server's play history (Section 6.3).

### 4.8 Playlists

- Selecting a playlist shows its entries as a track list with grouping, filter, sort, and double-click.
- Regular playlists keep the spike's editing, with continuous scrolling in place of pages.
  - Editing covers drag reorder, remove, insert, rename and delete.
- Smart playlists keep Edit Rule…, read-only results, and Create Regular Playlist from Results….
- Sorting a regular playlist by a column header changes the display only. Saving a sorted order is not in this pass. Drag reorder needs the playlist's own order (no sort column), and the UI makes that state visible.

### 4.9 Appearance

- Follow the system light/dark setting.
- Native controls, system font, alternating row backgrounds in tables, compact rows (about 20 pt).
- Accent: the system accent colour, for selection and the playing indicator. *(Builder's call recorded here: the mockup used a crimson accent only to make the illustrations clear.)*

## 5. Behaviour

### 5.1 Double-click and Return in a track list

1. Double-click (or Return) on a row builds a **run**: the clicked track, then every following track in the visible list, in its current order. The order includes the source, browser selection, filter, sort, and grouping.
2. If Up Next has no entries after the current one, the run replaces the upcoming entries and playback starts at the clicked track.
3. If Up Next has upcoming entries, the client applies the **Up Next on play** setting:
   - **Ask** (default): a sheet asks "Clear Up Next?". Buttons: Clear (the run replaces upcoming entries) and Keep (the run plays now and the old upcoming entries follow it). It also has a "Don't ask again" checkbox, which stores the choice as Replace or Keep.
   - **Replace** or **Keep**: act without asking.
4. History is never cleared by this.
5. The client sends the server a **list description**, not track IDs (Section 6.5). The server builds the entries in one revision-bound step.
6. In the Up Next view, double-click plays that entry (existing behaviour).

### 5.2 Other actions on a selection

- Context menu and keyboard on any track list selection: Play (the selection only, as a run with the same Ask/Replace/Keep handling), Play Next (insert after current), Add to Up Next (append), Add to Playlist…, Show in Album, Show in Artist.
- **Large selections.** A virtualized list does not hold every row's ID. So a selection is sent to the server in one of two forms:
  - explicit IDs, when the selection has at most 500 rows and all are loaded;
  - a list description plus selected index ranges and excluded IDs, for anything larger. ⌘A is "the whole list".

  Play, Play Next, Add to Up Next, Add to Playlist, and drag all accept either form, and each is applied atomically under one revision check. *(Builder's call recorded here.)*
- Multi-select drag keeps the spike's native drag support. Destinations are: the source-list rows, the Up Next drawer, the Up Next view, and regular playlists.

### 5.3 Keyboard

- Space: play/pause.
- ⌘→ / ⌘←: next / previous.
- ⌘F: focus the filter field.
- ⌘B: column browser.
- ⌥⌘U: Up Next drawer.
- ⌘1 … ⌘7: the library and playing destinations in source-list order.
- Return: play (Section 5.1).
- ⌥Return: add to Up Next.
- Configurable keybindings are out of scope.

### 5.4 Selection and scroll stability

- Selection is held by ID, not row index. It survives refreshes, revision changes, and regrouping.
- After a server revision change, the client keeps the first visible row's ID anchored, where it still exists.
- The current track can be revealed with ⌘L (scroll to and select the playing row in the current list, if present).

## 6. Server and schema work

### 6.1 New track fields

Extract these with the existing native FFmpeg metadata path:

| Field | Source | Notes |
|---|---|---|
| year | date / year / originaldate tags | Integer. Keep the original date string in metadata JSON. |
| genres | genre tag(s) | Multiple values allowed. Display joins them with ", ". |
| codec | stream codec | For example FLAC, MP3, AAC, Opus, Vorbis, ALAC, WAV. |
| bit depth | stream sample format | Lossless only. Null for lossy. |
| bitrate | stream or container bitrate | kbps. |
| file size | filesystem | Bytes. Used for totals. |
| date added | earlier of file birth time and first successful index | UTC timestamp. Birth time keeps a first import from stamping the whole library with one date. Falls back to mtime where birth time is unavailable. |

The **Format** column is built from codec, bit depth and sample rate, for example "FLAC 24/96" or "MP3 320". The client or the server may compose it.

Sortable and filterable fields need real columns with indexes: year, genre (through a join table for multiple genres), date added, play count, last played. Metadata JSON alone is not enough.

**Existing libraries.** Rows indexed before this change lack the new fields.
- Add a per-asset metadata version. A rescan re-extracts tags for assets with an older version, even when size and mtime are unchanged.
- On first start after the migration, the server queues that rescan for every available root by itself, in the background. The user does not have to start it.
- Until the rescan reaches a file, its new fields are null, and the UI shows blanks, not errors.
- Existing tracks get date added from the file's birth time, by the same rule as new tracks.

*(Builder's call recorded here: the automatic rescan and the birth-time rule.)*

### 6.2 Totals

`LibraryState` (or a new query) reports track count, total duration and total file size, for the status bar. Filtered views report the same totals for the filtered result.

### 6.3 Play history and counts

- A `play_history` table records one row per counted play: track ID, queue entry ID, start time, and the source label.
- A play **counts** when playback of an occurrence passes 50% of its duration or 4 minutes, whichever comes first. A track that reaches its natural end also counts. Seeking does not inflate the count: one occurrence counts at most once. *(Builder's call recorded here; this is the common scrobbling rule.)*
- Play count and last played per track are kept as indexed columns, maintained in the same transaction as the history insert.
- Unindexed (direct-path) queue entries are recorded in history without a track ID and do not change counts.
- New paged query: history, newest first.

### 6.4 Queue entry source

Each queue entry stores a short source label, for example `{kind: album|playlist|smart|songs|genre|artist|recent|drag|direct, name}`. Queue pages return it. Existing entries get a null label and show blank.

### 6.5 List descriptions and run enqueue

Introduce one **list description** type, used by both track queries and enqueue:

```
ListDescription {
  source: library | album(id) | artist(id) | genre(id) | recent | playlist(id) | smart(id),
  browser: { genre_ids?, artist_ids?, album_ids? },   // column-browser narrowing, AND
  query: String,                                     // existing filter syntax
  sort: [ { field, descending } ],                   // server-applied; deterministic tiebreak appended
  group: none | artist | album | artist_album,       // implies a sort prefix
}
```

- **Track list query**: `ListDescription` + `revision` + `offset`/`limit`. It returns rows and the total count. When grouping is on, each row also carries its group keys (artist ID, album ID). The client draws group headers and album blocks from changes in those keys.
  - Album facts for blocks come from an album summary query by IDs: year, genre, track count, total time, format, compilation flag. Album blocks are always complete in the server's grouped order.
- **Run enqueue**: `ListDescription` + `library_revision` (+ playlist revision where relevant) + `expected_revision` (queue) + `start_track` (track ID or playlist occurrence ID, plus its offset for duplicates) + `mode: replace_upcoming | insert_before_upcoming` + `source_label`.
  - The server evaluates the description in one transaction. It builds entries from the start position to the end and applies the mode. It starts playback at the first new entry.
  - A stale revision is rejected, as today.
- **Selection play** (Section 5.2) uses the existing batch insert, with the same mode choice.
- **Shuffle Upcoming**: one queue command with expected revision.
- **Save Up Next as Playlist**: one command that creates a regular playlist from upcoming entries.
- **Clear Upcoming**: one command that removes entries after the current one (Section 4.7).
- **Played-prefix trim**: the playback owner keeps at most 100 played entries before the current one (Section 4.7).
- **Selection form**: commands that take tracks accept explicit IDs or a list description with ranges and exclusions (Section 5.2).

Album, artist and genre list queries gain sort options and counts:
- albums: by album artist, year, date added
- artists: album and track counts
- genres: album and track counts

### 6.6 Filter fields

Add qualified filter fields: `genre:`, `year:` (exact, and a range form such as `year:1970..1979`), `albumartist:`. Bare words keep searching the existing corpus, with genre added to it. *(Builder's call recorded here: the range syntax.)* Smart playlists may use the new fields.

### 6.7 Playback modes: shuffle, repeat, volume

Decided by the user. All three are server state, persisted with settings, reported in the control snapshot, and set by new protocol commands. macOS system controls route their shuffle and repeat commands to the same server commands; `SystemMediaControls.swift` disables them today.

**Shuffle.**
- A server play-order mode. When on, the next entry is chosen at random from the upcoming entries in the current repeat context (below). The queue's visible order does not change.
- Next and Previous follow the shuffled order. Previous returns to the entry that actually played before.
- The order-choosing logic sits behind a Rust trait, for example `ShuffleStrategy`, with one naive implementation: a uniform random pick without repeats until the context is exhausted. A more sophisticated shuffle comes later as another implementation of the trait. The trait boundary is a requirement, not an option.
- **Shuffle Upcoming** (Section 4.7) stays a separate, one-off reorder of the visible queue.

**Repeat: Off, All, One.**
- **One** repeats the current entry.
- **All** repeats the current **context**. The context depends on where the current entry came from:
  - an album, a regular playlist or a smart playlist: the contiguous run of entries enqueued from that source, by a run enqueue or by Play on that source;
  - anything else (Songs, Genres, Recently Added, manual adds, drags, direct paths): the whole queue.
- The context comes from a **run ID** that the server stores on each queue entry, alongside the source label (Section 6.4). One run enqueue, or one whole-album or whole-playlist enqueue, gives its entries one run ID.
- At the end of the context with Repeat All, playback continues at the context's first entry. Entries outside the context are not reached until repeat is turned off or the user picks one.
- **Off** keeps the spike rule: at the end of the queue, playback stops.
- *Interpretation recorded here: "context" is the run the current entry belongs to. The user named playlist, album and queue. Treating the other sources as "queue" is my reading.*

**Volume.**
- A server output volume, 0–100%, applied in the CoreAudio output stage after buffering, so a change is heard at once. Gain stays before buffering.
- Persisted across restarts.
- The toolbar slider and the macOS system controls (where available) set it.

### 6.8 Versions

- Protocol version 9. Client and server must match, as today.
- Main SQLite schema 5, migrated transactionally from 4. Existing queue, playlists, smart rules, settings and checkpoints survive.

## 7. Client architecture constraints

- **Continuous scrolling**: tables and the grid are virtualized.
  - The client fetches windows of rows around the visible range: up to the server's 500-row limit, a builder-chosen window size, a bounded number of windows kept. Windows are keyed by list description and revision.
  - Rows not loaded yet render as blank placeholders at the right height.
  - A revision change invalidates the cached windows. The client refetches the visible window and keeps the scroll anchor (Section 5.4).
  - The scroll bar reflects the total count from the first reply.
- Keep AppKit `NSTableView` where native drag and drop needs it (the spike found SwiftUI drop delegates did not receive these drags). The album block layout (cover beside its tracks) and the grid may use AppKit or SwiftUI, as long as they stay virtualized and support the same drag sources.
- One `PlayerModel` is 1,300 lines today. The builder should split it along view lines: connection, transport, library lists, queue, playlists. This is not required, but it is strongly advised before adding these views.
- Accessibility identifiers stay on all interactive controls. The native smoke harnesses (`NativeSmoke*.swift`, `scripts/*_gate.py`) use them and must be updated to the new shell.

## 8. Verification

- **Rust**: unit tests for the migration (schema 4 → 5 with existing data), new metadata extraction, the play-count rule, history, list-description evaluation (sort, group order, browser narrowing, filter fields), run enqueue in both modes with stale-revision rejection, Shuffle Upcoming, and Save as Playlist.
- **Gates**: `cargo test --locked --all-targets`, `cargo clippy --locked --all-targets -- -D warnings`, Swift debug and release builds with warnings as errors, and the existing Python and native gates updated to the new shell.
- **Scale**: extend the scale gate to the 50,000-track index.
  - First visible window of Songs within 1 s of warm launch.
  - Sorted, grouped and filtered track-list windows below 100 ms p95.
  - Run enqueue of a 50,000-track list measured and reported. A target will be set from the first measurement.
  - Native scroll from top to bottom without loading the whole list into memory. Check client memory stays bounded.
  - Audio stays uninterrupted throughout.
- **Native workflow**: through the real UI, switch every destination from the source list. Then double-click in Songs with Up Next empty, then non-empty with each of Ask/Replace/Keep. Group and ungroup a playlist. Open and close an album in the grid. Drag to a sidebar playlist and to the drawer. Confirm the status bar shows scan progress during a rescan.

## 9. Left to the builder

- Exact sizes, spacing and icons, within the mockups' intent.
- Window and cache sizes for continuous scrolling.
- AppKit vs SwiftUI for the album blocks and the grid.
- Whether Format is composed in the client or the server.
- Whether History groups by day.
- How hidden columns are toggled.
- Order of work. Suggested: schema and fields → list description and queries → shell (toolbar, source list, status bar, Settings) → Songs with continuous scrolling → grouping → grid → Artists/Genres/Recently Added → Up Next, History and drawer → double-click run → gates.

## 10. Deferred, recorded for later

Folders view, Radio, the bottom-bar transport option, user-chosen columns, saved sort orders for regular playlists, ratings, artwork-derived colours and themes, keybinding configuration, vi modes, dynamic autofill.

## Amendments during planning

These were settled while writing the implementation plans. They refine the design without changing what the user approved.

- **Library revision.** It changes only when rows visible to list queries change. Scan-progress saves no longer bump it, so list windows and enqueue checks are not invalidated by progress alone.
- **Play statistics.** Play count and last played live in a separate `track_stats` table, written by the playback owner. This keeps the library coordinator the only writer of library rows, and stops play counts from changing the library revision.
- **Up Next sections.** HISTORY / NOW PLAYING / UP NEXT follow one definition of "upcoming", shared by the server (Clear, Shuffle and Save Upcoming, Replace Upcoming) and the client (sections, counts, drawer, the double-click Ask decision). With shuffle off, upcoming means the entries after the current one in queue order; skipped entries before it do not play and show under HISTORY. With shuffle on, upcoming means every unplayed entry other than the current one. With nothing current, it means every unplayed entry. `ControlState` carries `current_index` and `upcoming_count`. (Amended during execution: the earlier rule used played state alone.) The 100-entry trim keeps the 100 most recently played entries, by play order.
- **Grouped tables.** The server returns the group spans (`ListGroups`) for a whole list. The client can then size a grouped table without loading every row.
- **Album block height.** An album block takes at least 8 rows (cover plus facts; amended from 7 during execution, since four facts lines need about 49 pt below a 104 pt cover). Shorter albums are padded.
- **Display fields.** Queue pages carry title, artist, album and duration. Now Playing carries format and the applied gain, for the toolbar display.
- **Selection limits.** One enqueue or playlist add resolves at most 100,000 tracks. A selection sent as a list uses at most 64 ranges, or one bounding range with at most 500 exclusions. Otherwise the client says "Selection is too scattered; select fewer separate ranges."

