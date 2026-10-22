#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Opt-in acceptance driver for Artists and Genres (`--artists-smoke-output`). The gate prepares
/// the songs library plus one compilation album on the server. Every control is driven through
/// the external Accessibility driver; server queries here only read the expected values.
@MainActor
enum NativeArtistsSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    /// 1,129 songs-fixture tracks plus the compilation's four.
    static let tracks = 1133
    static let compilationName = "Smoke Compilation"
    static let appearsOnArtist = "Folk Artist 1"
    static let plainArtist = "Jazz Artist 2"
    static let genre = "Folk"

    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-likes-smoke-artists-smoke")
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--artists-smoke-output"), index + 1 < args.count else { return }
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
                try? capture("artists-failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    /// The Artists or Genres model, set by the window.
    static weak var browse: NamedBrowseModel?

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try await artists(model, navigation)
        try await genres(model, navigation)
    }

    // MARK: Artists

    private static func artists(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try await drive("click-control", Destination.artists.identifier)
        try await wait("artists destination", model) { navigation.destination == .artists && browse?.kind == .artists }
        guard let browse else { throw Failure(message: "Artists model missing") }
        try await drive("exists", "main-artists")
        try await drive("exists", "artists-list")
        try await wait("artists loaded", model) {
            browse.column.cache.revision != nil && browse.column.cache.loadedRow(at: 0) != nil
                && browse.column.compilations != nil
        }
        let reply = try await artistsPage(model)
        try check(UInt64(browse.column.cache.total) == reply.total, "Artists total differs from the server's")
        guard let compilations = reply.compilations else { throw Failure(message: "Server reports no compilations") }
        try check(compilations.album_count == 1 && compilations.track_count == 4, "Compilations row counts differ")
        cases.append("native-artists-list-matches-server-with-compilations-row")

        // Compilations first: its tracks are the compilation album, grouped by Album.
        try await drive("click-control", "artists-list-compilations")
        try await wait("compilations detail", model) {
            browse.selected == [BrowserSelection.compilationsID] && browse.detail?.cache.total == 4
                && browse.detail?.layoutIsCurrent == true
        }
        guard let detail = browse.detail else { throw Failure(message: "Compilations detail missing") }
        try check(detail.source == .compilations && detail.grouping == .album, "Compilations detail is not grouped by Album")
        try await drive("exists", "main-artists-tracks")
        try await drive("expect-text", "list-count", DisplayFormat.songs(4))
        try check(albumBlocks(detail) == 1, "Compilations shows \(albumBlocks(detail)) album blocks")
        try check(browse.appearsOn == nil, "Compilations has an Appears On section")
        observations["compilations"] = ["albums": compilations.album_count, "tracks": compilations.track_count]
        cases.append("native-artists-compilations-row-lists-the-compilation")

        // A track artist on the compilation: Appears On lists its tracks.
        guard let artist = reply.entries.first(where: { $0.name == appearsOnArtist }) else {
            throw Failure(message: "\(appearsOnArtist) is not an album artist")
        }
        try await drive("click-control", "artists-list-\(artist.id)")
        try await wait("artist detail", model) {
            browse.selected == [artist.id] && browse.detail?.cache.total == Int(artist.track_count)
                && browse.appearsOn?.cache.total == 2
        }
        try check(browse.detail?.source == .artist(id: artist.id), "Detail is not the artist's list")
        try check(albumBlocks(try unwrap(browse.detail)) == Int(artist.album_count), "Artist detail album blocks differ")
        let expected: ListTracksResponse = try await model.send("list_tracks", [
            "list": try JSONValue.encoding(ListDescription(source: .appearsOn(artistID: artist.id))),
            "revision": .null, "offset": .int(0), "limit": .int(500)])
        try check(expected.total == 2, "Server lists \(expected.total) Appears On tracks")
        try await drive("exists", "artist-appears-on")
        try await drive("expect-text", "artist-appears-on-count", DisplayFormat.songs(2))
        try await Task.sleep(for: .milliseconds(400))
        try capture("artists")
        observations["appears_on"] = ["artist": artist.name, "tracks": expected.total]
        cases.append("native-artists-appears-on-lists-compilation-tracks")

        // An artist with no compilation tracks has no Appears On section.
        guard let plain = reply.entries.first(where: { $0.name == plainArtist }) else {
            throw Failure(message: "\(plainArtist) is not an album artist")
        }
        try await drive("click-control", "artists-list-\(plain.id)")
        try await wait("plain artist detail", model) {
            browse.selected == [plain.id] && browse.detail?.cache.total == Int(plain.track_count)
                && browse.appearsOn?.cache.revision != nil
        }
        try check(browse.appearsOn?.cache.total == 0, "\(plainArtist) has Appears On tracks")
        cases.append("native-artists-appears-on-hidden-when-empty")
    }

    // MARK: Genres

    private static func genres(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try await drive("click-control", Destination.genres.identifier)
        try await wait("genres destination", model) { navigation.destination == .genres && browse?.kind == .genres }
        guard let browse else { throw Failure(message: "Genres model missing") }
        try await drive("exists", "main-genres")
        try await drive("exists", "genres-list")
        try await wait("genres loaded", model) {
            browse.column.cache.revision != nil && browse.column.cache.loadedRow(at: 0) != nil
        }
        let reply: ListGenresResponse = try await model.send("list_genres", [
            "query": .string(""), "revision": .null, "offset": .int(0), "limit": .int(500)])
        try check(UInt64(browse.column.cache.total) == reply.total, "Genres total differs from the server's")
        guard let folk = reply.entries.first(where: { $0.name == genre }) else { throw Failure(message: "No \(genre) genre") }
        try await drive("click-control", "genres-list-\(folk.id)")
        try await wait("genre detail", model) {
            browse.selected == [folk.id] && browse.detail?.cache.total == Int(folk.track_count)
                && browse.detail?.layoutIsCurrent == true
        }
        guard let detail = browse.detail else { throw Failure(message: "Genre detail missing") }
        try check(detail.source == .genre(id: folk.id) && detail.grouping == .album, "Genre detail is not grouped by Album")
        try check(browse.appearsOn == nil, "Genres has an Appears On section")
        try await drive("expect-text", "list-count", DisplayFormat.songs(UInt64(folk.track_count)))
        try check(albumBlocks(detail) == Int(folk.album_count), "\(genre) shows \(albumBlocks(detail)) album blocks; server says \(folk.album_count)")
        try await Task.sleep(for: .milliseconds(400))
        try capture("genres")
        observations["genre"] = ["name": genre, "albums": folk.album_count, "tracks": folk.track_count]
        cases.append("native-genres-selection-narrows-to-server-album-count")
    }

    // MARK: Helpers

    private static func artistsPage(_ model: PlayerModel) async throws -> ListArtistsResponse {
        try await model.send("list_artists", [
            "query": .string(""), "list": .null, "revision": .null, "offset": .int(0), "limit": .int(500)])
    }

    /// Distinct albums in the list's grouped layout.
    private static func albumBlocks(_ list: ListModel) -> Int {
        var albums = Set<UInt64>()
        let layout = list.layout
        for index in 0..<layout.count {
            if let album = layout.albumKey(atDisplayIndex: index) { albums.insert(album) }
        }
        return albums.count
    }

    private static func unwrap<T>(_ value: T?) throws -> T {
        guard let value else { throw Failure(message: "Missing value") }
        return value
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
            "automation": "External Accessibility source-list clicks and list-row clicks by identifier; server reads only for expected values.",
            "scenario": "artists"]
        if let error { report["error"] = error }
        if let state = model.snapshot {
            report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state))
        }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("ARTISTS_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
