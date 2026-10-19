#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for the Songs view (`--songs-smoke-output`). The gate
/// prepares a 1,129-track library on the server. Every control is driven through the
/// external Accessibility driver; server queries here only read the expected values.
@MainActor
enum NativeSongsSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-songs-smoke")
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]
    private static let table = "track-table-songs"

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--songs-smoke-output"), index + 1 < args.count else { return }
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
                try? capture("songs-failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try check(navigation.destination == .songs, "Songs is not the first destination")
        try check(!navigation.columnBrowserVisible, "Fixture must start with the browser hidden")
        try await drive("exists", "main-songs")
        try await drive("exists", table)
        try await drive("exists", "group-control")
        try await drive("expect-enabled", "group-control", "false")
        try await wait("songs list", model) { songsList != nil }
        guard let list = songsList else { throw Failure(message: "Songs list missing") }
        try await wait("first window", model) { list.cache.total == 1129 && list.cache.loadedRow(at: 0) != nil }
        try await drive("expect-text", "list-count", "1,129 songs")
        try await wait("list totals", model) { list.totals?.tracks == 1129 }
        if let totals = list.totals {
            try await drive("expect-text", "status-totals", DisplayFormat.totals(totals))
        }
        cases.append("native-songs-continuous-table-shows-library-count-and-totals")

        // Scroll to the end through Accessibility; the last row matches the server's.
        try await drive("scroll-fraction", table, "1")
        try await wait("last window", model) { list.cache.loadedRow(at: 1128) != nil }
        let last = try await serverRow(model, list.description, offset: 1128)
        try check(list.cache.loadedRow(at: 1128)?.row_id == last.row_id, "Last row differs from the server's")
        try await drive("expect-text", "\(table)-cell-\(last.row_id)-title", last.track.title)
        let loaded = list.cache.loadedRows().count
        observations["loaded_rows_at_end"] = loaded
        observations["last_row_title"] = last.track.title
        try check(loaded <= 2000, "The cache holds \(loaded) rows; it must stay bounded")
        cases.append("native-songs-scroll-to-end-shows-server-last-row")

        // Sort by Year, descending: two header clicks.
        try await drive("click-control", "track-header-year")
        try await wait("year ascending", model) { list.sortKeys == [SortKey(field: .year)] }
        try await drive("click-control", "track-header-year")
        try await wait("year descending", model) {
            list.sortKeys == [SortKey(field: .year, descending: true)] && list.cache.loadedRow(at: 0) != nil
                && list.description.sort == [SortKey(field: .year, descending: true)]
        }
        let first = try await serverRow(model, list.description, offset: 0)
        try check(list.cache.loadedRow(at: 0)?.row_id == first.row_id, "Year-descending first row differs from the server's")
        try await drive("expect-text", "\(table)-cell-\(first.row_id)-title", first.track.title)
        observations["year_descending_first"] = ["title": first.track.title, "year": first.track.year as Any]
        cases.append("native-songs-header-sorts-year-descending")

        // Column browser: narrow to one genre.
        try await press("browser-toggle")
        try await wait("browser shown", model) {
            navigation.columnBrowserVisible && list.browser.genres.cache.loadedRow(at: 0) != nil
        }
        try await drive("exists", "column-browser")
        try await drive("exists", "browser-artists")
        try await drive("exists", "browser-albums")
        guard let genre = list.browser.genres.cache.loadedRow(at: 0) else { throw Failure(message: "No genres") }
        try await drive("select", "browser-genres", "1")
        try await wait("genre narrowing", model) {
            list.description.browser.genre_ids == [genre.id] && list.cache.loadedRow(at: 0) != nil
        }
        let narrowed = try await serverPage(model, list.description, offset: 0, limit: 1)
        try check(UInt64(list.cache.total) == narrowed.total, "Browser count differs from the server total")
        try check(narrowed.total == genre.track_count, "Genre count differs from its track count")
        try await drive("expect-text", "list-count", "\(DisplayFormat.count(narrowed.total)) of 1,129 songs")
        try await wait("narrowed albums", model) { list.browser.albums.cache.revision != nil }
        observations["genre"] = ["name": genre.name, "tracks": narrowed.total,
                                 "albums": list.browser.albums.cache.total]
        cases.append("native-songs-browser-narrows-to-one-genre")
        try await Task.sleep(for: .milliseconds(400))
        try capture("songs")

        // Select a range and drag it to Up Next.
        try await drive("scroll-fraction", table, "0")
        try await drive("select", table, "0,1,2,3,4")
        try await wait("range selected", model) { list.selectedPositions == IndexSet(integersIn: 0..<5) }
        let expected = (0..<5).compactMap { list.cache.loadedRow(at: $0)?.track.id }
        try check(expected.count == 5, "Selected rows are not loaded")
        guard let before = model.snapshot?.queue_length else { throw Failure(message: "No snapshot") }
        let plan = try JSONSerialization.data(withJSONObject: [
            "row": 1, "steps": [["id": "source-up-next", "hold": 0.8]]])
        try await drive("drag-rows", table, String(decoding: plan, as: UTF8.self))
        try await wait("queue grew", model) { model.snapshot?.queue_length == before + 5 && !model.busy }
        let queue: Page<QueueItem> = try await model.send("queue_page", [
            "revision": .null, "offset": .int(before), "limit": .int(5)])
        try check(queue.entries.map(\.track_id) == expected, "Up Next did not receive the dragged rows in order")
        try check(queue.entries.allSatisfy { $0.source == QueueSource(kind: .drag, name: "Songs") },
                  "Dragged entries lack the drag source")
        observations["dragged_track_ids"] = expected
        cases.append("native-songs-drag-range-appends-to-up-next")
    }

    /// The shown Songs list, set by its view for observation.
    static weak var songsList: ListModel?

    private static func serverPage(_ model: PlayerModel, _ list: ListDescription, offset: Int,
                                   limit: Int) async throws -> ListTracksResponse {
        try await model.send("list_tracks", [
            "list": try JSONValue.encoding(list), "revision": .null, "offset": .int(offset), "limit": .int(limit)])
    }

    private static func serverRow(_ model: PlayerModel, _ list: ListDescription, offset: Int) async throws -> ListRow {
        guard let row = try await serverPage(model, list, offset: offset, limit: 1).entries.first else {
            throw Failure(message: "Server has no row at \(offset)")
        }
        return row
    }

    private static func drive(_ action: String, _ identifier: String, _ value: String? = nil) async throws {
        try await NativeSmoke.drive(action, identifier, value)
    }
    private static func press(_ identifier: String) async throws { try await drive("press", identifier) }
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
        if process.terminationStatus != 0 {
            // Without Screen Recording permission, keep a self-rendered image for review.
            // It is not a composited screenshot and is named so.
            selfRender(window, name: name + "-self-rendered")
        }
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }
    private static func selfRender(_ window: NSWindow, name: String) {
        guard let frame = window.contentView?.superview,
              let bitmap = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) else { return }
        frame.cacheDisplay(in: frame.bounds, to: bitmap)
        try? bitmap.representation(using: .png, properties: [:])?
            .write(to: output.appendingPathComponent(name + ".png"))
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases, "observations": observations,
            "automation": "External Accessibility scroll, header clicks, browser and table selection, and a CGEvent drag onto the Up Next source row; server reads only for expected values."]
        if let error { report["error"] = error }
        if let state = model.snapshot {
            report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state))
        }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("SONGS_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
