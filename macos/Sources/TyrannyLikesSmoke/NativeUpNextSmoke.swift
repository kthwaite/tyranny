#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Opt-in acceptance driver for Up Next and its drawer (`--upnext-smoke-output`). The gate
/// prepares the 1,129-track songs library. Runs are enqueued over the wire (double-click on a
/// Songs row arrives in Task 13). Navigation, Clear, Save as Playlist…, the drawer toggle and
/// the drop onto the drawer go through the external Accessibility driver.
@MainActor
enum NativeUpNextSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    static let tracks = 1129
    static let playlistName = "Saved From Up Next"

    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-likes-smoke-upnext-smoke")
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]

    static weak var upNext: UpNextModel?

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--upnext-smoke-output"), index + 1 < args.count else { return }
        started = true
        upNext = model.upNext
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
                try? capture("upnext-failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    // MARK: Wire helpers

    private static func apply(_ model: PlayerModel, _ fields: (ControlState) throws -> [String: JSONValue]) async throws {
        guard let state = model.snapshot else { throw Failure(message: "No snapshot") }
        let _: Applied = try await model.send("enqueue_tracks", try fields(state))
    }

    private static func queuePage(_ model: PlayerModel) async throws -> Page<QueueItem> {
        try await model.send("queue_page", ["revision": .null, "offset": .int(0), "limit": .int(500)])
    }

    private static func firstTracks(_ model: PlayerModel, query: String, count: Int) async throws -> [TrackSummary] {
        let list = ListDescription(source: .library, query: query)
        let found: ListTracksResponse = try await model.send("list_tracks", [
            "list": try JSONValue.encoding(list), "revision": .null, "offset": .int(0), "limit": .int(count)])
        guard found.entries.count == count else {
            throw Failure(message: "Expected \(count) tracks for \(query), found \(found.entries.count)")
        }
        return found.entries.map(\.track)
    }

    // MARK: Scenario

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        // A 2-minute track plays; two album runs and a playlist-sourced run wait behind it.
        let long = try await firstTracks(model, query: "\"Untagged 0001\"", count: 1)
        let albums: ListAlbumsResponse = try await model.send("list_albums", [
            "list": try JSONValue.encoding(ListDescription(source: .library)),
            "sort": .string(AlbumSort.albumArtist.rawValue), "descending": .bool(false),
            "revision": .null, "offset": .int(0), "limit": .int(2)])
        guard albums.entries.count == 2 else { throw Failure(message: "Expected two albums") }
        let (first, second) = (albums.entries[0], albums.entries[1])
        let picks = try await firstTracks(model, query: "\"Untagged 0002\" | \"Untagged 0003\" | \"Untagged 0004\"", count: 3)

        try await apply(model) { state in
            let selection = TrackSelection.ids(libraryRevision: state.library.revision, trackIDs: [long[0].id])
            return ["selection": try JSONValue.encoding(selection), "expected_revision": .uint(state.queue_revision),
                    "placement": .string(Placement.replaceUpcoming.rawValue), "play": .bool(true),
                    "source": try JSONValue.encoding(QueueSource(kind: .songs, name: "Songs"))]
        }
        for album in [first, second] {
            try await apply(model) { state in
                try EnqueueRequest(
                    selection: AlbumPlayback.selection(albumID: album.id, libraryRevision: state.library.revision),
                    placement: .append, play: false, source: AlbumPlayback.source(title: album.title))
                    .fields(expectedRevision: state.queue_revision)
            }
        }
        try await apply(model) { state in
            let selection = TrackSelection.ids(libraryRevision: state.library.revision, trackIDs: picks.map(\.id))
            return ["selection": try JSONValue.encoding(selection), "expected_revision": .uint(state.queue_revision),
                    "placement": .string(Placement.append.rawValue), "play": .bool(false),
                    "source": try JSONValue.encoding(QueueSource(kind: .playlist, name: "Late Night"))]
        }
        let total = 1 + Int(first.track_count) + Int(second.track_count) + picks.count
        try await wait("queue seeded", model) { model.snapshot?.queue_length == total && model.snapshot?.transport == "playing" }

        // Up Next shows sections and where each run came from.
        try await drive("click-control", Destination.upNext.identifier)
        try await wait("up next destination", model) { navigation.destination == .upNext && upNext != nil }
        guard let upNext else { throw Failure(message: "Up Next model missing") }
        try await drive("exists", "main-up-next")
        try await drive("exists", "up-next-table")
        try await wait("queue loaded", model) { upNext.cache.total == total && upNext.cache.loadedRow(at: total - 1) != nil }
        let page = try await queuePage(model)
        try check(page.total == total, "Server queue has \(page.total) entries, expected \(total)")
        try await drive("exists", "up-next-section-now-playing")
        try await drive("exists", "up-next-section-up-next")
        try await drive("expect-text", "up-next-count", "\(DisplayFormat.count(total - 1)) upcoming")
        let labels = [
            (page.entries[0], "Songs"),
            (page.entries[1], "Album · \(first.title)"),
            (page.entries[Int(first.track_count)], "Album · \(first.title)"),
            (page.entries[1 + Int(first.track_count)], "Album · \(second.title)"),
            (page.entries[total - 1], "Playlist · Late Night"),
        ]
        for (entry, label) in labels {
            try await drive("expect-text", "up-next-table-cell-\(entry.id)-added-from", label)
        }
        try await drive("expect-text", "up-next-table-cell-\(page.entries[0].id)-title", long[0].title)
        observations["current_id"] = page.entries[0].id
        observations["labels"] = labels.map { ["id": $0.0.id, "label": $0.1] }
        try await Task.sleep(for: .milliseconds(400))
        try capture("upnext")
        cases.append("native-up-next-sections-and-added-from-labels")

        // Clear removes the upcoming entries; the current track keeps playing.
        let current = model.snapshot?.current_id
        let before = model.snapshot?.position_frames ?? 0
        try await drive("press", "up-next-clear")
        try await wait("cleared", model) { model.snapshot?.queue_length == 1 && !model.busy }
        try check(model.snapshot?.current_id == current && model.snapshot?.transport == "playing",
                  "Clear stopped or changed the current track")
        try await wait("still advancing", model) { (model.snapshot?.position_frames ?? 0) > before }
        try await drive("expect-text", "up-next-count", "0 upcoming")
        cases.append("native-up-next-clear-keeps-the-current-track-playing")

        // Save as Playlist… needs upcoming entries: queue two tracks again.
        let more = try await firstTracks(model, query: "\"Untagged 0005\" | \"Untagged 0006\"", count: 2)
        try await apply(model) { state in
            let selection = TrackSelection.ids(libraryRevision: state.library.revision, trackIDs: more.map(\.id))
            return ["selection": try JSONValue.encoding(selection), "expected_revision": .uint(state.queue_revision),
                    "placement": .string(Placement.append.rawValue), "play": .bool(false),
                    "source": try JSONValue.encoding(QueueSource(kind: .songs, name: "Songs"))]
        }
        try await wait("two upcoming", model) { model.snapshot?.queue_length == 3 && upNext.upcomingCount == 2 }
        try await drive("expect-enabled", "up-next-save", "true")
        try await drive("press", "up-next-save")
        try await drive("exists", "up-next-save-name")
        try await drive("value", "up-next-save-name", playlistName)
        try await drive("press", "up-next-save-confirm")
        try await wait("playlist saved", model) { !model.busy && model.snapshot?.playlists_revision != 0 }
        let saved: Page<Playlist> = try await model.send("saved_playlists", [
            "revision": .null, "offset": .int(0), "limit": .int(50), "query": .string(""), "kind": .null])
        guard let playlist = saved.entries.first(where: { $0.name == playlistName }) else {
            throw Failure(message: "Saved playlist \(playlistName) not found")
        }
        observations["saved_playlist"] = ["id": playlist.id, "name": playlist.name, "entry_count": playlist.entry_count ?? -1]
        cases.append("native-up-next-save-as-playlist-creates-a-playlist")

        // Drawer: toggle it on Songs, then drop three Songs rows on its target.
        try await drive("click-control", Destination.songs.identifier)
        try await wait("songs destination", model) { navigation.destination == .songs && NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw Failure(message: "Songs list missing") }
        try check(!navigation.upNextDrawerVisible, "The drawer starts hidden")
        try await drive("press", "up-next-drawer-toggle")
        try await wait("drawer shown", model) { navigation.upNextDrawerVisible }
        try await drive("exists", "up-next-drawer")
        try await drive("exists", "up-next-drawer-art")
        try await drive("expect-text", "up-next-drawer-title", long[0].title)
        try await drive("exists", "up-next-drawer-drop")
        try await wait("drawer rows", model) { upNext.upcoming(limit: 10).count == 2 }
        let upcoming = upNext.upcoming(limit: 10)
        try await drive("exists", "up-next-drawer-row-\(upcoming[0].id)")
        try await Task.sleep(for: .milliseconds(400))
        try capture("drawer")
        cases.append("native-up-next-drawer-shows-now-playing-and-upcoming")

        let table = "track-table-songs"
        try await wait("songs rows", model) { songs.cache.loadedRow(at: 2) != nil }
        try await drive("scroll-fraction", table, "0")
        try await drive("select", table, "0,1,2")
        try await wait("range selected", model) { songs.selectedPositions == IndexSet(integersIn: 0..<3) }
        let expected = (0..<3).compactMap { songs.cache.loadedRow(at: $0)?.track.id }
        try check(expected.count == 3, "Selected rows are not loaded")
        guard let length = model.snapshot?.queue_length else { throw Failure(message: "No snapshot") }
        let plan = try JSONSerialization.data(withJSONObject: [
            "row": 1, "steps": [["id": "up-next-drawer-drop", "hold": 0.8]]])
        try await drive("drag-rows", table, String(decoding: plan, as: UTF8.self))
        try await wait("drop appended", model) { model.snapshot?.queue_length == length + 3 && !model.busy }
        let tail: Page<QueueItem> = try await model.send("queue_page", [
            "revision": .null, "offset": .int(length), "limit": .int(3)])
        try check(tail.entries.map(\.track_id) == expected, "The drawer did not receive the dropped rows in order")
        try check(tail.entries.allSatisfy { $0.source == QueueSource(kind: .drag, name: "Songs") },
                  "Dropped entries lack the drag source")
        try await wait("drawer lists the drop", model) { upNext.upcoming(limit: 10).count == 5 }
        observations["dropped_track_ids"] = expected
        cases.append("native-up-next-drawer-drop-appends-the-dragged-rows")
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
        guard let window = NSApp.windows.first(where: { $0.title == "tyranny likes smoke" }) else {
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
            "automation": "External Accessibility source-list clicks, Clear, Save as Playlist…, the drawer toggle and a CGEvent drag onto the drawer's drop target; runs are enqueued over the wire; server reads only for expected values.",
            "scenario": "upnext"]
        if let error { report["error"] = error }
        if let state = model.snapshot {
            report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state))
        }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("UPNEXT_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
