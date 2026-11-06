#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Opt-in real-server command-language regression. No command-line UI or visual verification.
@MainActor
enum NativeCommandSmoke {
    private static var started = false

    static func startIfRequested(_ player: PlayerModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--command-smoke-output"), index + 1 < args.count else { return }
        started = true
        let output = URL(fileURLWithPath: args[index + 1])
        Task {
            var cases: [String] = []
            var failure: String?
            do {
                guard let folder = args.firstIndex(of: "--command-smoke-folder"), folder + 1 < args.count else {
                    throw CommandLanguageError("--command-smoke-folder is required")
                }
                try await exercise(player, folder: args[folder + 1], cases: &cases)
            } catch { failure = error.localizedDescription }
            var report: [String: Any] = ["passed": failure == nil, "cases": cases, "visual_verification": false]
            if let failure { report["failure"] = failure }
            do {
                try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]).write(to: output)
            } catch { print(error) }
            print("COMMAND_SMOKE \(failure == nil ? "PASS" : "FAIL") \(output.path)")
            NSApp.terminate(nil)
        }
    }

    private static func check(_ condition: Bool, _ message: String) throws {
        guard condition else { throw CommandLanguageError(message) }
    }

    private static func wait(_ label: String, until condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(20)
        while !condition() {
            guard Date() < deadline else { throw CommandLanguageError("Timed out: \(label)") }
            try await Task.sleep(for: .milliseconds(25))
        }
    }

    private static func exercise(_ player: PlayerModel, folder: String, cases: inout [String]) async throws {
        let suite = "dev.tyranny-likes-smoke.command-smoke.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let navigation = NavigationModel(defaults: defaults)
        let lists = TrackLists()
        let commands = CommandDispatcher(player: player, navigation: navigation, lists: lists)
        navigation.commandSubmission = { try await commands.execute($0) }
        try await wait("connection") { player.connected && player.snapshot != nil }
        await player.addRoot(folder)
        try await wait("three imported tracks") {
            player.snapshot?.library.scan.phase == "complete" && player.snapshot?.library.tracks == 3 && !player.busy
        }
        try await commands.execute("browse-library")
        try await commands.execute(":go songs")
        let songs = lists.list(for: .songs, source: .library, player: player, navigation: navigation)
        songs.sync()
        songs.cache.visible(0..<3)
        try await wait("songs loaded") { songs.cache.loadedRow(at: 2) != nil }
        let trackIDs = (0..<3).compactMap { songs.cache.loadedRow(at: $0)?.track.id }
        try await commands.execute("selection-all")
        try await commands.execute("selection-enqueue")
        try await wait("three queue entries") { player.snapshot?.queue_length == 3 && !player.busy }
        let queue: Page<QueueItem> = try await player.send("queue_page", ["revision": .null, "offset": .int(0), "limit": .int(10)])
        try check(queue.entries.map(\.track_id) == trackIDs.map(Optional.some), "Selection enqueue changed track order")
        cases.append("selected-library-tracks-enqueued-in-order")

        try await commands.execute("play")
        try await wait("playback") { player.snapshot?.transport == "playing" }
        try await commands.execute("pause")
        try await wait("paused") { player.snapshot?.transport == "paused" }
        try await commands.execute("seek 10s")
        try await commands.execute(":seek +1m")
        try await wait("relative minute seek") { player.positionSeconds == 70 }
        try await commands.execute("seek -10s")
        try await wait("relative backward seek") { player.positionSeconds == 60 }
        try await commands.execute("seek 50%")
        try await wait("percentage seek") {
            guard let state = player.snapshot, let duration = state.now_playing?.duration_frames else { return false }
            return state.position_frames == duration / 2
        }
        let occurrence = player.snapshot?.current_id
        try await commands.execute("next")
        try await wait("next occurrence") { player.snapshot?.current_id != occurrence }
        try await commands.execute("previous")
        try await wait("previous occurrence") { player.snapshot?.current_id == occurrence }
        cases.append("absolute-relative-percent-seek-and-occurrence-navigation")

        for command in ["volume 40%", "volume +5", "repeat all", "shuffle on", "gain album", "resume off"] {
            try await commands.execute(command)
        }
        try check(player.snapshot?.settings.volume == 45 && player.snapshot?.settings.repeat == .all
            && player.snapshot?.settings.shuffle == true && player.snapshot?.settings.gain_mode == "album"
            && player.snapshot?.settings.resume_on_start == false, "Settings were not committed by server")
        cases.append("relative-volume-and-playback-settings-committed")

        try await commands.execute("playlist-create \"Night 日本語\"")
        try await commands.execute("playlist-add \"Night 日本語\"")
        try await commands.execute("playlist-rename \"Night 日本語\" \"Night Mix\"")
        try await commands.execute("playlist-open \"Night Mix\"")
        guard case .playlist(let id) = navigation.destination else { throw CommandLanguageError("Playlist did not open") }
        try check(player.savedPlaylists.playlist(id: id)?.entry_count == 3, "Playlist selection add did not persist")
        try await wait("playlist catalog") {
            player.savedPlaylists.loadAll()
            return player.savedPlaylists.rows.contains { $0.name == "Night Mix" }
        }
        try check(commands.completions("playlist-open Ni").contains("playlist-open \"Night Mix\""), "Playlist completion missing")
        try await commands.execute("queue-save \"Saved Queue\"")
        try await commands.execute("smart-create \"All Tracks\" \"title:Track\"")
        try await commands.execute("smart-update \"All Tracks\" \"title:Track\"")
        try await commands.execute("smart-snapshot \"All Tracks\" \"Smart Copy\"")
        cases.append("quoted-playlist-create-add-rename-open-completion-and-smart-snapshot")

        try await commands.execute("filter \"title:Track\"")
        try await commands.execute("browser on")
        try await commands.execute("drawer off")
        try await commands.execute("artwork on")
        try await commands.execute("go songs")
        try await commands.execute("group album")
        try check(navigation.filter == "title:Track" && navigation.columnBrowserVisible
            && !navigation.upNextDrawerVisible && navigation.nowPlayingArtExpanded
            && navigation.grouping(for: .songs) == .album, "Local view commands did not apply")
        cases.append("navigation-filter-and-view-preferences")

        let before = player.snapshot?.settings.volume
        do {
            try await commands.execute("volume nan")
            throw CommandLanguageError("Invalid numeric command accepted")
        } catch let error as CommandLanguageError {
            try check(error.localizedDescription != "Invalid numeric command accepted", error.localizedDescription)
        }
        try check(player.snapshot?.settings.volume == before, "Invalid command changed server state")
        try await commands.execute("playlist-delete \"Night Mix\"")
        try await commands.execute("queue-clear")
        let savedColours = player.theme.settings
        defer { player.theme.update(savedColours) }
        try await commands.execute("colour accent #3366ff")
        try check(player.theme.settings[.accent] == RGB(hex: "#3366FF"), "colour did not set the accent")
        try await commands.execute("tint off")
        try check(!player.theme.settings.tintFromArtwork, "tint off did not turn the tint off")
        try await commands.execute("colours reset")
        try check(player.theme.settings == ColourSettings(), "colours reset did not restore defaults")
        try await commands.execute("settings appearance")
        try check(navigation.settingsTab == .appearance, "settings appearance did not open the tab")
        player.theme.update(savedColours)
        cases.append("colour-tint-reset-and-appearance-commands-apply-locally")
        navigation.openCommandPalette()
        navigation.palette.text = ":go albums"
        await navigation.palette.submitText()
        try check(navigation.destination == .albums && !navigation.palette.isOpen
            && navigation.palette.history.entries.first == "go albums", "Palette did not run go albums")
        try await commands.execute("history")
        try check(navigation.palette.isOpen && navigation.palette.mode == .history, "history did not open the history palette")
        navigation.openCommandPalette()
        navigation.palette.text = ":seek +1q"
        await navigation.palette.submitText()
        try check(navigation.palette.error != nil && navigation.palette.isOpen, "Palette accepted an invalid command")
        cases.append("palette-submits-records-history-and-keeps-errors-open")

        // Search selects a row without filtering. The fixture has 3 rows, all loaded, so a match
        // outside the loaded windows is not exercised here; the Rust find_in_list tests cover it.
        let lastSearchBefore = navigation.lastSearch
        defer { navigation.lastSearch = lastSearchBefore }
        try await commands.execute("go songs")
        guard let found = lists.current(player: player, navigation: navigation) else {
            throw CommandLanguageError("Songs has no list")
        }
        found.sync()
        found.cache.visible(0..<3)
        try await wait("songs reloaded for search") { found.cache.loadedRow(at: 2) != nil && found.totals != nil }
        try await commands.execute("selection-clear")
        let totalBefore = found.totals?.tracks
        let filterBefore = navigation.filter
        guard let target = found.cache.loadedRow(at: 1)?.track.title else { throw CommandLanguageError("Row 1 is not loaded") }
        let revisionBefore = found.cache.revision
        let rowIDsBefore = (0..<3).map { found.cache.loadedRow(at: $0)?.row_id }
        try await commands.execute("search \"title:\(target)\"")
        try check(found.selectedPositions == IndexSet(integer: 1), "search did not select row 1: \(found.selectedPositions)")
        try check(found.totals?.tracks == totalBefore && navigation.filter == filterBefore,
                  "search changed the total or the filter")
        try check(navigation.lastSearch == "title:\(target)", "search did not record the term")
        try check(found.revealRequest?.focus == true && found.revealRequest?.position == 1, "a match did not ask for focus")
        // Settle as the neighbouring cases do, then confirm the rows were not refiltered.
        found.cache.visible(0..<3)
        try await wait("songs settled after search") { found.cache.loadedRow(at: 2) != nil && found.totals != nil }
        try check(found.cache.revision == revisionBefore && (0..<3).map({ found.cache.loadedRow(at: $0)?.row_id }) == rowIDsBefore,
                  "search changed the list's rows")
        // One matching row: n wraps back to it.
        try await commands.execute("search-next")
        try check(found.selectedPositions == IndexSet(integer: 1), "search-next did not wrap to the only match")
        try check(player.message == ListSearch.wrappedMessage(backward: false), "search-next did not report the wrap")
        let term = navigation.lastSearch
        func rejected(_ command: String, _ expected: String?) async throws {
            var message: String?
            do { try await commands.execute(command) } catch { message = error.localizedDescription }
            guard let message else { throw CommandLanguageError("\(command) was accepted") }
            if let expected { try check(message == expected, message) }
        }
        try await rejected("search zzzz-nothing", ListSearch.noMatch("zzzz-nothing"))
        try check(found.selectedPositions == IndexSet(integer: 1), "a no-match search moved the selection")
        try check(navigation.lastSearch == "zzzz-nothing", "a no-match search was not kept as the last search")
        navigation.lastSearch = term
        // The server rejects the term, so the selection and the last search stay.
        try await rejected("search \"year:notanumber\"", "invalid filter at UTF-8 byte 5: expected a 4-digit year or an ascending range like 1970..1979")
        try check(found.selectedPositions == IndexSet(integer: 1) && navigation.lastSearch == term,
                  "an invalid term changed the selection or the last search")
        // A broad term steps row by row and wraps off the last row.
        try await commands.execute("selection-clear")
        try await commands.execute("search title:Track")
        try check(found.selectedPositions == IndexSet(integer: 0), "title:Track did not select row 0: \(found.selectedPositions)")
        try await commands.execute("search-next")
        try check(found.selectedPositions == IndexSet(integer: 1), "search-next did not step to row 1")
        try await commands.execute("search-next")
        try check(found.selectedPositions == IndexSet(integer: 2), "search-next did not step to row 2")
        player.message = nil
        try await commands.execute("search-next")
        try check(found.selectedPositions == IndexSet(integer: 0), "search-next did not wrap to row 0")
        try check(player.message == ListSearch.wrappedMessage(backward: false), "the wrap was not reported")
        try await commands.execute("go albums")
        try await rejected("search-next", "Search requires a track list.")
        navigation.palette.close()
        try await rejected("search", "Search requires a track list.")
        try check(!navigation.palette.isOpen, "search opened the prompt outside a track list")
        try await commands.execute("go songs")
        try await commands.execute("selection-clear")
        cases.append("search-selects-without-filtering")
        try await commands.execute("settings keys")
        try check(navigation.settingsTab == .keys, "settings keys did not open the tab")
        let lyricsWasVisible = player.lyrics.isVisible
        try await commands.execute("lyrics toggle")
        try check(player.lyrics.isVisible != lyricsWasVisible, "lyrics toggle did not change visibility")
        try await commands.execute("lyrics toggle")
        try check(player.lyrics.isVisible == lyricsWasVisible, "lyrics toggle twice did not restore visibility")
        cases.append("keys-settings-tab-and-lyrics-toggle-round-trip")
        try await commands.execute("disconnect")
        do {
            try await commands.execute("play")
            throw CommandLanguageError("Disconnected playback command accepted")
        } catch let error as CommandLanguageError {
            try check(error.localizedDescription != "Disconnected playback command accepted", error.localizedDescription)
        }
        cases.append("invalid-and-disconnected-commands-rejected-without-mutation")
    }
}
#endif
