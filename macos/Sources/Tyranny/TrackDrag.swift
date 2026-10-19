import AppKit
import SwiftUI
import TyrannyCore

enum TrackDragSource: Equatable {
    case library
    case queue
    case playlist(UInt64)
}

struct TrackDrag {
    static let type = "org.tyranny.track-occurrences"
    let token: String
    let connection: UInt64
    let source: TrackDragSource
    let ids: [UInt64]
    let positions: [Int]
    let revision: UInt64
    let libraryRevision: UInt64
    /// A track-list drag: the rows as a selection the server resolves. `ids` is then empty.
    var selection: TrackSelection?
    /// The selection's row count and the list it came from, for the drop's source label.
    var count = 0
    var label = ""
}

/// Owns a public AppKit table, not SwiftUI's implementation or its delegates.
/// AppKit supplies selection, drag tracking, autoscroll and between-row feedback.
struct DragTrackTable: NSViewRepresentable {
    struct Row: Equatable {
        let id: UInt64
        let cells: [String]
        var help: String = ""
    }
    struct Column {
        let title: String
        let width: CGFloat
    }
    let model: PlayerModel
    let source: TrackDragSource
    let identifier: String
    let columns: [Column]
    let rows: [Row]
    @Binding var selection: Set<UInt64>

    let activate: (UInt64) -> Void
    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        let table = NSTableView()
        table.setAccessibilityIdentifier(identifier)
        table.allowsMultipleSelection = true
        table.allowsEmptySelection = true
        table.usesAlternatingRowBackgroundColors = true
        table.rowHeight = 32
        table.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        for (index, column) in columns.enumerated() {
            let native = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(String(index)))
            native.title = column.title
            native.width = column.width
            native.minWidth = min(35, column.width)
            table.addTableColumn(native)
        }
        table.dataSource = context.coordinator
        table.delegate = context.coordinator
        table.target = context.coordinator
        table.doubleAction = #selector(Coordinator.activateClickedRow(_:))
        table.registerForDraggedTypes([NSPasteboard.PasteboardType(TrackDrag.type)])
        table.setDraggingSourceOperationMask([.copy, .move], forLocal: true)
        table.setDraggingSourceOperationMask([], forLocal: false)
        scroll.documentView = table
        context.coordinator.update(self, table: table)
        return scroll
    }
    func updateNSView(_ scroll: NSScrollView, context: Context) {
        guard let table = scroll.documentView as? NSTableView else { return }
        context.coordinator.update(self, table: table)
    }

    @MainActor final class Coordinator: NSObject, @MainActor NSTableViewDataSource, NSTableViewDelegate {
        var parent: DragTrackTable
        private var updating = false
        private var displayedRows: [Row] = []
        init(_ parent: DragTrackTable) { self.parent = parent }
        func update(_ parent: DragTrackTable, table: NSTableView) {
            self.parent = parent
            updating = true
            if displayedRows != parent.rows {
                displayedRows = parent.rows
                table.reloadData()
            }
            let indexes = IndexSet(parent.rows.indices.filter { parent.selection.contains(parent.rows[$0].id) })
            if table.selectedRowIndexes != indexes { table.selectRowIndexes(indexes, byExtendingSelection: false) }
            updating = false
        }
        private var dragLifetime: Coordinator?
        private var draggingTable: NSTableView?
        private var dragToken: String?
        func numberOfRows(in tableView: NSTableView) -> Int { parent.rows.count }
        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard parent.rows.indices.contains(row), let tableColumn,
                  let column = Int(tableColumn.identifier.rawValue) else { return nil }
            let item = parent.rows[row]
            let reuseID = NSUserInterfaceItemIdentifier("track-cell")
            let field = tableView.makeView(withIdentifier: reuseID, owner: nil) as? NSTextField ?? NSTextField(labelWithString: "")
            field.identifier = reuseID
            field.stringValue = item.cells[column]
            field.font = tableColumn.title == "Time"
                ? .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
                : .systemFont(ofSize: NSFont.systemFontSize)
            field.lineBreakMode = .byTruncatingTail
            field.toolTip = item.help.isEmpty ? item.cells[column] : item.cells[column] + "\n" + item.help
            field.setAccessibilityIdentifier("\(parent.identifier)-cell-\(item.id)-\(column)")
            return field
        }
        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            guard parent.rows.indices.contains(row) else { return nil }
            let reuseID = NSUserInterfaceItemIdentifier("track-row")
            let view = tableView.makeView(withIdentifier: reuseID, owner: nil) as? NSTableRowView ?? NSTableRowView()
            view.identifier = reuseID
            view.setAccessibilityIdentifier("\(parent.identifier)-row-\(parent.rows[row].id)")
            return view
        }
        @objc func activateClickedRow(_ table: NSTableView) {
            guard parent.rows.indices.contains(table.clickedRow) else { return }
            parent.activate(parent.rows[table.clickedRow].id)
        }
        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table = notification.object as? NSTableView else { return }
            parent.selection = Set(table.selectedRowIndexes.compactMap { row in
                parent.rows.indices.contains(row) ? parent.rows[row].id : nil
            })
        }
        func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> (any NSPasteboardWriting)? {
            guard parent.rows.indices.contains(row) else { return nil }
            if dragToken == nil {
                let indexes = tableView.selectedRowIndexes.contains(row) ? tableView.selectedRowIndexes : IndexSet(integer: row)
                let ids = indexes.compactMap { parent.rows.indices.contains($0) ? parent.rows[$0].id : nil }
                dragToken = parent.model.beginTrackDrag(source: parent.source, ids: ids)
            }
            guard let dragToken else { return nil }
            let item = NSPasteboardItem()
            item.setString(dragToken, forType: NSPasteboard.PasteboardType(TrackDrag.type))
            return item
        }
        func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession,
                       willBeginAt screenPoint: NSPoint, forRowIndexes rowIndexes: IndexSet) {
            // Keep the native source alive when spring navigation removes its pane.
            dragLifetime = self
            draggingTable = tableView
        }
        func tableView(_ tableView: NSTableView, draggingSession session: NSDraggingSession,
                       endedAt screenPoint: NSPoint, operation: NSDragOperation) {
            dragToken = nil
            draggingTable = nil
            dragLifetime = nil
        }
        func tableView(_ tableView: NSTableView, validateDrop info: any NSDraggingInfo,
                       proposedRow row: Int, proposedDropOperation operation: NSTableView.DropOperation) -> NSDragOperation {
            guard let token = info.draggingPasteboard.string(forType: NSPasteboard.PasteboardType(TrackDrag.type)),
                  parent.model.validateTrackDrop(token: token, destination: parent.source) else { return [] }
            tableView.setDropRow(max(0, min(row, parent.rows.count)), dropOperation: .above)
            return parent.model.trackDragIsCopy ? .copy : .move
        }
        func tableView(_ tableView: NSTableView, acceptDrop info: any NSDraggingInfo,
                       row: Int, dropOperation: NSTableView.DropOperation) -> Bool {
            guard let token = info.draggingPasteboard.string(forType: NSPasteboard.PasteboardType(TrackDrag.type)) else { return false }
            return parent.model.dropTracks(token: token, destination: parent.source, row: max(0, min(row, parent.rows.count)))
        }
    }
}

/// Public native destinations also work in toolbar items and during AppKit tracking.
private struct TrackDragHover: ViewModifier {
    let model: PlayerModel
    let enabled: Bool
    let destination: TrackDragSource?
    let hover: () -> Void
    let drop: ((String) -> Bool)?
    @State private var targeted = false

    func body(content: Content) -> some View {
        content
            .background(targeted ? Color.accentColor.opacity(0.2) : Color.clear)
            .overlay(Region(model: model, enabled: enabled, destination: destination,
                hover: hover, drop: drop, targeted: $targeted))
    }

    private struct Region: NSViewRepresentable {
        let model: PlayerModel
        let enabled: Bool
        let destination: TrackDragSource?
        let hover: () -> Void
        let drop: ((String) -> Bool)?
        @Binding var targeted: Bool
        func makeNSView(context: Context) -> Destination {
            let view = Destination()
            view.registerForDraggedTypes([NSPasteboard.PasteboardType(TrackDrag.type)])
            view.region = self
            return view
        }
        func updateNSView(_ view: Destination, context: Context) { view.region = self }
        static func dismantleNSView(_ view: Destination, coordinator: ()) {
            // SwiftUI may be invalidating this binding's graph during teardown.
            view.region = nil
            view.stop()
        }

        final class Destination: NSView {
            var region: Region?
            private var timer: Timer?
            override func hitTest(_ point: NSPoint) -> NSView? { nil }
            private func token(_ sender: any NSDraggingInfo) -> String? {
                guard let region, region.enabled,
                      let token = sender.draggingPasteboard.string(forType: NSPasteboard.PasteboardType(TrackDrag.type)),
                      region.model.validateTrackDrop(token: token, destination: region.destination) else { return nil }
                return token
            }
            override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
                guard token(sender) != nil, let region else { return [] }
                region.targeted = true
                timer?.invalidate()
                let timer = Timer(timeInterval: 0.65, repeats: false) { [weak self] _ in
                    MainActor.assumeIsolated {
                        guard let self, let region = self.region, region.enabled,
                              region.model.validateTrackHover(destination: region.destination) else { return }
                        region.hover()
                    }
                }
                self.timer = timer
                RunLoop.main.add(timer, forMode: .common)
                return region.model.trackDragIsCopy ? .copy : .move
            }
            override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
                guard token(sender) != nil, let region else { return [] }
                return region.model.trackDragIsCopy ? .copy : .move
            }
            override func draggingExited(_ sender: (any NSDraggingInfo)?) { stop() }
            override func prepareForDragOperation(_ sender: any NSDraggingInfo) -> Bool {
                token(sender) != nil && region?.drop != nil
            }
            override func performDragOperation(_ sender: any NSDraggingInfo) -> Bool {
                defer { stop() }
                guard let token = token(sender), let drop = region?.drop else { return false }
                return drop(token)
            }
            func stop() {
                timer?.invalidate(); timer = nil
                region?.targeted = false
            }
            isolated deinit { timer?.invalidate() }
        }
    }
}

extension View {
    func trackDragHover(model: PlayerModel, enabled: Bool = true, destination: TrackDragSource? = nil,
                        hover: @escaping () -> Void, drop: ((String) -> Bool)? = nil) -> some View {
        modifier(TrackDragHover(model: model, enabled: enabled, destination: destination, hover: hover, drop: drop))
    }
}
