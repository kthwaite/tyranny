#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Uses the real heart, toolbar and column-menu controls against an isolated three-track library.
@MainActor
enum NativeLikesSmoke {
    private static var started = false

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--likes-smoke-output"), index + 1 < args.count else { return }
        started = true
        let kit = NativePlaylistKit(model: model, navigation: navigation,
                                    output: URL(fileURLWithPath: args[index + 1], isDirectory: true), tag: "LIKES_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                try kit.requireIsolatedDaemon()
                try await kit.wait("indexed fixture") {
                    model.connected && model.snapshot?.library.tracks == 3
                        && model.snapshot?.library.scan.phase == "complete"
                }
                try await kit.drive("activate")
                try await exercise(kit)
            } catch {
                failure = error.localizedDescription
                try? kit.capture("likes-failure")
            }
            try? kit.finish(automation: "Actual AX/CGEvent heart clicks, toolbar toggle, typed filter and column menu; observation of authoritative reloaded rows; isolated Alpha/Beta/Gamma FLAC fixture.", error: failure)
            NativePlaylistKit.endSheets()
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ kit: NativePlaylistKit) async throws {
        let model = kit.model, navigation = kit.navigation
        try kit.check(model.snapshot?.queue_length == 0, "Likes smoke requires an empty queue")
        let connectionGeneration = model.playlistConnectionGeneration
        try await kit.open(.songs)
        try await kit.wait("songs") { NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw NativePlaylistKit.Failure(message: "Songs missing") }
        try await kit.drive("choose", "group-control", "None")
        try await kit.value("library-filter", "")
        try await kit.wait("three songs") { songs.cache.total == 3 && songs.cache.loadedRows().count == 3 }
        guard let alpha = songs.cache.loadedRows().map(\.row).first(where: { $0.track.title == "Alpha" }) else {
            throw NativePlaylistKit.Failure(message: "Alpha fixture missing")
        }
        try kit.check(songs.cache.loadedRows().allSatisfy { !$0.row.track.liked }, "Fixture must start unliked")
        let heart = "track-table-songs-cell-\(alpha.row_id)-liked"
        if ColumnPreferences.shared.isHidden("liked") { try await column(kit) }
        try await kit.drive("exists", heart)
        let selection = songs.selectedPositions
        try await clickHeart(kit, identifier: heart, tableID: "track-table-songs")
        try await kit.wait("liked Alpha") { !model.busy && songs.cache.loadedRows().contains { $0.row.track.id == alpha.track.id && $0.row.track.liked } }
        try kit.check(songs.selectedPositions == selection && model.snapshot?.queue_length == 0,
                      "Heart click changed selection or playback")
        try await kit.drive("expect-text", heart, "Liked")
        kit.passed("heart-click-likes-without-selection-or-playback")

        let text = "title:Alpha OR title:Beta"
        try await kit.value("library-filter", text)
        try await kit.wait("OR filter") { songs.cache.total == 2 }
        try await kit.press("library-liked-only")
        try await kit.wait("AND-composed likes") {
            navigation.likedOnly && songs.cache.total == 1 && songs.cache.loadedRow(at: 0)?.track.id == alpha.track.id
        }
        try kit.check(navigation.filter == text, "Liked only overwrote typed text")
        try await revealLikedColumn(kit, tableID: "track-table-songs")
        try kit.capture("liked-only")
        try await clickHeart(kit, identifier: heart, tableID: "track-table-songs")
        try await kit.wait("unliked row disappears") { !model.busy && songs.cache.total == 0 }
        try kit.check(navigation.filter == text && model.snapshot?.queue_length == 0, "Unlike changed text or playback")
        kit.passed("liked-only-AND-composes-with-OR-and-unlike-removes-row")

        try await kit.value("library-filter", "artist:[")
        try await kit.wait("malformed filter visible") { songs.filterError != nil && navigation.filterError != nil }
        try await kit.drive("exists", "library-filter-error")
        try kit.check(navigation.filter == "artist:[", "Malformed text was replaced")
        // Dismiss the error popover before interacting with toolbar controls again.
        try await kit.value("library-filter", text)
        try await kit.wait("valid empty liked filter") { songs.filterError == nil && songs.cache.total == 0 }
        try await kit.press("library-liked-only")
        try await kit.wait("original typed filter restored") { !navigation.likedOnly && songs.cache.total == 2 }
        try kit.check(navigation.filter == text, "Disabling likes lost typed text")
        kit.passed("invalid-filter-remains-visible-and-text-survives-toggle")

        try await column(kit)
        try await kit.wait("hidden liked column") { ColumnPreferences.shared.isHidden("liked") }
        try kit.check(ColumnPreferences().isHidden("liked"), "Hidden column preference was not persisted")
        try kit.check(table("track-table-songs")?.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier("liked"))?.isHidden == true,
                      "Native table still shows the hidden column")
        try await column(kit)
        try await kit.wait("shown liked column") { !ColumnPreferences.shared.isHidden("liked") }
        try kit.check(!ColumnPreferences().isHidden("liked"), "Shown column preference was not persisted")
        try await kit.drive("exists", heart)
        kit.passed("column-menu-hides-shows-and-persists-hearts")

        try await kit.value("library-filter", "")
        try await kit.drive("choose", "group-control", "Artist › Album")
        try await kit.wait("grouped songs") {
            songs.cache.total == 3 && songs.description.group == .artistAlbum && songs.layoutIsCurrent
        }
        try await clickHeart(kit, identifier: heart, tableID: "track-table-songs")
        try await kit.wait("grouped heart refresh") { !model.busy && songs.cache.loadedRows().contains { $0.row.track.id == alpha.track.id && $0.row.track.liked } }
        try kit.capture("grouped-hearts")
        kit.passed("grouped-table-heart-refreshes-from-library-revision")

        try await kit.open(.albums)
        try await kit.wait("albums") { NativeAlbumsSmoke.grid?.cache.loadedRow(at: 0) != nil }
        guard let grid = NativeAlbumsSmoke.grid else { throw NativePlaylistKit.Failure(message: "Albums missing") }
        try await kit.drive("click-control", "album-cell-\(alpha.track.album_id)")
        try await kit.wait("album tracks") { grid.expansion?.isLoaded == true && grid.expansion?.tracks.count == 3 }
        guard let expansion = grid.expansion, let row = expansion.tracks.first(where: { $0.track.id == alpha.track.id }) else {
            throw NativePlaylistKit.Failure(message: "Album Alpha row missing")
        }
        try kit.check(row.track.liked, "Album did not inherit liked state")
        let albumSelection = expansion.selectedRowIDs
        try await clickHeart(kit, identifier: "album-track-table-cell-\(row.row_id)-liked", tableID: "album-track-table")
        try await kit.wait("album heart refresh") { !model.busy && expansion.tracks.contains { $0.track.id == alpha.track.id && !$0.track.liked } }
        try kit.check(expansion.selectedRowIDs == albumSelection && model.snapshot?.queue_length == 0,
                      "Album heart changed selection or playback")
        try kit.capture("album-hearts")
        kit.passed("album-expansion-heart-refreshes-without-selection-or-playback")
        try kit.check(model.connected && model.message == nil
                        && model.playlistConnectionGeneration == connectionGeneration,
                      "Likes caused a connection reset or protocol error: \(model.message ?? "none")")
        kit.observations["track_id"] = alpha.track.id
        kit.observations["final_liked"] = false
    }

    private static func column(_ kit: NativePlaylistKit) async throws {
        try await kit.drive("choose", "columns-menu", "Liked")
    }

    private static func clickHeart(_ kit: NativePlaylistKit, identifier: String, tableID: String) async throws {
        try await revealLikedColumn(kit, tableID: tableID)
        try await kit.drive("click-control", identifier)
    }

    private static func revealLikedColumn(_ kit: NativePlaylistKit, tableID: String) async throws {
        guard let table = table(tableID) else { throw NativePlaylistKit.Failure(message: "Track table missing") }
        let index = table.column(withIdentifier: NSUserInterfaceItemIdentifier("liked"))
        try kit.check(index >= 0, "Liked column missing")
        let column = table.rect(ofColumn: index)
        let visible = table.visibleRect
        guard column.minX < visible.minX || column.maxX > visible.maxX else { return }
        // AppKit preserves user column order, including columns added by an upgrade.
        // Scroll through the real native scroller before sending a pointer click.
        let maximum = table.bounds.width - visible.width
        try kit.check(maximum > 0, "Liked column cannot be scrolled into view")
        let fraction = min(1, max(0, (column.midX - visible.width / 2) / maximum))
        try await kit.drive("scroll-horizontal-fraction", tableID, String(Double(fraction)))
        try await kit.wait("visible liked column") {
            column.minX >= table.visibleRect.minX && column.maxX <= table.visibleRect.maxX
        }
    }

    private static func table(_ identifier: String) -> NSTableView? {
        func find(_ view: NSView) -> NSTableView? {
            if let table = view as? NSTableView, table.accessibilityIdentifier() == identifier { return table }
            for child in view.subviews { if let table = find(child) { return table } }
            return nil
        }
        return NSApp.windows.first(where: { $0.title == "Tyranny" })?.contentView.flatMap(find)
    }
}
#endif
