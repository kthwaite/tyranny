#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Opt-in acceptance driver for playlists in the source list, in three modes:
///
/// - `--playlist-smoke-output`: the gate's fixture is 105 indexed tracks, an empty queue and no
///   playlists. Playlists are made with the picker and the "+" menu, filled by drops on the
///   source list, enqueued and played, then all but one are deleted.
/// - `--playlist-smoke-relaunch`: after a daemon and app restart that playlist is still there.
/// - `--drag-smoke-output`: insertion drops, reorder, the refused reorder in a grouped list,
///   and a drop on Up Next, with real mouse drags.
///
/// Controls are driven through the external Accessibility driver and CGEvent mouse drags.
/// Rename and Delete sit in context menus, which that driver cannot open: they call the
/// model's edits instead.
@MainActor
enum NativePlaylistSmoke {
    private static var started = false

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        let drag = args.firstIndex(of: "--drag-smoke-output")
        guard !started, let index = drag ?? args.firstIndex(of: "--playlist-smoke-output"), index + 1 < args.count else { return }
        started = true
        let kit = NativePlaylistKit(model: model, navigation: navigation,
                                    output: URL(fileURLWithPath: args[index + 1], isDirectory: true),
                                    tag: drag == nil ? "PLAYLIST_SMOKE" : "DRAG_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                try await kit.wait("connection and catalog") {
                    model.connected && model.snapshot?.library.tracks == 105
                        && model.snapshot?.library.scan.phase == "complete" && model.savedPlaylists.isLoaded
                }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                if drag != nil {
                    try await exerciseDrag(kit)
                } else if args.contains("--playlist-smoke-relaunch") {
                    try await relaunch(kit)
                } else {
                    try await exercise(kit)
                }
            } catch {
                try? kit.capture("failure")
                failure = error.localizedDescription
            }
            try? kit.finish(automation: "External Accessibility clicks, picker and sheet controls, and CGEvent mouse drags on the displayed SwiftUI/AppKit views; server reads only for expected values; composited window screenshots. Capture-only, no device-output claim.",
                            error: failure)
            NativePlaylistKit.endSheets()
            NSApp.terminate(nil)
        }
    }

    // MARK: Create, add, enqueue

    private static func exercise(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        try kit.check(model.snapshot?.queue_length == 0 && model.savedPlaylists.cache.total == 0,
                      "The gate needs an empty queue and no playlists")
        let songs = try await songsList(kit)
        let table = "track-table-songs"

        // 100 selected rows, a new playlist from the picker.
        try await kit.select(table, Array(0..<100))
        try await kit.wait("100 rows selected") { songs.selectedPositions.count == 100 }
        try await kit.press("add-to-playlist")
        try await kit.value("playlist-picker-name", "Native Retained Playlist")
        try kit.capture("create-from-picker")
        try await kit.press("playlist-picker-create-add")
        let retained = try await kit.waitForPlaylist("Native Retained Playlist", entries: 100)
        try await kit.wait("sidebar row") { model.savedPlaylists.isLoaded }
        try await kit.drive("exists", Destination.playlist(retained.id).identifier)
        try kit.check(songs.selectedPositions.count == 100, "Creating from the picker lost the selection")
        kit.passed("native-picker-creates-a-playlist-and-adds-100-selected-rows")

        // Five more rows: a duplicate name first, then the existing playlist.
        try await kit.drive("scroll-fraction", table, "1")
        try await kit.wait("last window") { songs.cache.loadedRow(at: 104) != nil }
        try await kit.select(table, [100, 101, 102, 103, 104])
        try await kit.wait("five rows selected") { songs.selectedPositions.count == 5 }
        try await kit.press("add-to-playlist")
        try await kit.value("playlist-picker-name", "Native Retained Playlist")
        try await kit.press("playlist-picker-create-add")
        try await kit.drive("exists", "playlist-picker-create-error")
        try kit.check(model.savedPlaylists.rows.count == 1 && model.savedPlaylists.rows[0].entry_count == 100,
                      "A rejected creation changed the catalog")
        try await kit.select("playlist-picker-list", [0])
        try await kit.press("playlist-picker-add")
        let filled = try await kit.waitForPlaylist("Native Retained Playlist", entries: 105)
        kit.passed("native-picker-reports-a-duplicate-name-and-adds-to-an-existing-playlist")

        // The playlist page: rows in playlist order, Enqueue and Play All.
        try await kit.open(.playlist(filled.id))
        try await kit.wait("playlist list") { NativePlaylistKit.shown?.destination == .playlist(filled.id) }
        guard let list = NativePlaylistKit.shown else { throw NativePlaylistKit.Failure(message: "Playlist list missing") }
        try await kit.wait("playlist rows") { list.cache.total == 105 && list.cache.loadedRow(at: 0) != nil }
        let entries = try await kit.entries(filled)
        try kit.check((0..<5).allSatisfy { list.cache.loadedRow(at: $0)?.row_id == entries[$0].id }, "Rows differ from the server's order")
        try await kit.drive("expect-text", "list-count", "105 songs")
        try kit.capture("playlist")
        kit.passed("native-playlist-page-shows-the-playlist-in-its-own-order")

        try await kit.press("playlist-enqueue")
        try await kit.wait("whole playlist enqueued") { !model.busy && model.snapshot?.queue_length == 105 }
        try await kit.press("playlist-play")
        try await kit.wait("whole playlist appended and playing") {
            !model.busy && model.snapshot?.queue_length == 210 && model.snapshot?.transport == "playing"
        }
        try kit.check(model.savedPlaylists.playlist(id: filled.id)?.entry_count == 105, "Enqueue changed the playlist")
        kit.passed("native-enqueue-and-play-all-append-independent-occurrences")

        // A second playlist from the "+" menu, filled by a drop on its source row.
        try await kit.drive("choose", "source-add", "New Playlist")
        try await kit.value("playlist-name", "Temporary Playlist")
        try await kit.press("playlist-name-save")
        let temporary = try await kit.waitForPlaylist("Temporary Playlist", entries: 0)
        try await kit.open(.songs)
        try await kit.select(table, [0, 2])
        try await kit.wait("two rows selected") { songs.selectedPositions.count == 2 }
        let first = try await kit.songs(3)
        try await kit.drag(table, row: 0, steps: [NativePlaylistKit.target(Destination.playlist(temporary.id).identifier, hold: 0.5)])
        _ = try await kit.waitForPlaylist("Temporary Playlist", entries: 2)
        let dropped = try await kit.entries(temporary)
        try kit.check(dropped.map(\.track_id) == [first[0].track.id, first[2].track.id], "Row drop did not append in order")
        kit.passed("native-drop-on-a-regular-playlist-row-appends")

        // Rename and delete (context menus: model edits). The queue keeps playing.
        let queue = model.snapshot?.queue_length
        try await model.savedPlaylists.rename(temporary, name: "Renamed Temporary")
        _ = try await kit.waitForPlaylist("Renamed Temporary", entries: 2)
        let renamed = kit.playlist(named: "Renamed Temporary")!
        try await model.savedPlaylists.delete(renamed)
        try await kit.wait("deleted") { kit.playlists.count == 1 }
        try kit.check(model.snapshot?.queue_length == queue && model.snapshot?.transport == "playing",
                      "Deleting a playlist changed the queue or playback")
        kit.passed("native-rename-and-delete-keep-the-queue-and-playback")
        try await kit.open(.playlist(filled.id))
        try kit.capture("retained-for-restart")
    }

    private static func relaunch(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        try kit.check(model.savedPlaylists.rows.count == 1, "Restart must retain exactly one playlist")
        try kit.check(model.snapshot?.transport == "paused", "The restart fixture must be paused")
        let playlist = model.savedPlaylists.rows[0]
        try await kit.open(.playlist(playlist.id))
        try await kit.wait("playlist rows") {
            NativePlaylistKit.shown?.destination == .playlist(playlist.id) && (NativePlaylistKit.shown?.cache.total ?? 0) > 100
        }
        try kit.capture("restart-persistence")
        kit.passed("native-relaunch-shows-the-persisted-playlist")
    }

    // MARK: Drags

    private static func exerciseDrag(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        try kit.check(model.snapshot?.queue_length == 0 && model.savedPlaylists.cache.total == 0,
                      "The drag fixture needs an empty queue and no playlists")
        let songs = try await songsList(kit)
        let table = "track-table-songs"
        let source = try await kit.songs(5)
        let ids = source.map(\.track.id)

        try await kit.drive("choose", "source-add", "New Playlist")
        try await kit.value("playlist-name", "Drag Destination")
        try await kit.press("playlist-name-save")
        let created = try await kit.waitForPlaylist("Drag Destination", entries: 0)
        let row = Destination.playlist(created.id).identifier
        let tableID = "track-table-playlist-\(created.id)"
        try await kit.open(.songs)

        // Drop on the source row: appended in order.
        try await kit.select(table, [0, 2, 4])
        try await kit.wait("three rows selected") { songs.selectedPositions.count == 3 }
        try await kit.drag(table, row: 0, steps: [NativePlaylistKit.target(row, hold: 0.5)])
        _ = try await kit.waitForPlaylist("Drag Destination", entries: 3)
        try kit.check(try await kit.entries(created).map(\.track_id) == [ids[0], ids[2], ids[4]], "The row drop did not append in order")
        kit.passed("native-drag-on-a-playlist-row-appends-the-selection-in-order")

        // Hold over the row to open the playlist, then drop at an insertion point.
        try await kit.open(.songs)
        try await kit.select(table, [1, 3])
        try await kit.wait("two rows selected") { songs.selectedPositions.count == 2 }
        try await kit.drag(table, row: 1, steps: [
            NativePlaylistKit.hover(row), NativePlaylistKit.target(tableID, row: 1, edge: "before")],
            capture: "playlist-insertion-indicator")
        _ = try await kit.waitForPlaylist("Drag Destination", entries: 5)
        var expected = try await kit.entries(created)
        try kit.check(expected.map(\.track_id) == [0, 1, 3, 2, 4].map { ids[$0] }, "The insertion drop appended instead of inserting")
        kit.passed("native-drag-inserts-before-the-row-under-the-pointer")

        // Reorder inside the playlist: a non-contiguous selection to the top.
        guard let list = NativePlaylistKit.shown else { throw NativePlaylistKit.Failure(message: "Playlist list missing") }
        try await kit.wait("playlist rows") { list.cache.total == 5 && list.cache.loadedRow(at: 4) != nil }
        try await kit.select(tableID, [1, 3])
        try await kit.wait("rows selected") { list.selectedPositions == IndexSet([1, 3]) }
        let revision = model.savedPlaylists.playlist(id: created.id)?.revision ?? 0
        let reordered = NativePlaylistKit.moved(expected, indexes: [1, 3], before: expected[0].id)
        try await kit.drag(tableID, row: 1, steps: [NativePlaylistKit.target(tableID, row: 0, edge: "before")],
                           capture: "playlist-reorder-indicator")
        try await kit.wait("playlist reordered") { (model.savedPlaylists.playlist(id: created.id)?.revision ?? 0) > revision && !model.busy }
        expected = try await kit.entries(created)
        try kit.check(expected.map(\.id) == reordered.map(\.id), "The reorder changed identities or order")
        kit.passed("native-drag-reorders-a-non-contiguous-selection-inside-a-playlist")

        // Grouped, the shown order is not the playlist's: the drop is refused with a hint.
        kit.navigation.setGrouping(.album, for: .playlist(created.id))
        try await kit.wait("grouped") { list.grouping == .album }
        try await kit.wait("grouped rows") { list.cache.loadedRow(at: 0) != nil && list.layoutIsCurrent }
        let from = list.layout.displayIndex(ofPosition: 0) ?? 1
        let to = list.layout.displayIndex(ofPosition: 3) ?? 3
        try await kit.select(tableID, [from])
        let before = expected.map(\.id)
        try await kit.drag(tableID, row: from, steps: [NativePlaylistKit.target(tableID, row: to, edge: "before")])
        try await kit.drive("exists", "playlist-reorder-hint")
        try kit.check(try await kit.entries(created).map(\.id) == before, "A refused reorder changed the playlist")
        kit.navigation.setGrouping(.none, for: .playlist(created.id))
        kit.passed("native-grouped-playlist-refuses-reorder-and-shows-the-hint")

        // Dropping the playlist's rows on Up Next appends them, named for the playlist.
        try await kit.wait("ungrouped") { list.grouping == .none && list.cache.loadedRow(at: 1) != nil }
        try await kit.select(tableID, [0, 1])
        let length = model.snapshot?.queue_length ?? 0
        try await kit.drag(tableID, row: 0, steps: [NativePlaylistKit.target(Destination.upNext.identifier, hold: 0.5)])
        try await kit.wait("queue grew") { !model.busy && model.snapshot?.queue_length == length + 2 }
        let tail: Page<QueueItem> = try await model.send("queue_page", [
            "revision": .null, "offset": .int(length), "limit": .int(2)])
        try kit.check(tail.entries.allSatisfy { $0.source == QueueSource(kind: .drag, name: "Drag Destination") },
                      "Up Next entries lack the playlist as their source")
        try kit.capture("after-drag")
        kit.passed("native-drag-on-up-next-appends-with-the-playlist-as-source")

        // Delete removes the selected entries by entry ID. The playlist's other entries stay.
        let remaining = try await kit.entries(created)
        try kit.check(remaining.count == 5, "The removal case needs the five-entry playlist")
        try await kit.select(tableID, [0, 2])
        try await kit.wait("two entries selected") { list.selectedPositions == IndexSet([0, 2]) }
        let removedIDs = Set([remaining[0].id, remaining[2].id])
        let queueLength = model.snapshot?.queue_length
        try await kit.drive("key", tableID, "delete")
        try await kit.wait("entries removed") { !model.busy && list.cache.total == 3 }
        let left = try await kit.entries(created)
        try kit.check(left.map(\.id) == [remaining[1].id, remaining[3].id, remaining[4].id]
                      && removedIDs.isDisjoint(with: left.map(\.id)), "Delete removed the wrong entries")
        try kit.check(model.snapshot?.queue_length == queueLength, "Removing entries changed the queue")
        try await kit.drive("expect-text", "list-count", "3 songs")
        try kit.check(list.selectedPositions.isEmpty, "The selection survived the removal")
        try kit.capture("after-remove")
        kit.passed("native-delete-removes-the-selected-playlist-entries-by-entry-id")
    }

    private static func songsList(_ kit: NativePlaylistKit) async throws -> ListModel {
        try await kit.wait("songs list") { NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw NativePlaylistKit.Failure(message: "Songs list missing") }
        try await kit.wait("songs rows") { songs.cache.total == 105 && songs.cache.loadedRow(at: 4) != nil }
        return songs
    }
}
#endif
