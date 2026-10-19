import AppKit
import SwiftUI
import TyrannyCore

/// A continuous, virtualized track table over a `ListModel` (spec 4.2, 7).
///
/// The table has `cache.total` rows. A row not loaded yet is a blank placeholder of the
/// same height; drawing it requests its window. Scrolling reports the visible rows to
/// the cache. Selection is held by row ID across reloads and revision changes; the
/// first visible row stays in view after a refresh pass. Drags carry a `TrackSelection`.
struct VirtualTrackTable: NSViewRepresentable {
    let list: ListModel
    let player: PlayerModel
    /// Read in the parent's body so SwiftUI updates the table when they change.
    let generation: UInt64
    let total: Int
    let epoch: UInt64
    let hidden: Set<String>
    let playingTrackID: UInt64?

    var identifier: String { "track-table-\(list.destination.key)" }

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        let table = NSTableView()
        table.setAccessibilityIdentifier(identifier)
        table.style = .fullWidth
        table.rowHeight = 20
        table.intercellSpacing = NSSize(width: 6, height: 0)
        table.usesAlternatingRowBackgroundColors = true
        table.allowsMultipleSelection = true
        table.allowsEmptySelection = true
        table.allowsColumnReordering = true
        table.columnAutoresizingStyle = .noColumnAutoresizing
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
        table.autosaveName = identifier
        table.autosaveTableColumns = true
        let menu = NSMenu(title: "Columns")
        menu.delegate = context.coordinator
        table.headerView?.menu = menu
        table.dataSource = context.coordinator
        table.delegate = context.coordinator
        table.target = context.coordinator
        table.doubleAction = #selector(Coordinator.activateClickedRow(_:))
        table.setDraggingSourceOperationMask(.copy, forLocal: true)
        table.setDraggingSourceOperationMask([], forLocal: false)
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
    }

    @MainActor final class Coordinator: NSObject, @MainActor NSTableViewDataSource, NSTableViewDelegate,
        @MainActor NSMenuDelegate {
        var parent: VirtualTrackTable
        private weak var table: NSTableView?
        private weak var scroll: NSScrollView?
        private var updating = false
        private var shownTotal = 0
        private var shownGeneration: UInt64?
        private var shownEpoch: UInt64?
        private var shownPlaying: UInt64?
        private var shownHidden: Set<String>?
        private var reported: Range<Int>?
        /// A drag in progress. Its pasteboard item is written for the first rows only.
        private var dragToken: String?
        private var dragItems = 0
        private var dragLifetime: Coordinator?

        init(_ parent: VirtualTrackTable) { self.parent = parent }

        func attach(table: NSTableView, scroll: NSScrollView) {
            self.table = table
            self.scroll = scroll
        }

        private var cache: ListWindowCache<ListRow> { parent.list.cache }

        // MARK: Updates

        func update(_ parent: VirtualTrackTable) {
            self.parent = parent
            guard let table else { return }
            updating = true
            defer { updating = false }
            if parent.hidden != shownHidden {
                shownHidden = parent.hidden
                for column in table.tableColumns {
                    column.isHidden = parent.hidden.contains(column.identifier.rawValue)
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
            if parent.total != shownTotal {
                shownTotal = parent.total
                table.noteNumberOfRowsChanged()
            }
            if parent.generation != shownGeneration || epochChanged {
                shownGeneration = parent.generation
                if let anchor = cache.anchorPosition {
                    cache.anchorPosition = nil
                    scrollToTop(anchor)
                }
                reloadVisible()
                let positions = parent.list.rowsChanged()
                if table.selectedRowIndexes != positions { table.selectRowIndexes(positions, byExtendingSelection: false) }
            } else if parent.playingTrackID != shownPlaying, let status = table.tableColumns.firstIndex(where: {
                $0.identifier.rawValue == TrackColumns.status
            }) {
                reloadVisible(columns: IndexSet(integer: status))
            }
            shownPlaying = parent.playingTrackID
            reportVisible()
        }

        private func reloadVisible(columns: IndexSet? = nil) {
            guard let table else { return }
            let rows = table.rows(in: table.visibleRect)
            guard rows.length > 0 else { return }
            table.reloadData(forRowIndexes: IndexSet(integersIn: rows.location..<rows.location + rows.length),
                             columnIndexes: columns ?? IndexSet(integersIn: 0..<table.numberOfColumns))
        }

        /// Puts `position` at the top of the visible rows.
        private func scrollToTop(_ position: Int) {
            guard let table, let scroll, position < table.numberOfRows else { return }
            let first = table.rows(in: table.visibleRect).location
            guard first != position else { return }
            let y = table.rect(ofRow: position).minY - scroll.contentInsets.top
            scroll.contentView.scroll(to: NSPoint(x: scroll.contentView.bounds.origin.x, y: y))
            scroll.reflectScrolledClipView(scroll.contentView)
        }

        @objc func scrolled(_ notification: Notification) { reportVisible() }

        private func reportVisible() {
            guard let table else { return }
            let rows = table.rows(in: table.visibleRect)
            guard rows.length > 0 else { return }
            let range = rows.location..<rows.location + rows.length
            guard range != reported else { return }
            reported = range
            parent.list.visible(range)
        }

        // MARK: Data source

        func numberOfRows(in tableView: NSTableView) -> Int { shownTotal }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard let tableColumn, let column = TrackColumns.column(tableColumn.identifier.rawValue) else { return nil }
            let cell = tableView.makeView(withIdentifier: TrackCellView.reuse, owner: nil) as? TrackCellView
                ?? TrackCellView()
            guard let item = cache.row(at: row) else {
                cell.show(text: "", font: font(column), alignment: column.alignment, secondary: false,
                          identifier: "\(parent.identifier)-placeholder-\(row)-\(column.id)")
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
            cell.show(text: text, font: font(column), alignment: column.alignment, secondary: !item.available,
                      identifier: identifier)
            cell.toolTip = item.available ? (text.isEmpty ? nil : text) : "\(text)\nUnavailable: \(item.track.path)"
            return cell
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
            let id = cache.loadedRow(at: row).map { "\(parent.identifier)-row-\($0.row_id)" }
            view.setAccessibilityIdentifier(id ?? "\(parent.identifier)-placeholder-\(row)")
            return view
        }

        // MARK: Delegate

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table else { return }
            parent.list.userSelected(table.selectedRowIndexes)
        }

        func tableView(_ tableView: NSTableView, sortDescriptorsDidChange oldDescriptors: [NSSortDescriptor]) {
            guard !updating else { return }
            // The server appends its own tiebreak, so only the primary key is sent.
            let keys = tableView.sortDescriptors.prefix(1).compactMap { descriptor in
                descriptor.key.flatMap(SortField.init(rawValue:)).map { SortKey(field: $0, descending: !descriptor.ascending) }
            }
            parent.list.setSort(Array(keys))
        }

        @objc func activateClickedRow(_ table: NSTableView) {
            guard table.clickedRow >= 0, table.clickedRow < shownTotal else { return }
            parent.list.activate(position: table.clickedRow)
        }

        // MARK: Header menu

        func menuNeedsUpdate(_ menu: NSMenu) {
            menu.removeAllItems()
            for column in TrackColumns.all where column.hideable {
                let item = NSMenuItem(title: column.title, action: #selector(toggleColumn(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = column.id
                item.state = ColumnPreferences.shared.isHidden(column.id) ? .off : .on
                menu.addItem(item)
            }
        }

        @objc func toggleColumn(_ item: NSMenuItem) {
            guard let id = item.representedObject as? String else { return }
            ColumnPreferences.shared.toggle(id)
        }

        // MARK: Drag

        func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> (any NSPasteboardWriting)? {
            guard row >= 0, row < shownTotal else { return nil }
            if dragToken == nil {
                let positions = tableView.selectedRowIndexes.contains(row)
                    ? tableView.selectedRowIndexes : IndexSet(integer: row)
                do {
                    let selection = try parent.list.encode(positions)
                    dragToken = parent.player.beginSelectionDrag(selection, count: positions.count,
                                                                 label: parent.list.destination.title)
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
        }
    }
}

/// A table cell: one vertically centred text field, or the playing indicator.
final class TrackCellView: NSTableCellView {
    static let reuse = NSUserInterfaceItemIdentifier("track-cell")
    private let field = NSTextField(labelWithString: "")
    private let icon = NSImageView()

    init() {
        super.init(frame: .zero)
        identifier = Self.reuse
        field.lineBreakMode = .byTruncatingTail
        field.translatesAutoresizingMaskIntoConstraints = false
        field.cell?.usesSingleLineMode = true
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.image = NSImage(systemSymbolName: "speaker.wave.2.fill", accessibilityDescription: "Playing")
        icon.contentTintColor = .controlAccentColor
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 10, weight: .regular)
        addSubview(field)
        addSubview(icon)
        textField = field
        NSLayoutConstraint.activate([
            field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2),
            field.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.centerXAnchor.constraint(equalTo: centerXAnchor),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func show(text: String, font: NSFont, alignment: NSTextAlignment, secondary: Bool, identifier: String) {
        icon.isHidden = true
        field.isHidden = false
        field.stringValue = text
        field.font = font
        field.alignment = alignment
        field.textColor = secondary ? .secondaryLabelColor : .labelColor
        field.setAccessibilityIdentifier(identifier)
    }

    func showPlaying(_ playing: Bool, identifier: String) {
        field.isHidden = true
        field.stringValue = ""
        icon.isHidden = !playing
        icon.setAccessibilityIdentifier(identifier)
        field.setAccessibilityIdentifier(nil)
    }
}
