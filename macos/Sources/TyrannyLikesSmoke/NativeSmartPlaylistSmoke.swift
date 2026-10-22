#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Opt-in acceptance driver for smart playlists (`--smart-smoke-output`, `--smart-smoke-mode
/// exercise|restore`). The gate prepares 508 tracks and a regular playlist "Ordinary Fixture".
///
/// Rules are made and edited in the editor sheet, copies in the snapshot sheet, and rows are
/// opened from the source list, all through the external Accessibility driver. The playlist's
/// context menu cannot be opened that way, so nothing here renames or deletes. A run is
/// enqueued over the wire (double-click arrives in Task 13), and the import is started with the
/// model's rescan.
@MainActor
enum NativeSmartPlaylistSmoke {
    private static var started = false
    private static let rule = "artist:\"Miles Davis\" & !album:\"Kind of Blue\""
    private static let smart = "Miles without Blue"
    private static let frozen = "Frozen Miles"

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--smart-smoke-output"), index + 1 < args.count else { return }
        started = true
        let kit = NativePlaylistKit(model: model, navigation: navigation,
                                    output: URL(fileURLWithPath: args[index + 1], isDirectory: true), tag: "SMART_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                try await kit.wait("connected indexed library", seconds: 30) {
                    model.connected && model.snapshot?.library.tracks ?? 0 >= 508 && model.savedPlaylists.isLoaded
                        && model.snapshot?.library.scan.phase == "complete"
                }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                if argument("--smart-smoke-mode") == "restore" { try await restore(kit) }
                else { try await exercise(kit) }
            } catch {
                try? kit.capture("failure")
                failure = error.localizedDescription
            }
            try? kit.finish(automation: "External Accessibility clicks and text entry in the editor and snapshot sheets and the source list; a tagged FLAC import started with the model's rescan; server reads only for expected values; isolated clock-paced PCM capture.",
                            error: failure)
            NativePlaylistKit.endSheets()
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        try kit.check(model.snapshot?.library.tracks == 508 && model.snapshot?.queue_length == 0, "Unexpected initial smart fixture")

        // New Smart Playlist…: an invalid rule, then the global rule.
        try await kit.drive("choose", "source-add", "New Smart Playlist…")
        try await kit.value("smart-name", smart)
        try await kit.value("smart-query", rule)
        try await kit.drive("expect-text", "smart-preview-count", "506 matching tracks across the library")
        try await kit.value("smart-query", "artist:[]")
        try await kit.drive("exists", "smart-preview-error")
        try await kit.drive("expect-enabled", "smart-save", "false")
        try kit.capture("invalid-rule")
        let edits = try JSONSerialization.data(withJSONObject: ["artist:Ornette", "album:[", rule])
        try await kit.drive("values", "smart-query", String(decoding: edits, as: UTF8.self))
        try await kit.drive("expect-text", "smart-preview-count", "506 matching tracks across the library")
        try kit.capture("global-rule-preview")
        try await kit.press("smart-save")
        let created = try await kit.waitForPlaylist(smart)
        try kit.check(created.isSmart && created.query == rule, "The saved rule differed from the preview")
        try await kit.wait("opened") { kit.navigation.destination == .playlist(created.id) }
        let list = try await shown(kit, created)
        try await kit.wait("rule results") { list.cache.total == 506 && list.cache.loadedRow(at: 0) != nil }
        try kit.check(list.cache.loadedRow(at: 0)?.track.title == "Freedom", "Smart results do not start at the first match")
        kit.passed("native-new-smart-playlist-previews-invalid-and-latest-rule-then-saves")

        // Past the first window: page 099 is row 100.
        try await kit.drive("scroll-fraction", "track-table-playlist-\(created.id)", "1")
        try await kit.wait("last window") { list.cache.loadedRow(at: 505) != nil && list.cache.loadedRow(at: 100) != nil }
        try await kit.drive("scroll-fraction", "track-table-playlist-\(created.id)", "0")
        try kit.check(list.cache.loadedRow(at: 100)?.track.title == "Page 099", "Smart ordering skipped a matching track")
        try kit.capture("smart-results")
        kit.passed("native-smart-results-load-by-window-in-rule-order")

        // The picker lists regular playlists only.
        try await kit.open(.songs)
        try await kit.value("library-filter", "title:Freedom")
        guard let songs = NativeSongsSmoke.songsList else { throw NativePlaylistKit.Failure(message: "Songs list missing") }
        try await kit.wait("filtered songs") { songs.cache.total == 1 && songs.cache.loadedRow(at: 0) != nil }
        try await kit.select("track-table-songs", [0])
        try await kit.wait("one row selected") { songs.selectedPositions.count == 1 }
        try await kit.press("add-to-playlist")
        try await kit.drive("expect-row-count", "playlist-picker-list", "1")
        try await kit.value("playlist-picker-filter", smart)
        try await kit.drive("expect-row-count", "playlist-picker-list", "0")
        try await kit.drive("expect-enabled", "playlist-picker-add", "false")
        try await kit.press("playlist-picker-cancel")
        try await kit.value("library-filter", "")
        kit.passed("native-add-to-playlist-picker-excludes-smart-playlists")

        // Edit Rule…
        try await kit.open(.playlist(created.id))
        try await kit.press("playlist-edit-rule")
        try await kit.value("smart-query", "artist:[\"Ornette Coleman\"] & !album:\"Kind of Blue\"")
        try await kit.drive("expect-text", "smart-preview-count", "1 matching tracks across the library")
        try await kit.press("smart-save")
        try await kit.wait("edited rule results") {
            !model.busy && list.cache.total == 1 && list.cache.loadedRow(at: 0)?.track.title == "Lonely Woman"
        }
        try await kit.press("playlist-edit-rule")
        try await kit.value("smart-query", rule)
        try await kit.drive("expect-text", "smart-preview-count", "506 matching tracks across the library")
        try await kit.press("smart-save")
        try await kit.wait("restored rule") { !model.busy && list.cache.total == 506 }
        try kit.check(kit.playlist(named: smart)?.query == rule, "The rule was not restored")
        kit.passed("native-edit-rule-replaces-live-membership-in-place")

        // Create Regular Playlist from Results…: all 506, not the filtered view.
        try await kit.value("library-filter", "title:NoSuchMatchingTrack")
        try await kit.wait("filter narrows the view") { list.cache.total == 0 }
        try await kit.press("playlist-snapshot")
        try await kit.value("snapshot-name", frozen)
        try await kit.press("snapshot-create")
        let copy = try await kit.waitForPlaylist(frozen, entries: 506)
        try await kit.value("library-filter", "")
        try kit.check(!copy.isSmart, "The snapshot is not a regular playlist")
        let frozenRows = try await kit.entries(copy)
        try kit.check(frozenRows.first?.metadata.title == "Freedom" && frozenRows.last?.metadata.title == "Page 098",
                      "The copy lost the full-result order")
        kit.passed("native-regular-copy-captures-all-results-over-500")

        // Enqueue and Play All take the whole playlist, whatever the filter shows.
        try await kit.open(.playlist(created.id))
        try await kit.value("library-filter", "title:NoSuchMatchingTrack")
        try await kit.wait("filter narrows the view") { list.cache.total == 0 }
        try await kit.press("playlist-enqueue")
        try await kit.wait("whole smart enqueue") { !model.busy && model.snapshot?.queue_length == 506 }
        try kit.check(model.snapshot?.transport != "playing", "Enqueue started playback")
        try await kit.press("playlist-play")
        try await kit.wait("whole smart play") {
            !model.busy && model.snapshot?.queue_length == 1012 && model.snapshot?.transport == "playing"
        }
        try kit.check(model.snapshot?.now_playing?.title == "Freedom", "Play All did not begin at the first result")
        try await kit.value("library-filter", "")
        try await kit.wait("full results") { list.cache.total == 506 && list.cache.loadedRow(at: 1) != nil }
        // Double-click arrives in Task 13: play the second result by enqueueing it over the wire.
        guard let second = list.cache.loadedRow(at: 1)?.track else { throw NativePlaylistKit.Failure(message: "Second result missing") }
        let _: Applied = try await model.send("enqueue_tracks", [
            "selection": try JSONValue.encoding(TrackSelection.ids(libraryRevision: model.snapshot!.library.revision, trackIDs: [second.id])),
            "expected_revision": .uint(model.snapshot!.queue_revision),
            "placement": .string(Placement.append.rawValue), "play": .bool(true),
            "source": try JSONValue.encoding(QueueSource(kind: .songs, name: "Songs"))])
        try await kit.wait("one result played") {
            model.snapshot?.queue_length == 1013 && model.snapshot?.transport == "playing"
                && model.snapshot?.now_playing?.title == "Page 000"
        }
        let queueRevision = model.snapshot!.queue_revision
        let current = model.snapshot!.current_id
        kit.passed("native-whole-smart-enqueue-and-play-ignore-the-filter")

        // A new album joins the rule's results; the queue and the copy do not change.
        guard let source = argument("--smart-addition-source"), let folder = argument("--smart-music-folder"),
              let root = model.roots?.entries.first else {
            throw NativePlaylistKit.Failure(message: "Missing native import fixture")
        }
        try FileManager.default.copyItem(at: URL(fileURLWithPath: source),
                                         to: URL(fileURLWithPath: folder).appendingPathComponent("native-arrival.flac"))
        model.rescan(root.id)
        try await kit.wait("automatic new-album membership", seconds: 40) {
            model.snapshot?.library.tracks == 509 && model.snapshot?.library.scan.phase == "complete" && list.cache.total == 507
        }
        try await kit.value("library-filter", "album:Nefertiti")
        try await kit.wait("new album") { list.cache.total == 1 && list.cache.loadedRow(at: 0) != nil }
        try kit.check(list.cache.loadedRow(at: 0)?.track.title == "Sparks", "The new album did not join the saved rule")
        try await kit.value("library-filter", "")
        try kit.check(model.snapshot?.queue_revision == queueRevision && model.snapshot?.current_id == current
                      && model.snapshot?.transport == "playing", "The library refresh changed the queue or playback")
        try kit.capture("new-album-live-membership")
        try await kit.open(.playlist(copy.id))
        try await kit.wait("copy") { NativePlaylistKit.shown?.destination == .playlist(copy.id) }
        try kit.check(try await kit.entries(copy).map(\.id) == frozenRows.map(\.id), "The import changed the regular copy")
        try kit.check(model.snapshot?.underruns == 0, "The smart workflows underran captured playback")
        kit.passed("native-rescan-updates-smart-membership-and-keeps-the-queue-and-copy")
    }

    private static func restore(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        try kit.check(model.snapshot?.queue_length == 1013 && model.snapshot?.transport == "paused",
                      "Restart changed the independent queue or resume policy")
        guard let smartList = kit.playlist(named: smart), let copy = kit.playlist(named: frozen) else {
            throw NativePlaylistKit.Failure(message: "The smart playlist or its copy is missing")
        }
        try kit.check(smartList.isSmart && smartList.query == rule, "The rule did not survive the restart")
        try await kit.open(.playlist(smartList.id))
        let list = try await shown(kit, smartList)
        try await kit.wait("restored results") { list.cache.total == 507 }
        try await kit.value("library-filter", "album:Nefertiti")
        try await kit.wait("restored new album") { list.cache.total == 1 && list.cache.loadedRow(at: 0) != nil }
        try kit.check(list.cache.loadedRow(at: 0)?.track.title == "Sparks", "The restored rule omitted the imported album")
        try await kit.value("library-filter", "")
        try await kit.open(.playlist(copy.id))
        try kit.check(!copy.isSmart && copy.entry_count == 506, "The regular copy did not survive the restart")
        try kit.capture("restored-smart-and-regular-playlists")
        kit.passed("native-relaunch-retains-the-rule-the-copy-and-the-paused-queue")
    }

    /// The list on screen for `playlist`, once it shows.
    private static func shown(_ kit: NativePlaylistKit, _ playlist: Playlist) async throws -> ListModel {
        try await kit.wait("playlist list") { NativePlaylistKit.shown?.destination == .playlist(playlist.id) }
        guard let list = NativePlaylistKit.shown else { throw NativePlaylistKit.Failure(message: "Playlist list missing") }
        return list
    }

    private static func argument(_ key: String) -> String? {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: key), index + 1 < args.count else { return nil }
        return args[index + 1]
    }
}
#endif
