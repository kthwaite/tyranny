import AppKit
import SwiftUI
import TyrannyCore

/// The virtualized Up Next table (`up-next-table`): status, #, Title, Artist, Album, Time and
/// Added From, one row per queue position, windowed through `UpNextModel.cache`.
///
/// A row that starts a section (HISTORY, NOW PLAYING, UP NEXT; `UpNextSections.startsSection`)
/// shows the section label beside its number; all rows have one height. History rows are dimmed. Selection is
/// held by entry ID. Rows drag to reorder, and library rows drop between them.
struct UpNextTable: NSViewRepresentable {
    let model: UpNextModel
    let player: PlayerModel
    /// Read in the parent's body so SwiftUI updates the table when they change.
    let generation: UInt64
    let epoch: UInt64
    /// The current entry, its position and the shuffle state: what decides the sections.
    let context: QueueContext

    static let identifier = "up-next-table"

    struct Column {
        let id: String
        let title: String
        let width: CGFloat
        var alignment: NSTextAlignment = .left
        var monospaced = false
    }

    static let columns = [
        Column(id: "status", title: "", width: 24),
        Column(id: "number", title: "#", width: 96, alignment: .right, monospaced: true),
        Column(id: "title", title: "Title", width: 240),
        Column(id: "artist", title: "Artist", width: 160),
        Column(id: "album", title: "Album", width: 180),
        Column(id: "time", title: "Time", width: 52, alignment: .right, monospaced: true),
        Column(id: "added-from", title: "Added From", width: 190),
    ]

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        let table = QueueNSTableView()
        table.setAccessibilityIdentifier(Self.identifier)
        table.style = .fullWidth
        table.rowHeight = Coordinator.rowHeight
        table.intercellSpacing = NSSize(width: 6, height: 0)
        table.usesAlternatingRowBackgroundColors = true
        table.allowsMultipleSelection = true
        table.allowsEmptySelection = true
        table.columnAutoresizingStyle = .noColumnAutoresizing
        for column in Self.columns {
            let native = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(column.id))
            native.title = column.title
            native.width = column.width
            native.minWidth = column.id == "status" ? column.width : 40
            if column.id == "status" {
                native.resizingMask = []
                native.headerToolTip = "Playing"
            }
            native.headerCell.alignment = column.alignment
            native.headerCell.setAccessibilityIdentifier("up-next-header-\(column.id)")
            table.addTableColumn(native)
        }
        let coordinator = context.coordinator
        table.dataSource = coordinator
        table.delegate = coordinator
        table.target = coordinator
        table.doubleAction = #selector(Coordinator.activateClickedRow(_:))
        table.onReturn = { [weak coordinator] in coordinator?.parent.model.playSelection() }
        table.onDelete = { [weak coordinator] in coordinator?.parent.model.removeSelection() }
        table.registerForDraggedTypes([NSPasteboard.PasteboardType(TrackDrag.type)])
        table.setDraggingSourceOperationMask(.move, forLocal: true)
        table.setDraggingSourceOperationMask([], forLocal: false)
        let menu = NSMenu(title: "Up Next")
        menu.delegate = coordinator
        table.menu = menu
        scroll.documentView = table
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(
            coordinator, selector: #selector(Coordinator.scrolled(_:)),
            name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        NotificationCenter.default.addObserver(
            coordinator, selector: #selector(Coordinator.scrolled(_:)),
            name: NSView.frameDidChangeNotification, object: scroll)
        coordinator.table = table
        coordinator.update(self)
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
        static let rowHeight: CGFloat = 20
        var parent: UpNextTable
        weak var table: QueueNSTableView?
        private var updating = false
        private var shownGeneration: UInt64?
        private var shownEpoch: UInt64?
        private var shownContext: QueueContext?
        private var shownCount = -1
        private var focusCurrent = true
        private var shownRefreshes: UInt64 = 0
        /// The first entry in view and how far its row's top is below the top of the view.
        private var anchor: (id: UInt64, offset: CGFloat)?
        private var reported: Range<Int>?
        private var dragToken: String?
        private var dragItems = 0
        private var dragLifetime: Coordinator?

        init(_ parent: UpNextTable) { self.parent = parent }

        private var model: UpNextModel { parent.model }
        private var cache: ListWindowCache<QueueItem> { parent.model.cache }

        // MARK: Updates

        func update(_ parent: UpNextTable) {
            self.parent = parent
            guard let table else { return }
            let count = cache.total
            if parent.generation != shownGeneration || parent.epoch != shownEpoch
                || parent.context != shownContext || count != shownCount {
                let epochChanged = parent.epoch != shownEpoch
                shownGeneration = parent.generation
                shownEpoch = parent.epoch
                shownContext = parent.context
                shownCount = count
                if epochChanged {
                    reported = nil
                    anchor = nil
                    focusCurrent = true
                }
                updating = true
                table.reloadData()
                let rows = IndexSet(model.selection.compactMap { cache.position(of: $0) })
                table.selectRowIndexes(rows, byExtendingSelection: false)
                updating = false
                if epochChanged { table.scrollRowToVisible(0) }
                restoreAnchor(table)
                focusCurrentEntry(table)
            }
            reportVisible()
        }

        /// After a refresh pass, the entry that was first in view goes back to the same distance
        /// from the top.
        private func restoreAnchor(_ table: NSTableView) {
            guard cache.refreshes != shownRefreshes else { return }
            shownRefreshes = cache.refreshes
            guard let anchor, let position = cache.position(of: anchor.id), position < table.numberOfRows,
                  let scroll = table.enclosingScrollView, scroll.contentView.bounds.minY > 0 else { return }
            let y = max(0, table.rect(ofRow: position).minY - anchor.offset)
            guard abs(y - scroll.contentView.bounds.minY) > 0.5 else { return }
            scroll.contentView.scroll(to: NSPoint(x: scroll.contentView.bounds.origin.x, y: y))
            scroll.reflectScrolledClipView(scroll.contentView)
        }

        /// The first time rows arrive, the view opens at the playing entry, with the entry
        /// before it in view.
        private func focusCurrentEntry(_ table: NSTableView) {
            guard focusCurrent, cache.loadedRow(at: 0) != nil else { return }
            focusCurrent = false
            guard let current = parent.context.currentID, let position = cache.position(of: current),
                  let scroll = table.enclosingScrollView, position > 0 else { return }
            let y = table.rect(ofRow: position - 1).minY
            scroll.contentView.scroll(to: NSPoint(x: scroll.contentView.bounds.origin.x, y: y))
            scroll.reflectScrolledClipView(scroll.contentView)
        }

        @objc func scrolled(_ notification: Notification) { reportVisible() }

        private func reportVisible() {
            guard let table else { return }
            let rows = table.rows(in: table.visibleRect)
            guard rows.length > 0 else { return }
            let range = rows.location..<rows.location + rows.length
            if range != reported {
                reported = range
                model.visible(range)
            }
            anchor = range.lazy.compactMap { [cache] position in
                cache.loadedRow(at: position).map {
                    (id: $0.id, offset: table.rect(ofRow: position).minY - table.visibleRect.minY)
                }
            }.first
        }

        // MARK: Data source

        func numberOfRows(in tableView: NSTableView) -> Int { cache.total }

        /// Rows have one height. A row that starts a section shows its label beside the number.
        private func startsSection(_ position: Int) -> QueueSection? {
            guard let item = cache.loadedRow(at: position) else { return nil }
            let starts = UpNextSections.startsSection(
                item: item, previous: position > 0 ? cache.loadedRow(at: position - 1) : nil,
                position: position, in: parent.context)
            return starts ? UpNextSections.section(of: item, at: position, in: parent.context) : nil
        }

        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            let reuse = NSUserInterfaceItemIdentifier("up-next-row")
            let view = tableView.makeView(withIdentifier: reuse, owner: nil) as? QueueRowView ?? QueueRowView()
            view.identifier = reuse
            let item = cache.loadedRow(at: row)
            view.setAccessibilityIdentifier(item.map { "up-next-row-\($0.id)" } ?? "up-next-placeholder-\(row)")
            return view
        }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard let id = tableColumn?.identifier.rawValue,
                  let column = UpNextTable.columns.first(where: { $0.id == id }) else { return nil }
            let cell = tableView.makeView(withIdentifier: QueueCellView.reuse, owner: nil) as? QueueCellView
                ?? QueueCellView()
            let font: NSFont = column.monospaced
                ? .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
                : .systemFont(ofSize: NSFont.systemFontSize)
            guard let item = cache.row(at: row) else {
                cell.show(text: "", font: font, alignment: column.alignment, dimmed: false,
                          identifier: "\(UpNextTable.identifier)-placeholder-\(row)-\(id)")
                cell.toolTip = nil
                return cell
            }
            let section = UpNextSections.section(of: item, at: row, in: parent.context)
            let identifier = "\(UpNextTable.identifier)-cell-\(item.id)-\(id)"
            if id == "status" {
                cell.showPlaying(section == .nowPlaying, identifier: identifier)
                cell.toolTip = nil
                return cell
            }
            let text: String
            switch id {
            case "number": text = String(row + 1)
            case "title": text = Self.title(item)
            case "artist": text = item.artist
            case "album": text = item.album
            case "time": text = item.duration_ms.map { DisplayFormat.clock(Double($0) / 1000) } ?? ""
            default: text = UpNextSections.addedFrom(item.source)
            }
            let weight: NSFont.Weight = section == .nowPlaying && id == "title" ? .semibold : .regular
            let shown = weight == .semibold ? NSFont.systemFont(ofSize: NSFont.systemFontSize, weight: weight) : font
            cell.show(text: text, font: shown, alignment: column.alignment, dimmed: section == .history,
                      identifier: identifier)
            cell.show(section: id == "number" ? startsSection(row) : nil)
            cell.toolTip = text.isEmpty ? nil : text
            return cell
        }

        /// An entry the library does not know has no title: its file name stands in.
        private static func title(_ item: QueueItem) -> String {
            item.title.isEmpty ? (item.path as NSString).lastPathComponent : item.title
        }

        // MARK: Delegate

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table else { return }
            model.selection = Set(table.selectedRowIndexes.compactMap { cache.loadedRow(at: $0)?.id })
        }

        @objc func activateClickedRow(_ table: NSTableView) {
            guard table.clickedRow >= 0, let item = cache.loadedRow(at: table.clickedRow) else { return }
            model.play(item.id)
        }

        // MARK: Context menu

        func menuNeedsUpdate(_ menu: NSMenu) {
            menu.removeAllItems()
            guard let table, table.clickedRow >= 0, let item = cache.loadedRow(at: table.clickedRow) else { return }
            if !table.selectedRowIndexes.contains(table.clickedRow) {
                table.selectRowIndexes(IndexSet(integer: table.clickedRow), byExtendingSelection: false)
            }
            let play = NSMenuItem(title: "Play", action: #selector(playClicked(_:)), keyEquivalent: "")
            play.target = self
            play.representedObject = item.id
            let remove = NSMenuItem(title: "Remove from Up Next", action: #selector(removeSelected(_:)),
                                    keyEquivalent: "")
            remove.target = self
            menu.addItem(play)
            menu.addItem(remove)
        }

        @objc private func playClicked(_ item: NSMenuItem) {
            if let id = item.representedObject as? UInt64 { model.play(id) }
        }

        @objc private func removeSelected(_ item: NSMenuItem) { model.removeSelection() }

        // MARK: Drag

        func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> (any NSPasteboardWriting)? {
            guard let item = cache.loadedRow(at: row) else { return nil }
            if dragToken == nil {
                let positions = tableView.selectedRowIndexes.contains(row) ? Array(tableView.selectedRowIndexes) : [row]
                let ids = positions.compactMap { cache.loadedRow(at: $0)?.id }
                dragToken = model.beginDrag(ids: ids.isEmpty ? [item.id] : ids)
                dragItems = 0
            }
            guard let dragToken, dragItems < 24 else { return nil }
            dragItems += 1
            let pasteboardItem = NSPasteboardItem()
            pasteboardItem.setString(dragToken, forType: NSPasteboard.PasteboardType(TrackDrag.type))
            return pasteboardItem
        }

        func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession,
                       willBeginAt screenPoint: NSPoint, forRowIndexes rowIndexes: IndexSet) {
            dragLifetime = self
        }

        func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession,
                       endedAt screenPoint: NSPoint, operation: NSDragOperation) {
            dragToken = nil
            dragItems = 0
            dragLifetime = nil
        }

        func tableView(_ tableView: NSTableView, validateDrop info: any NSDraggingInfo, proposedRow row: Int,
                       proposedDropOperation dropOperation: NSTableView.DropOperation) -> NSDragOperation {
            let position = min(max(0, row), cache.total)
            guard let token = info.draggingPasteboard.string(forType: NSPasteboard.PasteboardType(TrackDrag.type)),
                  model.canDrop(token: token, at: position) else { return [] }
            tableView.setDropRow(position, dropOperation: .above)
            return parent.player.trackDragIsCopy ? .copy : .move
        }

        func tableView(_ tableView: NSTableView, acceptDrop info: any NSDraggingInfo, row: Int,
                       dropOperation: NSTableView.DropOperation) -> Bool {
            guard let token = info.draggingPasteboard.string(forType: NSPasteboard.PasteboardType(TrackDrag.type))
            else { return false }
            return model.drop(token: token, at: min(max(0, row), cache.total))
        }
    }
}

/// Return plays the selected entry; Delete removes the selected entries.
final class QueueNSTableView: NSTableView {
    var onReturn: (() -> Void)?
    var onDelete: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        switch event.keyCode {
        case 36, 76: onReturn?()
        case 51, 117: onDelete?()
        default: super.keyDown(with: event)
        }
    }
}

/// A queue row. Its section label lives in the # cell, which knows its live width.
final class QueueRowView: NSTableRowView {}

/// A queue table cell: one text field, or the playing indicator.
final class QueueCellView: NSTableCellView {
    static let reuse = NSUserInterfaceItemIdentifier("up-next-cell")
    private let field = NSTextField(labelWithString: "")
    private let icon = NSImageView()
    /// Only in the # column: the section name, truncated so it stays clear of the number.
    private let sectionLabel = NSTextField(labelWithString: "")

    init() {
        super.init(frame: .zero)
        identifier = Self.reuse
        field.lineBreakMode = .byTruncatingTail
        field.cell?.usesSingleLineMode = true
        field.translatesAutoresizingMaskIntoConstraints = false
        addSubview(field)
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.image = NSImage(systemSymbolName: "speaker.wave.2.fill", accessibilityDescription: "Playing")
        icon.contentTintColor = .controlAccentColor
        icon.symbolConfiguration = NSImage.SymbolConfiguration(pointSize: 10, weight: .regular)
        addSubview(icon)
        textField = field
        sectionLabel.font = .systemFont(ofSize: 9, weight: .bold)
        sectionLabel.textColor = .secondaryLabelColor
        sectionLabel.lineBreakMode = .byTruncatingTail
        sectionLabel.cell?.usesSingleLineMode = true
        sectionLabel.translatesAutoresizingMaskIntoConstraints = false
        sectionLabel.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        sectionLabel.isHidden = true
        addSubview(sectionLabel)
        field.setContentCompressionResistancePriority(.defaultHigh + 50, for: .horizontal)
        let fieldLeading = field.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2)
        fieldLeading.priority = .defaultHigh
        NSLayoutConstraint.activate([
            fieldLeading,
            field.leadingAnchor.constraint(greaterThanOrEqualTo: sectionLabel.trailingAnchor, constant: 4),
            sectionLabel.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            sectionLabel.centerYAnchor.constraint(equalTo: centerYAnchor),
            field.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -2),
            field.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.centerXAnchor.constraint(equalTo: centerXAnchor),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

    func show(text: String, font: NSFont, alignment: NSTextAlignment, dimmed: Bool, identifier: String?) {
        icon.isHidden = true
        sectionLabel.isHidden = true
        field.isHidden = false
        field.stringValue = text
        field.font = font
        field.alignment = alignment
        field.textColor = dimmed ? .tertiaryLabelColor : .labelColor
        field.setAccessibilityIdentifier(identifier)
    }

    func show(section: QueueSection?) {
        sectionLabel.isHidden = section == nil
        sectionLabel.stringValue = section?.title ?? ""
        sectionLabel.toolTip = section?.title
        sectionLabel.setAccessibilityLabel(section?.title)
        sectionLabel.setAccessibilityIdentifier(section.map { "up-next-section-\($0.key)" })
    }

    func showPlaying(_ playing: Bool, identifier: String) {
        field.isHidden = true
        sectionLabel.isHidden = true
        field.stringValue = ""
        icon.isHidden = !playing
        icon.setAccessibilityIdentifier(identifier)
        icon.setAccessibilityLabel(playing ? "Playing" : "")
    }
}
