#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Mutations use actual AX controls; model/image access is observation only.
@MainActor
enum NativeArtworkSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-likes-smoke-artwork-smoke")
    private static var cases: [String] = []

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        guard !started, let path = argument("--artwork-smoke-output") else { return }
        started = true
        output = URL(fileURLWithPath: path, isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                let settingsOnly = argument("--artwork-smoke-mode") == "settings"
                let providersOnly = argument("--artwork-smoke-mode") == "providers"
                try await wait(settingsOnly ? "connected server" : "indexed library", model) {
                    model.connected && (settingsOnly || (model.snapshot?.library.scan.phase == "complete" && (model.snapshot?.library.tracks ?? 0) >= (providersOnly ? 1 : 6)))
                }
                try await NativeSmoke.drive("activate")
                UserDefaults.standard.set(UpNextPolicy.replace.rawValue, forKey: "upNextOnPlay")
                defer { UserDefaults.standard.removeObject(forKey: "upNextOnPlay") }
                // `now-playing-art` is the player bar's thumbnail, which shows only while the art is
                // collapsed. This is the user's real preference; restore it when done.
                let savedColours = model.theme.settings
                model.theme.update(ColourSettings())
                defer { model.theme.update(savedColours) }
                let savedArtExpanded = navigation.nowPlayingArtExpanded
                navigation.nowPlayingArtExpanded = false
                defer { navigation.nowPlayingArtExpanded = savedArtExpanded }
                if settingsOnly { try await settings(model) }
                else if providersOnly { try await providers(model, navigation) }
                else if argument("--artwork-smoke-mode") == "restore" { try await restore(model, navigation) }
                else { try await exercise(model, navigation) }
                try finish(model, error: nil)
            } catch {
                try? capture("failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    /// The album grid's loaded albums, found by title.
    private static func album(_ title: String) -> AlbumSummary? {
        NativeAlbumsSmoke.grid?.cache.loadedRows().map(\.row).first { $0.title == title }
    }

    private static func openAlbums(_ model: PlayerModel, _ navigation: NavigationModel, expecting titles: [String]) async throws {
        try await NativeSmoke.drive("click-control", Destination.albums.identifier)
        try await wait("album grid", model) {
            navigation.destination == .albums && titles.allSatisfy { album($0) != nil }
        }
    }

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        let titles = ["Embedded Album", "Folder Album", "Broken Folder Album", "Online Album", "Missing Album"]
        try await openAlbums(model, navigation, expecting: titles)
        for (title, source, channel) in [("Embedded Album", "embedded", 0), ("Folder Album", "folder", 2), ("Broken Folder Album", "folder", 1), ("Online Album", "cover_art_archive", -1), ("Missing Album", "missing", -1)] {
            guard let album = album(title) else { throw Failure(message: "Missing album \(title)") }
            try await wait("thumbnail \(title)", model) { model.artwork.value(for: model.artwork.albumRequest(album.id)).state == source }
            let image = model.artwork.value(for: model.artwork.albumRequest(album.id))
            if channel >= 0 { try colour(image.image, channel) }
            try await NativeSmoke.drive("exists", "album-cell-\(album.id)")
        }
        try capture("album-thumbnails")
        cases.append("native-visible-album-thumbnails-decode-embedded-folder-online-and-neutral-missing")
        try await play("Embedded Track", source: "embedded", model, navigation)
        try colour(model.artwork.nowPlaying.image, 0)
        try await wait("red accent tint", model) { tintedAccent(model) { $0.red > 0.8 && $0.green < 0.2 && $0.blue < 0.2 } }
        let current = model.snapshot?.current_id
        let position = model.snapshot?.position_frames ?? 0
        try await NativeSmoke.drive("value", "library-filter", "album:\"Online Album\"")
        try await wait("independent browser", model) { NativeSongsSmoke.songsList?.cache.total == 1 }
        try check(model.snapshot?.current_id == current && model.snapshot?.transport == "playing" && model.artwork.nowPlaying.state == "embedded", "Browsing replaced current artwork or playback")
        try await wait("playhead advances during browsing", model) { (model.snapshot?.position_frames ?? 0) > position }
        try capture("independent-now-playing")
        cases.append("native-now-playing-image-follows-occurrence-not-browser-filter-and-audio-advances")
        try await play("Folder Track", source: "folder", model, navigation)
        try colour(model.artwork.nowPlaying.image, 2)
        try await wait("blue accent tint", model) { tintedAccent(model) { $0.blue > 0.8 && $0.red < 0.2 && $0.green < 0.2 } }
        let blueAccent = model.theme.tokens.accent
        try await NativeSmoke.drive("press", "transport-play-pause")
        try await wait("paused", model) { model.snapshot?.transport == "paused" }
        try check(model.theme.tokens.accent == blueAccent, "Pausing changed the tinted accent")
        try await NativeSmoke.drive("press", "transport-play-pause")
        try await wait("playing again", model) { model.snapshot?.transport == "playing" }
        let commands = CommandDispatcher(player: model, navigation: navigation, lists: TrackLists())
        try await commands.execute("tint off")
        try check(model.theme.tokens.accent == .system, "tint off did not revert the accent")
        try await commands.execute("tint on")
        try await wait("blue accent tint restored", model) { tintedAccent(model) { $0.blue > 0.8 && $0.red < 0.2 && $0.green < 0.2 } }
        cases.append("native-accent-follows-playing-cover-holds-on-pause-and-reverts-with-tint-off")
        let previousKey = model.artwork.nowPlaying.cacheKey
        let folderCurrent = model.snapshot?.current_id
        guard let folder = argument("--artwork-music-folder"), let green = argument("--artwork-green-source"), let root = model.roots?.entries.first else { throw Failure(message: "Rescan fixture missing") }
        let bytes = try Data(contentsOf: URL(fileURLWithPath: green))
        try bytes.write(to: URL(fileURLWithPath: folder).appendingPathComponent("Folder/CoVeR.PNG"), options: .atomic)
        try bytes.write(to: URL(fileURLWithPath: folder).appendingPathComponent("Missing/folder.png"), options: .atomic)
        try await NativeSmoke.drive("press", "status-connection")
        try await NativeSmoke.drive("press-title", "Library")
        try await NativeSmoke.drive("press", "rescan-root-\(root.id)")
        NSApp.windows.first(where: { $0.title == "tyranny likes smoke" })?.makeKeyAndOrderFront(nil)
        try await wait("folder-only artwork refresh", model) { model.snapshot?.library.scan.phase == "complete" && model.artwork.nowPlaying.state == "folder" && model.artwork.nowPlaying.cacheKey != previousKey }
        try colour(model.artwork.nowPlaying.image, 1)
        try check(model.snapshot?.current_id == folderCurrent && model.snapshot?.transport == "playing", "Rescan interrupted playback")
        try capture("folder-only-rescan")
        cases.append("native-rescan-refreshes-decoded-current-folder-image-without-changing-audio-occurrence")
        try await play("Missing Track", source: "folder", model, navigation)
        try colour(model.artwork.nowPlaying.image, 1)
        try await play("Online Track", source: "cover_art_archive", model, navigation)
        try check(model.artwork.nowPlaying.image != nil, "Online source did not decode")
        try capture("online-now-playing")
        cases.append("native-cached-missing-invalidates-and-real-online-artwork-renders")
    }

    private static func restore(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try check(model.snapshot?.transport == "paused" && model.snapshot?.now_playing?.title == "Online Track", "Restart lost current occurrence or paused resume policy")
        try await wait("offline current image", model) { model.artwork.nowPlaying.state == "cover_art_archive" && model.artwork.nowPlaying.image != nil }
        try await NativeSmoke.drive("expect-text", "now-playing-art", "cover_art_archive")
        try await NativeSmoke.drive("press", "transport-play-pause")
        try await wait("offline playback", model) { model.snapshot?.transport == "playing" && (model.snapshot?.position_frames ?? 0) > 0 }
        try await openAlbums(model, navigation, expecting: ["Offline Probe Album"])
        guard let probe = album("Offline Probe Album") else { throw Failure(message: "Offline probe album missing") }
        try await wait("neutral unavailable thumbnail", model) { model.artwork.value(for: model.artwork.albumRequest(probe.id)).state == "unavailable" }
        try await NativeSmoke.drive("exists", "album-cell-\(probe.id)")
        try check(model.snapshot?.error == nil && model.snapshot?.transport == "playing", "Artwork network failure became a playback fault")
        try capture("offline-restored-artwork")
        cases.append("native-restart-restores-online-image-offline-and-transport-remains-usable")
    }

    /// Run with an empty library and isolated preferences/Keychain domain; never use real credentials.
    private static func settings(_ model: PlayerModel) async throws {
        let settings = model.artwork.settings
        try check(settings.preferences == ArtworkPreferences() && !settings.hasToken,
                  "Artwork settings smoke requires a fresh isolated preferences/Keychain domain")
        try check(model.snapshot?.library.tracks == 0, "Settings credential smoke requires an empty isolated library")
        let saved = UserDefaults.standard.object(forKey: ArtworkSettings.preferencesKey)
        defer {
            if let saved { UserDefaults.standard.set(saved, forKey: ArtworkSettings.preferencesKey) }
            else { UserDefaults.standard.removeObject(forKey: ArtworkSettings.preferencesKey) }
        }
        try await NativeSmoke.drive("press", "status-connection")
        try await NativeSmoke.drive("press-title", "Artwork")
        try await NativeSmoke.drive("exists", "artwork-discogs-token")
        let generation = model.artwork.generation
        try await NativeSmoke.drive("press", "artwork-enabled-deezer")
        try await wait("Deezer opt-in", model) { settings.preferences.enabled.contains(.deezer) && model.artwork.generation > generation }
        try await NativeSmoke.drive("press", "artwork-up-deezer")
        try await wait("provider reorder", model) { settings.preferences.order.first == .deezer }
        try await NativeSmoke.drive("press", "artwork-up-discogs")
        try await wait("disabled provider reorder", model) { settings.preferences.order == [.deezer, .discogs, .coverArtArchive] }
        let reloaded = ArtworkSettings()
        try check(reloaded.preferences == settings.preferences, "Provider choices did not persist")
        try check(reloaded.preferences.providers == [.deezer, .coverArtArchive], "Disabled Discogs leaked into provider requests")
        try await NativeSmoke.drive("press", "artwork-enabled-discogs")
        try await NativeSmoke.drive("exists", "artwork-settings-error")
        try check(!settings.preferences.enabled.contains(.discogs), "Discogs enabled without a Keychain token")
        try await NativeSmoke.drive("press", "artwork-discogs-save")
        try check(!settings.hasToken && settings.error != nil, "An empty token was accepted")
        try await NativeSmoke.drive("press", "artwork-enabled-deezer")
        try await wait("Deezer disabled", model) { !settings.preferences.enabled.contains(.deezer) }
        try await NativeSmoke.drive("press", "artwork-enabled-cover_art_archive")
        try await wait("local-only preference", model) { settings.preferences.providers.isEmpty }
        try check(ArtworkSettings().preferences.providers.isEmpty, "Local-only preference did not persist")
        // Exercise the actual Keychain path only after every remote provider is disabled.
        // This deliberately synthetic value must never be sent to a provider or copied to reports.
        defer { if settings.hasToken { settings.removeToken() } }
        try await NativeSmoke.drive("value", "artwork-discogs-token",
                                    "tyranny-likes-smoke-native-smoke-not-a-real-discogs-token", sensitive: true)
        try await NativeSmoke.drive("press", "artwork-discogs-save")
        try check(settings.hasToken && settings.error == nil,
                  "Keychain save failed: \(settings.error ?? "token presence was not confirmed")")
        try check(settings.preferences.providers.isEmpty, "Credential smoke unexpectedly enabled a remote provider")
        let savedCredential = ArtworkSettings()
        try check(savedCredential.hasToken && savedCredential.error == nil,
                  "Keychain reload failed: \(savedCredential.error ?? "saved token was not found")")
        try await NativeSmoke.drive("press", "artwork-discogs-remove")
        try check(!settings.hasToken && settings.error == nil,
                  "Keychain removal failed: \(settings.error ?? "token remains present")")
        let removedCredential = ArtworkSettings()
        try check(!removedCredential.hasToken && removedCredential.error == nil,
                  "Keychain removal did not persist: \(removedCredential.error ?? "token remains present")")
        try capture("artwork-settings", window: NSApp.keyWindow)
        cases.append("native-keychain-save-reload-delete-with-disabled-providers-and-synthetic-token")
        cases.append("native-artwork-settings-opt-in-order-persistence-local-only-and-missing-token-validation")
    }

    /// A real remote lookup, with no local cover/MBID: Daft Punk / Discovery / One More Time.
    private static func providers(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        let settings = model.artwork.settings
        try check(settings.preferences == ArtworkPreferences() && !settings.hasToken,
                  "Provider smoke requires a fresh isolated preferences/Keychain domain")
        let saved = UserDefaults.standard.object(forKey: ArtworkSettings.preferencesKey)
        defer {
            if let saved { UserDefaults.standard.set(saved, forKey: ArtworkSettings.preferencesKey) }
            else { UserDefaults.standard.removeObject(forKey: ArtworkSettings.preferencesKey) }
        }
        try await NativeSmoke.drive("press", "status-connection")
        try await NativeSmoke.drive("press-title", "Artwork")
        try await NativeSmoke.drive("press", "artwork-enabled-deezer")
        try await wait("Deezer enabled", model) { settings.preferences.enabled.contains(.deezer) }
        NSApp.windows.first(where: { $0.title == "tyranny likes smoke" })?.makeKeyAndOrderFront(nil)
        try await openAlbums(model, navigation, expecting: ["Discovery"])
        guard let album = album("Discovery") else { throw Failure(message: "Discovery fixture missing") }
        try await wait("real Deezer cover", model) {
            let value = model.artwork.value(for: model.artwork.albumRequest(album.id))
            return value.state == "deezer" && value.image != nil
        }
        let displayed = model.artwork.value(for: model.artwork.albumRequest(album.id))
        try check(displayed.sourceURL?.host == "www.deezer.com" && displayed.cachePolicy == .displayOnly,
                  "Deezer provenance or display-only policy missing")
        try check(ArtworkBrand.deezerLogo != nil, "Official Deezer branding resource missing")
        try await NativeSmoke.drive("exists", "artwork-deezer-attribution")
        try capture("deezer-visible-attributed-cover")
        try await NativeSmoke.drive("click-control", "album-cell-\(album.id)")
        try await wait("attributed expansion", model) {
            let artwork = model.artwork.value(for: model.artwork.albumRequest(album.id))
            return NativeAlbumsSmoke.grid?.expansion?.albumID == album.id
                && NativeAlbumsSmoke.grid?.expansion?.isLoaded == true
                && artwork.state == "deezer" && artwork.image != nil
        }
        let expandedArtwork = model.artwork.value(for: model.artwork.albumRequest(album.id))
        try await NativeSmoke.drive("expect-text", "album-expansion-cover", "deezer")
        try capture("deezer-attributed-expansion")
        try await NativeSmoke.drive("press", "status-connection")
        try await NativeSmoke.drive("press-title", "Artwork")
        try await NativeSmoke.drive("press", "artwork-enabled-deezer")
        try await wait("Deezer pixels revoked", model) {
            !settings.preferences.enabled.contains(.deezer) && displayed.image == nil && expandedArtwork.image == nil
        }
        NSApp.windows.first(where: { $0.title == "tyranny likes smoke" })?.makeKeyAndOrderFront(nil)
        try await wait("disabled source cannot redisplay", model) {
            let value = model.artwork.value(for: model.artwork.albumRequest(album.id))
            return value.state == "missing" && value.image == nil
        }
        try capture("deezer-disabled-neutral-cover")
        cases.append("native-real-deezer-art-with-official-linked-branding-and-disable-revokes-pixels-without-redisplay")
    }

    /// Filters Songs to one track and double-clicks it: the run of one plays.
    private static func play(_ title: String, source: String, _ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try await NativeSmoke.drive("click-control", Destination.songs.identifier)
        try await wait("songs destination", model) { navigation.destination == .songs && NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw Failure(message: "Songs list missing") }
        try await NativeSmoke.drive("value", "library-filter", "title:\"\(title)\"")
        try await wait("track \(title)", model) { songs.cache.total == 1 && songs.cache.loadedRow(at: 0)?.track.title == title }
        try await NativeSmoke.drive("double-click-row", "track-table-songs", "0,0.5")
        try await wait("play \(title)", model) { model.snapshot?.transport == "playing" && model.snapshot?.now_playing?.title == title && model.artwork.nowPlaying.state == source }
        try await NativeSmoke.drive("expect-text", "now-playing-art", source)
    }

    private static func tintedAccent(_ model: PlayerModel, matches: (RGB) -> Bool) -> Bool {
        if case .tinted(let rgb) = model.theme.tokens.accent { return matches(rgb) }
        return false
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
    private static func capture(_ name: String, window: NSWindow? = nil) throws {
        guard let window = window ?? NSApp.windows.first(where: { $0.title == "tyranny likes smoke" }) else { throw Failure(message: "Native window missing") }
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
