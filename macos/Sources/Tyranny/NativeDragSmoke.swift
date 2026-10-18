#if DEBUG
import AppKit
import Foundation

/// Exercises real native dragging; model state is observed, never used to issue mutations.
@MainActor
enum NativeDragSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-drag-smoke")
    private static var cases: [String] = []

    static func startIfRequested(_ model: PlayerModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--drag-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("initial library", model) { model.connected && model.tracks?.entries.count == 100 && model.playlists != nil }
                try await NativeSmoke.drive("activate")
                try await exercise(model)
                try finish(model, error: nil)
            } catch {
                try? capture("failure")
                try? finish(model, error: error.localizedDescription)
            }
            for window in NSApp.windows {
                if let sheet = window.attachedSheet { window.endSheet(sheet) }
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel) async throws {
        try check(model.snapshot?.queue_length == 0 && model.playlists?.total == 0 && model.tracks?.total == 105,
                  "Drag fixture requires 105 indexed tracks, empty queue and empty playlist catalog")
        let firstTracks = model.tracks!.entries
        try await select("track-table", Array(0..<100))
        try await wait("100 source rows selected", model) { model.selectedTracks.count == 100 }
        try await drag("track-table", row: 0, steps: [hover("toggle-queue"), target("queue-table")])
        try await wait("copy 100 rows into empty queue", model) { !model.busy && model.showQueue && model.snapshot?.queue_length == 100 && model.queue?.entries.count == 100 }
        try check(model.queue!.entries.compactMap(\.track_id) == firstTracks.map(\.id), "Empty queue drop changed source order")
        try check(model.snapshot?.transport != "playing", "Library drop unexpectedly started playback")
        cases.append("mouse-library-multiselect-copy-to-empty-queue-with-spring-navigation")

        try await NativeSmoke.drive("click-control", "toggle-queue")
        try await wait("navigation remains mouse-clickable", model) { !model.showQueue && !model.showPlaylists }
        try await press("tracks-next")
        try await wait("last library page", model) { model.tracks?.offset == 100 && model.tracks?.entries.count == 5 }
        let lastTracks = model.tracks!.entries
        try await select("track-table", [0, 1, 2])
        try await wait("last-page selection", model) { model.selectedTracks.count == 3 }
        try await drag("track-table", row: 0, steps: [hover("toggle-queue"), target("queue-table", row: 1, edge: "before")], capture: "queue-insertion-indicator")
        try await wait("copy into queue interior", model) { !model.busy && model.snapshot?.queue_length == 103 && model.queue?.total == 103 }
        var expectedQueue = try await collectQueue(model)
        let expectedTracks = [firstTracks[0].id] + lastTracks.prefix(3).map(\.id) + firstTracks.dropFirst().map(\.id)
        try check(expectedQueue.compactMap(\.track_id) == expectedTracks, "Interior queue insertion appended or reordered tracks")
        try check(Set(expectedQueue.map(\.id)).count == 103, "Queue insertion reused occurrence IDs")
        cases.append("mouse-library-copy-inserts-before-exact-queue-occurrence")

        try await NativeSmoke.drive("double-click-row", "queue-table", "0,0.4")
        try await wait("queue playing before reorder", model) { model.snapshot?.transport == "playing" && model.snapshot?.current_id == expectedQueue[0].id }
        let current = model.snapshot!.current_id
        let position = model.snapshot!.position_frames
        try await select("queue-table", [1, 3])
        try await wait("noncontiguous queue selection", model) { model.selectedQueue.count == 2 }
        let revision = model.snapshot!.queue_revision
        let movedQueue = moved(expectedQueue, indexes: [1, 3], before: expectedQueue[7].id)
        try await drag("queue-table", row: 1, steps: [target("queue-table", row: 7, edge: "before")], capture: "queue-reorder-indicator")
        try await wait("atomic queue reorder", model) { !model.busy && model.snapshot?.queue_revision == revision + 1 && model.queue?.revision == revision + 1 }
        expectedQueue = movedQueue
        try check(try await collectQueue(model) == expectedQueue, "Queue batch lost source order or occurrence identity")
        try check(model.snapshot?.current_id == current && model.snapshot?.transport == "playing" && model.snapshot!.position_frames >= position,
                  "Queue reorder restarted or interrupted current playback")
        cases.append("mouse-noncontiguous-queue-reorder-preserves-live-playback")

        try await select("queue-table", [1, 3])
        try await wait("cross-page queue source", model) { model.selectedQueue.count == 2 }
        let crossQueueRevision = model.snapshot!.queue_revision
        let crossQueue = moved(expectedQueue, indexes: [1, 3], before: expectedQueue[101].id)
        try await drag("queue-table", row: 1, steps: [hover("queue-next"), target("queue-table", row: 1, edge: "before")])
        try await wait("cross-page queue drop", model) { !model.busy && model.snapshot?.queue_revision == crossQueueRevision + 1 && model.queue?.revision == crossQueueRevision + 1 }
        expectedQueue = crossQueue
        try check(try await collectQueue(model) == expectedQueue, "Cross-page queue drop used a local index as a global anchor")
        try check(model.snapshot?.current_id == current && model.snapshot?.transport == "playing" && model.snapshot!.position_frames >= position,
                  "Cross-page queue reorder interrupted playback")
        cases.append("mouse-queue-hover-paging-keeps-captured-source-and-global-anchor")

        try await select("queue-table", [1, 3])
        try await wait("cancelled drag source", model) { model.selectedQueue.count == 2 }
        let cancellationRevision = model.snapshot!.queue_revision
        try await drag("queue-table", row: 1, steps: [target("queue-table", row: 0, edge: "before")], cancel: true)
        try check(try await collectQueue(model) == expectedQueue && model.snapshot?.queue_revision == cancellationRevision,
                  "Escape cancellation applied a queue edit")
        try check(model.snapshot?.current_id == current && model.snapshot?.transport == "playing",
                  "Escape cancellation changed playback")
        cases.append("mouse-escape-cancels-drag-without-queue-edit")

        try await press("toggle-playlists")
        try await press("playlist-create")
        try await NativeSmoke.drive("value", "playlist-name", "Drag Destination")
        try await press("playlist-name-save")
        try await wait("empty saved destination", model) { !model.busy && model.playlistReady && model.currentPlaylist?.name == "Drag Destination" }
        let playlistID = model.currentPlaylist!.id
        let catalogTarget = "playlist-drop-\(playlistID)"
        let queueRevision = model.snapshot!.queue_revision
        try await press("toggle-playlists")
        try await press("tracks-previous")
        try await wait("library first page for saved drop", model) { model.tracks?.offset == 0 && model.tracks?.entries.count == 100 }
        try await select("track-table", [0, 2, 4])
        try await wait("saved-drop library selection", model) { model.selectedTracks.count == 3 }
        try await drag("track-table", row: 0, steps: [hover("toggle-playlists"), target(catalogTarget)])
        try await wait("copy into empty saved playlist", model) { !model.busy && model.playlistReady && model.playlistEntries?.total == 3 }
        try check(model.playlistEntries!.entries.map(\.track_id) == [firstTracks[0].id, firstTracks[2].id, firstTracks[4].id], "Catalog drop did not preserve selected order")
        cases.append("mouse-library-copy-to-empty-saved-playlist-catalog-target")

        try await press("toggle-playlists")
        try await select("track-table", [1, 3])
        try await wait("interior saved insertion source", model) { model.selectedTracks.count == 2 }
        try await drag("track-table", row: 1, steps: [hover("toggle-playlists"), hover(catalogTarget), target("playlist-entry-table", row: 1, edge: "before")], capture: "playlist-insertion-indicator")
        try await wait("saved interior insertion", model) { !model.busy && model.playlistReady && model.playlistEntries?.total == 5 }
        try check(model.playlistEntries!.entries.map(\.track_id) == [0, 1, 3, 2, 4].map { firstTracks[$0].id }, "Saved insertion appended instead of inserting at the row")
        cases.append("mouse-library-copy-to-saved-playlist-insertion-point")

        var expectedEntries = model.playlistEntries!.entries
        try await select("playlist-entry-table", [1, 3])
        try await wait("saved reorder selection", model) { model.selectedPlaylistEntries.count == 2 }
        let playlistRevision = model.playlistEntries!.revision
        let reordered = moved(expectedEntries, indexes: [1, 3], before: expectedEntries[0].id)
        try await drag("playlist-entry-table", row: 1, steps: [target("playlist-entry-table", row: 0, edge: "before")], capture: "playlist-reorder-indicator")
        try await wait("saved batch reorder", model) { !model.busy && model.playlistReady && model.playlistEntries?.revision == playlistRevision + 1 }
        expectedEntries = reordered
        try check(model.playlistEntries!.entries == expectedEntries, "Saved batch reorder changed occurrence identities or source order")
        cases.append("mouse-noncontiguous-saved-playlist-reorder")

        try await press("toggle-playlists")
        try await select("track-table", Array(0..<100))
        try await wait("100-row saved insertion source", model) { model.selectedTracks.count == 100 }
        try await drag("track-table", row: 0, steps: [hover("toggle-playlists"), target(catalogTarget)])
        try await wait("saved playlist spans pages", model) { !model.busy && model.playlistReady && model.playlistEntries?.total == 105 }
        expectedEntries = try await collectPlaylist(model)
        try check(expectedEntries.prefix(5).map(\.id) == reordered.map(\.id), "Append replaced existing saved occurrences")
        try check(expectedEntries.dropFirst(5).map(\.track_id) == firstTracks.map(\.id) && Set(expectedEntries.map(\.id)).count == 105,
                  "Saved append deduplicated tracks or reused occurrence identities")
        try await select("playlist-entry-table", [1, 3])
        try await wait("cross-page saved selection", model) { model.selectedPlaylistEntries.count == 2 }
        let crossPlaylistRevision = model.playlistEntries!.revision
        let crossPlaylist = moved(expectedEntries, indexes: [1, 3], before: expectedEntries[101].id)
        try await drag("playlist-entry-table", row: 1, steps: [hover("playlist-entries-next"), target("playlist-entry-table", row: 1, edge: "before")])
        try await wait("cross-page saved drop", model) { !model.busy && model.playlistReady && model.playlistEntries?.revision == crossPlaylistRevision + 1 }
        try check(try await collectPlaylist(model) == crossPlaylist, "Cross-page saved move corrupted duplicate occurrences or global order")
        try check(model.snapshot?.queue_revision == queueRevision && model.snapshot?.queue_length == 103,
                  "Saved editing changed the playback queue")
        try capture("saved-playlist-after-drag")
        cases.append("mouse-saved-hover-paging-and-duplicate-occurrences-preserve-queue")
    }

    private static func moved<T: Identifiable>(_ rows: [T], indexes: [Int], before: T.ID) -> [T] {
        let selected = Set(indexes)
        let moving = rows.enumerated().filter { selected.contains($0.offset) }.map(\.element)
        var remaining = rows.enumerated().filter { !selected.contains($0.offset) }.map(\.element)
        remaining.insert(contentsOf: moving, at: remaining.firstIndex { $0.id == before }!)
        return remaining
    }
    private static func collectQueue(_ model: PlayerModel) async throws -> [QueueItem] {
        if model.queue?.offset != 0 {
            try await press("queue-previous")
            try await wait("queue first page", model) { model.queue?.offset == 0 }
        }
        var entries = model.queue!.entries
        try await press("queue-next")
        try await wait("queue final page", model) { model.queue?.offset == 100 }
        entries += model.queue!.entries
        try check(entries.count == model.snapshot?.queue_length, "Queue page collection incomplete")
        try await press("queue-previous")
        try await wait("restore queue first page", model) { model.queue?.offset == 0 }
        return entries
    }
    private static func collectPlaylist(_ model: PlayerModel) async throws -> [PlaylistEntry] {
        if model.playlistEntries?.offset != 0 {
            try await press("playlist-entries-previous")
            try await wait("saved first page", model) { model.playlistReady && model.playlistEntries?.offset == 0 }
        }
        var entries = model.playlistEntries!.entries
        try await press("playlist-entries-next")
        try await wait("saved final page", model) { model.playlistReady && model.playlistEntries?.offset == 100 }
        entries += model.playlistEntries!.entries
        try check(entries.count == model.playlistEntries?.total, "Saved page collection incomplete")
        try await press("playlist-entries-previous")
        try await wait("restore saved first page", model) { model.playlistReady && model.playlistEntries?.offset == 0 }
        return entries
    }
    private static func target(_ id: String, row: Int? = nil, edge: String? = nil) -> [String: Any] {
        var result: [String: Any] = ["id": id]
        if let row { result["row"] = row }
        if let edge { result["edge"] = edge }
        return result
    }
    private static func hover(_ id: String) -> [String: Any] { ["id": id, "hold": 1.1] }
    private static func drag(_ id: String, row: Int, steps: [[String: Any]], capture name: String? = nil, cancel: Bool = false) async throws {
        var plan: [String: Any] = ["row": row, "steps": steps, "cancel": cancel]
        if let name, let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) {
            plan["capture_window"] = window.windowNumber
            plan["capture_path"] = output.appendingPathComponent(name + ".png").path
        }
        let data = try JSONSerialization.data(withJSONObject: plan)
        try await NativeSmoke.drive("drag-rows", id, String(decoding: data, as: UTF8.self))
    }
    private static func press(_ id: String) async throws { try await NativeSmoke.drive("press", id) }
    private static func select(_ id: String, _ rows: [Int]) async throws {
        try await NativeSmoke.drive("select", id, rows.map(String.init).joined(separator: ","))
    }
    private static func wait(_ label: String, _ model: PlayerModel, until predicate: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(20)
        while !predicate() {
            if Date() >= deadline { throw Failure(message: "Timed out: \(label); model=\(model.message ?? "none"); playlist=\(model.playlistError ?? "none")") }
            try await Task.sleep(for: .milliseconds(50))
        }
        try await Task.sleep(for: .milliseconds(200))
    }
    private static func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }
    private static func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else { throw Failure(message: "Native window unavailable") }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        try check(process.terminationStatus == 0, "Native capture failed")
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases,
            "automation": "Real CGEvent multi-row drags with native insertion indicators, spring navigation and paging; AX used only to select and locate controls. Isolated clock-paced capture, no device-output claim."]
        if let error { report["error"] = error }
        if let snapshot = model.snapshot { report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(snapshot)) }
        if let playlist = model.currentPlaylist { report["playlist"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(playlist)) }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]).write(to: output.appendingPathComponent("report.json"))
        print("DRAG_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
