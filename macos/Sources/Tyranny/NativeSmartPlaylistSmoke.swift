#if DEBUG
import AppKit
import Foundation

/// Public AX controls and real mouse activation; model state is observation only.
@MainActor
enum NativeSmartPlaylistSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-smart-smoke")
    private static var cases: [String] = []
    private static let rule = "artist:\"Miles Davis\" & !album:\"Kind of Blue\""

    static func startIfRequested(_ model: PlayerModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--smart-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("connected indexed library", model) { model.connected && model.libraryReady && model.playlists != nil }
                try await NativeSmoke.drive("activate")
                if argument("--smart-smoke-mode") == "restore" { try await restore(model) }
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
        try check(model.snapshot?.library.tracks == 508 && model.snapshot?.queue_length == 0, "Unexpected initial smart fixture")
        try await value("library-filter", rule)
        try await wait("library rule", model) { model.libraryReady && model.tracks?.total == 506 }
        guard let album = model.albums?.entries.firstIndex(where: { $0.title == "Sessions" }) else { throw Failure(message: "Sessions album missing") }
        try await select("album-list", [album])
        try await wait("album-scoped browsing", model) { model.libraryReady && model.tracks?.total == 505 }
        try await press("library-save-smart")
        try await value("smart-name", "Miles without Blue")
        try await expect("smart-preview-count", "506")
        try await value("smart-query", "artist:[]")
        try await NativeSmoke.drive("exists", "smart-preview-error")
        try await NativeSmoke.drive("expect-enabled", "smart-save", "false")
        try capture("invalid-rule")
        let edits = try JSONSerialization.data(withJSONObject: ["artist:Ornette", "album:[", rule])
        try await NativeSmoke.drive("values", "smart-query", String(decoding: edits, as: UTF8.self))
        try await expect("smart-preview-count", "506")
        try capture("global-rule-preview")
        try await press("smart-save")
        try await wait("saved smart rule", model) { !model.busy && model.playlistReady && model.currentPlaylist?.name == "Miles without Blue" }
        try check(model.currentPlaylist?.isSmart == true && model.currentPlaylist?.query == rule && model.playlistEntries?.total == 506, "Saved rule differed from global preview")
        try check(model.playlistEntries?.entries.count == 100, "Smart results were not bounded")
        cases.append("native-save-global-rule-from-album-scope-invalid-preview-and-latest-query-recovery")

        try await press("playlist-entries-next")
        try await wait("second smart page", model) { model.playlistReady && model.playlistEntries?.offset == 100 }
        try check(model.playlistEntries?.entries.first?.metadata.title == "Page 099", "Smart page ordering skipped a matching track")
        try await press("playlist-entries-previous")
        try await wait("first smart page", model) { model.playlistReady && model.playlistEntries?.offset == 0 }
        try await value("playlist-filter", "Ordinary Fixture")
        try await wait("smart selection outside visible catalog", model) { !model.playlistsLoading && model.playlists?.total == 1 && model.playlists?.entries.first?.name == "Ordinary Fixture" }
        try check(model.currentPlaylist?.name == "Miles without Blue" && model.playlistReady, "Catalog filtering lost selected smart authority")
        try await value("playlist-filter", "")
        try await wait("mixed catalog", model) { !model.playlistsLoading && model.playlists?.total == 2 }
        try capture("smart-results")
        cases.append("native-bounded-smart-pages-and-selection-retained-outside-catalog-filter")

        try await press("toggle-playlists")
        try await value("library-filter", "title:Freedom")
        try await wait("picker library selection", model) { model.libraryReady && model.tracks?.total == 1 }
        try await select("track-table", [0])
        try await wait("selected library track", model) { model.selectedTracks.count == 1 }
        try await press("add-to-playlist")
        try await expect("playlist-picker-list-range", "1–1 of 1")
        try await value("playlist-picker-filter", "Miles without Blue")
        try await expect("playlist-picker-list-range", "0 items")
        try await NativeSmoke.drive("expect-enabled", "playlist-picker-add", "false")
        try await NativeSmoke.drive("press-title", "Cancel")
        try await press("toggle-playlists")
        try await choose("Miles without Blue", model)
        cases.append("native-add-to-playlist-picker-excludes-smart-rules-without-changing-catalog")

        try await press("playlist-edit-rule")
        try await value("smart-query", "artist:[\"Ornette Coleman\"] & !album:\"Kind of Blue\"")
        try await expect("smart-preview-count", "1")
        try await press("smart-save")
        try await wait("edited rule results", model) { !model.busy && model.playlistReady && model.playlistEntries?.entries.map(\.metadata.title) == ["Lonely Woman"] }
        try await press("playlist-edit-rule")
        try await value("smart-query", rule)
        try await expect("smart-preview-count", "506")
        try await press("smart-save")
        try await wait("restored rule", model) { !model.busy && model.playlistReady && model.currentPlaylist?.query == rule && model.playlistEntries?.total == 506 }
        cases.append("native-edit-smart-rule-replaces-live-membership")

        try await entryFilter("title:NoSuchMatchingTrack", total: 0, model)
        try await press("playlist-snapshot")
        try await value("snapshot-name", "Frozen Miles")
        try await press("snapshot-create")
        try await wait("regular full-result snapshot", model) { !model.busy && model.playlistReady && model.currentPlaylist?.name == "Frozen Miles" }
        try check(model.currentPlaylist?.isSmart == false && model.currentPlaylist?.entry_count == 506, "Snapshot copied transient view instead of full rule")
        if model.playlistEntryFilterActive { try await entryFilter("", total: 506, model) }
        let frozenPage = model.playlistEntries!.entries
        try check(frozenPage.first?.metadata.title == "Freedom" && frozenPage.last?.metadata.title == "Page 098", "Regular snapshot lost full-result ordering")
        try await select("playlist-entry-table", [0])
        try await NativeSmoke.drive("expect-enabled", "playlist-remove", "true")
        cases.append("native-regular-snapshot-captures-all-results-over-500-and-is-editable")

        try await choose("Miles without Blue", model)
        try await entryFilter("title:NoSuchMatchingTrack", total: 0, model)
        try await press("playlist-enqueue")
        try await wait("whole smart enqueue from empty transient view", model) { !model.busy && model.snapshot?.queue_length == 506 }
        try check(model.snapshot?.transport != "playing", "Enqueue started playback")
        try await press("playlist-play")
        try await wait("whole smart play", model) { !model.busy && model.snapshot?.queue_length == 1012 && model.snapshot?.transport == "playing" }
        try check(model.snapshot?.now_playing?.title == "Freedom", "Smart Play did not begin at first rule result")
        try await entryFilter("", total: 506, model)
        try await NativeSmoke.drive("double-click-row", "playlist-entry-table", "1,0.35")
        try await wait("double-click smart result", model) { !model.busy && model.snapshot?.queue_length == 1013 && model.snapshot?.transport == "playing" && model.snapshot?.now_playing?.title == "Page 000" }
        let queueRevision = model.snapshot!.queue_revision
        let current = model.snapshot!.current_id
        cases.append("native-whole-smart-enqueue-play-ignore-transient-filter-and-double-click-plays-one-result")

        try await select("playlist-entry-table", [0, 2])
        try await wait("selection before library change", model) { model.selectedPlaylistEntries.count == 2 }
        guard let source = argument("--smart-addition-source"), let folder = argument("--smart-music-folder"), let root = model.roots?.entries.first else {
            throw Failure(message: "Missing native import fixture")
        }
        try FileManager.default.copyItem(at: URL(fileURLWithPath: source), to: URL(fileURLWithPath: folder).appendingPathComponent("native-arrival.flac"))
        try await press("show-roots")
        try await press("rescan-root-\(root.id)")
        try await NativeSmoke.drive("press-title", "Done")
        try await wait("automatic new-album membership", model) { model.snapshot?.library.tracks == 509 && model.snapshot?.library.scan.phase == "complete" && model.playlistReady && model.playlistEntries?.total == 507 }
        try check(model.selectedPlaylistEntries.isEmpty, "Library revision left stale smart selection actionable")
        try await entryFilter("album:Nefertiti", total: 1, model)
        try check(model.playlistEntries?.entries.first?.metadata.title == "Sparks", "New album did not join saved rule")
        try check(model.snapshot?.queue_revision == queueRevision && model.snapshot?.current_id == current && model.snapshot?.transport == "playing", "Library refresh mutated enqueued occurrences or playback")
        try capture("new-album-live-membership")
        try await choose("Frozen Miles", model)
        if model.playlistEntryFilterActive { try await entryFilter("", total: 506, model) }
        try check(model.currentPlaylist?.entry_count == 506 && model.playlistEntries?.entries == frozenPage, "Library import changed frozen occurrence order or identity")
        try await choose("Miles without Blue", model)
        if model.playlistEntryFilterActive { try await entryFilter("", total: 507, model) }
        try check(model.snapshot?.underruns == 0, "Native smart workflows underrun captured playback")
        cases.append("native-rescan-updates-smart-membership-clears-selection-and-preserves-queue-and-regular-snapshot")
    }

    private static func restore(_ model: PlayerModel) async throws {
        try check(model.snapshot?.queue_length == 1013 && model.snapshot?.transport == "paused", "Restart changed independent queue or resume policy")
        try await press("toggle-playlists")
        try await choose("Miles without Blue", model)
        try check(model.currentPlaylist?.query == rule && model.currentPlaylist?.isSmart == true && model.playlistEntries?.total == 507, "Smart rule or membership did not survive restart")
        try await entryFilter("album:Nefertiti", total: 1, model)
        try check(model.playlistEntries?.entries.first?.metadata.title == "Sparks", "Restored rule omitted imported album")
        try await choose("Frozen Miles", model)
        if model.playlistEntryFilterActive { try await entryFilter("", total: 506, model) }
        try check(model.currentPlaylist?.isSmart == false && model.currentPlaylist?.entry_count == 506, "Frozen regular snapshot did not survive restart")
        try capture("restored-smart-and-regular-playlists")
        cases.append("native-relaunch-retains-live-rule-regular-snapshot-and-paused-independent-queue")
    }

    private static func argument(_ key: String) -> String? {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: key), index + 1 < args.count else { return nil }
        return args[index + 1]
    }
    private static func choose(_ name: String, _ model: PlayerModel) async throws {
        try await wait("catalog ready", model) { !model.playlistsLoading && model.playlists?.entries.contains(where: { $0.name == name }) == true }
        let index = model.playlists!.entries.firstIndex { $0.name == name }!
        try await select("playlist-list", [index])
        try await wait("selected \(name)", model) { model.playlistReady && model.currentPlaylist?.name == name }
    }
    private static func entryFilter(_ query: String, total: Int, _ model: PlayerModel) async throws {
        try await value("playlist-entry-filter", query)
        try await wait("entry filter \(query)", model) { model.playlistReady && model.playlistEntryFilter == query && model.playlistEntries?.total == total }
    }
    private static func value(_ id: String, _ text: String) async throws { try await NativeSmoke.drive("value", id, text) }
    private static func press(_ id: String) async throws { try await NativeSmoke.drive("press", id) }
    private static func expect(_ id: String, _ text: String) async throws { try await NativeSmoke.drive("expect-text", id, text) }
    private static func select(_ id: String, _ rows: [Int]) async throws { try await NativeSmoke.drive("select", id, rows.map(String.init).joined(separator: ",")) }
    private static func wait(_ label: String, _ model: PlayerModel, until predicate: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(25)
        while !predicate() {
            if Date() >= deadline { throw Failure(message: "Timed out: \(label); model=\(model.message ?? "none"); playlist=\(model.playlistError ?? "none"); filter=\(model.playlistEntryFilterError ?? "none")") }
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
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases,
            "automation": "Actual AX text/buttons and mouse activation, tagged FLAC import via native Rescan; model observation only; isolated clock-paced PCM capture."]
        if let error { report["error"] = error }
        if let state = model.snapshot { report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state)) }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]).write(to: output.appendingPathComponent("report.json"))
        print("SMART_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
