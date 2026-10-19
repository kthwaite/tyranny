#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for Recently Added and History (`--history-smoke-output`). The gate
/// prepares the 1,129-track songs library. Controls are driven through the external
/// Accessibility driver. The one track is enqueued through the wire (a double-click on a Songs row plays
/// runs, see `NativeRunsSmoke`) and played to its end on the clocked capture output.
@MainActor
enum NativeHistorySmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    static let tracks = 1129
    /// A 1-second tagged track; the phrase matches exactly one title.
    static let title = "Ambient 0 Song 1"

    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-history-smoke")
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]

    static weak var history: HistoryModel?
    static weak var recent: ListModel?

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--history-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("connection", model) {
                    model.connected && model.snapshot?.library.tracks == tracks
                        && model.snapshot?.library.scan.phase == "complete"
                }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                try await exercise(model, navigation)
                try finish(model, error: nil)
            } catch {
                try? capture("history-failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        // Recently Added: every fixture track was added just now; newest first, grouped by Album.
        try await drive("click-control", Destination.recent.identifier)
        try await wait("recent destination", model) { navigation.destination == .recent && recent != nil }
        guard let recent else { throw Failure(message: "Recently Added list missing") }
        try await drive("exists", "main-recent")
        try await wait("recent loaded", model) { recent.cache.total == tracks && recent.layoutIsCurrent }
        try check(recent.source == .recent, "Recently Added is not the recent source")
        try check(recent.description.sort == [SortKey(field: .dateAdded, descending: true)],
                  "Recently Added is not sorted by date added, newest first")
        try check(recent.grouping == .album, "Recently Added is not grouped by Album")
        try await drive("expect-text", "list-count", DisplayFormat.songs(tracks))
        let expected: ListTracksResponse = try await model.send("list_tracks", [
            "list": try JSONValue.encoding(recent.description), "revision": .null, "offset": .int(0), "limit": .int(1)])
        try check(expected.total == UInt64(tracks), "Server lists \(expected.total) recent tracks")
        cases.append("native-recent-lists-new-tracks-by-date-added-grouped-by-album")

        // History starts empty.
        try await drive("click-control", Destination.history.identifier)
        try await wait("history destination", model) { navigation.destination == .history && history != nil }
        guard let history else { throw Failure(message: "History model missing") }
        try await drive("exists", "main-history")
        try await drive("exists", "history-table")
        try await wait("history loaded", model) { history.cache.revision != nil }
        try check(history.cache.total == 0, "History is not empty before playing")
        try await drive("exists", "history-empty")
        try await drive("expect-text", "history-count", "0 plays")
        cases.append("native-history-starts-empty")

        // Play one short track to its end.
        let before = model.snapshot?.history_revision ?? 0
        let list = ListDescription(source: .library, query: "\"\(title)\"")
        let found: ListTracksResponse = try await model.send("list_tracks", [
            "list": try JSONValue.encoding(list), "revision": .null, "offset": .int(0), "limit": .int(5)])
        guard found.entries.count == 1, let track = found.entries.first?.track else {
            throw Failure(message: "Expected one track titled \(title), found \(found.entries.count)")
        }
        guard let state = model.snapshot else { throw Failure(message: "No snapshot") }
        let selection = TrackSelection.ids(libraryRevision: state.library.revision, trackIDs: [track.id])
        let _: Applied = try await model.send("enqueue_tracks", [
            "selection": try JSONValue.encoding(selection), "expected_revision": .uint(state.queue_revision),
            "placement": .string(Placement.replaceUpcoming.rawValue), "play": .bool(true),
            "source": try JSONValue.encoding(QueueSource(kind: .songs, name: "Songs"))])
        try await wait("play counted", model, seconds: 30) { (model.snapshot?.history_revision ?? 0) > before }
        try await wait("history row", model) { history.cache.total == 1 && history.cache.loadedRow(at: 0) != nil }
        guard let entry = history.cache.loadedRow(at: 0) else { throw Failure(message: "History row missing") }
        try check(entry.track_id == track.id && entry.title == title, "History row is not the played track")
        try await drive("expect-text", "history-count", "1 play")
        try await drive("expect-text", "history-table-cell-\(entry.id)-title", title)
        try await drive("expect-text", "history-table-cell-\(entry.id)-played-at",
                        DisplayFormat.playedAt(entry.played_at, now: Date(), calendar: .current))
        try await Task.sleep(for: .milliseconds(400))
        try capture("history")
        observations["history_entry"] = ["id": entry.id, "track_id": track.id, "title": title]
        cases.append("native-history-row-appears-after-a-counted-play")

        // Double-click plays that track again (nothing is upcoming, so no sheet).
        let queueBefore = model.snapshot?.queue_length ?? 0
        try await drive("double-click-row", "history-table", "0,0.5")
        try await wait("history enqueue", model) { !model.busy && (model.snapshot?.queue_length ?? 0) == queueBefore + 1 }
        cases.append("native-history-double-click-plays-the-track")

        // Songs shows the play in its Plays column.
        try await drive("click-control", Destination.songs.identifier)
        try await wait("songs destination", model) { navigation.destination == .songs && NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw Failure(message: "Songs list missing") }
        try await drive("value", "library-filter", "\"\(title)\"")
        try await wait("songs narrowed", model) { songs.cache.total == 1 && songs.cache.loadedRow(at: 0) != nil }
        guard let row = songs.cache.loadedRow(at: 0) else { throw Failure(message: "Songs row missing") }
        try await wait("play count", model) { songs.cache.loadedRow(at: 0)?.track.play_count == 1 }
        try await drive("expect-text", "track-table-songs-cell-\(row.row_id)-plays", "1")
        try await drive("value", "library-filter", "")
        try await Task.sleep(for: .milliseconds(300))
        try capture("history-songs-plays")
        cases.append("native-songs-plays-column-shows-1-after-the-play")
    }

    // MARK: Helpers

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
            "automation": "External Accessibility source-list clicks, a row double-click and filter entry by identifier; the one track is enqueued over the wire; server reads only for expected values.",
            "scenario": "history"]
        if let error { report["error"] = error }
        if let state = model.snapshot {
            report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state))
        }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("HISTORY_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
