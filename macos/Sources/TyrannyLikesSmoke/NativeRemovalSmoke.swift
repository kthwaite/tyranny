#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Exercises real NSTableView context-menu actions and native confirmation buttons.
@MainActor
enum NativeRemovalSmoke {
    private static var started = false

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--removal-smoke-output"), index + 1 < args.count else { return }
        started = true
        let kit = NativePlaylistKit(model: model, navigation: navigation,
                                    output: URL(fileURLWithPath: args[index + 1], isDirectory: true), tag: "REMOVAL_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                try kit.requireIsolatedDaemon()
                try await kit.wait("three indexed fixture tracks") {
                    model.connected && model.snapshot?.library.tracks == 3
                        && model.snapshot?.library.scan.phase == "complete"
                }
                try await kit.drive("activate")
                try await exercise(kit)
            } catch {
                failure = error.localizedDescription
                try? kit.capture("removal-failure")
            }
            try? kit.finish(automation: "Native table selection via AX; real NSTableView context menu and NSMenu action; AX native alert Cancel/Remove; authoritative refreshed rows and disk checks.", error: failure)
            NativePlaylistKit.endSheets()
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        try await kit.open(.songs)
        try await kit.wait("Songs mounted") { NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw NativePlaylistKit.Failure(message: "Songs missing") }
        try await kit.drive("choose", "group-control", "None")
        try await kit.value("library-filter", "")
        try await kit.wait("three native rows") { songs.cache.total == 3 && songs.cache.loadedRows().count == 3 }
        guard let original = songs.cache.loadedRow(at: 0) else { throw NativePlaylistKit.Failure(message: "First row missing") }
        let revision = model.snapshot?.library.revision
        let queueLength = model.snapshot?.queue_length
        let currentID = model.snapshot?.current_id
        try await kit.select("track-table-songs", [0])
        try await kit.wait("selected row") { songs.selectedPositions == IndexSet(integer: 0) }
        try invokeRemoval()
        try await kit.drive("exists", "library-removal-cancel")
        try kit.capture("removal-confirmation")
        try await kit.press("library-removal-cancel")
        try await kit.wait("cancelled sheet") { table()?.window?.attachedSheet == nil }
        try kit.check(model.snapshot?.library.revision == revision && songs.cache.total == 3,
                      "Cancellation changed the library")
        kit.passed("native-cancel-preserves-library")

        try invokeRemoval()
        try await kit.press("library-removal-confirm")
        try await kit.wait("removed row and refreshed native list") {
            !model.busy && model.snapshot?.library.tracks == 2 && songs.cache.total == 2
                && songs.cache.loadedRows().count == 2
                && !songs.cache.loadedRows().contains { $0.row.track.id == original.track.id }
                && table()?.numberOfRows == 2
        }
        try kit.check(FileManager.default.fileExists(atPath: original.track.path), "Removal deleted the audio file")
        try kit.check(model.snapshot?.queue_length == queueLength && model.snapshot?.current_id == currentID,
                      "Removal changed queued playback")
        try kit.check(model.message == nil, "Removal reported an error: \(model.message ?? "")")
        kit.passed("native-removal-refreshes-list-preserves-file-and-queue")
        try await kit.select("track-table-songs", [0])
        try invokeRemoval()
        try await kit.drive("exists", "library-removal-cancel")
        try await kit.press("library-removal-cancel")
        try await kit.wait("refreshed menu cancelled") { table()?.window?.attachedSheet == nil }
        try kit.check(songs.cache.total == 2, "Refreshed menu cancellation changed tracks")
        try kit.capture("removal-refreshed")
        kit.passed("refreshed-native-row-menu-remains-usable")
    }

    private static func invokeRemoval() throws {
        guard let table = table(), let window = table.window else {
            throw NativePlaylistKit.Failure(message: "Native Songs table missing")
        }
        let rect = table.rect(ofRow: 0)
        let point = table.convert(NSPoint(x: table.visibleRect.midX, y: rect.midY), to: nil)
        guard let event = NSEvent.mouseEvent(with: .rightMouseDown, location: point,
                                            modifierFlags: [], timestamp: ProcessInfo.processInfo.systemUptime,
                                            windowNumber: window.windowNumber, context: nil,
                                            eventNumber: 0, clickCount: 1, pressure: 1),
              let menu = table.menu(for: event),
              let index = menu.items.firstIndex(where: { $0.title == "Remove from Library…" }),
              menu.items[index].action != nil else {
            throw NativePlaylistKit.Failure(message: "Enabled native Remove from Library menu item missing")
        }
        guard !menu.items.contains(where: { $0.title == "Remove from Playlist" }) else {
            throw NativePlaylistKit.Failure(message: "Songs offers playlist removal")
        }
        menu.performActionForItem(at: index)
    }

    private static func table() -> ActivatingTableView? {
        func find(_ view: NSView) -> ActivatingTableView? {
            if let table = view as? ActivatingTableView, table.accessibilityIdentifier() == "track-table-songs" { return table }
            return view.subviews.lazy.compactMap(find).first
        }
        return NSApp.windows.compactMap(\.contentView).lazy.compactMap(find).first
    }
}
#endif
