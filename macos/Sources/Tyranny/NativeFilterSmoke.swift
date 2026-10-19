#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for the toolbar filter (`--filter-smoke-output`). The gate prepares 112
/// indexed tracks (7 tagged, 105 `Page` tracks), an empty queue and one 113-entry saved playlist,
/// "Filter Fixture", that holds one track twice. Text goes in through the external Accessibility
/// driver, rows are double-clicked and dragged with real mouse events, and expected rows come from
/// the server. Model state is observation only.
///
/// One filter field narrows whichever list is shown, so the saved-playlist cases filter the
/// playlist's own table; a drop is anchored by the entry under the pointer, so rows the filter
/// hides keep their places.
@MainActor
enum NativeFilterSmoke {
    private static var started = false
    private static let songsTable = "track-table-songs"
    private static let defaultsKey = "upNextOnPlay"

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--filter-smoke-output"), index + 1 < args.count else { return }
        started = true
        let kit = NativePlaylistKit(model: model, navigation: navigation,
                                    output: URL(fileURLWithPath: args[index + 1], isDirectory: true), tag: "FILTER_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                try await kit.wait("indexed fixture and catalog") {
                    model.connected && model.snapshot?.library.tracks == 112
                        && model.snapshot?.library.scan.phase == "complete" && model.savedPlaylists.isLoaded
                        && model.savedPlaylists.cache.total == 1
                }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                UserDefaults.standard.set(UpNextPolicy.replace.rawValue, forKey: defaultsKey)
                try await exercise(kit)
            } catch {
                try? kit.capture("filter-failure")
                failure = error.localizedDescription
            }
            UserDefaults.standard.removeObject(forKey: defaultsKey)
            try? kit.finish(automation: "Real AX text inputs, row double-clicks and CGEvent drags on the displayed views; server reads for expected rows; model observation only; isolated real tagged FLACs and clock-paced capture.",
                            error: failure)
            NativePlaylistKit.endSheets()
            NSApp.terminate(nil)
        }
    }

    // MARK: Scenario

    private static func exercise(_ kit: NativePlaylistKit) async throws {
        let model = kit.model, navigation = kit.navigation
        try kit.check(model.snapshot?.queue_length == 0, "Filter fixture requires an empty queue")
        try await kit.open(.songs)
        try await kit.wait("songs list") { NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw NativePlaylistKit.Failure(message: "Songs list missing") }
        try await kit.wait("all songs") { songs.cache.total == 112 && songs.cache.loadedRow(at: 0) != nil }

        // Symbolic filter: the rows are the server's rows for the same list description.
        try await query(kit, songs, "artist:\"Miles Davis\" & !album:\"Kind of Blue\"", total: 107)
        let expected = try await page(kit, songs.description, offset: 0, limit: 5)
        try kit.check(expected.total == 107, "The server counts \(expected.total) matches, not 107")
        try kit.check((0..<5).allSatisfy { songs.cache.loadedRow(at: $0)?.row_id == expected.entries[$0].row_id },
                      "Filtered rows differ from the server's first rows")
        try await kit.drive("scroll-fraction", songsTable, "1")
        try await kit.wait("filtered tail") { songs.cache.loadedRow(at: 106) != nil }
        let tail = try await page(kit, songs.description, offset: 106, limit: 1)
        try kit.check(songs.cache.loadedRow(at: 106)?.row_id == tail.entries.first?.row_id, "Filtered tail differs from the server's")
        try kit.check(songs.cache.loadedRows().count <= 2000, "The filtered cache is not bounded")
        kit.passed("native-symbolic-library-filter-and-matching-rows-come-from-the-server")

        // Field lists, negation and parentheses give the same three tracks.
        let alternatives = "artist:[\"Miles Davis\", \"Ornette Coleman\"] & !album:[\"Sessions\", \"Kind of Blue\"]"
        let three: Set<String> = ["Companion", "Freedom", "Lonely Woman"]
        try await query(kit, songs, alternatives, total: 3)
        try kit.check(Set(songs.cache.loadedRows().map(\.row.track.title)) == three, "Field lists matched the wrong tracks")
        try kit.capture("library-list-filter")
        try await query(kit, songs, "(artist:\"Miles Davis\" | artist:\"Ornette Coleman\") & !album:[\"Sessions\", \"Kind of Blue\"]", total: 3)
        try kit.check(Set(songs.cache.loadedRows().map(\.row.track.title)) == three, "Parenthesized OR matched the wrong tracks")
        kit.passed("native-field-lists-negation-and-parenthesized-or")

        // A syntax error shows no rows and leaves the queue alone; the latest of several quick queries wins.
        try await kit.select(songsTable, [0, 2])
        try await kit.value("library-filter", "artist:[\"Miles Davis\",")
        try await kit.wait("visible syntax failure") { songs.filterError != nil && navigation.filterError != nil }
        try await kit.drive("exists", "library-filter-error")
        try kit.check(songs.cache.loadedRows().isEmpty && model.snapshot?.queue_length == 0,
                      "An invalid filter kept actionable old rows or changed the queue")
        try kit.capture("library-filter-error")
        let changes = try JSONSerialization.data(withJSONObject: ["album:Sessions", "artist:[", "artist:\"BJÖRK\""])
        try await kit.drive("values", "library-filter", String(decoding: changes, as: UTF8.self))
        try await kit.wait("latest rapid query wins") {
            songs.filterError == nil && songs.cache.total == 1 && songs.cache.loadedRow(at: 0)?.track.title == "Jóga"
        }
        kit.passed("native-syntax-error-shows-no-rows-and-latest-query-recovers")

        // Double-click on the filtered list plays the run from the clicked row, not the hidden rows.
        try await query(kit, songs, alternatives, total: 3)
        let titles = (0..<3).compactMap { songs.cache.loadedRow(at: $0)?.track.title }
        try kit.check(titles.count == 3, "The filtered rows are not loaded")
        try await kit.drive("double-click-row", songsTable, "1,0.35")
        try await kit.wait("clicked filtered row plays its run") {
            !model.busy && model.snapshot?.queue_length == 2 && model.snapshot?.transport == "playing"
                && model.snapshot?.now_playing?.title == titles[1]
        }
        let current = model.snapshot!.current_id
        let queueRevision = model.snapshot!.queue_revision
        kit.passed("native-filtered-double-click-plays-the-run-from-the-clicked-row")

        // The saved playlist, narrowed to album:Sessions, reorders by entry: hidden rows keep their places.
        try await kit.value("library-filter", "")
        guard let playlist = kit.playlist(named: "Filter Fixture") else { throw NativePlaylistKit.Failure(message: "Fixture playlist missing") }
        let tableID = "track-table-playlist-\(playlist.id)"
        try await kit.open(.playlist(playlist.id))
        try await kit.wait("playlist list") { NativePlaylistKit.shown?.destination == .playlist(playlist.id) }
        guard let list = NativePlaylistKit.shown else { throw NativePlaylistKit.Failure(message: "Playlist list missing") }
        try await kit.wait("playlist rows") { list.cache.total == 113 && list.cache.loadedRow(at: 0) != nil }
        var full = try await kit.entries(playlist)
        try kit.check(full.count == 113, "The saved fixture has \(full.count) entries")
        try await query(kit, list, "album:Sessions", total: 105)
        let moving = [1, 3].compactMap { list.cache.loadedRow(at: $0)?.row_id }
        guard moving.count == 2, let anchor = list.cache.loadedRow(at: 50)?.row_id else {
            throw NativePlaylistKit.Failure(message: "Filtered playlist rows are not loaded")
        }
        try await kit.select(tableID, [1, 3])
        try await kit.wait("filtered drag selection") { list.selectedPositions == IndexSet([1, 3]) }
        let revision = model.savedPlaylists.playlist(id: playlist.id)?.revision ?? 0
        try await kit.drag(tableID, row: 1, steps: [NativePlaylistKit.target(tableID, row: 50, edge: "before")],
                           capture: "filtered-reorder-indicator")
        try await kit.wait("filtered reorder") {
            !model.busy && (model.savedPlaylists.playlist(id: playlist.id)?.revision ?? 0) > revision
        }
        let movingRows = full.filter { moving.contains($0.id) }
        full.removeAll { moving.contains($0.id) }
        full.insert(contentsOf: movingRows, at: full.firstIndex { $0.id == anchor }!)
        try kit.check(try await kit.entries(playlist).map(\.id) == full.map(\.id),
                      "The filtered move changed hidden rows or used a filtered position as the anchor")
        try kit.check(model.snapshot?.queue_revision == queueRevision && model.snapshot?.current_id == current
                        && model.snapshot?.transport == "playing", "Filtering or reordering a saved playlist changed playback")
        kit.passed("native-filtered-drag-keeps-hidden-entries-in-place-and-playback")

        // Tracks dropped from Songs at an insertion point are anchored by the entry under the pointer.
        try await kit.open(.songs)
        try await query(kit, songs, alternatives, total: 3)
        let copied = (0..<3).compactMap { songs.cache.loadedRow(at: $0)?.track.id }
        try kit.check(copied.count == 3, "The rows to copy are not loaded")
        let visible = full.filter { copied.contains($0.track_id) }
        guard visible.count >= 2 else { throw NativePlaylistKit.Failure(message: "The filtered playlist has too few rows to insert before") }
        let insertionAnchor = visible[1].id
        let beforeIDs = Set(full.map(\.id))
        let insertRevision = model.savedPlaylists.playlist(id: playlist.id)?.revision ?? 0
        try await kit.select(songsTable, [0, 1, 2])
        try await kit.wait("copy selection") { songs.selectedPositions.count == 3 }
        try await kit.drag(songsTable, row: 0, steps: [
            NativePlaylistKit.hover(Destination.playlist(playlist.id).identifier),
            NativePlaylistKit.target(tableID, row: 1, edge: "before")])
        _ = try await kit.waitForPlaylist("Filter Fixture", entries: 116)
        try kit.check((model.savedPlaylists.playlist(id: playlist.id)?.revision ?? 0) > insertRevision, "The playlist revision did not move")
        let inserted = try await kit.entries(playlist)
        let additions = inserted.filter { !beforeIDs.contains($0.id) }
        try kit.check(additions.map(\.track_id) == copied, "The insert changed the copied order")
        full.insert(contentsOf: additions, at: full.firstIndex { $0.id == insertionAnchor }!)
        try kit.check(inserted.map(\.id) == full.map(\.id), "The filtered insert used the wrong full-playlist anchor")
        kit.passed("native-drop-into-a-filtered-playlist-anchors-on-the-entry-under-the-pointer")

        // A saved-list syntax error shows no rows; fixing it brings them back; playback never changed.
        guard let shown = NativePlaylistKit.shown else { throw NativePlaylistKit.Failure(message: "Playlist list missing") }
        try await kit.open(.playlist(playlist.id))
        try await kit.value("library-filter", "artist:[\"Miles Davis\",]")
        try await kit.wait("saved syntax error") { shown.filterError != nil && shown.cache.loadedRows().isEmpty }
        try await kit.drive("exists", "library-filter-error")
        try kit.capture("playlist-filter-error")
        try await query(kit, shown, "album:Sessions", total: 105)
        try kit.capture("filtered-playlist")
        try kit.check(model.snapshot?.queue_revision == queueRevision && model.snapshot?.queue_length == 2
                        && model.snapshot?.current_id == current && model.snapshot?.transport == "playing"
                        && model.snapshot?.underruns == 0, "Filter editing interrupted the independent playback queue")
        kit.passed("native-saved-filter-syntax-recovery-without-playback-mutation")
    }

    // MARK: Helpers

    /// Types `text` in the filter and waits for `list` to show `total` rows, the first of them loaded.
    private static func query(_ kit: NativePlaylistKit, _ list: ListModel, _ text: String, total: Int) async throws {
        try await kit.value("library-filter", text)
        try await kit.wait("result of \(text)") {
            kit.navigation.filter == text && list.filterError == nil && list.cache.total == total
                && (total == 0 || list.cache.loadedRow(at: 0) != nil)
        }
    }

    private static func page(_ kit: NativePlaylistKit, _ description: ListDescription, offset: Int,
                             limit: Int) async throws -> ListTracksResponse {
        try await kit.model.send("list_tracks", [
            "list": try JSONValue.encoding(description), "revision": .null, "offset": .int(offset), "limit": .int(limit)])
    }
}
#endif
