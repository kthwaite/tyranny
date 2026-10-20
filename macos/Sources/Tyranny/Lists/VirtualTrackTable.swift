import AppKit
import Observation
import SwiftUI
import TyrannyCore

/// A continuous, virtualized track table over a `ListModel` (spec 4.2, 4.3, 7).
///
/// The table shows the list's `GroupLayout`: one row per position when ungrouped; artist
/// headers, album blocks (tracks, fillers, a gap) or both when grouped. Display rows map to
/// list positions through the layout everywhere: cells, selection, drag, activation and the
/// visible range reported to the cache. A track row not loaded yet is a blank placeholder of
/// the same height; drawing it requests its window. Headers, fillers and gaps are never
/// selected, dragged or activated. Selection is held by row ID across reloads, revision
/// changes and regrouping; the anchor row stays in view after a refresh pass once the
/// layout matches the rows. Drags carry a `TrackSelection`.
struct VirtualTrackTable: NSViewRepresentable {
    let list: ListModel
    let player: PlayerModel
    /// Read in the parent's body so SwiftUI updates the table when they change.
    let generation: UInt64
    let epoch: UInt64
    let layoutToken: ListModel.LayoutToken
    let factsGeneration: UInt64
    let hidden: Set<String>
    let explicit: Set<String>
    let playingTrackID: UInt64?
    /// Read in the parent's body so a ⌘L reveal reaches the table.
    let revealToken: UInt64?
    let selectionToken: UInt64

    var identifier: String { "track-table-\(list.key)" }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        let table = ActivatingTableView()
        table.setAccessibilityIdentifier(identifier)
        table.style = .fullWidth
        table.rowHeight = Coordinator.rowHeight
        table.intercellSpacing = NSSize(width: 6, height: 0)
        table.usesAlternatingRowBackgroundColors = true
        table.allowsMultipleSelection = true
        table.allowsEmptySelection = true
        table.allowsColumnReordering = true
        table.floatsGroupRows = false
        table.columnAutoresizingStyle = .noColumnAutoresizing
        let gutter = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(AlbumBlockCell.columnID))
        gutter.title = "Album"
        gutter.width = AlbumBlockCell.columnWidth
        gutter.minWidth = AlbumBlockCell.columnWidth
        gutter.maxWidth = AlbumBlockCell.columnWidth
        gutter.resizingMask = []
        gutter.headerToolTip = "Album"
        gutter.isHidden = true
        table.addTableColumn(gutter)
        for column in TrackColumns.all {
            let native = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(column.id))
            native.title = column.title
            native.width = column.width
            native.minWidth = column.minWidth
            native.headerCell.alignment = column.alignment
            if let sort = column.sort {
                native.sortDescriptorPrototype = NSSortDescriptor(key: sort.rawValue, ascending: true)
            }
            native.headerCell.setAccessibilityIdentifier("track-header-\(column.id)")
            if column.id == TrackColumns.status {
                native.resizingMask = []
                native.headerToolTip = "Playing"
            }
            table.addTableColumn(native)
        }
        if !NativeRenderViews.isRendering {
            table.autosaveName = identifier
            table.autosaveTableColumns = true
        }
        let menu = NSMenu(title: "Columns")
        menu.delegate = context.coordinator
        table.headerView?.menu = menu
        table.dataSource = context.coordinator
        table.delegate = context.coordinator
        table.target = context.coordinator
        table.doubleAction = #selector(Coordinator.activateClickedRow(_:))
        table.setDraggingSourceOperationMask(.copy, forLocal: true)
        table.setDraggingSourceOperationMask([], forLocal: false)
        if case .playlist = list.source {
            // Own-row drops are reorder attempts (spec 4.3).
            table.registerForDraggedTypes([NSPasteboard.PasteboardType(TrackDrag.type)])
        }
        scroll.documentView = table
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(
            context.coordinator, selector: #selector(Coordinator.scrolled(_:)),
            name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        NotificationCenter.default.addObserver(
            context.coordinator, selector: #selector(Coordinator.scrolled(_:)),
            name: NSView.frameDidChangeNotification, object: scroll)
        context.coordinator.attach(table: table, scroll: scroll)
        context.coordinator.update(self)
        return scroll
    }

    func updateNSView(_ scroll: NSScrollView, context: Context) {
        context.coordinator.update(self)
    }

    static func dismantleNSView(_ scroll: NSScrollView, coordinator: Coordinator) {
        NotificationCenter.default.removeObserver(coordinator)
        coordinator.releaseArtwork()
    }

    @MainActor final class Coordinator: NSObject, @MainActor NSTableViewDataSource, NSTableViewDelegate,
        @MainActor NSMenuDelegate {
        static let rowHeight: CGFloat = 20
        var parent: VirtualTrackTable
        weak var table: NSTableView?
        private weak var scroll: NSScrollView?
        private var updating = false
        /// The layout the table's rows show. Replaced only in `update`, with a reload.
        private(set) var layout = GroupLayout(grouping: .none, total: 0, spans: [])
        private var shownExplicit: Set<String> = []
        private var shownSort: [SortKey] = []
        private var shownLayout: ListModel.LayoutToken?
        private var shownGeneration: UInt64?
        private var shownEpoch: UInt64?
        private var shownFacts: UInt64?
        private var shownPlaying: UInt64?
        private var shownHidden: Set<String>?
        private var shownBlocks: Bool?
        private var reported: Range<Int>?
        /// The reveal request last handled. The outer nil: none seen yet.
        var shownReveal: UInt64??
        /// Album covers retained for the albums in view.
        private var artwork: [UInt64: ArtworkRequest] = [:]
        private var artworkObserved = false
        /// Set when the view is dismantled: no more artwork demand.
        private var dismantled = false
        /// A drag in progress. Its pasteboard item is written for the first rows only.
        private var dragToken: String?
        private var dragItems = 0
        private var dragLifetime: Coordinator?

        init(_ parent: VirtualTrackTable) { self.parent = parent }

        func attach(table: NSTableView, scroll: NSScrollView) {
            self.table = table
            self.scroll = scroll
            if let table = table as? ActivatingTableView { configureInput(table) }
        }

        private var cache: ListWindowCache<ListRow> { parent.list.cache }
        private var store: ArtworkStore { parent.player.artwork }

        // MARK: Updates

        func update(_ parent: VirtualTrackTable) {
            self.parent = parent
            guard let table else { return }
            updating = true
            defer { updating = false }
            let blocks = parent.list.showsAlbumBlocks
            if parent.hidden != shownHidden || parent.explicit != shownExplicit || blocks != shownBlocks || parent.list.sortKeys != shownSort {
                shownSort = parent.list.sortKeys
                shownHidden = parent.hidden
                shownExplicit = parent.explicit
                shownBlocks = blocks
                for column in table.tableColumns {
                    let id = column.identifier.rawValue
                    column.isHidden = id == AlbumBlockCell.columnID ? !blocks : isHidden(id)
                }
                // Autosaved column order may predate the gutter; it is always first.
                if let gutter = table.tableColumns.firstIndex(where: { $0.identifier.rawValue == AlbumBlockCell.columnID }),
                   gutter != 0 {
                    table.moveColumn(gutter, toColumn: 0)
                }
            }
            let descriptors = parent.list.sortKeys.map {
                NSSortDescriptor(key: $0.field.rawValue, ascending: !$0.descending)
            }
            if table.sortDescriptors != descriptors { table.sortDescriptors = descriptors }

            let epochChanged = parent.epoch != shownEpoch
            if epochChanged {
                shownEpoch = parent.epoch
                reported = nil
                table.scrollRowToVisible(0)
            }
            let layoutChanged = parent.layoutToken != shownLayout
            /// A full `reloadData` already refreshed every row view.
            var reloadedAll = false
            var rowsWereCreated = false
            if layoutChanged {
                let wasGrouped = layout.grouping != .none
                layout = parent.list.layout
                shownLayout = parent.layoutToken
                reported = nil
                if wasGrouped || layout.grouping != .none {
                    // Row kinds and heights may all have changed.
                    table.reloadData()
                    reloadedAll = true
                } else {
                    // Ungrouped, a refresh pass changes the total and the rows together.
                    let rowsBefore = table.numberOfRows
                    table.noteNumberOfRowsChanged()
                    // First rows were just created; skipping the reload relies on `viewFor` reading the current cache when row views are made.
                    rowsWereCreated = rowsBefore == 0 && table.numberOfRows > 0
                }
            }
            if parent.generation != shownGeneration || epochChanged || layoutChanged {
                shownGeneration = parent.generation
                restoreAnchor()
                if !reloadedAll && !rowsWereCreated { reloadVisible() }
                let rows = layout.displayIndexes(ofPositions: parent.list.rowsChanged())
                if table.selectedRowIndexes != rows { table.selectRowIndexes(rows, byExtendingSelection: false) }
            } else if parent.playingTrackID != shownPlaying, let status = columnIndex(TrackColumns.status) {
                reloadVisible(columns: IndexSet(integer: status))
            }
            if parent.factsGeneration != shownFacts {
                shownFacts = parent.factsGeneration
                if !reloadedAll { reloadGutter() }
            }
            shownPlaying = parent.playingTrackID
            let selected = layout.displayIndexes(ofPositions: parent.list.selectedPositions)
            if table.selectedRowIndexes != selected { table.selectRowIndexes(selected, byExtendingSelection: false) }
            applyReveal(parent.revealToken)
            reportVisible()
        }

        private func columnIndex(_ id: String) -> Int? {
            table?.tableColumns.firstIndex { $0.identifier.rawValue == id }
        }

        private func reloadVisible(columns: IndexSet? = nil) {
            guard let table else { return }
            let rows = table.preparedRows
            guard !rows.isEmpty else { return }
            table.reloadData(forRowIndexes: rows,
                             columnIndexes: columns ?? IndexSet(integersIn: 0..<table.numberOfColumns))
            guard columns == nil else { return }
            // Full-width header rows have no column; refresh them in place.
            for row in rows {
                guard case .artistHeader(let key, let span)? = layout.row(at: row),
                      let cell = table.view(atColumn: 0, row: row, makeIfNecessary: false) as? ArtistHeaderCell else { continue }
                configure(cell, artistKey: key, span: span)
            }
        }

        private func reloadGutter() {
            guard layout.grouping == .album || layout.grouping == .artistAlbum,
                  let gutter = columnIndex(AlbumBlockCell.columnID) else { return }
            reloadVisible(columns: IndexSet(integer: gutter))
        }

        /// After a refresh pass, puts the anchor row back where it was in view. Waits while the
        /// layout is older than the rows, whose positions it would misplace.
        private func restoreAnchor() {
            guard let anchor = cache.anchorPosition, parent.list.layoutIsCurrent else { return }
            cache.anchorPosition = nil
            guard let display = layout.displayIndex(ofPosition: anchor) else { return }
            scrollToTop(max(0, display - parent.list.anchorOffset))
        }

        /// Puts display row `index` at the top of the visible rows.
        private func scrollToTop(_ index: Int) {
            guard let table, let scroll, index < table.numberOfRows else { return }
            // After a reload the frame may still have the old height, and the clip view would
            // clamp the scroll to it.
            table.tile()
            let first = table.rows(in: table.visibleRect).location
            guard first != index else { return }
            let y = table.rect(ofRow: index).minY - scroll.contentInsets.top
            scroll.contentView.scroll(to: NSPoint(x: scroll.contentView.bounds.origin.x, y: y))
            scroll.reflectScrolledClipView(scroll.contentView)
        }

        @objc func scrolled(_ notification: Notification) { reportVisible() }

        private func reportVisible() {
            guard !dismantled, let table else { return }
            let rows = table.rows(in: table.visibleRect)
            guard rows.length > 0 else { return }
            let range = rows.location..<rows.location + rows.length
            guard range != reported else { return }
            reported = range
            parent.list.visible(range)
            updateArtwork(range)
        }

        // MARK: Artwork

        /// Retains the covers of the albums in `range` and releases the rest.
        private func updateArtwork(_ range: Range<Int>) {
            guard !dismantled else { return }
            var albums = Set<UInt64>()
            if layout.grouping == .album || layout.grouping == .artistAlbum {
                for index in range { if let album = layout.albumKey(atDisplayIndex: index) { albums.insert(album) } }
            }
            var next: [UInt64: ArtworkRequest] = [:]
            for album in albums { if let request = store.albumRequest(album) { next[album] = request } }
            for (album, request) in artwork where next[album] != request { store.release(request) }
            for (album, request) in next where artwork[album] != request { store.retain(request) }
            artwork = next
            observeArtwork()
        }

        /// Redraws the gutter when a retained cover arrives or the artwork context changes.
        private func observeArtwork() {
            guard !artworkObserved, !artwork.isEmpty else { return }
            artworkObserved = true
            let store = self.store
            let albums = Array(artwork.keys)
            withObservationTracking {
                for album in albums { _ = store.value(for: store.albumRequest(album)) }
            } onChange: { [weak self] in
                Task { @MainActor in
                    guard let self, !self.dismantled else { return }
                    self.artworkObserved = false
                    if let range = self.reported { self.updateArtwork(range) }
                    self.reloadGutter()
                }
            }
        }

        func releaseArtwork() {
            dismantled = true
            reported = nil
            for request in artwork.values { store.release(request) }
            artwork.removeAll()
        }

        // MARK: Data source

        func numberOfRows(in tableView: NSTableView) -> Int { layout.count }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            let display = layout.row(at: row)
            if case .artistHeader(let key, let span)? = display {
                guard tableColumn == nil else { return nil }
                let cell = tableView.makeView(withIdentifier: ArtistHeaderCell.reuse, owner: nil) as? ArtistHeaderCell
                    ?? ArtistHeaderCell()
                configure(cell, artistKey: key, span: span)
                return cell
            }
            guard let tableColumn else { return nil }
            let id = tableColumn.identifier.rawValue
            if id == AlbumBlockCell.columnID { return gutterCell(tableView, row: row) }
            guard let column = TrackColumns.column(id) else { return nil }
            let cell = tableView.makeView(withIdentifier: TrackCellView.reuse, owner: nil) as? TrackCellView
                ?? TrackCellView()
            guard case .track(let position, let blockIndex)? = display else {
                // Fillers and gaps pad album blocks: nothing to show, nothing to select.
                cell.show(text: "", font: font(column), alignment: column.alignment, secondary: false,
                          identifier: nil)
                cell.toolTip = nil
                return cell
            }
            guard let item = cache.row(at: position) else {
                cell.show(text: "", font: font(column), alignment: column.alignment, secondary: false,
                          identifier: "\(parent.identifier)-placeholder-\(position)-\(column.id)")
                cell.toolTip = nil
                return cell
            }
            let identifier = "\(parent.identifier)-cell-\(item.row_id)-\(column.id)"
            if column.id == TrackColumns.status {
                cell.showPlaying(item.track.id == parent.playingTrackID && parent.playingTrackID != nil,
                                 identifier: identifier)
                cell.toolTip = nil
                return cell
            }
            let text = column.value(item.track)
            // Compilation blocks keep a track Artist column even when the table's is hidden.
            let artist = column.id == "title" && blockIndex != nil && item.track.compilation
                && parent.hidden.contains("artist") ? item.track.artist : nil
            cell.show(text: text, font: font(column), alignment: column.alignment, secondary: !item.available,
                      identifier: identifier, detail: artist)
            let tip = artist.map { "\(text) — \($0)" } ?? text
            cell.toolTip = item.available ? (tip.isEmpty ? nil : tip) : "\(tip)\nUnavailable: \(item.track.path)"
            return cell
        }

        private func gutterCell(_ tableView: NSTableView, row: Int) -> NSView {
            let cell = tableView.makeView(withIdentifier: AlbumBlockCell.reuse, owner: nil) as? AlbumBlockCell
                ?? AlbumBlockCell()
            let album = layout.albumKey(atDisplayIndex: row)
            let slice = layout.blockIndex(atDisplayIndex: row) ?? AlbumBlockCell.contentRows
            let needsContent = slice < AlbumBlockCell.contentRows
            let image = needsContent ? album.map { store.value(for: store.albumRequest($0)) } ?? .missing : .missing
            let facts = needsContent ? album.flatMap { parent.list.facts.summary($0) }.map(DisplayFormat.albumFacts) ?? [] : []
            cell.show(albumKey: album, slice: slice, rowHeight: Self.rowHeight, artwork: image, facts: facts)
            return cell
        }

        private func configure(_ cell: ArtistHeaderCell, artistKey: UInt64, span: Range<Int>) {
            let name = cache.row(at: span.lowerBound)?.track.album_artist
            cell.show(artistKey: artistKey, artist: name, albums: parent.list.albumCounts[artistKey], tracks: span.count)
        }

        private func font(_ column: TrackColumn) -> NSFont {
            column.monospacedDigits
                ? .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
                : .systemFont(ofSize: NSFont.systemFontSize)
        }

        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            let reuse = NSUserInterfaceItemIdentifier("track-row")
            let view = tableView.makeView(withIdentifier: reuse, owner: nil) as? NSTableRowView ?? NSTableRowView()
            view.identifier = reuse
            let id: String
            switch layout.row(at: row) {
            case .track(let position, _)?:
                id = cache.loadedRow(at: position).map { "\(parent.identifier)-row-\($0.row_id)" }
                    ?? "\(parent.identifier)-placeholder-\(position)"
            case .artistHeader(let key, _)?:
                id = "\(parent.identifier)-header-\(key)"
            case .blockFiller?, .blockGap?, nil:
                id = "\(parent.identifier)-filler-\(row)"
            }
            view.setAccessibilityIdentifier(id)
            return view
        }

        func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool {
            if case .artistHeader? = layout.row(at: row) { return true }
            return false
        }

        func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
            if case .artistHeader? = layout.row(at: row) { return ArtistHeaderCell.height }
            return Self.rowHeight
        }

        // MARK: Delegate

        /// Only track rows are selected (`GroupLayout.selection`). ⌘A, shift-ranges and clicks drop
        /// headers, fillers and gaps; an arrow or shift-arrow onto one goes on to the next track.
        func tableView(_ tableView: NSTableView, selectionIndexesForProposedSelection proposed: IndexSet) -> IndexSet {
            guard layout.grouping != .none else { return proposed }
            let current = tableView.selectedRowIndexes
            let result = layout.selection(proposed: proposed, current: current,
                                          keyboard: NSApp.currentEvent?.type == .keyDown)
            if let added = result.subtracting(current).last ?? result.subtracting(current).first {
                tableView.scrollRowToVisible(added)
            }
            return result
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table else { return }
            parent.list.userSelected(layout.positions(ofDisplayIndexes: table.selectedRowIndexes))
        }

        func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
            guard !updating else { return }
            // The server appends its own tiebreak, so only the primary key is sent.
            let keys = tableView.sortDescriptors.prefix(1).compactMap { descriptor in
                descriptor.key.flatMap(SortField.init(rawValue:)).map { SortKey(field: $0, descending: !descriptor.ascending) }
            }
            parent.list.setSort(Array(keys))
        }

        func tableView(_ tableView: NSTableView, shouldReorderColumn columnIndex: Int, toColumn newColumnIndex: Int) -> Bool {
            // The album gutter stays first.
            let gutter = self.columnIndex(AlbumBlockCell.columnID)
            return columnIndex != gutter && newColumnIndex != gutter
        }

        private func isHidden(_ id: String) -> Bool {
            !ColumnPreferences.shared.isShown(id, sortFields: parent.list.sortKeys.map(\.field))
        }

        @objc func activateClickedRow(_ table: NSTableView) {
            guard case .track(let position, _)? = layout.row(at: table.clickedRow) else { return }
            parent.list.activate(position: position)
        }

        // MARK: Header menu

        func menuNeedsUpdate(_ menu: NSMenu) {
            menu.removeAllItems()
            for column in TrackColumns.all where column.hideable {
                let item = NSMenuItem(title: column.title, action: #selector(toggleColumn(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = column.id
                item.state = isHidden(column.id) ? .off : .on
                menu.addItem(item)
            }
        }

        @objc func toggleColumn(_ item: NSMenuItem) {
            guard let id = item.representedObject as? String else { return }
            ColumnPreferences.shared.toggle(id, sortFields: parent.list.sortKeys.map(\.field))
        }

        // MARK: Drag

        func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> (any NSPasteboardWriting)? {
            guard case .track(let position, _)? = layout.row(at: row) else { return nil }
            if dragToken == nil {
                let positions = tableView.selectedRowIndexes.contains(row)
                    ? layout.positions(ofDisplayIndexes: tableView.selectedRowIndexes) : IndexSet(integer: position)
                do {
                    let selection = try parent.list.encode(positions)
                    dragToken = parent.player.beginSelectionDrag(selection, count: positions.count,
                                                                 label: parent.list.displayName ?? parent.list.destination.title,
                                                                 trackIDs: parent.list.loadedTrackIDs(positions))
                    if dragToken != nil { parent.list.dragBegan(positions: positions) }
                } catch {
                    parent.player.message = (error as? LocalizedError)?.errorDescription
                        ?? "Wait for the list to load, then drag again."
                }
                dragItems = 0
            }
            // A ⌘A drag of 50,000 rows writes one token; a few items give the drag image.
            guard let dragToken, dragItems < 24 else { return nil }
            dragItems += 1
            let item = NSPasteboardItem()
            item.setString(dragToken, forType: NSPasteboard.PasteboardType(TrackDrag.type))
            return item
        }

        func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession,
                       willBeginAt screenPoint: NSPoint, forRowIndexes rowIndexes: IndexSet) {
            // Keep the native source alive if the view is replaced during the drag.
            dragLifetime = self
        }

        func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession,
                       endedAt screenPoint: NSPoint, operation: NSDragOperation) {
            dragToken = nil
            dragItems = 0
            dragLifetime = nil
            parent.list.dragEnded()
        }

        // MARK: Drops (regular playlists)

        private static let dragType = NSPasteboard.PasteboardType(TrackDrag.type)

        func tableView(_ tableView: NSTableView, validateDrop info: any NSDraggingInfo, proposedRow row: Int,
                       proposedDropOperation dropOperation: NSTableView.DropOperation) -> NSDragOperation {
            guard let token = info.draggingPasteboard.string(forType: Self.dragType) else { return [] }
            let own = (info.draggingSource as? NSTableView) === tableView
            switch parent.list.proposeDrop(token: token, own: own, row: row) {
            case .refuse:
                return []
            case .row(let at):
                tableView.setDropRow(at, dropOperation: .above)
                return .copy
            case .table:
                tableView.setDropRow(-1, dropOperation: .on)
                return .copy
            }
        }

        func tableView(_ tableView: NSTableView, acceptDrop info: any NSDraggingInfo, row: Int,
                       dropOperation: NSTableView.DropOperation) -> Bool {
            guard let token = info.draggingPasteboard.string(forType: Self.dragType) else { return false }
            let own = (info.draggingSource as? NSTableView) === tableView
            return parent.list.acceptDrop(token: token, own: own, row: row)
        }
    }
}

extension NSTableView {
    /// The rows that have views: the visible rows and those responsive scrolling prepared
    /// around them. Reloads must cover all of them, or prepared rows keep stale content.
    var preparedRows: IndexSet {
        let visible = rows(in: visibleRect)
        let prepared = rows(in: preparedContentRect)
        var result = IndexSet(integersIn: visible.location..<visible.location + visible.length)
        result.insert(integersIn: prepared.location..<prepared.location + prepared.length)
        return result.filteredIndexSet { $0 < numberOfRows }
    }
}

/// A table cell: one vertically centred text field, optionally followed by a secondary
/// detail field (a compilation track's artist), or the playing indicator.
final class TrackCellView: NSTableCellView {
    static let reuse = NSUserInterfaceItemIdentifier("track-cell")
    private let field = NSTextField(labelWithString: "")
    private let detail = NSTextField(labelWithString: "")
    private let icon = NSImageView()
    /// Built once: creating a symbol image per cell was a measurable part of first-row cost.
    private static let playingImage = NSImage(systemSymbolName: "speaker.wave.2.fill", accessibilityDescription: "Playing")
    private var fieldToEdge: NSLayoutConstraint!
    private var fieldToDetail: NSLayoutConstraint!

    init() {
        super.init(frame: .zero)
        identifier = Self.reuse
        for text in [field, detail] {
            text.lineBreakMode = .byTruncatingTail
            text.translatesAutoresizingMaskIntoConstraints = false
            text.cell?.usesSingleLineMode = true
            addSubview(text)
        }
        detail.textColor = .secondaryLabelColor
        detail.isHidden = true
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.contentTintColor = .controlAccentColor
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 10, weight: .regular)
        addSubview(icon)
        textField = field
        fieldToEdge = field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2)
        fieldToDetail = field.trailingAnchor.constraint(equalTo: detail.leadingAnchor, constant: -8)
        NSLayoutConstraint.activate([
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            fieldToEdge,
            field.centerYAnchor.constraint(equalTo: centerYAnchor),
            detail.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2),
            detail.widthAnchor.constraint(equalTo: widthAnchor, multiplier: 0.4),
            detail.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.centerXAnchor.constraint(equalTo: centerXAnchor),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func show(text: String, font: NSFont, alignment: NSTextAlignment, secondary: Bool, identifier: String?,
              detail detailText: String? = nil) {
        icon.isHidden = true
        icon.image = nil
        field.isHidden = false
        field.stringValue = text
        field.font = font
        field.alignment = alignment
        field.textColor = secondary ? .secondaryLabelColor : .labelColor
        field.setAccessibilityIdentifier(identifier)
        let showsDetail = detailText != nil
        detail.isHidden = !showsDetail
        detail.stringValue = detailText ?? ""
        detail.font = font
        detail.setAccessibilityIdentifier(showsDetail ? identifier.map { $0 + "-artist" } : nil)
        // Deactivate first: both active at once conflict.
        if showsDetail {
            fieldToEdge.isActive = false
            fieldToDetail.isActive = true
        } else {
            fieldToDetail.isActive = false
            fieldToEdge.isActive = true
        }
    }

    func showPlaying(_ playing: Bool, identifier: String) {
        field.isHidden = true
        field.stringValue = ""
        detail.isHidden = true
        icon.isHidden = !playing
        icon.image = playing ? Self.playingImage : nil
        icon.setAccessibilityIdentifier(identifier)
        field.setAccessibilityIdentifier(nil)
    }
}
