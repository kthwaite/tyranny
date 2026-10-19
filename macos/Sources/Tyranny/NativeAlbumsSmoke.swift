#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for the Albums grid (`--albums-smoke-output`). The gate prepares
/// the 1,129-track songs library on the server. Every control is driven through the external
/// Accessibility driver; server queries here only read the expected values.
@MainActor
enum NativeAlbumsSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-albums-smoke")
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]
    private static let collection = AlbumCollection.identifier

    /// The shown grid, set by its view for observation.
    static weak var grid: AlbumGridModel?

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--albums-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("connection", model) {
                    model.connected && model.snapshot?.library.tracks == 1129
                        && model.snapshot?.library.scan.phase == "complete"
                }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                try await exercise(model, navigation)
                try finish(model, error: nil)
            } catch {
                try? capture("albums-failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try await drive("click-control", Destination.albums.identifier)
        try await wait("albums destination", model) { navigation.destination == .albums && grid != nil }
        guard let grid else { throw Failure(message: "Albums grid missing") }
        try await drive("exists", "main-albums")
        try await drive("exists", collection)
        try await drive("exists", "album-sort")
        try await drive("exists", "album-size")
        try await wait("first window", model) { grid.cache.revision != nil && grid.cache.loadedRow(at: 0) != nil }
        let first = try await serverPage(model, grid, offset: 0, limit: 1)
        try check(UInt64(grid.cache.total) == first.total, "Grid total \(grid.cache.total) differs from the server's \(first.total)")
        let total = Int(first.total)
        try await drive("expect-text", "list-count", "\(DisplayFormat.count(total)) albums")
        observations["albums"] = total
        cases.append("native-albums-grid-shows-server-album-count")

        // Scroll to the end through Accessibility; the last album matches the server's.
        try await drive("scroll-fraction", collection, "1")
        try await wait("last window", model) { grid.cache.loadedRow(at: total - 1) != nil }
        guard let last = try await serverPage(model, grid, offset: total - 1, limit: 1).entries.first else {
            throw Failure(message: "Server has no last album")
        }
        try check(grid.cache.loadedRow(at: total - 1)?.id == last.id, "Last album differs from the server's")
        try await drive("expect-text", "album-cell-\(last.id)",
                        "\(last.title.isEmpty ? "Unknown Album" : last.title)\n\(DisplayFormat.albumSubtitle(last))")
        observations["last_album"] = ["id": last.id, "title": last.title]
        cases.append("native-albums-scroll-to-end-shows-server-last-album")

        // Open the last album: the panel lists exactly the server's track count.
        try await drive("click-control", "album-cell-\(last.id)")
        try await wait("panel open", model) {
            grid.expansion?.albumID == last.id && grid.expansion?.isLoaded == true && grid.selectedID == last.id
        }
        try await drive("exists", "album-expansion")
        let summaries: AlbumSummariesResponse = try await model.send("album_summaries", [
            "album_ids": .uints([last.id]), "revision": .null])
        guard let summary = summaries.entries.first else { throw Failure(message: "No album summary") }
        let shown = grid.expansion?.tracks.count ?? -1
        try check(UInt64(shown) == summary.track_count, "Panel shows \(shown) tracks; album_summaries says \(summary.track_count)")
        try await drive("expect-text", "album-expansion-tracks", String(summary.track_count))
        try await drive("expect-text", "album-expansion-facts", DisplayFormat.albumExpansionFacts(summary))
        observations["expanded"] = ["id": last.id, "tracks": shown, "facts": DisplayFormat.albumExpansionFacts(summary)]
        cases.append("native-albums-panel-lists-album-summary-track-count")
        try await Task.sleep(for: .milliseconds(400))
        try capture("albums")

        // Play: the queue holds the album as one run with an album source.
        try await drive("press", "album-play")
        try await wait("album queued", model, seconds: 20) {
            model.snapshot?.queue_length == Int(summary.track_count) && model.snapshot?.transport == "playing"
                && !model.busy
        }
        let queue: Page<QueueItem> = try await model.send("queue_page", [
            "revision": .null, "offset": .int(0), "limit": .int(500)])
        let expected = QueueSource(kind: .album, name: summary.title)
        try check(queue.entries.allSatisfy { $0.source == expected }, "Queued entries lack the album source")
        try check(Set(queue.entries.compactMap(\.run_id)).count == 1, "Album entries are not one run")
        // The last album may be longer than one queue page: compare the page against its prefix.
        let tracks = grid.expansion?.tracks.map(\.track.id) ?? []
        try check(queue.total == Int(summary.track_count), "Queue holds \(queue.total) entries")
        try check(queue.entries.map(\.track_id) == Array(tracks.prefix(queue.entries.count)),
                  "Queue order differs from the album's tracks")
        observations["queued"] = ["total": queue.total, "source": ["kind": "album", "name": summary.title]]
        cases.append("native-albums-play-queues-album-run")

        // Escape closes the panel.
        try await drive("key", collection, "escape")
        try await wait("panel closed", model) { grid.expansion == nil }
        cases.append("native-albums-escape-closes-panel")
    }

    private static func serverPage(_ model: PlayerModel, _ grid: AlbumGridModel, offset: Int,
                                   limit: Int) async throws -> ListAlbumsResponse {
        try await model.send("list_albums", [
            "list": try JSONValue.encoding(ListDescription(source: .library)), "sort": .string(grid.sort.rawValue),
            "descending": .bool(grid.descending), "revision": .null, "offset": .int(offset), "limit": .int(limit)])
    }

    private static func drive(_ action: String, _ identifier: String, _ value: String? = nil) async throws {
        try await NativeSmoke.drive(action, identifier, value)
    }
    private static func wait(_ label: String, _ model: PlayerModel, seconds: Double = 15,
                             until condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            if Date() >= deadline {
                throw Failure(message: "Timed out waiting for \(label); message=\(model.message ?? "none")")
            }
            try await Task.sleep(for: .milliseconds(40))
        }
        try await Task.sleep(for: .milliseconds(150))
    }
    private static func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }
    private static func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else {
            throw Failure(message: "Native window unavailable")
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        if process.terminationStatus != 0, let frame = window.contentView?.superview,
           let bitmap = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) {
            // Without Screen Recording permission, keep a self-rendered image for review.
            // It is not a composited screenshot and is named so.
            frame.cacheDisplay(in: frame.bounds, to: bitmap)
            try? bitmap.representation(using: .png, properties: [:])?
                .write(to: output.appendingPathComponent(name + "-self-rendered.png"))
        }
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases, "observations": observations,
            "automation": "External Accessibility source-list click, grid scroll, a CGEvent click on the last cover, the Play button press and an Escape key; server reads only for expected values.",
            "scenario": "albums"]
        if let error { report["error"] = error }
        if let state = model.snapshot {
            report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state))
        }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("ALBUMS_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
