#if DEBUG
import AppKit
import Foundation
import TyrannyCore

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
        let suite = "dev.tyranny.command-smoke.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: suite)!
        defer { defaults.removePersistentDomain(forName: suite) }
        let navigation = NavigationModel(defaults: defaults)
        let lists = TrackLists()
        let commands = CommandDispatcher(player: player, navigation: navigation, lists: lists)
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
