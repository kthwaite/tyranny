# Album panel track table

The album panel (spec 4.4) lists tracks in two SwiftUI columns. Rows cannot be selected or
double-clicked. This plan replaces them with a native table, drops the panel's Play and
Shuffle buttons, and makes Add to Up Next and Add to Playlist… act on the selected tracks,
or the whole album when nothing is selected.

Worktree: `../tyranny-album-tracks`, branch `album-track-table`. Gates:
`swift build --package-path macos --scratch-path target/swift-client`,
`swift test --package-path macos --scratch-path target/swift-client`, and the albums client gate
(`scripts/client_gate.py`, scenario `albums`).

## Behaviour

- The tracklist is one `NSTableView` (an `ActivatingTableView`), one column list, no header.
  Columns: # (disc-track on multi-disc albums, as today), Title, Artist (compilations only), Time.
  Unavailable tracks are dimmed, as today. Rows are 20 pt; at most 12 rows show, longer albums
  scroll inside the table. "and N more…" sits under the table, outside it.
- Multiple selection. Selection is held by row ID on the `AlbumExpansion`, so it survives a
  track reload and is gone when the panel moves to another album.
- Double-click a row: play the album from that row to its end (a run, like Songs).
- Return: one selected row plays a run from it; several play exactly those rows. ⌥Return adds
  the selected rows to Up Next. Space toggles play/pause. Escape closes the panel and gives
  focus back to the grid.
- Context menu on a row (selecting it first when unselected): Play, Play Next, Add to Up Next,
  then Add to Playlist ▸ with the regular playlists. Play sends exactly the selected rows.
- Buttons: Add to Up Next and Add to Playlist… only. Each acts on the selected rows, or the
  whole album when none are selected. Status text counts the tracks.
- Grid Return / ⌥Return on a selected cover still play / add the whole album.
- Shuffle is gone from the panel. `EnqueueJob.shuffleFirst` and its `set_shuffle` call go too
  (their only user).
- Queue source stays `Album · <title>` for everything the panel queues.

## Task 1 — core: panel selections and row counts (TDD)

File: `macos/Sources/TyrannyCore/AlbumGrid.swift`; tests in
`macos/Tests/TyrannyCoreTests/AlbumGridTests.swift`.

1. Failing tests, then `AlbumPanel.selection(albumID:revision:positions:asRun:rowID:trackID:) throws -> TrackSelection`:
   - empty `positions` → `AlbumPlayback.selection(albumID:libraryRevision:)` (whole album, server picks start);
   - one position with `asRun` → `SelectionEncoder.run` over `AlbumPlayback.list` from that position, `firstRowID` = its row ID;
   - otherwise → `SelectionEncoder.activation(... asRun: false)` over `AlbumPlayback.list` (track IDs).
2. Failing tests, then `AlbumPanel.visibleRows(trackCount:) -> Int` = `max(1, min(trackCount, trackLimit, maximumRows))`
   with `public static let maximumRows = 12`, and `AlbumPanel.showsMore(trackCount:) -> Bool` = `trackCount > trackLimit`.
3. `swift test` green.

## Task 2 — app: the table, the model actions, the buttons

Files: new `macos/Sources/Tyranny/Albums/AlbumTrackTable.swift`; edit `AlbumExpansionView.swift`,
`AlbumGridModel.swift`, `Models/ActivationModel.swift`.

1. `AlbumExpansion`: `selectedRowIDs: Set<UInt64>`, `selectedPositions: IndexSet` (from `tracks`),
   and drop row IDs no longer in `tracks` on `loaded`.
2. `AlbumGridModel`:
   - `play(_ album:)` (grid Return) loses its `shuffle` parameter.
   - `playTracks(_ expansion:, positions:, asRun:)`, `playNext(_ expansion:)`, `addToUpNext(_ expansion:)`,
     `addToPlaylist(_ expansion:, playlist:)` build their selection with `AlbumPanel.selection`, under the
     revision the tracks were loaded at (`basis.revision`), via `SelectionRequest.source`.
     Empty selection = whole album, as today. Status: "Added N tracks to Up Next." / "Added N tracks to play next." /
     "Added <title or N tracks> to <playlist>.".
   - `addToUpNext(_ album:)` stays for grid ⌥Return.
3. `AlbumTrackTable: NSViewRepresentable` over `expansion.tracks`, matching `VirtualTrackTable`'s idiom
   (Coordinator data source/delegate, `ActivatingTableView`, `doubleAction`). Identifier `album-track-table`;
   title cells `album-track-table-cell-<row_id>-title`. Escape: collapse, then make the enclosing
   `AlbumCollectionView` first responder.
4. `AlbumExpansionContent`: table instead of the two columns; "and N more…" (`album-expansion-more`) under it;
   keep `album-expansion-tracks` (its value: the total) on the table's container. Remove Play and Shuffle.
   `AlbumExpansionMetrics` uses `AlbumPanel.visibleRows` / `showsMore` and the 20 pt row height.
5. Remove `EnqueueJob.shuffleFirst` and the `set_shuffle` branch in `ActivationModel`.
6. `swift build` and `swift test` green.

## Task 3 — smoke, gate, spec

1. `NativeAlbumsSmoke`: replace `press album-play` with `double-click-row album-track-table 0,0.5`; the existing
   queue checks still hold (a run from row 0 is the whole album, one run, album source). Then `select album-track-table 0,1`,
   `press album-add-up-next`, and check the queue grew by exactly those two track IDs at its end. New case
   `native-albums-add-selected-to-up-next`. Rename the play case to `native-albums-double-click-queues-album-run`.
2. `scripts/client_gate.py` `albums`: docstring and queue-total assertion follow (total = track_count + 2).
3. Spec `docs/superpowers/specs/2026-10-18-library-ui-design.md`: 4.4 panel line (table, actions, selection rules),
   6.7 drop the album Shuffle line.
4. Run the albums client gate.
