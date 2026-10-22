import AppKit
import Observation
import SwiftUI
import TyrannyLikesSmokeCore

/// One column of the column browser: a window cache over `list_genres`,
/// `list_artists` or `list_albums`.
@MainActor @Observable
final class BrowserColumnModel<Row: Sendable & Identifiable> where Row.ID == UInt64 {
    let cache: ListWindowCache<Row>
    /// The Artist column's Compilations row, from the latest reply.
    fileprivate(set) var compilations: NamedCount?
    /// Bumps when the column's request changes.
    private(set) var epoch: UInt64 = 0

    init() {
        cache = ListWindowCache { _, _, _ in throw WindowFetchError.failed("not started") }
    }

    func reset(_ fetch: @escaping (_ epoch: UInt64) -> ListWindowCache<Row>.Fetch) {
        epoch &+= 1
        compilations = nil
        cache.reset(fetch: fetch(epoch))
    }
}

/// The column browser's three lists (spec 4.2). The Artist column follows the Genre
/// selection; the Album column follows the Genre and Artist selections; all three
/// follow the filter field.
@MainActor @Observable
final class ColumnBrowserModel {
    let genres = BrowserColumnModel<NamedCount>()
    let artists = BrowserColumnModel<NamedCount>()
    let albums = BrowserColumnModel<AlbumSummary>()

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private var revision: UInt64?
    @ObservationIgnored private var query: String?
    @ObservationIgnored private var narrowing: Browser?
    @ObservationIgnored private var genreNarrowing: Browser?

    func disconnected(session: UInt64) {
        self.session = nil
        genres.cache.discard(session: session)
        artists.cache.discard(session: session)
        albums.cache.discard(session: session)
    }

    /// Loads only while shown. A hidden browser catches up when it is shown again.
    func sync(player: PlayerModel, session: UInt64, revision: UInt64, query: String, narrowing: Browser,
              visible: Bool) {
        guard visible else { return }
        self.player = player
        let genreNarrowing = Browser(genre_ids: narrowing.genre_ids)
        if session != self.session || query != self.query {
            self.session = session
            self.query = query
            self.narrowing = narrowing
            self.genreNarrowing = genreNarrowing
            self.revision = revision
            genres.reset { self.namedFetch("list_genres", column: self.genres, query: query, narrowing: nil, epoch: $0) }
            artists.reset {
                self.namedFetch("list_artists", column: self.artists, query: query, narrowing: genreNarrowing, epoch: $0)
            }
            albums.reset { _ in self.albumFetch(query: query, narrowing: narrowing) }
            return
        }
        if genreNarrowing != self.genreNarrowing {
            self.genreNarrowing = genreNarrowing
            artists.reset {
                self.namedFetch("list_artists", column: self.artists, query: query, narrowing: genreNarrowing, epoch: $0)
            }
        }
        if narrowing != self.narrowing {
            self.narrowing = narrowing
            albums.reset { _ in self.albumFetch(query: query, narrowing: narrowing) }
        }
        if revision != self.revision {
            self.revision = revision
            for cache in [genres.cache, artists.cache] { cache.serverRevisionChanged(revision, anchor: nil) }
            albums.cache.serverRevisionChanged(revision, anchor: nil)
        }
    }

    /// `narrowing` restricts the Artist column to a list; no genre selected sends `null`.
    private func namedFetch(_ command: String, column: BrowserColumnModel<NamedCount>, query: String,
                            narrowing: Browser?, epoch: UInt64) -> ListWindowCache<NamedCount>.Fetch {
        namedCountFetch(player: { [weak self] in self?.player }, command: command, column: column,
                        query: query, narrowing: narrowing, epoch: epoch)
    }

    private func albumFetch(query: String, narrowing: Browser) -> ListWindowCache<AlbumSummary>.Fetch {
        let list = ListDescription(source: .library, browser: narrowing, query: query)
        return { @MainActor [weak self] offset, limit, revision in
            guard let player = self?.player else { throw WindowFetchError.failed("The browser closed.") }
            do {
                let reply: ListAlbumsResponse = try await player.send("list_albums", [
                    "list": try JSONValue.encoding(list), "sort": .string(AlbumSort.albumArtist.rawValue),
                    "descending": .bool(false), "revision": revision.map(JSONValue.uint) ?? .null,
                    "offset": .int(offset), "limit": .int(limit)])
                return WindowPage(revision: reply.revision, total: Int(reply.total), offset: reply.offset,
                                  rows: reply.entries)
            } catch let failure as ServerFailure {
                throw failure.code == "stale_library_revision"
                    ? WindowFetchError.stale : WindowFetchError.failed(failure.message)
            }
        }
    }
}

/// A fetch over `list_artists` or `list_genres`. For `list_artists`, `narrowing` restricts the
/// list to a genre selection; none sends `null`. The Compilations row comes from offset 0.
@MainActor
func namedCountFetch(player: @escaping @MainActor () -> PlayerModel?, command: String,
                     column: BrowserColumnModel<NamedCount>, query: String, narrowing: Browser?,
                     epoch: UInt64) -> ListWindowCache<NamedCount>.Fetch {
    { @MainActor [weak column] offset, limit, revision in
        guard let player = player() else { throw WindowFetchError.failed("The browser closed.") }
        var fields: [String: JSONValue] = [
            "query": .string(query), "revision": revision.map(JSONValue.uint) ?? .null,
            "offset": .int(offset), "limit": .int(limit)]
        do {
            if command == "list_artists" {
                if let narrowing, !narrowing.genre_ids.isEmpty {
                    fields["list"] = try JSONValue.encoding(ListDescription(source: .library, browser: narrowing))
                } else {
                    fields["list"] = .null
                }
                let reply: ListArtistsResponse = try await player.send(command, fields)
                if let column, column.epoch == epoch, offset == 0 { column.compilations = reply.compilations }
                return WindowPage(revision: reply.revision, total: Int(reply.total), offset: reply.offset,
                                  rows: reply.entries)
            }
            let reply: ListGenresResponse = try await player.send(command, fields)
            return WindowPage(revision: reply.revision, total: Int(reply.total), offset: reply.offset,
                              rows: reply.entries)
        } catch let failure as ServerFailure {
            throw failure.code == "stale_library_revision"
                ? WindowFetchError.stale : WindowFetchError.failed(failure.message)
        }
    }
}

/// Genre | Artist | Album, above the track table (⌘B).
struct ColumnBrowser: View {
    let list: ListModel
    let colours: TableColours

    var body: some View {
        let browser = list.browser
        let selection = list.browserSelection
        HStack(spacing: 0) {
            BrowserColumnTable(
                column: browser.genres, identifier: "browser-genres", title: "Genres",
                singular: "Genre", plural: "Genres", selected: selection.genres,
                generation: browser.genres.cache.generation, total: browser.genres.cache.total,
                compilations: nil, label: { $0.name.isEmpty ? "Unknown Genre" : $0.name },
                help: { "\($0.name) · \(DisplayFormat.count($0.album_count)) albums · \(DisplayFormat.songs($0.track_count))" },
                select: list.setBrowserGenres, colours: colours)
            Divider()
            BrowserColumnTable(
                column: browser.artists, identifier: "browser-artists", title: "Artists",
                singular: "Artist", plural: "Artists", selected: selection.artists,
                generation: browser.artists.cache.generation, total: browser.artists.cache.total,
                compilations: browser.artists.compilations,
                label: { $0.name.isEmpty ? "Unknown Artist" : $0.name },
                help: { "\($0.name) · \(DisplayFormat.count($0.album_count)) albums · \(DisplayFormat.songs($0.track_count))" },
                select: list.setBrowserArtists, colours: colours)
            Divider()
            BrowserColumnTable(
                column: browser.albums, identifier: "browser-albums", title: "Albums",
                singular: "Album", plural: "Albums", selected: selection.albums,
                generation: browser.albums.cache.generation, total: browser.albums.cache.total,
                compilations: nil, label: { $0.title.isEmpty ? "Unknown Album" : $0.title },
                help: { album in
                    [album.title, album.album_artist, album.year.map(String.init)].compactMap { $0 }
                        .filter { !$0.isEmpty }.joined(separator: " · ")
                },
                select: list.setBrowserAlbums, colours: colours)
        }
        .frame(height: 168)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("column-browser")
    }
}

/// A virtualized single-column list. Row 0 is "All (n …)"; the Artist column's
/// Compilations row follows it when the library has compilations.
struct BrowserColumnTable<Row: Sendable & Identifiable>: NSViewRepresentable where Row.ID == UInt64 {
    let column: BrowserColumnModel<Row>
    let identifier: String
    let title: String
    let singular: String
    let plural: String
    let selected: Set<UInt64>
    let generation: UInt64
    let total: Int
    let compilations: NamedCount?
    let label: (Row) -> String
    let help: (Row) -> String
    let select: (Set<UInt64>) -> Void
    /// Row 0 is "All"; choosing it clears the selection.
    let colours: TableColours
    var showsAll = true
    var allowsMultiple = true

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        let table = NSTableView()
        table.setAccessibilityIdentifier(identifier)
        table.style = .fullWidth
        table.rowHeight = 20
        table.intercellSpacing = NSSize(width: 0, height: 0)
        table.allowsMultipleSelection = allowsMultiple
        table.allowsEmptySelection = true
        table.usesAlternatingRowBackgroundColors = true
        table.columnAutoresizingStyle = .uniformColumnAutoresizingStyle
        let native = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("name"))
        native.title = title
        native.resizingMask = .autoresizingMask
        table.addTableColumn(native)
        table.dataSource = context.coordinator
        table.delegate = context.coordinator
        scroll.documentView = table
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(
            context.coordinator, selector: #selector(Coordinator.scrolled(_:)),
            name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        context.coordinator.table = table
        context.coordinator.update(self)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.update(self)
    }

    static func dismantleNSView(_ scroll: NSScrollView, coordinator: Coordinator) {
        NotificationCenter.default.removeObserver(coordinator)
    }

    @MainActor final class Coordinator: NSObject, @MainActor NSTableViewDataSource, NSTableViewDelegate {
        var parent: BrowserColumnTable
        weak var table: NSTableView?
        private var updating = false
        private var shownGeneration: UInt64?
        private var shownCount = 0
        private var shownCompilations: NamedCount?
        private var shownSelection: Set<UInt64>?
        private var shownColours: UInt64?

        init(_ parent: BrowserColumnTable) { self.parent = parent }

        /// Rows before the list's own: "All" and the Compilations row.
        private var leading: Int { (parent.showsAll ? 1 : 0) + (parent.compilations == nil ? 0 : 1) }
        private var compilationsRow: Int { parent.showsAll ? 1 : 0 }
        private var count: Int { leading + parent.total }

        func update(_ parent: BrowserColumnTable) {
            self.parent = parent
            guard let table else { return }
            updating = true
            defer { updating = false }
            let changed = parent.generation != shownGeneration || count != shownCount
                || parent.compilations != shownCompilations
            if changed {
                shownGeneration = parent.generation
                shownCompilations = parent.compilations
                if count != shownCount {
                    shownCount = count
                    table.reloadData()
                } else {
                    table.reloadData(forRowIndexes: table.preparedRows, columnIndexes: IndexSet(integer: 0))
                }
            }
            if changed || parent.selected != shownSelection {
                shownSelection = parent.selected
                let indexes = selectedIndexes()
                if table.selectedRowIndexes != indexes { table.selectRowIndexes(indexes, byExtendingSelection: false) }
            }
            if parent.colours.revision != shownColours {
                shownColours = parent.colours.revision
                table.enumerateAvailableRowViews { view, _ in
                    (view as? TokenRowView)?.selection = parent.colours.selection
                }
            }
            reportVisible()
        }

        private func selectedIndexes() -> IndexSet {
            guard !parent.selected.isEmpty else { return parent.showsAll ? IndexSet(integer: 0) : IndexSet() }
            var indexes = IndexSet()
            if parent.compilations != nil, parent.selected.contains(BrowserSelection.compilationsID) {
                indexes.insert(compilationsRow)
            }
            for row in parent.column.cache.loadedRows() where parent.selected.contains(row.row.id) {
                indexes.insert(leading + row.position)
            }
            return indexes
        }

        private func id(at row: Int) -> UInt64? {
            if row == compilationsRow, parent.compilations != nil { return BrowserSelection.compilationsID }
            guard row >= leading else { return nil }
            return parent.column.cache.loadedRow(at: row - leading)?.id
        }

        @objc func scrolled(_ notification: Notification) { reportVisible() }

        private func reportVisible() {
            guard let table else { return }
            let rows = table.rows(in: table.visibleRect)
            let low = max(0, rows.location - leading)
            let high = max(low, rows.location + rows.length - leading)
            if high > low { parent.column.cache.visible(low..<high) }
        }

        func numberOfRows(in tableView: NSTableView) -> Int { count }

        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            let reuse = NSUserInterfaceItemIdentifier("browser-row")
            let view = tableView.makeView(withIdentifier: reuse, owner: nil) as? TokenRowView ?? TokenRowView()
            view.identifier = reuse
            view.selection = parent.colours.selection
            return view
        }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            let cell = tableView.makeView(withIdentifier: TrackCellView.reuse, owner: nil) as? TrackCellView
                ?? TrackCellView()
            let text: String
            var help = ""
            var cellID = "\(parent.identifier)-placeholder-\(row)"
            if row == 0, parent.showsAll {
                text = DisplayFormat.browserAll(UInt64(parent.total), singular: parent.singular, plural: parent.plural)
                cellID = "\(parent.identifier)-all"
            } else if row == compilationsRow, let compilations = parent.compilations {
                text = "Compilations"
                help = "\(DisplayFormat.count(compilations.album_count)) albums · \(DisplayFormat.songs(compilations.track_count))"
                cellID = "\(parent.identifier)-compilations"
            } else if let item = parent.column.cache.row(at: row - leading) {
                text = parent.label(item)
                help = parent.help(item)
                cellID = "\(parent.identifier)-\(item.id)"
            } else {
                text = ""
            }
            cell.show(text: text, font: .systemFont(ofSize: NSFont.systemFontSize), alignment: .left,
                      secondary: false, identifier: cellID)
            cell.toolTip = help.isEmpty ? nil : help
            return cell
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table else { return }
            let indexes = table.selectedRowIndexes
            let ids: Set<UInt64>
            if !parent.showsAll {
                ids = Set(indexes.compactMap(id(at:)))
                shownSelection = ids
                parent.select(ids)
                return
            }
            if indexes.contains(0) {
                // Adding a row to "All" starts a selection of that row; choosing "All" clears.
                ids = indexes.count > 1 && parent.selected.isEmpty
                    ? Set(indexes.filter { $0 != 0 }.compactMap(id(at:))) : []
            } else {
                ids = Set(indexes.compactMap(id(at:)))
            }
            var shown = indexes
            if ids.isEmpty { shown = IndexSet(integer: 0) } else { shown.remove(0) }
            if shown != indexes {
                updating = true
                table.selectRowIndexes(shown, byExtendingSelection: false)
                updating = false
            }
            shownSelection = ids
            parent.select(ids)
        }
    }
}
