import Foundation
import Observation
import TyrannyCore

/// One visible track list: its list description, its window cache over `list_tracks`,
/// its totals and its selection.
///
/// `sync()` follows the connection, the library and history revisions, the filter,
/// the browser and the grouping. The view calls it when any of them changes.
@MainActor @Observable
final class ListModel {
    let destination: Destination
    let source: ListSource
    /// The description the cache's rows belong to.
    private(set) var description: ListDescription
    /// Bumps when `description` changes; positions from an older epoch mean nothing.
    private(set) var epoch: UInt64 = 0
    let cache: ListWindowCache<ListRow>
    /// The list's totals, from the latest first-page reply.
    private(set) var totals: ListTotals?
    /// The server's `invalid_filter` message for the current filter.
    private(set) var filterError: String?
    /// The column browser's selection. It narrows the list while the browser is shown.
    private(set) var browserSelection = BrowserSelection()
    let browser = ColumnBrowserModel()
    @ObservationIgnored private(set) var rows = RowSelection()

    /// Selected row IDs (spec 5.4).
    var selection: Set<UInt64> { rows.ids }
    var selectedPositions: IndexSet { rows.positions }
    /// The unfiltered count while a filter or browser selection narrows the list.
    var filteredFrom: UInt64? {
        guard isNarrowed, source == .library, let tracks = player?.snapshot?.library.tracks else { return nil }
        return UInt64(tracks)
    }
    var isNarrowed: Bool { !description.query.isEmpty || description.browser != Browser() }

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private let navigation: NavigationModel
    @ObservationIgnored private var sort: [SortKey] = []
    @ObservationIgnored private var query = ""
    @ObservationIgnored private var filterTask: Task<Void, Never>?
    @ObservationIgnored private var totalsTask: Task<Void, Never>?
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private var needsReset = true
    @ObservationIgnored private var libraryRevision: UInt64?
    @ObservationIgnored private var historyRevision: UInt64?
    /// The first visible row, kept in view across revision changes.
    @ObservationIgnored private var anchorRowID: UInt64?

    init(destination: Destination, source: ListSource, player: PlayerModel, navigation: NavigationModel) {
        self.destination = destination
        self.source = source
        self.player = player
        self.navigation = navigation
        description = ListDescription(source: source, group: navigation.grouping(for: destination))
        // Replaced by `reset` on the first sync.
        cache = ListWindowCache { _, _, _ in throw WindowFetchError.failed("not started") }
    }

    // MARK: Following state

    /// Applies the current connection, revisions, filter, browser and grouping.
    func sync() {
        guard let player else { return }
        let session = player.playlistConnectionGeneration
        guard player.connected, let snapshot = player.snapshot else {
            if self.session != session {
                self.session = session
                cache.discard(session: session)
                browser.disconnected(session: session)
            }
            needsReset = true
            return
        }
        if self.session != session { cache.discard(session: session) }
        self.session = session
        let filter = navigation.filter.trimmingCharacters(in: .whitespacesAndNewlines)
        if filter != query {
            if filter.isEmpty || needsReset {
                filterTask?.cancel()
                query = filter
            } else {
                scheduleFilter()
            }
        }
        let library = snapshot.library.revision
        let history = snapshot.history_revision
        let next = currentDescription()
        if needsReset || next != description {
            needsReset = false
            libraryRevision = library
            historyRevision = history
            apply(next)
        } else if library != libraryRevision || history != historyRevision {
            if library != libraryRevision { scheduleTotals() }
            libraryRevision = library
            historyRevision = history
            // Play counts change Plays and Last Played without a library revision change.
            cache.serverRevisionChanged(library, anchor: anchorRowID)
        }
        browser.sync(player: player, session: session, revision: library, query: query,
                     narrowing: browserSelection.albumColumnBrowser, visible: navigation.columnBrowserVisible)
    }

    func setSort(_ keys: [SortKey]) {
        guard keys != sort else { return }
        sort = keys
        sync()
    }

    var sortKeys: [SortKey] { sort }

    func setBrowserGenres(_ ids: Set<UInt64>) { browserSelection.setGenres(ids); sync() }
    func setBrowserArtists(_ ids: Set<UInt64>) { browserSelection.setArtists(ids); sync() }
    func setBrowserAlbums(_ ids: Set<UInt64>) { browserSelection.setAlbums(ids); sync() }

    private func currentDescription() -> ListDescription {
        ListDescription(source: source,
                        browser: navigation.columnBrowserVisible ? browserSelection.browser : Browser(),
                        query: query, sort: sort, group: navigation.grouping(for: destination))
    }

    private func apply(_ next: ListDescription) {
        filterError = nil
        description = next
        epoch &+= 1
        anchorRowID = nil
        cache.reset(fetch: makeFetch())
    }

    /// Typing waits 250 ms before the list refetches.
    private func scheduleFilter() {
        filterTask?.cancel()
        filterTask = Task { [weak self] in
            do { try await Task.sleep(for: .milliseconds(250)) } catch { return }
            guard let self else { return }
            self.query = self.navigation.filter.trimmingCharacters(in: .whitespacesAndNewlines)
            self.sync()
        }
    }

    // MARK: Table

    /// The table's visible rows. Prefetches around them and records the anchor row.
    func visible(_ range: Range<Int>) {
        cache.visible(range)
        if let row = cache.loadedRow(at: range.lowerBound) { anchorRowID = row.row_id }
    }

    var selectionBasis: RowSelection.Basis { RowSelection.Basis(epoch: epoch, revision: cache.revision) }

    func userSelected(_ positions: IndexSet) {
        rows.select(positions, rowID: { cache.loadedRow(at: $0)?.row_id }, basis: selectionBasis)
    }

    /// Rows or the total changed. Returns the positions to show selected.
    func rowsChanged() -> IndexSet {
        rows.rowsChanged(loaded: cache.loadedRows().map { ($0.position, $0.row.row_id) },
                         total: cache.total, basis: selectionBasis)
        return rows.positions
    }

    /// The rows at `positions` as a selection for the server.
    func encode(_ positions: IndexSet) throws -> TrackSelection {
        guard let revision = cache.revision else { throw EmptySelection() }
        return try SelectionEncoder.selection(
            list: description, revision: revision, positions: positions,
            rowID: { cache.loadedRow(at: $0)?.row_id }, trackID: { cache.loadedRow(at: $0)?.track.id })
    }

    /// Double-click or Return on a row. Runs arrive in Task 13.
    func activate(position: Int) {}

    // MARK: Fetching

    private func makeFetch() -> ListWindowCache<ListRow>.Fetch {
        let description = self.description
        let epoch = self.epoch
        return { @MainActor [weak self] offset, limit, revision in
            guard let self else { throw WindowFetchError.failed("The list closed.") }
            return try await self.fetch(description, epoch: epoch, offset: offset, limit: limit, revision: revision)
        }
    }

    /// Always completes or throws: the wire client fails a request after its deadline.
    private func fetch(_ description: ListDescription, epoch: UInt64, offset: Int, limit: Int,
                       revision: UInt64?) async throws -> WindowPage<ListRow> {
        guard let player else { throw WindowFetchError.failed("The list closed.") }
        do {
            let reply: ListTracksResponse = try await player.send("list_tracks", [
                "list": try JSONValue.encoding(description),
                "revision": revision.map(JSONValue.uint) ?? .null,
                "offset": .int(offset), "limit": .int(limit)])
            if epoch == self.epoch {
                if let totals = reply.totals { self.totals = totals }
                filterError = nil
            }
            return WindowPage(revision: reply.revision, total: Int(reply.total), offset: reply.offset, rows: reply.entries)
        } catch let failure as ServerFailure {
            switch failure.code {
            case "stale_library_revision":
                throw WindowFetchError.stale
            case "invalid_filter":
                if epoch == self.epoch {
                    filterError = failure.message
                    totals = nil
                }
                throw WindowFetchError.failed(failure.message)
            default:
                throw WindowFetchError.failed(failure.localizedDescription)
            }
        }
    }

    /// The totals follow library revisions even when the first window is out of view.
    /// At most one request a second while a scan changes the revision.
    private func scheduleTotals() {
        totalsTask?.cancel()
        totalsTask = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(1)) } catch { return }
            guard let self else { return }
            let description = self.description
            let epoch = self.epoch
            // A cancelled request would close the shared connection: send it unstructured.
            Task { @MainActor [weak self] in
                guard let player = self?.player, player.connected else { return }
                let reply: ListTracksResponse? = try? await player.send("list_tracks", [
                    "list": try JSONValue.encoding(description), "revision": .null,
                    "offset": .int(0), "limit": .int(1)])
                guard let self, epoch == self.epoch, let totals = reply?.totals else { return }
                self.totals = totals
            }
        }
    }
}

/// The track lists the window keeps, one per destination, so a list keeps its
/// scroll position, sort and browser selection while another destination shows.
@MainActor
final class TrackLists {
    private var lists: [Destination: ListModel] = [:]

    func list(for destination: Destination, source: ListSource, player: PlayerModel,
              navigation: NavigationModel) -> ListModel {
        if let list = lists[destination] { return list }
        let list = ListModel(destination: destination, source: source, player: player, navigation: navigation)
        lists[destination] = list
        return list
    }
}
