import AppKit
import Observation
import SwiftUI
import TyrannyLikesSmokeCore

/// History (spec 4.7): the server's counted plays, newest first, continuous through
/// `ListWindowCache<HistoryEntry>`. It refetches when `history_revision` changes.
@MainActor @Observable
final class HistoryModel {
    let cache: ListWindowCache<HistoryEntry>
    /// Bumps when the cache is reset; table positions from an older epoch mean nothing.
    private(set) var epoch: UInt64 = 0
    var selectedID: UInt64?

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private var historyRevision: UInt64?
    @ObservationIgnored private var needsReset = true
    /// The first visible loaded entry, kept in view when plays are added above it.
    @ObservationIgnored private var anchorID: UInt64?

    init(player: PlayerModel) {
        self.player = player
        cache = ListWindowCache { _, _, _ in throw WindowFetchError.failed("not started") }
    }

    /// Follows the connection and `history_revision`. The view calls it when either changes.
    func sync() {
        guard let player else { return }
        let generation = player.playlistConnectionGeneration
        guard player.connected, let snapshot = player.snapshot else {
            if session != generation {
                session = generation
                cache.discard(session: generation)
                selectedID = nil
            }
            needsReset = true
            return
        }
        if session != generation {
            cache.discard(session: generation)
            selectedID = nil
        }
        session = generation
        let history = snapshot.history_revision
        if needsReset {
            needsReset = false
            historyRevision = history
            epoch &+= 1
            anchorID = nil
            let fetch: ListWindowCache<HistoryEntry>.Fetch = { @MainActor [weak self] offset, limit, revision in
                guard let player = self?.player else { throw WindowFetchError.failed("History closed.") }
                do {
                    let reply: HistoryResponse = try await player.send("history", [
                        "revision": revision.map(JSONValue.uint) ?? .null,
                        "offset": .int(offset), "limit": .int(limit)])
                    return WindowPage(revision: reply.revision, total: Int(reply.total), offset: reply.offset,
                                      rows: reply.entries)
                } catch let failure as ServerFailure {
                    throw WindowFetchError.failed(failure.message)
                }
            }
            cache.reset(fetch: fetch)
        } else if history != historyRevision {
            historyRevision = history
            cache.serverRevisionChanged(history, anchor: anchorID)
        }
    }

    /// The table reports the first loaded entry in view.
    func visible(_ range: Range<Int>) {
        cache.visible(range)
        for position in range {
            if let entry = cache.loadedRow(at: position) {
                anchorID = entry.id
                return
            }
        }
    }

    func playPause() { player?.transport.playPause() }

    /// Double-click, Return and ⌥Return: plays the track once, or adds it to Up Next. The source
    /// is Songs named History. A play whose track left the library says so instead.
    func activate(position: Int, action: ActivationAction = .play) {
        guard let player, let entry = cache.loadedRow(at: position) else { return }
        guard let trackID = entry.track_id, let revision = player.snapshot?.library.revision else {
            player.message = "\"\(entry.title)\" is no longer in the library."
            return
        }
        let selection = HistoryPlayback.selection(trackID: trackID, libraryRevision: revision)
        let title = entry.title.isEmpty ? "the track" : entry.title
        player.activation.start(EnqueueJob(
            action: action, source: HistoryPlayback.source, request: .source(selection, player: player),
            status: action == .addToUpNext ? "Added \(title) to Up Next." : nil))
    }
}

struct HistoryView: View {
    let model: HistoryModel
    let player: PlayerModel

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Text("History")
                    .font(.title2.weight(.semibold))
                    .accessibilityAddTraits(.isHeader)
                Text(countText)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                    .accessibilityIdentifier("history-count")
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 8)
            Divider()
            HistoryTable(model: model, generation: model.cache.generation, epoch: model.epoch, selectedID: model.selectedID,
                         colours: player.theme.tableColours)
                .overlay {
                    if model.cache.revision != nil, model.cache.total == 0 {
                        Text("Nothing played yet")
                            .font(.title3)
                            .foregroundStyle(.secondary)
                            .allowsHitTesting(false)
                            .accessibilityIdentifier("history-empty")
                    }
                }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("main-history")
        .onChange(of: syncKey, initial: true) { _, _ in model.sync() }
        #if DEBUG
        .onAppear { NativeHistorySmoke.history = model }
        #endif
    }

    private var countText: String {
        guard model.cache.revision != nil else { return "" }
        let total = model.cache.total
        return "\(DisplayFormat.count(total)) \(total == 1 ? "play" : "plays")"
    }

    private var syncKey: SyncKey {
        SyncKey(connected: player.connected, session: player.playlistConnectionGeneration,
                history: player.snapshot?.history_revision)
    }

    private struct SyncKey: Equatable {
        let connected: Bool
        let session: UInt64
        let history: UInt64?
    }
}

/// The virtualized History table (`history-table`): Played At, Title, Artist, Album, Time.
struct HistoryTable: NSViewRepresentable {
    let model: HistoryModel
    /// Read in the parent's body so SwiftUI updates the table when they change.
    let generation: UInt64
    let epoch: UInt64
    let selectedID: UInt64?
    let colours: TableColours

    private struct Column {
        let id: String
        let title: String
        let width: CGFloat
        var alignment: NSTextAlignment = .left
        var monospaced = false
    }

    private static let columns = [
        Column(id: "played-at", title: "Played At", width: 150),
        Column(id: "title", title: "Title", width: 240),
        Column(id: "artist", title: "Artist", width: 160),
        Column(id: "album", title: "Album", width: 180),
        Column(id: "time", title: "Time", width: 52, alignment: .right, monospaced: true),
    ]

    static let identifier = "history-table"

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    func makeNSView(context: Context) -> NSScrollView {
        let scroll = NSScrollView()
        scroll.hasVerticalScroller = true
        scroll.hasHorizontalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .noBorder
        let table = ActivatingTableView()
        table.setAccessibilityIdentifier(Self.identifier)
        table.style = .fullWidth
        table.rowHeight = 20
        table.intercellSpacing = NSSize(width: 6, height: 0)
        table.usesAlternatingRowBackgroundColors = true
        table.allowsMultipleSelection = false
        table.allowsEmptySelection = true
        table.columnAutoresizingStyle = .noColumnAutoresizing
        for column in Self.columns {
            let native = NSTableColumn(identifier: NSUserInterfaceItemIdentifier(column.id))
            native.title = column.title
            native.width = column.width
            native.minWidth = 40
            native.headerCell.alignment = column.alignment
            native.headerCell.setAccessibilityIdentifier("history-header-\(column.id)")
            table.addTableColumn(native)
        }
        table.dataSource = context.coordinator
        table.delegate = context.coordinator
        table.target = context.coordinator
        table.doubleAction = #selector(Coordinator.activateClickedRow(_:))
        // Return plays the selected play's track; ⌥Return adds it to Up Next.
        table.onActivate = { [weak table, weak model = model] action in
            guard let table, table.selectedRow >= 0 else { return }
            model?.activate(position: table.selectedRow, action: action)
        }
        table.onPlayPause = { [weak model] in model?.playPause() }
        scroll.documentView = table
        scroll.contentView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(
            context.coordinator, selector: #selector(Coordinator.scrolled(_:)),
            name: NSView.boundsDidChangeNotification, object: scroll.contentView)
        NotificationCenter.default.addObserver(
            context.coordinator, selector: #selector(Coordinator.scrolled(_:)),
            name: NSView.frameDidChangeNotification, object: scroll)
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
        var parent: HistoryTable
        weak var table: NSTableView?
        private var shownGeneration: UInt64?
        private var shownEpoch: UInt64?
        private var shownCount = 0
        private var shownRefreshes: UInt64 = 0
        private var shownColours: UInt64?
        private var anchor: (id: UInt64, offset: Int)?
        private var updating = false

        init(_ parent: HistoryTable) { self.parent = parent }

        private var cache: ListWindowCache<HistoryEntry> { parent.model.cache }

        func update(_ parent: HistoryTable) {
            self.parent = parent
            guard let table else { return }
            updating = true
            defer { updating = false }
            let count = cache.total
            if parent.generation != shownGeneration || parent.epoch != shownEpoch || count != shownCount {
                let reloadAll = parent.epoch != shownEpoch || count != shownCount
                shownGeneration = parent.generation
                shownEpoch = parent.epoch
                shownCount = count
                if reloadAll {
                    table.reloadData()
                } else {
                    table.reloadData(forRowIndexes: table.preparedRows, columnIndexes: IndexSet(0..<table.numberOfColumns))
                }
                restoreAnchor(table)
            }
            if parent.colours.revision != shownColours {
                shownColours = parent.colours.revision
                table.enumerateAvailableRowViews { view, _ in
                    (view as? TokenRowView)?.selection = parent.colours.selection
                }
            }
            let selected = parent.selectedID.flatMap { cache.position(of: $0) }.map { IndexSet(integer: $0) } ?? []
            if table.selectedRowIndexes != selected { table.selectRowIndexes(selected, byExtendingSelection: false) }
            reportVisible()
        }

        /// New plays arrive above the rows in view: after a refresh pass, the entry that was first
        /// in view goes back to the same distance from the top.
        private func restoreAnchor(_ table: NSTableView) {
            guard cache.refreshes != shownRefreshes else { return }
            shownRefreshes = cache.refreshes
            guard let anchor, let position = cache.position(of: anchor.id) else { return }
            let target = max(0, position - anchor.offset)
            let top = table.rows(in: table.visibleRect).location
            guard target != top, top > 0, let scroll = table.enclosingScrollView else { return }
            let y = table.rect(ofRow: target).minY
            scroll.contentView.scroll(to: NSPoint(x: scroll.contentView.bounds.origin.x, y: y))
            scroll.reflectScrolledClipView(scroll.contentView)
        }

        @objc func scrolled(_ notification: Notification) { reportVisible() }

        private func reportVisible() {
            guard let table else { return }
            let rows = table.rows(in: table.visibleRect)
            guard rows.length > 0 else { return }
            let range = rows.location..<rows.location + rows.length
            parent.model.visible(range)
            anchor = range.lazy.compactMap { [cache] position in
                cache.loadedRow(at: position).map { (id: $0.id, offset: position - range.lowerBound) }
            }.first
        }

        @objc func activateClickedRow(_ table: NSTableView) {
            guard table.clickedRow >= 0 else { return }
            parent.model.activate(position: table.clickedRow)
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard !updating, let table else { return }
            parent.model.selectedID = table.selectedRow >= 0 ? cache.loadedRow(at: table.selectedRow)?.id : nil
        }

        func numberOfRows(in tableView: NSTableView) -> Int { cache.total }

        func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
            let reuse = NSUserInterfaceItemIdentifier("history-row")
            let view = tableView.makeView(withIdentifier: reuse, owner: nil) as? TokenRowView ?? TokenRowView()
            view.identifier = reuse
            view.selection = parent.colours.selection
            return view
        }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            guard let id = tableColumn?.identifier.rawValue,
                  let column = HistoryTable.columns.first(where: { $0.id == id }) else { return nil }
            let cell = tableView.makeView(withIdentifier: TrackCellView.reuse, owner: nil) as? TrackCellView
                ?? TrackCellView()
            let font: NSFont = column.monospaced
                ? .monospacedDigitSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
                : .systemFont(ofSize: NSFont.systemFontSize)
            guard let entry = cache.row(at: row) else {
                cell.show(text: "", font: font, alignment: column.alignment, secondary: false,
                          identifier: "\(HistoryTable.identifier)-placeholder-\(row)-\(id)")
                cell.toolTip = nil
                return cell
            }
            let text: String
            switch id {
            case "played-at": text = DisplayFormat.playedAt(entry.played_at, now: Date(), calendar: .current)
            case "title": text = entry.title
            case "artist": text = entry.artist
            case "album": text = entry.album
            default: text = entry.duration_ms.map { DisplayFormat.clock(Double($0) / 1000) } ?? ""
            }
            // A play whose track left the library cannot be queued: shown dimmed.
            cell.show(text: text, font: font, alignment: column.alignment, secondary: entry.track_id == nil,
                      identifier: "\(HistoryTable.identifier)-cell-\(entry.id)-\(id)")
            cell.toolTip = text.isEmpty ? nil : text
            return cell
        }
    }
}
