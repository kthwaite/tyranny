#if DEBUG
import AppKit
import Foundation

/// Mutations use actual AX controls; model/image access is observation only.
@MainActor
enum NativeArtworkSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-artwork-smoke")
    private static var cases: [String] = []

    static func startIfRequested(_ model: PlayerModel) {
        guard !started, let path = argument("--artwork-smoke-output") else { return }
        started = true
        output = URL(fileURLWithPath: path, isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("indexed library", model) { model.connected && model.libraryReady && model.albums != nil }
                try await NativeSmoke.drive("activate")
                if argument("--artwork-smoke-mode") == "restore" { try await restore(model) }
                else { try await exercise(model) }
                try finish(model, error: nil)
            } catch {
                try? capture("failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel) async throws {
        for (title, source, channel) in [("Embedded Album", "embedded", 0), ("Folder Album", "folder", 2), ("Broken Folder Album", "folder", 1), ("Online Album", "cover_art_archive", -1), ("Missing Album", "missing", -1)] {
            guard let album = model.albums?.entries.first(where: { $0.title == title }) else { throw Failure(message: "Missing album \(title)") }
            try await wait("thumbnail \(title)", model) { model.artwork.value(for: model.artwork.albumRequest(album.id)).state == source }
            let image = model.artwork.value(for: model.artwork.albumRequest(album.id))
            if channel >= 0 { try colour(image.image, channel) }
            try await NativeSmoke.drive("expect-text", "album-art-\(album.id)", source)
        }
        try capture("album-thumbnails")
        cases.append("native-visible-album-thumbnails-decode-embedded-folder-online-and-neutral-missing")
        try await play("Embedded Track", source: "embedded", model)
        try colour(model.artwork.nowPlaying.image, 0)
        let current = model.snapshot?.current_id
        let position = model.snapshot?.position_frames ?? 0
        try await NativeSmoke.drive("value", "library-filter", "album:\"Online Album\"")
        try await wait("independent browser", model) { model.libraryReady && model.tracks?.total == 1 }
        try check(model.snapshot?.current_id == current && model.snapshot?.transport == "playing" && model.artwork.nowPlaying.state == "embedded", "Browsing replaced current artwork or playback")
        try await wait("playhead advances during browsing", model) { (model.snapshot?.position_frames ?? 0) > position }
        try capture("independent-now-playing")
        cases.append("native-now-playing-image-follows-occurrence-not-browser-filter-and-audio-advances")
        try await play("Folder Track", source: "folder", model)
        try colour(model.artwork.nowPlaying.image, 2)
        let previousKey = model.artwork.nowPlaying.cacheKey
        let folderCurrent = model.snapshot?.current_id
        guard let folder = argument("--artwork-music-folder"), let green = argument("--artwork-green-source"), let root = model.roots?.entries.first else { throw Failure(message: "Rescan fixture missing") }
        let bytes = try Data(contentsOf: URL(fileURLWithPath: green))
        try bytes.write(to: URL(fileURLWithPath: folder).appendingPathComponent("Folder/CoVeR.PNG"), options: .atomic)
        try bytes.write(to: URL(fileURLWithPath: folder).appendingPathComponent("Missing/folder.png"), options: .atomic)
        try await NativeSmoke.drive("press", "show-roots")
        try await NativeSmoke.drive("press", "rescan-root-\(root.id)")
        try await NativeSmoke.drive("press-title", "Done")
        try await wait("folder-only artwork refresh", model) { model.snapshot?.library.scan.phase == "complete" && model.artwork.nowPlaying.state == "folder" && model.artwork.nowPlaying.cacheKey != previousKey }
        try colour(model.artwork.nowPlaying.image, 1)
        try check(model.snapshot?.current_id == folderCurrent && model.snapshot?.transport == "playing", "Rescan interrupted playback")
        try capture("folder-only-rescan")
        cases.append("native-rescan-refreshes-decoded-current-folder-image-without-changing-audio-occurrence")
        try await play("Missing Track", source: "folder", model)
        try colour(model.artwork.nowPlaying.image, 1)
        try await play("Online Track", source: "cover_art_archive", model)
        try check(model.artwork.nowPlaying.image != nil, "Online source did not decode")
        try capture("online-now-playing")
        cases.append("native-cached-missing-invalidates-and-real-online-artwork-renders")
    }

    private static func restore(_ model: PlayerModel) async throws {
        try check(model.snapshot?.transport == "paused" && model.snapshot?.now_playing?.title == "Online Track", "Restart lost current occurrence or paused resume policy")
        try await wait("offline current image", model) { model.artwork.nowPlaying.state == "cover_art_archive" && model.artwork.nowPlaying.image != nil }
        try await NativeSmoke.drive("expect-text", "now-playing-art", "cover_art_archive")
        try await NativeSmoke.drive("press", "transport-play")
        try await wait("offline playback", model) { model.snapshot?.transport == "playing" && (model.snapshot?.position_frames ?? 0) > 0 }
        guard let probe = model.albums?.entries.first(where: { $0.title == "Offline Probe Album" }) else { throw Failure(message: "Offline probe album missing") }
        try await wait("neutral unavailable thumbnail", model) { model.artwork.value(for: model.artwork.albumRequest(probe.id)).state == "unavailable" }
        try await NativeSmoke.drive("expect-text", "album-art-\(probe.id)", "unavailable")
        try check(model.snapshot?.error == nil && model.snapshot?.transport == "playing", "Artwork network failure became a playback fault")
        try capture("offline-restored-artwork")
        cases.append("native-restart-restores-online-image-offline-and-transport-remains-usable")
    }

    private static func play(_ title: String, source: String, _ model: PlayerModel) async throws {
        try await NativeSmoke.drive("value", "library-filter", "title:\"\(title)\"")
        try await wait("track \(title)", model) { model.libraryReady && model.tracks?.entries.contains(where: { $0.metadata.title == title }) == true }
        guard let row = model.tracks?.entries.firstIndex(where: { $0.metadata.title == title }) else { throw Failure(message: "Track disappeared: \(title)") }
        try await NativeSmoke.drive("double-click-row", "track-table", "\(row),0.35")
        try await wait("play \(title)", model) { model.snapshot?.transport == "playing" && model.snapshot?.now_playing?.title == title && model.artwork.nowPlaying.state == source }
        try await NativeSmoke.drive("expect-text", "now-playing-art", source)
    }

    private static func colour(_ image: CGImage?, _ channel: Int) throws {
        guard let image else { throw Failure(message: "Expected decoded image") }
        var pixel = [UInt8](repeating: 0, count: 4)
        let drawn = pixel.withUnsafeMutableBytes { bytes -> Bool in
            guard let context = CGContext(data: bytes.baseAddress, width: 1, height: 1, bitsPerComponent: 8, bytesPerRow: 4, space: CGColorSpaceCreateDeviceRGB(), bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
            context.draw(image, in: CGRect(x: 0, y: 0, width: 1, height: 1))
            return true
        }
        try check(drawn && pixel[channel] > 220 && (0..<3).filter { $0 != channel }.allSatisfy { pixel[$0] < 30 }, "Decoded artwork has wrong colour: \(pixel)")
    }

    private static func argument(_ key: String) -> String? {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: key), index + 1 < args.count else { return nil }
        return args[index + 1]
    }
    private static func wait(_ label: String, _ model: PlayerModel, until predicate: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(30)
        while !predicate() {
            if Date() >= deadline { throw Failure(message: "Timed out: \(label); model=\(model.message ?? "none"); artwork=\(model.artwork.nowPlaying.state)") }
            try await Task.sleep(for: .milliseconds(40))
        }
        try await Task.sleep(for: .milliseconds(150))
    }
    private static func check(_ condition: Bool, _ message: String) throws { if !condition { throw Failure(message: message) } }
    private static func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else { throw Failure(message: "Native window missing") }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), output.appendingPathComponent(name + ".png").path]
        try process.run(); process.waitUntilExit()
        try check(process.terminationStatus == 0, "Screenshot failed")
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases, "automation": "Actual AX controls and mouse activation; decoded image pixels/model observation; isolated PCM capture."]
        if let error { report["error"] = error }
        if let state = model.snapshot { report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state)) }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]).write(to: output.appendingPathComponent("report.json"))
        print("ARTWORK_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
