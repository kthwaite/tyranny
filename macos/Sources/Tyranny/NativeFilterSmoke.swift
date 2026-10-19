#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Real text input, mouse activation and dragging; model state is observation only.
@MainActor
enum NativeFilterSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-filter-smoke")
    private static var cases: [String] = []

    static func startIfRequested(_ model: PlayerModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--filter-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("indexed fixture", model) { model.connected && model.libraryReady && model.tracks?.total == 112 && model.playlists?.total == 1 }
                try await NativeSmoke.drive("activate")
                try await exercise(model)
                try finish(model, error: nil)
            } catch {
                try? capture("failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel) async throws {
        try check(model.snapshot?.queue_length == 0, "Filter fixture requires an empty queue")
        try await value("library-filter", "artist:\"Miles Davis\" & !album:\"Kind of Blue\"")
        try await wait("symbolic library filter", model) { model.libraryReady && model.tracks?.total == 107 }
        try check(model.tracks!.entries.count == 100 && model.tracks!.entries.prefix(2).map(\.metadata.title) == ["Companion", "Freedom"], "Library query lost bounded order")
        try check(Set(model.artists!.entries.map(\.name)) == ["Miles Davis", "Various Artists"], "Artist pane disagrees with matching tracks")
        try await press("tracks-next")
        try await wait("matching library tail", model) { model.libraryReady && model.tracks?.offset == 100 }
        try check(model.tracks!.entries.map(\.metadata.title) == (98...104).map { String(format: "Page %03d", $0) }, "Filtered library page skipped or repeated matches")
        cases.append("native-symbolic-library-filter-and-bounded-matching-pages")

        let alternatives = "artist:[\"Miles Davis\", \"Ornette Coleman\"] & !album:[\"Sessions\", \"Kind of Blue\"]"
        let three = ["Companion", "Freedom", "Lonely Woman"]
        try await library(alternatives, titles: three, model)
        try capture("library-list-filter")
        try await library("(artist:\"Miles Davis\" | artist:\"Ornette Coleman\") & !album:[\"Sessions\", \"Kind of Blue\"]", titles: three, model)
        cases.append("native-field-lists-negation-and-parenthesized-or")
        try await select("track-table", [0, 2])
        try await wait("selection before invalid query", model) { model.selectedTracks.count == 2 }
        try await value("library-filter", "artist:[\"Miles Davis\",")
        try await wait("visible library syntax failure", model) { model.libraryFilterError != nil && !model.libraryReady }
        try await NativeSmoke.drive("exists", "library-filter-error")
        try check(model.tracks == nil && model.selectedTracks.isEmpty && model.snapshot?.queue_length == 0, "Invalid filter retained actionable old rows or changed queue")
        try capture("library-filter-error")
        let changes = try JSONSerialization.data(withJSONObject: ["album:Sessions", "artist:[", "artist:\"BJÖRK\""])
        try await NativeSmoke.drive("values", "library-filter", String(decoding: changes, as: UTF8.self))
        try await wait("latest rapid query wins", model) { model.libraryReady && model.libraryFilterError == nil && model.tracks?.entries.map(\.metadata.title) == ["Jóga"] }
        cases.append("native-syntax-error-clears-stale-selection-and-latest-query-recovers")

        try await library(alternatives, titles: three, model)
        let copiedTrackIDs = model.tracks!.entries.map(\.id)
        try await select("track-table", [0, 2])
        try await NativeSmoke.drive("double-click-row", "track-table", "1,0.35")
        try await wait("clicked filtered track plays", model) { !model.busy && model.snapshot?.queue_length == 1 && model.snapshot?.transport == "playing" && model.snapshot?.now_playing?.title == "Freedom" }
        let current = model.snapshot!.current_id
        let queueRevision = model.snapshot!.queue_revision
        cases.append("native-filtered-multiselection-plays-only-double-clicked-track")

        try await press("toggle-playlists")
        try await select("playlist-list", [0])
        try await wait("saved fixture", model) { model.playlistReady && model.currentPlaylist?.name == "Filter Fixture" }
        var expected = try await allPlaylist(model, total: 113)
        let anchor = expected.first { $0.metadata.title == "Page 101" }!.id
        try await entryFilter("album:Sessions", total: 105, model)
        let moving = [model.playlistEntries!.entries[1].id, model.playlistEntries!.entries[3].id]
        let revision = model.playlistEntries!.revision
        try await select("playlist-entry-table", [1, 3])
        try await wait("filtered drag selection", model) { model.selectedPlaylistEntries == Set(moving) }
        try await drag("playlist-entry-table", row: 1, steps: [
            ["id": "playlist-entries-next", "hold": 1.1],
            ["id": "playlist-entry-table", "row": 1, "edge": "before"]], capture: "filtered-reorder-indicator")
        try await wait("filtered cross-page reorder", model) { !model.busy && model.playlistReady && model.playlistEntries?.revision == revision + 1 }
        let selectedRows = expected.filter { moving.contains($0.id) }
        expected.removeAll { moving.contains($0.id) }
        expected.insert(contentsOf: selectedRows, at: expected.firstIndex { $0.id == anchor }!)
        try check(try await allPlaylist(model, total: 113) == expected, "Filtered move changed hidden-row order or used an unfiltered page index")
        try check(model.snapshot?.queue_revision == queueRevision && model.snapshot?.current_id == current && model.snapshot?.transport == "playing", "Filtering or saved reordering changed playback")
        cases.append("native-filtered-cross-page-drag-keeps-hidden-occurrences-and-playback")

        try await entryFilter("album:Sessions", total: 105, model)
        let insertionAnchor = model.playlistEntries!.entries[1].id
        let insertRevision = model.playlistEntries!.revision
        try await press("toggle-playlists")
        try await wait("library source restored", model) { !model.showPlaylists && model.libraryReady && model.tracks?.entries.map(\.id) == copiedTrackIDs }
        try await select("track-table", [0, 1, 2])
        try await wait("copy selection", model) { model.selectedTracks.count == 3 }
        try await drag("track-table", row: 0, steps: [
            ["id": "toggle-playlists", "hold": 1.1],
            ["id": "playlist-entry-table", "row": 1, "edge": "before"]])
        try await wait("hidden inserted tracks acknowledged", model) { !model.busy && model.playlistReady && model.playlistEntries?.revision == insertRevision + 1 && model.currentPlaylist?.entry_count == 116 }
        try check(model.playlistEntryFilter == "album:Sessions" && model.playlistEntries?.total == 105, "Copy changed the filter or exposed nonmatching rows")
        let insertedFull = try await allPlaylist(model, total: 116)
        let existingIDs = Set(expected.map(\.id))
        let additions = insertedFull.filter { !existingIDs.contains($0.id) }
        try check(additions.map(\.track_id) == copiedTrackIDs, "Filtered insert changed copy order or duplicated extra tracks")
        expected.insert(contentsOf: additions, at: expected.firstIndex { $0.id == insertionAnchor }!)
        try check(insertedFull == expected, "Filtered insert used the wrong full-playlist occurrence anchor")
        cases.append("native-library-copy-into-filtered-playlist-preserves-hidden-insertion-order")

        try await entryFilter("album:Sessions", total: 105, model)
        let removed = Set([model.playlistEntries!.entries[0].id, model.playlistEntries!.entries[2].id])
        try await select("playlist-entry-table", [0, 2])
        try await wait("filtered remove selection", model) { model.selectedPlaylistEntries == removed }
        try await press("playlist-remove")
        try await wait("filtered removal", model) { !model.busy && model.playlistReady && model.playlistEntries?.total == 103 }
        expected.removeAll { removed.contains($0.id) }
        try check(try await allPlaylist(model, total: 114) == expected, "Filtered removal touched unselected or hidden occurrences")
        cases.append("native-filtered-removal-removes-only-selected-visible-occurrences")
        try await value("playlist-entry-filter", "artist:[\"Miles Davis\",]")
        try await wait("saved syntax error", model) { !model.playlistEntriesLoading && model.playlistEntryFilterError != nil && !model.playlistReady }
        try await NativeSmoke.drive("exists", "playlist-entry-filter-error")
        try check(model.playlistEntries == nil && model.selectedPlaylistEntries.isEmpty, "Saved syntax error left stale actionable rows")
        try capture("playlist-filter-error")
        try await entryFilter("album:Sessions", total: 103, model)
        try capture("filtered-playlist")
        try check(model.snapshot?.queue_revision == queueRevision && model.snapshot?.queue_length == 1 && model.snapshot?.current_id == current && model.snapshot?.transport == "playing" && model.snapshot?.underruns == 0, "Filter editing interrupted the independent playback queue")
        cases.append("native-saved-filter-syntax-recovery-without-playback-mutation")
    }

    private static func library(_ query: String, titles: [String], _ model: PlayerModel) async throws {
        try await value("library-filter", query)
        try await wait("library result: \(query)", model) { model.libraryReady && model.filter == query && model.tracks?.entries.map(\.metadata.title) == titles }
    }
    private static func entryFilter(_ query: String, total: Int, _ model: PlayerModel) async throws {
        try await value("playlist-entry-filter", query)
        try await wait("saved result: \(query)", model) { model.playlistReady && model.playlistEntryFilter == query && model.playlistEntries?.total == total && model.playlistEntries?.offset == 0 }
    }
    private static func allPlaylist(_ model: PlayerModel, total: Int) async throws -> [PlaylistEntry] {
        if !model.playlistEntryFilter.isEmpty { try await entryFilter("", total: total, model) }
        if model.playlistEntries?.offset != 0 {
            try await press("playlist-entries-previous")
            try await wait("first saved page", model) { model.playlistReady && model.playlistEntries?.offset == 0 }
        }
        var entries = model.playlistEntries!.entries
        try await press("playlist-entries-next")
        try await wait("last saved page", model) { model.playlistReady && model.playlistEntries?.offset == 100 }
        entries += model.playlistEntries!.entries
        try check(entries.count == total, "Saved fixture collection was incomplete")
        try await press("playlist-entries-previous")
        try await wait("restore saved page", model) { model.playlistReady && model.playlistEntries?.offset == 0 }
        return entries
    }
    private static func drag(_ id: String, row: Int, steps: [[String: Any]], capture name: String? = nil) async throws {
        var plan: [String: Any] = ["row": row, "steps": steps]
        if let name, let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) {
            plan["capture_window"] = window.windowNumber
            plan["capture_path"] = output.appendingPathComponent(name + ".png").path
        }
        let data = try JSONSerialization.data(withJSONObject: plan)
        try await NativeSmoke.drive("drag-rows", id, String(decoding: data, as: UTF8.self))
    }
    private static func value(_ id: String, _ text: String) async throws { try await NativeSmoke.drive("value", id, text) }
    private static func press(_ id: String) async throws { try await NativeSmoke.drive("press", id) }
    private static func select(_ id: String, _ rows: [Int]) async throws { try await NativeSmoke.drive("select", id, rows.map(String.init).joined(separator: ",")) }
    private static func wait(_ label: String, _ model: PlayerModel, until predicate: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(20)
        while !predicate() {
            if Date() >= deadline { throw Failure(message: "Timed out: \(label); model=\(model.message ?? "none"); library=\(model.libraryFilterError ?? "none"); playlist=\(model.playlistEntryFilterError ?? model.playlistError ?? "none")") }
            try await Task.sleep(for: .milliseconds(40))
        }
        try await Task.sleep(for: .milliseconds(150))
    }
    private static func check(_ condition: Bool, _ message: String) throws { if !condition { throw Failure(message: message) } }
    private static func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else { throw Failure(message: "Native window unavailable") }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases,
            "automation": "Real AX text inputs and CGEvent clicks/drags; model observation only; isolated real tagged FLACs and clock-paced capture."]
        report["library_filter"] = model.filter
        report["library_ready"] = model.libraryReady
        report["library_total"] = model.tracks?.total
        if let error { report["error"] = error }
        if let state = model.snapshot { report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state)) }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]).write(to: output.appendingPathComponent("report.json"))
        print("FILTER_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
