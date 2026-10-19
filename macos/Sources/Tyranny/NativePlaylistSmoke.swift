#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in observer: interaction uses external Accessibility actions and real mouse events.
@MainActor
enum NativePlaylistSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var cases: [String] = []
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-playlist-smoke")
    private static var retainedID: UInt64?
    private static var retainedEntries: [PlaylistEntry] = []

    static func startIfRequested(_ model: PlayerModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--playlist-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("connection and bounded catalog") { model.connected && model.tracks != nil && model.playlists != nil && !model.playlistsLoading }
                try await NativeSmoke.drive("activate")
                if args.contains("--playlist-smoke-double-click-only") {
                    try await press("toggle-playlists")
                    try await select("playlist-list", [0])
                    try await wait("saved playlist loaded") { model.playlistReady }
                    try await exerciseDoubleClick(model)
                } else if args.contains("--playlist-smoke-relaunch") {
                    try await relaunch(model)
                } else {
                    try await exercise(model)
                }
                try finish(model, passed: true, error: nil)
            } catch {
                try? capture("failure")
                try? finish(model, passed: false, error: error.localizedDescription)
            }
            for window in NSApp.windows {
                if let sheet = window.attachedSheet { window.endSheet(sheet) }
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel) async throws {
        try check(model.snapshot?.queue_length == 0 && model.playlists?.total == 0, "Initial gate requires empty queue and catalog")
        try check(model.tracks?.total == 105 && model.tracks?.entries.count == 100, "Initial gate requires 105 preindexed real tracks")
        try await select("track-table", Array(0..<100))
        try await wait("100 selected tracks") { model.selectedTracks.count == 100 }
        try await press("add-to-playlist")
        try await NativeSmoke.drive("value", "playlist-picker-filter", "no-matching-saved-playlist")
        try await NativeSmoke.drive("value", "playlist-picker-name", "Native Retained Playlist")
        try capture("create-from-picker")
        try await press("playlist-picker-create-add")
        try await wait("created and populated from picker") {
            !model.busy && !model.playlistsLoading && model.currentPlaylist?.entry_count == 100
        }
        retainedID = model.selectedPlaylist
        try check(retainedID != nil && !model.showPlaylists && model.selectedTracks.count == 100,
                  "Picker creation lost library navigation or selection")
        cases.append("AX-create-and-add-100-tracks-without-leaving-library")
        try await press("toggle-playlists")
        try await wait("playlist pane") { model.showPlaylists }
        try await NativeSmoke.drive("value", "playlist-filter", "no-matching-saved-playlist")
        try await wait("empty catalog filter") { !model.playlistsLoading && model.playlists?.total == 0 }
        try capture("filtered-playlists")
        try await NativeSmoke.drive("value", "playlist-filter", "")
        try await wait("catalog filter reset") { !model.playlistsLoading && model.playlists?.total == 1 }
        cases.append("AX-bounded-catalog-filter-and-empty-result")

        try await press("toggle-playlists")
        try await press("tracks-next")
        try await wait("last library page") { model.tracks?.offset == 100 && model.tracks?.entries.count == 5 }
        try await select("track-table", Array(0..<5))
        try await wait("five selected tracks") { model.selectedTracks.count == 5 }
        try await press("add-to-playlist")
        try await select("playlist-picker-list", [0])
        try await wait("existing picker destination") { model.currentPlaylist?.entry_count == 100 }
        try await NativeSmoke.drive("value", "playlist-picker-name", "Native Retained Playlist")
        try await press("playlist-picker-create-add")
        try await wait("duplicate creation rejected") { !model.busy }
        try check(model.playlists?.total == 1 && model.currentPlaylist?.entry_count == 100
                  && model.selectedTracks.count == 5 && !model.showPlaylists,
                  "Rejected creation changed playlist contents or selection")
        try capture("picker-create-error")
        cases.append("AX-duplicate-create-error-stays-in-picker-and-preserves-selection")
        try await select("playlist-picker-list", [0])
        try await press("playlist-picker-add")
        try await wait("existing destination after rejected creation") {
            !model.busy && !model.playlistsLoading && model.currentPlaylist?.entry_count == 105
        }
        cases.append("AX-library-multiselection-paged-picker-add-105-tracks")

        try await press("toggle-playlists")
        try await select("playlist-list", [0])
        try await wait("first entry page") { model.playlistReady && model.playlistEntries?.offset == 0 && model.playlistEntries?.entries.count == 100 }
        let originalIDs = model.playlistEntries!.entries.map(\.id)
        try await select("playlist-entry-table", [99])
        try await wait("boundary selection") { model.selectedPlaylistEntries == [originalIDs[99]] }
        try await press("playlist-down")
        try await wait("move down across page boundary") {
            !model.busy && model.playlistReady && model.playlistEntries?.offset == 100
                && model.playlistEntries?.entries.first?.id == originalIDs[99]
        }
        try capture("page-boundary-move")
        try await select("playlist-entry-table", [0])
        try await wait("moved entry selected") { model.selectedPlaylistEntries == [originalIDs[99]] }
        try await press("playlist-up")
        try await wait("move up across page boundary") {
            !model.busy && model.playlistReady && model.playlistEntries?.offset == 0
                && model.playlistEntries?.entries.map(\.id) == originalIDs
        }
        cases.append("AX-single-occurrence-move-across-both-page-boundaries")

        try await press("playlist-rename")
        try await NativeSmoke.drive("value", "playlist-name", "Native Renamed Playlist")
        try await press("playlist-name-save")
        try await wait("rename persisted") { !model.busy && model.playlistReady && model.currentPlaylist?.name == "Native Renamed Playlist" }
        try await select("playlist-entry-table", [0, 1])
        try await wait("entry multiselection") { model.selectedPlaylistEntries.count == 2 }
        try await press("playlist-remove")
        try await wait("atomic multiselect removal") { !model.busy && model.playlistReady && model.playlistEntries?.total == 103 }
        try check(model.playlistEntries?.entries.first?.id == originalIDs[2], "Removal did not preserve occurrence order")
        cases.append("AX-rename-and-multiselect-remove")
        try capture("saved-playlist")

        try await press("playlist-enqueue")
        try await wait("whole playlist enqueue") { !model.busy && model.snapshot?.queue_length == 103 }
        let firstQueued = model.snapshot?.current_id
        try await press("playlist-play")
        try await wait("whole playlist append and play") {
            !model.busy && model.snapshot?.queue_length == 206 && model.snapshot?.transport == "playing"
                && model.snapshot?.current_id != firstQueued
        }
        try check(model.currentPlaylist?.entry_count == 103, "Enqueue mutated saved entries")
        cases.append("AX-whole-playlist-enqueue-and-append-play-with-independent-occurrences")
        try capture("playlist-playing")

        try await create(model, name: "Temporary Playlist")
        try await press("playlist-delete")
        try await NativeSmoke.drive("press-title", "Delete Playlist")
        try await wait("confirmed playlist deletion") { !model.busy && !model.playlistsLoading && model.playlists?.total == 1 }
        try check(model.snapshot?.queue_length == 206 && model.snapshot?.transport == "playing", "Deleting saved playlist changed queue or playback")
        cases.append("AX-confirmed-delete-preserves-queue-and-playback")
        try await select("playlist-list", [0])
        try await wait("retained playlist") { model.playlistReady && model.selectedPlaylist == retainedID }
        try await collectRetained(model)
        try capture("retained-for-restart")
        try await exerciseDoubleClick(model)
    }

    private static func exerciseDoubleClick(_ model: PlayerModel) async throws {
        let originalLength = model.snapshot!.queue_length
        let originalCurrent = model.snapshot!.current_id
        try await press("toggle-playlists")
        if model.tracks?.offset != 0 {
            try await press("tracks-previous")
            try await wait("first library page before mouse scenario") { model.tracks?.offset == 0 && model.tracks?.entries.count == 100 }
        }
        try await press("tracks-next")
        try await wait("library for mouse activation") { !model.showPlaylists && model.tracks?.entries.count == 5 }
        let tracks = model.tracks!.entries
        try await select("track-table", [1])
        try await wait("different row selected before single click") { model.selectedTracks == [tracks[1].id] }
        try await NativeSmoke.drive("click-row", "track-table", "0,0.85")
        try await wait("single click selects only") { model.selectedTracks == [tracks[0].id] }
        try check(model.selectedTracks == [tracks[0].id], "Single click did not retain the clicked selection")
        try check(model.snapshot?.queue_length == originalLength && model.snapshot?.current_id == originalCurrent,
                  "Single click changed playback")
        try await select("track-table", [0, 1])
        try await wait("multiselection before activation") { model.selectedTracks.count == 2 }
        try await NativeSmoke.drive("double-click-row", "track-table", "1,0.85")
        try await wait("double click plays clicked library track only") {
            !model.busy && model.snapshot?.queue_length == originalLength + 1
                && model.snapshot?.transport == "playing"
                && model.snapshot?.now_playing?.title == tracks[1].metadata.title
        }
        cases.append("mouse-single-click-selects-double-click-plays-exact-library-row")
        let queueID = model.snapshot?.current_id

        try await press("toggle-playlists")
        try await wait("saved entries for mouse activation") { model.showPlaylists && model.playlistReady }
        let entries = model.playlistEntries!
        try await select("playlist-entry-table", [0, 1])
        try await NativeSmoke.drive("double-click-row", "playlist-entry-table", "1,0.8")
        try await wait("double click plays single saved occurrence") {
            !model.busy && model.snapshot?.queue_length == originalLength + 2
                && model.snapshot?.transport == "playing" && model.snapshot?.current_id != queueID
                && model.snapshot?.now_playing?.title == entries.entries[1].metadata.title
        }
        try check(model.playlistEntries?.revision == entries.revision
                  && model.playlistEntries?.entries == entries.entries, "Activation edited the saved playlist")
        try capture("double-click-playback")
        cases.append("mouse-saved-playlist-double-click-plays-only-clicked-track")
    }

    private static func relaunch(_ model: PlayerModel) async throws {
        try check(model.playlists?.total == 1, "Restart must retain exactly one playlist")
        try check(model.snapshot?.transport == "paused", "Restart fixture must be paused")
        try await press("toggle-playlists")
        try await select("playlist-list", [0])
        try await wait("persisted playlist entries") { model.playlistReady && (model.playlistEntries?.total ?? 0) > 100 }
        retainedID = model.selectedPlaylist
        try await collectRetained(model)
        try capture("restart-persistence")
        cases.append("AX-relaunch-bounded-persisted-playlist-browsing-with-paused-queue")
    }

    private static func collectRetained(_ model: PlayerModel) async throws {
        guard let page = model.playlistEntries, page.offset == 0 else { throw Failure(message: "Expected first retained page") }
        retainedEntries = page.entries
        try await press("playlist-entries-next")
        try await wait("retained final page") { model.playlistReady && model.playlistEntries?.offset == 100 }
        retainedEntries += model.playlistEntries!.entries
        try check(retainedEntries.count == model.playlistEntries?.total, "Retained entry report is incomplete")
        try await press("playlist-entries-previous")
        try await wait("restore retained first page") { model.playlistReady && model.playlistEntries?.offset == 0 }
    }

    private static func create(_ model: PlayerModel, name: String) async throws {
        try await press("playlist-create")
        try await NativeSmoke.drive("value", "playlist-name", name)
        try await press("playlist-name-save")
        try await wait("created \(name)") { !model.busy && model.playlistReady && model.currentPlaylist?.name == name }
    }
    private static func press(_ id: String) async throws { try await NativeSmoke.drive("press", id) }
    private static func select(_ id: String, _ indexes: [Int]) async throws {
        try await NativeSmoke.drive("select", id, indexes.map(String.init).joined(separator: ","))
    }
    private static func wait(_ label: String, until condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(20)
        while !condition() {
            if Date() >= deadline { throw Failure(message: "Timed out waiting for \(label)") }
            try await Task.sleep(for: .milliseconds(50))
        }
        try await Task.sleep(for: .milliseconds(200))
    }
    private static func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }
    private static func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else { throw Failure(message: "Native window missing") }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }
    private static func finish(_ model: PlayerModel, passed: Bool, error: String?) throws {
        var report: [String: Any] = ["passed": passed, "cases": cases,
            "automation": "External public Accessibility actions and CGEvent mouse clicks on actual SwiftUI controls; composited native window screenshots. Capture-only, no audible/device-output claim.",
            "retained_entries": try JSONSerialization.jsonObject(with: JSONEncoder().encode(retainedEntries))]
        if let retainedID { report["retained_playlist_id"] = retainedID }
        if let snapshot = model.snapshot { report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(snapshot)) }
        if let playlist = model.currentPlaylist { report["retained_playlist"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(playlist)) }
        if let error { report["error"] = error }
        report["selected_tracks"] = model.selectedTracks.sorted()
        if let message = model.message { report["model_message"] = message }
        if let error = model.playlistError { report["playlist_error"] = error }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("PLAYLIST_SMOKE \(passed ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
