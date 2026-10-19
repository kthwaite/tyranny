#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Shared steps of the playlist smoke drivers (`NativePlaylistSmoke`, `NativeSmartPlaylistSmoke`):
/// the external Accessibility driver, waiting on model state, server reads for expected values,
/// screenshots and the report.
@MainActor
final class NativePlaylistKit {
    struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    let model: PlayerModel
    let navigation: NavigationModel
    let output: URL
    let tag: String
    private(set) var cases: [String] = []
    var observations: [String: Any] = [:]

    /// The playlist list on screen, set by `TrackListView`.
    static weak var shown: ListModel?

    init(model: PlayerModel, navigation: NavigationModel, output: URL, tag: String) {
        self.model = model
        self.navigation = navigation
        self.output = output
        self.tag = tag
    }

    func passed(_ name: String) { cases.append(name) }

    // MARK: Driver

    func drive(_ action: String, _ identifier: String = "", _ value: String? = nil) async throws {
        try await NativeSmoke.drive(action, identifier, value)
    }
    func press(_ id: String) async throws { try await drive("press", id) }
    func value(_ id: String, _ text: String) async throws { try await drive("value", id, text) }
    func select(_ id: String, _ rows: [Int]) async throws {
        try await drive("select", id, rows.map(String.init).joined(separator: ","))
    }
    /// Opens a destination by clicking its source-list row.
    func open(_ destination: Destination) async throws {
        try await drive("click-control", destination.identifier)
        try await wait("destination \(destination.key)") { self.navigation.destination == destination }
    }

    /// A real drag from a row of `table`, over the steps' targets.
    func drag(_ table: String, row: Int, steps: [[String: Any]], capture name: String? = nil, cancel: Bool = false) async throws {
        var plan: [String: Any] = ["row": row, "steps": steps, "cancel": cancel]
        if let name, let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) {
            plan["capture_window"] = window.windowNumber
            plan["capture_path"] = output.appendingPathComponent(name + ".png").path
        }
        let data = try JSONSerialization.data(withJSONObject: plan)
        try await drive("drag-rows", table, String(decoding: data, as: UTF8.self))
    }
    static func target(_ id: String, row: Int? = nil, edge: String? = nil, hold: Double? = nil) -> [String: Any] {
        var result: [String: Any] = ["id": id]
        if let row { result["row"] = row }
        if let edge { result["edge"] = edge }
        if let hold { result["hold"] = hold }
        return result
    }
    /// Holding over a source-list row opens it (spring navigation).
    static func hover(_ id: String) -> [String: Any] { target(id, hold: 1.1) }

    // MARK: Waiting and checking

    func wait(_ label: String, seconds: Double = 20, until condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            if Date() >= deadline {
                throw Failure(message: "Timed out waiting for \(label); message=\(model.message ?? "none")")
            }
            try await Task.sleep(for: .milliseconds(50))
        }
        try await Task.sleep(for: .milliseconds(200))
    }
    func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }
    func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else {
            throw Failure(message: "Native window unavailable")
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }

    // MARK: Model and server reads

    var playlists: [Playlist] { model.savedPlaylists.rows }
    func playlist(named name: String) -> Playlist? { playlists.first { $0.name == name } }

    /// Waits for the catalog, then for `name` to be in it.
    func waitForPlaylist(_ name: String, entries count: Int? = nil) async throws -> Playlist {
        try await wait("playlist \(name)") {
            !self.model.busy && self.playlist(named: name).map { count == nil || $0.entry_count == count } == true
        }
        return playlist(named: name)!
    }

    /// The playlist's entries in order, read from the server.
    func entries(_ playlist: Playlist) async throws -> [PlaylistEntry] {
        var result: [PlaylistEntry] = []
        let first: PlaylistEntries = try await model.send("playlist_entries", [
            "playlist_id": .uint(playlist.id), "revision": .null, "library_revision": .null,
            "query": .string(""), "offset": .int(0), "limit": .int(500)])
        result = first.entries
        while result.count < first.total {
            let next: PlaylistEntries = try await model.send("playlist_entries", [
                "playlist_id": .uint(playlist.id), "revision": .uint(first.revision),
                "library_revision": .uint(first.library_revision), "query": .string(""),
                "offset": .int(result.count), "limit": .int(500)])
            guard !next.entries.isEmpty else { break }
            result += next.entries
        }
        return result
    }

    /// The first `count` library tracks in the default Songs order, from the server.
    func songs(_ count: Int) async throws -> [ListRow] {
        let reply: ListTracksResponse = try await model.send("list_tracks", [
            "list": try JSONValue.encoding(ListDescription(source: .library)), "revision": .null,
            "offset": .int(0), "limit": .int(count)])
        return reply.entries
    }

    static func moved<T: Identifiable>(_ rows: [T], indexes: [Int], before: T.ID) -> [T] {
        let selected = Set(indexes)
        let moving = rows.enumerated().filter { selected.contains($0.offset) }.map(\.element)
        var remaining = rows.enumerated().filter { !selected.contains($0.offset) }.map(\.element)
        remaining.insert(contentsOf: moving, at: remaining.firstIndex { $0.id == before }!)
        return remaining
    }

    // MARK: Report

    func finish(automation: String, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases, "observations": observations,
                                     "automation": automation]
        if let error { report["error"] = error }
        if let state = model.snapshot { report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state)) }
        report["playlists"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(playlists))
        if let message = model.message { report["model_message"] = message }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("\(tag) \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }

    /// Ends open sheets, so the app can terminate after a failure.
    static func endSheets() {
        for window in NSApp.windows {
            if let sheet = window.attachedSheet { window.endSheet(sheet) }
        }
    }
}
#endif
