import AppKit
import SwiftUI
import TyrannyCore

/// The album panel's tracklist (spec 4.4): one table over `AlbumExpansion.tracks`, without a
/// header. Columns: number, title, artist (compilations only) and time. Unavailable tracks are
/// dimmed. Selection is held by row ID on the expansion.
///
/// Double-click plays the album from the row to its end. Return plays the selection: one row
/// as a run, several as exactly those rows. ⌥Return adds the selection to Up Next. Space
/// toggles play/pause. Escape closes the panel and gives focus back to the grid. Left and
/// Right do nothing, so they do not reach the grid.
///
/// The scroll view is `album-expansion-tracks` in Accessibility, with the album's track count
/// as its value.
struct AlbumTrackTable: NSViewRepresentable {
    let expansion: AlbumExpansion
    let grid: AlbumGridModel
    let player: PlayerModel
    /// Read in the parent's body so SwiftUI updates the table when they change.
    let tracks: [ListRow]
    let selected: Set<UInt64>
    /// The album's track count, which can exceed the rows shown.
    let total: Int

    static let identifier = "album-track-table"
    /// The panel's tracks area. While the tracks load or fail, the panel's placeholder carries it.
    static let tracksIdentifier = "album-expansion-tracks"

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = false
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        scroll.drawsBackground = false
        scroll.automaticallyAdjustsContentInsets = false
        scroll.setAccessibilityIdentifier(Self.tracksIdentifier)
        scroll.setAccessibilityLabel("Tracks")
        let table = ActivatingTableView()
        table.setAccessibilityIdentifier(Self.identifier)
        table.ignoresLeftRight = true
        table.style = .plain
        table.headerView = nil
        table.backgroundColor = .clear
        table.rowHeight = AlbumExpansionMetrics.trackRowHeight
        table.intercellSpacing = NSSize(width: 6, height: 0)
        table.allowsMultipleSelection = true
        table.allowsEmptySelection = true
        table.allowsColumnReordering = false
        table.allowsColumnResizing = false
        table.columnAutoresizingStyle = .noColumnAutoresizing
        for column in Coordinator.columns {
            let native = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(column.id))
            native.title = column.title
            native.width = column.width
            native.minWidth = column.fixed ? column.width : 40
            native.resizingMask = []
            native.isHidden = column.id == Coordinator.artist
            table.addTableColumn(native)
        }
        table.dataSource = context.coordinator
        table.delegate = context.coordinator
        table.target = context.coordinator
        table.doubleAction = #selector(Coordinator.activateClickedRow(_:))
        scroll.documentView = table
        scroll.contentView.postsFrameChangedNotifications = true
        NotificationCenter.default.addObserver(
            context.coordinator, selector: #selector(Coordinator.resized(_:)),
            name: NSView.frameDidChangeNotification, object: scroll.contentView)
        context.coordinator.attach(table)
        context.coordinator.update(self)
        scroll.setAccessibilityValue(String(total))
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.update(self)
        scroll.setAccessibilityValue(String(total))
    }

    static func dismantleNSView(_ scroll: NSScrollView, coordinator: Coordinator) {
        NotificationCenter.default.removeObserver(coordinator)
    }

    @MainActor final class Coordinator: NSObject, @MainActor NSTableViewDataSource, NSTableViewDelegate {
        struct Column {
            let id: String
            let title: String
            let width: CGFloat
            /// Fixed columns keep their width; Title and Artist share the rest.
            let fixed: Bool
            let alignment: NSTextAlignment
        }

        static let artist = "artist"
        static let columns = [
            Column(id: "number", title: "#", width: 34, fixed: true, alignment: .right),
            Column(id: "title", title: "Title", width: 240, fixed: false, alignment: .left),
            Column(id: artist, title: "Artist", width: 160, fixed: false, alignment: .left),
            Column(id: "time", title: "Time", width: 44, fixed: true, alignment: .right),
        ]

        var parent: AlbumTrackTable
        weak var table: ActivatingTableView?
        private var updating = false
        /// The rows the table shows. Replaced only in `update`, with a reload.
        private var rows: [ListRow] = []
        private weak var shownExpansion: AlbumExpansion?

        init(_ parent: AlbumTrackTable) { self.parent = parent }

        func attach(_ table: ActivatingTableView) {
            self.table = table
            table.onActivate = { [weak self] action in self?.activateSelected(action) }
            table.onPlayPause = { [weak self] in self?.parent.player.transport.playPause() }
            table.onCancel = { [weak self, weak table] in
                guard let self, let table else { return }
                let grid = sequence(first: table as NSView, next: { $0.superview }).first { $0 is AlbumCollectionView }
                parent.grid.collapse()
                if let grid { grid.window?.makeFirstResponder(grid) }
            }
            table.contextMenu = { [weak self] row in self?.menu(forRow: row) }
        }

        // MARK: Updates

        /// Reloads when the tracks or the album change, then applies the expansion's selection.
        func update(_ parent: AlbumTrackTable) {
            self.parent = parent
            guard let table else { return }
            updating = true
            defer { updating = false }
            let albumChanged = parent.expansion !== shownExpansion
            if albumChanged || parent.tracks != rows {
                shownExpansion = parent.expansion
                rows = parent.tracks
                table.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier(Self.artist))?.isHidden =
                    !parent.expansion.album.compilation
                fitColumns()
                table.reloadData()
                if albumChanged { table.scrollRowToVisible(0) }
            }
            let wanted = IndexSet(rows.indices.filter { parent.selected.contains(rows[$0].row_id) })
            if table.selectedRowIndexes != wanted { table.selectRowIndexes(wanted, byExtendingSelection: false) }
        }

        @objc func resized(_ notification: Notification) { fitColumns() }

        /// Title takes the width the number and time leave. On compilations Artist takes 40% of it.
        private func fitColumns() {
            guard let table, let clip = table.enclosingScrollView?.contentView else { return }
            let shown = table.tableColumns.filter { !$0.isHidden }
            let fixed = Self.columns.filter(\.fixed).reduce(0) { $0 + $1.width }
            let free = max(80, floor(clip.bounds.width - fixed - CGFloat(shown.count) * table.intercellSpacing.width))
            guard let title = table.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier("title")),
                  let artist = table.tableColumn(withIdentifier: NSUserInterfaceItemIdentifier(Self.artist)) else { return }
            let titleWidth = artist.isHidden ? free : floor(free * 0.6)
            if title.width != titleWidth { title.width = titleWidth }
            if !artist.isHidden, artist.width != free - titleWidth { artist.width = free - titleWidth }
        }

        // MARK: Data source

        func numberOfRows(in tableView: NSTableView) -> Int { rows.count }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard let tableColumn, rows.indices.contains(row),
                  let column = Self.columns.first(where: { $0.id == tableColumn.identifier.rawValue }) else { return nil }
            let item = rows[row]
            let cell = tableView.makeView(withIdentifier: TrackCellView.reuse, owner: nil) as? TrackCellView
                ?? TrackCellView()
            let text = value(column.id, item.track)
            let font = column.fixed
                ? NSFont.monospacedDigitSystemFont(ofSize: 12, weight: .regular) : NSFont.systemFont(ofSize: 12)
            cell.show(text: text, font: font, alignment: column.alignment,
                      secondary: !item.available || column.id == "number" || column.id == "time",
                      identifier: "\(AlbumTrackTable.identifier)-cell-\(item.row_id)-\(column.id)")
            cell.toolTip = item.available ? (column.fixed || text.isEmpty ? nil : text)
                : "\(item.track.title)\nUnavailable: \(item.track.path)"
            return cell
        }

        private func value(_ column: String, _ track: TrackSummary) -> String {
            switch column {
            case "number":
                let number = track.track_number > 0 ? String(track.track_number) : ""
                return parent.expansion.album.disc_count > 1 && track.disc_number > 0
                    ? "\(track.disc_number)-\(number)" : number
            case "title": return track.title
            case Self.artist: return track.artist
            default: return track.duration_ms.map { DisplayFormat.clock(Double($0) / 1000) } ?? ""
            }
        }

        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            let reuse = NSUserInterfaceItemIdentifier("album-track-row")
            let view = tableView.makeView(withIdentifier: reuse, owner: nil) as? NSTableRowView ?? NSTableRowView()
            view.identifier = reuse
            view.setAccessibilityIdentifier(rows.indices.contains(row)
                ? "\(AlbumTrackTable.identifier)-row-\(rows[row].row_id)" : nil)
            return view
        }

        // MARK: Delegate

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table else { return }
            parent.expansion.selectedRowIDs = Set(table.selectedRowIndexes.compactMap {
                rows.indices.contains($0) ? rows[$0].row_id : nil
            })
        }

        // MARK: Input

        /// Double-click: the album from the clicked row to its end.
        @objc func activateClickedRow(_ table: NSTableView) {
            guard rows.indices.contains(table.clickedRow) else { return }
            let id = rows[table.clickedRow].row_id
            let expansion = parent.expansion
            guard let position = expansion.tracks.firstIndex(where: { $0.row_id == id }) else { return }
            parent.grid.playTracks(expansion, positions: IndexSet(integer: position), asRun: true)
        }

        /// Return and ⌥Return act on the selection; with nothing selected they do nothing.
        private func activateSelected(_ action: ActivationAction) {
            let expansion = parent.expansion
            let positions = expansion.selectedPositions
            guard !positions.isEmpty else { return }
            switch action {
            case .play: parent.grid.playTracks(expansion, positions: positions, asRun: true)
            case .playNext: parent.grid.playNext(expansion)
            case .addToUpNext: parent.grid.addToUpNext(expansion)
            }
        }

        // MARK: Context menu

        private func menu(forRow row: Int) -> NSMenu? {
            guard let table, rows.indices.contains(row) else { return nil }
            if !table.selectedRowIndexes.contains(row) {
                table.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
            }
            let menu = NSMenu(title: "Tracks")
            let busy = !parent.player.canMutate
            add(to: menu, "Play", "album-track-menu-play", #selector(menuPlay), enabled: !busy)
            add(to: menu, "Play Next", "album-track-menu-play-next", #selector(menuPlayNext), enabled: !busy)
            add(to: menu, "Add to Up Next", "album-track-menu-add-to-up-next", #selector(menuAddToUpNext), enabled: !busy)
            menu.addItem(.separator())
            let playlists = NSMenu(title: "Add to Playlist")
            let regular = parent.grid.regularPlaylists
            if regular.isEmpty {
                playlists.addItem(NSMenuItem(title: "No Playlists", action: nil, keyEquivalent: ""))
            }
            for playlist in regular {
                let item = add(to: playlists, playlist.name, "album-track-menu-playlist-\(playlist.id)",
                               #selector(menuAddToPlaylist(_:)), enabled: !busy)
                item.representedObject = playlist
            }
            let parentItem = NSMenuItem(title: "Add to Playlist", action: nil, keyEquivalent: "")
            parentItem.setAccessibilityIdentifier("album-track-menu-add-to-playlist")
            parentItem.submenu = playlists
            menu.addItem(parentItem)
            return menu
        }

        @discardableResult
        private func add(to menu: NSMenu, _ title: String, _ identifier: String, _ action: Selector,
                         enabled: Bool) -> NSMenuItem {
            let item = NSMenuItem(title: title, action: enabled ? action : nil, keyEquivalent: "")
            item.target = self
            item.setAccessibilityIdentifier(identifier)
            menu.addItem(item)
            return item
        }

        // The menu plays the selected rows only, not a run to the end of the album.
        @objc private func menuPlay() {
            parent.grid.playTracks(parent.expansion, positions: parent.expansion.selectedPositions, asRun: false)
        }
        @objc private func menuPlayNext() { parent.grid.playNext(parent.expansion) }
        @objc private func menuAddToUpNext() { parent.grid.addToUpNext(parent.expansion) }
        @objc private func menuAddToPlaylist(_ item: NSMenuItem) {
            guard let playlist = item.representedObject as? Playlist else { return }
            parent.grid.addToPlaylist(parent.expansion, playlist: playlist)
        }
    }
}
