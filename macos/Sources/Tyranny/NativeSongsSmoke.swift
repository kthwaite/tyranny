#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for the Songs view (`--songs-smoke-output`). The gate
/// prepares a 1,129-track library on the server. Every control is driven through the
/// external Accessibility driver; server queries here only read the expected values.
///
/// `--songs-smoke-scenario grouping` drives grouping instead: Artist › Album against the
/// server's spans, a selection inside an album block, and a rescan that adds an album
/// (`--songs-smoke-extra`, moved into `--songs-smoke-root`) while the anchor stays in view.
/// `--songs-smoke-scenario scroll-memory` checks navigation restoration in both layouts,
/// including partial-row offsets, then the reset after changing grouping.
@MainActor
enum NativeSongsSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-songs-smoke")
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]
    private static let table = "track-table-songs"
    private static var scenario = "songs"

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--songs-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        scenario = argument("--songs-smoke-scenario") ?? "songs"
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("connection", model) {
                    model.connected && model.snapshot?.library.tracks == 1129
                        && model.snapshot?.library.scan.phase == "complete"
                }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                // Group settings persist per destination; scenarios start ungrouped.
                navigation.setGrouping(.none, for: .songs)
                if scenario == "grouping" {
                    try await exerciseGrouping(model, navigation)
                } else if scenario == "scroll-memory" {
                    try await wait("songs list", model) { songsList?.cache.total == 1129 }
                    guard let list = songsList else { throw Failure(message: "Songs list missing") }
                    try await exerciseScrollMemory(model, navigation, list)
                    try await drive("choose", "group-control", "Artist › Album")
                    try await wait("grouped layout", model) {
                        list.grouping == .artistAlbum && list.layoutIsCurrent && list.layout.count > 0
                    }
                    try await exerciseScrollMemory(model, navigation, list)
                    try await drive("choose", "group-control", "None")
                    try await wait("grouping resets viewport", model) {
                        list.grouping == .none && list.layoutIsCurrent
                            && findTable().map { $0.visibleRect.minY < 30 } == true
                    }
                    cases.append("native-songs-description-change-resets-scroll")
                } else {
                    try await exercise(model, navigation)
                }
                try finish(model, error: nil)
            } catch {
                try? capture("\(scenario == "grouping" ? "grouped" : "songs")-failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try check(navigation.destination == .songs, "Songs is not the first destination")
        try check(!navigation.columnBrowserVisible, "Fixture must start with the browser hidden")
        try await drive("exists", "main-songs")
        try await drive("exists", table)
        try await drive("exists", "group-control")
        try await drive("expect-enabled", "group-control", "true")
        try await wait("songs list", model) { songsList != nil }
        guard let list = songsList else { throw Failure(message: "Songs list missing") }
        try await wait("first window", model) { list.cache.total == 1129 && list.cache.loadedRow(at: 0) != nil }
        try await drive("expect-text", "list-count", "1,129 songs")
        try await wait("list totals", model) { list.totals?.tracks == 1129 }
        if let totals = list.totals {
            try await drive("expect-text", "status-totals", DisplayFormat.totals(totals))
        }
        cases.append("native-songs-continuous-table-shows-library-count-and-totals")

        // Scroll to the end through Accessibility; the last row matches the server's.
        try await drive("scroll-fraction", table, "1")
        try await wait("last window", model) { list.cache.loadedRow(at: 1128) != nil }
        let last = try await serverRow(model, list.description, offset: 1128)
        try check(list.cache.loadedRow(at: 1128)?.row_id == last.row_id, "Last row differs from the server's")
        try await drive("expect-text", "\(table)-cell-\(last.row_id)-title", last.track.title)
        let loaded = list.cache.loadedRows().count
        observations["loaded_rows_at_end"] = loaded
        observations["last_row_title"] = last.track.title
        try check(loaded <= 2000, "The cache holds \(loaded) rows; it must stay bounded")
        cases.append("native-songs-scroll-to-end-shows-server-last-row")
        try await exerciseScrollMemory(model, navigation, list)

        // Sort by Year, descending: two header clicks.
        try await drive("click-control", "track-header-year")
        try await wait("year ascending", model) { list.sortKeys == [SortKey(field: .year)] }
        try await drive("click-control", "track-header-year")
        try await wait("year descending", model) {
            list.sortKeys == [SortKey(field: .year, descending: true)] && list.cache.loadedRow(at: 0) != nil
                && list.description.sort == [SortKey(field: .year, descending: true)]
        }
        let first = try await serverRow(model, list.description, offset: 0)
        try check(list.cache.loadedRow(at: 0)?.row_id == first.row_id, "Year-descending first row differs from the server's")
        try await drive("expect-text", "\(table)-cell-\(first.row_id)-title", first.track.title)
        observations["year_descending_first"] = ["title": first.track.title, "year": first.track.year as Any]
        cases.append("native-songs-header-sorts-year-descending")

        // Column browser: narrow to one genre.
        try await press("browser-toggle")
        try await wait("browser shown", model) {
            navigation.columnBrowserVisible && list.browser.genres.cache.loadedRow(at: 0) != nil
        }
        try await drive("exists", "column-browser")
        try await drive("exists", "browser-artists")
        try await drive("exists", "browser-albums")
        guard let genre = list.browser.genres.cache.loadedRow(at: 0) else { throw Failure(message: "No genres") }
        try await drive("select", "browser-genres", "1")
        try await wait("genre narrowing", model) {
            list.description.browser.genre_ids == [genre.id] && list.cache.loadedRow(at: 0) != nil
        }
        let narrowed = try await serverPage(model, list.description, offset: 0, limit: 1)
        try check(UInt64(list.cache.total) == narrowed.total, "Browser count differs from the server total")
        try check(narrowed.total == genre.track_count, "Genre count differs from its track count")
        try await drive("expect-text", "list-count", "\(DisplayFormat.count(narrowed.total)) of 1,129 songs")
        try await wait("narrowed albums", model) { list.browser.albums.cache.revision != nil }
        observations["genre"] = ["name": genre.name, "tracks": narrowed.total,
                                 "albums": list.browser.albums.cache.total]
        cases.append("native-songs-browser-narrows-to-one-genre")
        try await Task.sleep(for: .milliseconds(400))
        try capture("songs")

        // Select a range and drag it to Up Next.
        try await drive("scroll-fraction", table, "0")
        try await drive("select", table, "0,1,2,3,4")
        try await wait("range selected", model) { list.selectedPositions == IndexSet(integersIn: 0..<5) }
        let expected = (0..<5).compactMap { list.cache.loadedRow(at: $0)?.track.id }
        try check(expected.count == 5, "Selected rows are not loaded")
        guard let before = model.snapshot?.queue_length else { throw Failure(message: "No snapshot") }
        let plan = try JSONSerialization.data(withJSONObject: [
            "row": 1, "steps": [["id": "source-up-next", "hold": 0.8]]])
        try await drive("drag-rows", table, String(decoding: plan, as: UTF8.self))
        try await wait("queue grew", model) { model.snapshot?.queue_length == before + 5 && !model.busy }
        let queue: Page<QueueItem> = try await model.send("queue_page", [
            "revision": .null, "offset": .int(before), "limit": .int(5)])
        try check(queue.entries.map(\.track_id) == expected, "Up Next did not receive the dragged rows in order")
        try check(queue.entries.allSatisfy { $0.source == QueueSource(kind: .drag, name: "Songs") },
                  "Dragged entries lack the drag source")
        observations["dragged_track_ids"] = expected
        cases.append("native-songs-drag-range-appends-to-up-next")
    }

    // MARK: Grouping

    private static func exerciseGrouping(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        try check(navigation.destination == .songs, "Songs is not the first destination")
        try await wait("songs list", model) { songsList != nil }
        guard let list = songsList else { throw Failure(message: "Songs list missing") }
        try await wait("first window", model) { list.cache.total == 1129 && list.cache.loadedRow(at: 0) != nil }
        try await drive("expect-enabled", "group-control", "true")

        // Artist › Album: headers and album blocks match a layout of the server's spans.
        try await drive("choose", "group-control", "Artist › Album")
        try await wait("artist › album layout", model) {
            list.description.group == .artistAlbum && list.layoutIsCurrent && list.layout.count > 0
                && list.cache.loadedRow(at: 0) != nil
        }
        guard let revision = list.cache.revision else { throw Failure(message: "No revision") }
        let groups: ListGroupsResponse = try await model.send("list_groups", [
            "list": try JSONValue.encoding(list.description), "revision": .uint(revision)])
        let expected = GroupLayout(grouping: .artistAlbum, total: Int(groups.total), spans: groups.groups)
        let shown = kinds(list.layout), server = kinds(expected)
        observations["grouped_rows"] = ["headers": server.headers, "block_tracks": server.blockTracks,
                                        "fillers": server.fillers, "gaps": server.gaps, "count": expected.count,
                                        "spans": groups.groups.count]
        try check(shown == server, "Layout rows \(shown) differ from the server spans' \(server)")
        try check(server.headers == Set(groups.groups.map(\.artist_key)).count, "One header per artist expected")
        try check(server.gaps == groups.groups.count, "One block per album span expected")
        try await drive("expect-row-count", table, String(expected.count))
        guard let first = groups.groups.first, let album = first.album_key else { throw Failure(message: "No spans") }
        try await drive("exists", "artist-header-\(first.artist_key)")
        try await drive("exists", "album-cover-\(album)")
        try await wait("album facts", model) { list.facts.summary(album) != nil }
        let summaries: AlbumSummariesResponse = try await model.send("album_summaries", [
            "album_ids": .uints([album]), "revision": .null])
        guard let summary = summaries.entries.first else { throw Failure(message: "No album summary") }
        try await drive("expect-text", "album-facts-\(album)", DisplayFormat.albumFacts(summary).joined(separator: "\n"))
        cases.append("native-grouping-artist-album-rows-match-server-spans")
        try await Task.sleep(for: .milliseconds(400))
        try capture("grouped")

        // One track inside a block: the selection is that row's ID.
        let position = Int(first.start) + min(1, Int(first.count) - 1)
        guard let display = list.layout.displayIndex(ofPosition: position),
              let row = list.cache.loadedRow(at: position) else { throw Failure(message: "Block track not loaded") }
        try await drive("select", table, String(display))
        try await wait("block track selected", model) { list.selection == [row.row_id] }
        try check(list.selectedPositions == IndexSet(integer: position), "Selected position differs")
        observations["block_selection"] = ["row_id": row.row_id, "display": display, "position": position]
        cases.append("native-grouping-selects-one-block-track-by-row-id")
        try await exerciseScrollMemory(model, navigation, list)

        // A rescan adds an album before the anchor: the anchor track stays in view.
        guard let extra = argument("--songs-smoke-extra"), let root = argument("--songs-smoke-root") else {
            throw Failure(message: "Grouping scenario needs --songs-smoke-extra and --songs-smoke-root")
        }
        try await drive("scroll-fraction", table, "1")
        try await wait("end in view", model) { (visibleRowIDs(list)?.count ?? 0) > 0 }
        try await Task.sleep(for: .milliseconds(600))
        guard let tableView = findTable(), let anchor = firstVisibleTrack(list, tableView) else {
            throw Failure(message: "No visible track to anchor")
        }
        let roots: Page<Root> = try await model.send("library_roots", [
            "revision": .null, "offset": .int(0), "limit": .int(10), "query": .string("")])
        guard let rootID = roots.entries.first(where: { $0.path == root })?.id ?? roots.entries.first?.id else {
            throw Failure(message: "No library root")
        }
        let destination = URL(fileURLWithPath: root).appendingPathComponent(URL(fileURLWithPath: extra).lastPathComponent)
        try FileManager.default.moveItem(at: URL(fileURLWithPath: extra), to: destination)
        let _: ScanAccepted = try await model.send("rescan", ["root_id": .uint(rootID)])
        try await wait("rescan adds the album", model, seconds: 120) {
            model.snapshot?.library.tracks == 1136 && model.snapshot?.library.scan.phase == "complete"
                && list.cache.total == 1136 && list.layoutIsCurrent && list.cache.anchorPosition == nil
        }
        try await Task.sleep(for: .milliseconds(600))
        let visible = visibleRowIDs(list) ?? []
        let moved = list.cache.position(of: anchor.rowID)
        observations["anchor"] = ["row_id": anchor.rowID, "position_before": anchor.position,
                                  "position_after": moved as Any, "visible_rows": visible.count]
        try check(visible.contains(anchor.rowID), "The anchor track left the view after the rescan")
        try check(moved.map { $0 > anchor.position } ?? false, "The added album did not move the anchor")
        try await drive("exists", "\(table)-cell-\(anchor.rowID)-title")
        cases.append("native-grouping-rescan-keeps-anchor-track-in-view")
        navigation.setGrouping(.none, for: .songs)
    }

    /// The native table is destroyed off-screen; both row identity and pixel offset survive.
    private static func exerciseScrollMemory(_ model: PlayerModel, _ navigation: NavigationModel,
                                             _ list: ListModel) async throws {
        try await drive("scroll-fraction", table, "0.53")
        try await wait("middle window", model) {
            guard let table = findTable() else { return false }
            return firstVisibleTrack(list, table) != nil && table.visibleRect.minY > 100
        }
        guard let before = findTable(), let anchor = firstVisibleTrack(list, before),
              let origin = before.enclosingScrollView?.contentView.bounds.origin else {
            throw Failure(message: "No viewport to remember")
        }
        defer {
            observations["scroll_memory_\(list.grouping)"] = [
                "row_id": anchor.rowID, "y": origin.y,
                "restored_y": findTable()?.enclosingScrollView?.contentView.bounds.minY as Any,
                "remembered_y": list.scrollOrigin?.y as Any,
                "restored_row_id": findTable().flatMap { firstVisibleTrack(list, $0)?.rowID } as Any]
        }
        for _ in 0..<2 {
            try await drive("click-control", "source-albums")
            try await wait("Albums destination", model) { navigation.destination == .albums && findTable() == nil }
            observations["offscreen_y"] = list.scrollOrigin?.y as Any
            try await drive("click-control", "source-songs")
            try await wait("Songs viewport restored", model) {
                guard let restored = findTable(), restored !== before,
                      let point = restored.enclosingScrollView?.contentView.bounds.origin else { return false }
                return abs(point.y - origin.y) < 1 && abs(point.x - origin.x) < 1
                    && firstVisibleTrack(list, restored)?.rowID == anchor.rowID
            }
        }
        try capture("songs-scroll-restored-\(list.grouping)")
        cases.append("native-songs-\(list.grouping)-restores-scroll-after-navigation")
    }

    private struct RowKinds: Equatable, CustomStringConvertible {
        var headers = 0, blockTracks = 0, fillers = 0, gaps = 0, plainTracks = 0
        var description: String { "h\(headers) t\(blockTracks) f\(fillers) g\(gaps) p\(plainTracks)" }
    }

    private static func kinds(_ layout: GroupLayout) -> RowKinds {
        var kinds = RowKinds()
        for index in 0..<layout.count {
            switch layout.row(at: index) {
            case .artistHeader?: kinds.headers += 1
            case .track(_, nil)?: kinds.plainTracks += 1
            case .track?: kinds.blockTracks += 1
            case .blockFiller?: kinds.fillers += 1
            case .blockGap?: kinds.gaps += 1
            case nil: break
            }
        }
        return kinds
    }

    private static func findTable() -> NSTableView? {
        func visit(_ view: NSView) -> NSTableView? {
            if let table = view as? NSTableView, table.accessibilityIdentifier() == Self.table { return table }
            for child in view.subviews { if let found = visit(child) { return found } }
            return nil
        }
        return NSApp.windows.lazy.compactMap { $0.contentView.flatMap(visit) }.first
    }

    /// Row IDs of the track rows the table shows now.
    private static func visibleRowIDs(_ list: ListModel) -> [UInt64]? {
        guard let table = findTable() else { return nil }
        let rows = table.rows(in: table.visibleRect)
        let positions = list.layout.positions(ofDisplayIndexes: IndexSet(integersIn: rows.location..<rows.location + rows.length))
        return positions.compactMap { list.cache.loadedRow(at: $0)?.row_id }
    }

    private static func firstVisibleTrack(_ list: ListModel, _ table: NSTableView) -> (rowID: UInt64, position: Int)? {
        let rows = table.rows(in: table.visibleRect)
        let positions = list.layout.positions(ofDisplayIndexes: IndexSet(integersIn: rows.location..<rows.location + rows.length))
        guard let position = positions.first, let row = list.cache.loadedRow(at: position) else { return nil }
        return (row.row_id, position)
    }

    private static func argument(_ name: String) -> String? {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: name), index + 1 < args.count else { return nil }
        return args[index + 1]
    }

    /// The shown Songs list, set by its view for observation.
    static weak var songsList: ListModel?

    private static func serverPage(_ model: PlayerModel, _ list: ListDescription, offset: Int,
                                   limit: Int) async throws -> ListTracksResponse {
        try await model.send("list_tracks", [
            "list": try JSONValue.encoding(list), "revision": .null, "offset": .int(offset), "limit": .int(limit)])
    }

    private static func serverRow(_ model: PlayerModel, _ list: ListDescription, offset: Int) async throws -> ListRow {
        guard let row = try await serverPage(model, list, offset: offset, limit: 1).entries.first else {
            throw Failure(message: "Server has no row at \(offset)")
        }
        return row
    }

    private static func drive(_ action: String, _ identifier: String, _ value: String? = nil) async throws {
        try await NativeSmoke.drive(action, identifier, value)
    }
    private static func press(_ identifier: String) async throws { try await drive("press", identifier) }
    private static func wait(_ label: String, _ model: PlayerModel, seconds: Double = 15,
                             until condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            if Date() >= deadline {
                throw Failure(message: "Timed out waiting for \(label); message=\(model.message ?? "none")")
            }
            try await Task.sleep(for: .milliseconds(40))
        }
        try await Task.sleep(for: .milliseconds(150))
    }
    private static func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }
    private static func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else {
            throw Failure(message: "Native window unavailable")
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        if process.terminationStatus != 0 {
            // Without Screen Recording permission, keep a self-rendered image for review.
            // It is not a composited screenshot and is named so.
            selfRender(window, name: name + "-self-rendered")
        }
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }
    private static func selfRender(_ window: NSWindow, name: String) {
        guard let frame = window.contentView?.superview,
              let bitmap = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) else { return }
        frame.cacheDisplay(in: frame.bounds, to: bitmap)
        try? bitmap.representation(using: .png, properties: [:])?
            .write(to: output.appendingPathComponent(name + ".png"))
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases, "observations": observations,
            "automation": scenario == "grouping"
                ? "External Accessibility group pop-up choice, row count, table selection and scroll; the smoke moves a staged album into the root and requests a rescan; server reads otherwise only for expected values."
                : scenario == "scroll-memory"
                    ? "External Accessibility scroll, sidebar navigation and grouping choices; native table observations compare row identity and pixel offsets across recreation."
                    : "External Accessibility scroll, header clicks, browser and table selection, and a CGEvent drag onto the Up Next source row; server reads only for expected values.",
            "scenario": scenario]
        if let error { report["error"] = error }
        if let state = model.snapshot {
            report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state))
        }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("SONGS_SMOKE \(scenario) \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
