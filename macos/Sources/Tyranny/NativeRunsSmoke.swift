#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for runs from track lists (`--runs-smoke-output`, spec 8). The gate
/// prepares the 1,129-track songs library, an empty queue and no playlists. Songs is narrowed to
/// ten long untagged tracks; rows are double-clicked through the external Accessibility driver
/// and the queue is read back from the server in order. ⌘A is the table's own `selectAll`.
@MainActor
enum NativeRunsSmoke {
    private static var started = false
    private static let table = "track-table-songs"
    private static let defaultsKey = "upNextOnPlay"

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--runs-smoke-output"), index + 1 < args.count else { return }
        started = true
        let kit = NativePlaylistKit(model: model, navigation: navigation,
                                    output: URL(fileURLWithPath: args[index + 1], isDirectory: true), tag: "RUNS_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                try await kit.wait("connection and catalog") {
                    model.connected && model.snapshot?.library.tracks == 1129
                        && model.snapshot?.library.scan.phase == "complete" && model.savedPlaylists.isLoaded
                }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                try await exercise(kit)
            } catch {
                try? kit.capture("runs-failure")
                failure = error.localizedDescription
            }
            UserDefaults.standard.removeObject(forKey: defaultsKey)
            try? kit.finish(automation: "External Accessibility row double-clicks, sheet buttons and the playlist picker on the displayed views; ⌘A is the table's selectAll action; the queue is read back from the server in order.",
                            error: failure)
            NativePlaylistKit.endSheets()
            NSApp.terminate(nil)
        }
    }

    // MARK: Helpers

    private struct Entry {
        let id: UInt64
        let trackID: UInt64?
    }

    private static func queue(_ model: PlayerModel) async throws -> [Entry] {
        let page: Page<QueueItem> = try await model.send("queue_page", [
            "revision": .null, "offset": .int(0), "limit": .int(500)])
        return page.entries.map { Entry(id: $0.id, trackID: $0.track_id) }
    }

    /// The queue's track IDs from the current entry on.
    private static func fromCurrent(_ model: PlayerModel) async throws -> [UInt64?] {
        let entries = try await queue(model)
        guard let current = model.snapshot?.current_id, let index = entries.firstIndex(where: { $0.id == current }) else {
            throw NativePlaylistKit.Failure(message: "Nothing is current")
        }
        return entries[index...].map(\.trackID)
    }

    private static func clearQueue(_ kit: NativePlaylistKit) async throws {
        guard let state = kit.model.snapshot else { return }
        let _: Applied = try await kit.model.send("stop")
        let fresh = kit.model.snapshot ?? state
        let _: Applied = try await kit.model.send("clear", ["expected_revision": .uint(fresh.queue_revision)])
        try await kit.wait("queue cleared") { kit.model.snapshot?.queue_length == 0 && !kit.model.busy }
    }

    private static func doubleClick(_ kit: NativePlaylistKit, row: Int) async throws {
        try await kit.drive("double-click-row", table, "\(row),0.5")
    }

    private static func settle(_ kit: NativePlaylistKit, until condition: () -> Bool, _ label: String) async throws {
        try await kit.wait(label, until: condition)
        try await kit.wait("idle") { !kit.model.busy }
    }

    // MARK: Scenario

    private static func exercise(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        try kit.check(model.snapshot?.queue_length == 0 && model.savedPlaylists.cache.total == 0,
                      "The gate needs an empty queue and no playlists")
        UserDefaults.standard.set(UpNextPolicy.ask.rawValue, forKey: defaultsKey)
        try await kit.open(.songs)
        try await kit.wait("songs list") { NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw NativePlaylistKit.Failure(message: "Songs list missing") }

        // Ten long tracks, in title order: Untagged 0000 ... 0009.
        try await kit.value("library-filter", "\"Untagged 000\"")
        try await kit.wait("ten rows") { songs.cache.total == 10 && songs.cache.loadedRow(at: 9) != nil }
        let rows = (0..<10).compactMap { songs.cache.loadedRow(at: $0) }
        let ids = rows.map { Optional($0.track.id) }
        try kit.check(rows.count == 10, "Ten rows were not loaded")

        // Up Next empty: a double-click plays the run from that row, with no sheet.
        try await doubleClick(kit, row: 2)
        try await settle(kit, until: { model.snapshot?.queue_length == 8 }, "run of eight")
        try kit.check(model.activation.prompt == nil, "A sheet appeared with Up Next empty")
        var tail = try await fromCurrent(model)
        try kit.check(tail == Array(ids[2...]), "Empty queue: run is not rows 2...9")
        let entries = try await queue(model)
        try kit.check(entries.count == 8, "Empty queue: expected eight entries")
        kit.passed("native-runs-double-click-with-empty-up-next-plays-the-run")

        // Ask, Keep: the sheet appears, Keep puts the run next and keeps the rest.
        let before = Array(ids[3...])   // the old upcoming entries
        try await doubleClick(kit, row: 0)
        try await kit.wait("sheet") { model.activation.prompt != nil }
        try await kit.drive("exists", "up-next-prompt")
        try await kit.drive("exists", "up-next-prompt-clear")
        try await kit.press("up-next-prompt-keep")
        try await settle(kit, until: { model.snapshot?.queue_length == 18 }, "keep run")
        tail = try await fromCurrent(model)
        try kit.check(tail == ids + before, "Ask/Keep: order is not new run then old upcoming")
        try kit.check(UserDefaults.standard.string(forKey: defaultsKey) == "ask", "Keep without remember changed the policy")
        kit.passed("native-runs-ask-keep-puts-the-run-next-and-keeps-the-rest")

        // Replace policy: upcoming entries go; only the run follows the current entry.
        UserDefaults.standard.set(UpNextPolicy.replace.rawValue, forKey: defaultsKey)
        try await doubleClick(kit, row: 5)
        try await kit.wait("replaced") { !model.busy && model.activation.prompt == nil }
        try await Task.sleep(for: .milliseconds(400))
        tail = try await fromCurrent(model)
        try kit.check(tail == Array(ids[5...]), "Replace: entries from the current one are not rows 5...9")
        kit.passed("native-runs-replace-policy-clears-upcoming")

        // Keep policy: no sheet, the run goes next.
        UserDefaults.standard.set(UpNextPolicy.keep.rawValue, forKey: defaultsKey)
        let upcoming = Array(ids[6...])
        try await doubleClick(kit, row: 8)
        try await kit.wait("keep policy run") { !model.busy && model.activation.prompt == nil }
        try await Task.sleep(for: .milliseconds(400))
        tail = try await fromCurrent(model)
        try kit.check(tail == Array(ids[8...]) + upcoming, "Keep policy: order is not the run then the old upcoming")
        kit.passed("native-runs-keep-policy-puts-the-run-next")

        // "Don't ask again" persists, and the next play skips the sheet.
        UserDefaults.standard.set(UpNextPolicy.ask.rawValue, forKey: defaultsKey)
        try await doubleClick(kit, row: 3)
        try await kit.wait("sheet") { model.activation.prompt != nil }
        try await kit.press("up-next-prompt-remember")
        try await kit.press("up-next-prompt-clear")
        try await kit.wait("clear run") { model.activation.prompt == nil && !model.busy }
        try kit.check(UserDefaults.standard.string(forKey: defaultsKey) == "replace", "Don't ask again did not store Replace")
        try await Task.sleep(for: .milliseconds(400))
        try await doubleClick(kit, row: 6)
        try await kit.wait("no sheet") { !model.busy }
        try await Task.sleep(for: .milliseconds(400))
        try kit.check(model.activation.prompt == nil, "The sheet came back after Don't ask again")
        tail = try await fromCurrent(model)
        try kit.check(tail == Array(ids[6...]), "After Don't ask again: entries from the current one are not rows 6...9")
        try kit.capture("runs")
        kit.passed("native-runs-dont-ask-again-persists")

        // ⌘A on the whole of Songs, then Add to Playlist: every row in one playlist revision.
        try await clearQueue(kit)
        try await kit.value("library-filter", "")
        try await kit.wait("all songs") { songs.cache.total == 1129 && songs.cache.loadedRow(at: 0) != nil }
        let created: PlaylistApplied = try await model.send("create_playlist", [
            "expected_revision": .uint(model.snapshot?.playlists_revision ?? 0), "name": .string("Runs Playlist")])
        guard let playlist = created.playlist else { throw NativePlaylistKit.Failure(message: "Playlist was not created") }
        let revision = playlist.revision
        try await kit.drive("click-row", table, "0,0.5")
        _ = NSApp.sendAction(#selector(NSTableView.selectAll(_:)), to: nil, from: nil)
        try await kit.wait("all rows selected") { songs.selectedPositions.count == 1129 }
        try await kit.press("add-to-playlist")
        try await kit.drive("exists", "playlist-picker")
        try await kit.wait("playlist in picker") { kit.playlist(named: "Runs Playlist") != nil }
        try await kit.select("playlist-picker-list", [0])
        try await kit.press("playlist-picker-add")
        let filled = try await kit.waitForPlaylist("Runs Playlist", entries: 1129)
        try kit.check(filled.revision == revision + 1, "Add took \(filled.revision - revision) revisions, expected one")
        kit.passed("native-runs-select-all-add-to-playlist-adds-every-row-in-one-revision")
        kit.observations["playlist"] = ["id": filled.id, "revision": filled.revision, "start_revision": revision]
    }
}
#endif
