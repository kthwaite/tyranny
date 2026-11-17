#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

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
        let surfaceOnly = args.contains("--likes-album-surface")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                try kit.requireIsolatedDaemon()
                try await kit.wait("indexed fixture") {
                    model.connected && model.snapshot?.library.tracks == 3
                        && model.snapshot?.library.scan.phase == "complete"
                }
                if surfaceOnly {
                    try await albumSurface(kit)
                } else {
                    try await kit.drive("activate")
                    try await exercise(kit)
                }
            } catch {
                failure = error.localizedDescription
                if surfaceOnly {
                    kit.observations["render"] = NativeRenderViews.render("likes-failure", into: kit.output)
                } else {
                    try? kit.capture("likes-failure")
                }
            }
            try? kit.finish(automation: surfaceOnly
                ? "AppKit performClick on displayed album hearts with a queued album and local cover."
                : "Actual AX/CGEvent heart clicks, toolbar toggle, typed filter and column menu; observation of authoritative reloaded rows; isolated Alpha/Beta/Gamma FLAC fixture.", error: failure)
            NativePlaylistKit.endSheets()
            NSApp.terminate(nil)
        }
    }

    /// Focused in-process gate when the terminal cannot drive other processes through AX.
    private static func albumSurface(_ kit: NativePlaylistKit) async throws {
        let hidden = ColumnPreferences.shared.isHidden("liked")
        if hidden { ColumnPreferences.shared.setHidden("liked", false) }
        defer { if hidden { ColumnPreferences.shared.setHidden("liked", true) } }
        kit.navigation.destination = .albums
        try await kit.wait("album grid") { NativeAlbumsSmoke.grid?.album(at: 0) != nil }
        guard let grid = NativeAlbumsSmoke.grid, let album = grid.album(at: 0) else {
            throw NativePlaylistKit.Failure(message: "Missing fixture album")
        }
        grid.toggleExpansion(album)
        try await kit.wait("expanded album") { grid.expansion?.tracks.count == 3 && table("album-track-table") != nil }
        guard let expansion = grid.expansion, let row = expansion.tracks.first else {
            throw NativePlaylistKit.Failure(message: "Missing fixture tracks")
        }
        grid.addToUpNext(expansion)
        try await kit.wait("play enabled and cover displayed") {
            kit.model.transport.canPlayPause && kit.model.artwork.value(for: kit.model.artwork.albumRequest(album.id)).image != nil
        }
        try await measure(kit, tableID: "album-track-table", heart: "album-track-table-cell-\(row.row_id)-liked",
                          label: "album", albumID: album.id)
        kit.observations["render"] = NativeRenderViews.render("album-hearts", into: kit.output)
        kit.passed("heart-keeps-album-art-playlist-and-playback-actions-available")
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
        try await measure(kit, tableID: "track-table-songs", heart: heart, label: "ungrouped")
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
        try await measure(kit, tableID: "track-table-songs", heart: heart, label: "grouped")
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
        // Give the transport an enabled Play button without starting playback.
        try await kit.press("album-add-up-next")
        try await kit.wait("queued album and displayed cover") {
            model.snapshot?.queue_length == 3 && model.transport.canPlayPause
                && model.artwork.value(for: model.artwork.albumRequest(expansion.albumID)).image != nil
        }
        try await measure(kit, tableID: "album-track-table",
                          heart: "album-track-table-cell-\(row.row_id)-liked", label: "album", albumID: expansion.albumID)
        try await clickHeart(kit, identifier: "album-track-table-cell-\(row.row_id)-liked", tableID: "album-track-table")
        try await kit.wait("album heart refresh") { !model.busy && expansion.tracks.contains { $0.track.id == alpha.track.id && !$0.track.liked } }
        try kit.check(expansion.selectedRowIDs == albumSelection && model.snapshot?.queue_length == 3,
                      "Album heart changed selection or playback")
        try kit.capture("album-hearts")
        kit.passed("album-expansion-heart-refreshes-without-selection-or-playback")
        kit.passed("heart-keeps-album-art-playlist-and-playback-actions-available")
        try kit.check(model.connected && model.message == nil
                        && model.playlistConnectionGeneration == connectionGeneration,
                      "Likes caused a connection reset or protocol error: \(model.message ?? "none")")
        kit.observations["track_id"] = alpha.track.id
        kit.observations["final_liked"] = false
    }

    /// Timed samples use the displayed button's target/action, excluding external driver startup.
    /// The surrounding scenario also exercises actual pointer clicks.
    private static func measure(_ kit: NativePlaylistKit, tableID: String, heart: String, label: String,
                                albumID: UInt64? = nil) async throws {
        guard let table = table(tableID) as? ActivatingTableView else {
            throw NativePlaylistKit.Failure(message: "Missing measured table")
        }
        try await revealLikedColumn(kit, tableID: tableID)
        func button() -> NSButton? {
            let column = table.column(withIdentifier: NSUserInterfaceItemIdentifier("liked"))
            for row in 0..<table.numberOfRows {
                if let cell = table.view(atColumn: column, row: row, makeIfNecessary: false) as? LikedTrackCell,
                   cell.button.accessibilityIdentifier() == heart { return cell.button }
            }
            return nil
        }
        func checkSurroundings() throws {
            let model = kit.model
            try kit.check(model.canMutate && model.transport.canChangeModes,
                          "Heart disabled unrelated playlist or transport actions")
            if let albumID {
                try kit.check(model.transport.canPlayPause, "Heart disabled Play")
                try kit.check(model.artwork.value(for: model.artwork.albumRequest(albumID)).image != nil,
                              "Displayed album cover became a placeholder")
            }
        }
        var samples: [[String: Any]] = []
        for _ in 0..<10 {
            try await Task.sleep(for: .milliseconds(50))
            guard let control = button(), control.isEnabled else {
                throw NativePlaylistKit.Failure(message: "Measured heart unavailable")
            }
            let expected = control.accessibilityValue() as? String == "Liked" ? "Not liked" : "Liked"
            var fullReloads = 0, cellReloads = 0
            table.reloadObserver = { rows, columns in
                if let rows, let columns { cellReloads += rows.count * columns.count }
                else { fullReloads += 1 }
            }
            let start = ContinuousClock.now
            control.performClick(nil)
            try checkSurroundings()
            var replyMS: Double?
            while button()?.accessibilityValue() as? String != expected || kit.model.likePending {
                if !kit.model.likePending && replyMS == nil {
                    replyMS = milliseconds(start.duration(to: .now))
                }
                try kit.check(start.duration(to: .now) < .seconds(5), "Heart failed to update")
                try await Task.sleep(for: .milliseconds(1))
                try checkSurroundings()
            }
            table.displayIfNeeded()
            let visibleMS = milliseconds(start.duration(to: .now))
            // Include the grouped-layout reply that follows the row refresh.
            let settleUntil = ContinuousClock.now.advanced(by: .milliseconds(80))
            repeat {
                try checkSurroundings()
                try await Task.sleep(for: .milliseconds(4))
            } while ContinuousClock.now < settleUntil
            table.reloadObserver = nil
            samples.append(["visible_ms": visibleMS, "reply_ms": replyMS ?? visibleMS,
                            "full_reloads": fullReloads, "reloaded_cells": cellReloads])
            kit.observations["\(label)_timings"] = samples
            try kit.check(kit.model.message == nil, "Like failed: \(kit.model.message ?? "")")
        }
        kit.observations["\(label)_timings"] = samples
        if CommandLine.arguments.contains("--likes-performance-check") {
            try kit.check(samples.allSatisfy { ($0["visible_ms"] as? Double ?? .infinity) < 200 },
                          "\(label) heart exceeded 200ms")
            try kit.check(samples.allSatisfy { ($0["full_reloads"] as? Int) == 0 },
                          "\(label) rebuilt the table for a heart toggle")
        }
    }

    private static func milliseconds(_ duration: Duration) -> Double {
        Double(duration.components.seconds) * 1000 + Double(duration.components.attoseconds) / 1e15
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
        if CommandLine.arguments.contains("--likes-album-surface") {
            table.scrollColumnToVisible(index)
        } else {
            try await kit.drive("scroll-horizontal-fraction", tableID, String(Double(fraction)))
        }
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
        return NSApp.windows.first(where: { $0.title == "tyranny likes smoke" })?.contentView.flatMap(find)
    }
}
#endif
