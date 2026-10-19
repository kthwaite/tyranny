import Foundation
import Observation
import TyrannyCore

/// One visible track list: its list description, its window cache over `list_tracks`,
/// its group layout over `list_groups`, its totals and its selection.
///
/// `sync()` follows the connection, the library and history revisions, the filter,
/// the browser and the grouping. The view calls it when any of them changes, and
/// `syncLayout()` when the cache's rows change.
///
/// Grouped lists show `layout`: display rows over the cache's positions. The spans are
/// fetched for the cache's revision after each committed refresh pass, so they follow
/// the cache's throttle. Until they arrive the previous layout stays, and the anchor row
/// is restored only once the layout matches the rows again.
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
    /// Album block facts for the albums in view.
    let facts: AlbumFactsCache
    /// The grouped layout from the latest spans. Unused while ungrouped.
    private(set) var groupedLayout = GroupLayout(grouping: .none, total: 0, spans: [])
    /// Bumps when `groupedLayout` is replaced.
    private(set) var layoutVersion: UInt64 = 0
    /// Albums per artist key, for the artist headers.
    private(set) var albumCounts: [UInt64: Int] = [:]
    /// true briefly after a refused reorder drop (`playlist-reorder-hint`).
    private(set) var reorderHintShown = false

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
    /// The typed filter waiting out the debounce.
    @ObservationIgnored private var pendingFilter: String?
    @ObservationIgnored private var totalsTask: Task<Void, Never>?
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private var needsReset = true
    @ObservationIgnored private var libraryRevision: UInt64?
    @ObservationIgnored private var historyRevision: UInt64?
    /// The first visible row, kept in view across revision changes.
    @ObservationIgnored private var anchorRowID: UInt64?
    /// Display rows from the top of the view to the anchor row, restored with it.
    @ObservationIgnored private(set) var anchorOffset = 0
    /// The display rows in view, from the table.
    @ObservationIgnored private var visibleDisplay: Range<Int>?
    /// What `groupedLayout` was built for.
    @ObservationIgnored private var layoutBasis: LayoutBasis?
    @ObservationIgnored private var groupsRequest: LayoutBasis?
    @ObservationIgnored private var groupsFailed: LayoutBasis?
    /// Bumps per `list_groups` request; only the latest reply is applied.
    @ObservationIgnored private var groupsTicket: UInt64 = 0
    @ObservationIgnored private var hintTask: Task<Void, Never>?

    /// The rows and refresh a layout belongs to. Spans at another basis may disagree with
    /// the cache on positions.
    struct LayoutBasis: Equatable {
        let epoch: UInt64
        let revision: UInt64?
        let refresh: UInt64
    }

    init(destination: Destination, source: ListSource, player: PlayerModel, navigation: NavigationModel) {
        self.destination = destination
        self.source = source
        self.player = player
        self.navigation = navigation
        description = ListDescription(source: source, group: navigation.grouping(for: destination))
        // Replaced by `reset` on the first sync.
        cache = ListWindowCache { _, _, _ in throw WindowFetchError.failed("not started") }
        facts = AlbumFactsCache { _ in throw WindowFetchError.failed("not started") }
        facts.reset(fetch: makeFactsFetch())
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
                discardGroups()
                browser.disconnected(session: session)
            }
            needsReset = true
            return
        }
        if self.session != session {
            cache.discard(session: session)
            discardGroups()
        }
        self.session = session
        let filter = navigation.filter.trimmingCharacters(in: .whitespacesAndNewlines)
        if filter != query {
            if filter.isEmpty || needsReset {
                cancelFilter()
                query = filter
            } else if filter != pendingFilter {
                // Revision changes during a scan call sync() too; they must not restart the wait.
                scheduleFilter(filter)
            }
        } else if pendingFilter != nil {
            cancelFilter()
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
            recordAnchor()
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
        anchorOffset = 0
        visibleDisplay = nil
        cache.reset(fetch: makeFetch())
        syncLayout()
    }

    /// Typing waits 250 ms before the list refetches.
    private func scheduleFilter(_ filter: String) {
        filterTask?.cancel()
        pendingFilter = filter
        filterTask = Task { [weak self] in
            do { try await Task.sleep(for: .milliseconds(250)) } catch { return }
            guard let self, self.pendingFilter == filter else { return }
            self.pendingFilter = nil
            self.query = filter
            self.sync()
        }
    }

    private func cancelFilter() {
        filterTask?.cancel()
        filterTask = nil
        pendingFilter = nil
    }

    // MARK: Grouping

    var grouping: Grouping { description.group }
    /// Album blocks show covers and facts in the gutter column.
    var showsAlbumBlocks: Bool { grouping == .album || grouping == .artistAlbum }

    /// The display rows. Ungrouped, one row per position.
    var layout: GroupLayout {
        grouping == .none ? GroupLayout(grouping: .none, total: cache.total, spans: []) : groupedLayout
    }

    /// Changes whenever `layout` does.
    var layoutToken: LayoutToken {
        LayoutToken(epoch: epoch, version: grouping == .none ? 0 : layoutVersion,
                    total: grouping == .none ? cache.total : -1)
    }

    struct LayoutToken: Equatable {
        let epoch: UInt64
        let version: UInt64
        let total: Int
    }

    /// The layout matches the cache's rows.
    var layoutIsCurrent: Bool {
        grouping == .none || layoutBasis == LayoutBasis(epoch: epoch, revision: cache.revision, refresh: cache.refreshes)
    }

    /// Fetches spans when the cache's rows moved on from the layout's. Called when the
    /// description, the cache's revision or its refresh count changes.
    func syncLayout() {
        guard grouping != .none, let player, player.connected else { return }
        let target = LayoutBasis(epoch: epoch, revision: cache.revision, refresh: cache.refreshes)
        guard layoutBasis != target, groupsRequest != target, groupsFailed != target else { return }
        if target.revision == nil {
            // The first window is on its way. Ask at once, unpinned, after a reset; after a
            // reconnect, wait for the cache's revision.
            guard layoutBasis?.epoch != epoch, groupsRequest?.epoch != epoch else { return }
        }
        requestGroups(target)
    }

    private func requestGroups(_ target: LayoutBasis) {
        groupsTicket &+= 1
        let ticket = groupsTicket
        groupsRequest = target
        let grouping = self.grouping
        var request = description
        request.group = GroupSpans.requestGrouping(for: grouping)
        // A cancelled request would close the shared connection: send it unstructured.
        Task { @MainActor [weak self] in
            guard let player = self?.player else { return }
            let result: Result<ListGroupsResponse, any Error>
            do {
                result = .success(try await player.send("list_groups", [
                    "list": try JSONValue.encoding(request),
                    "revision": target.revision.map(JSONValue.uint) ?? .null]))
            } catch {
                result = .failure(error)
            }
            guard let self, ticket == self.groupsTicket else { return }
            self.groupsRequest = nil
            switch result {
            case .success(let reply):
                let previous = self.layoutBasis?.revision
                self.groupedLayout = GroupLayout(grouping: grouping, total: Int(reply.total),
                                                 spans: GroupSpans.layoutSpans(grouping: grouping, reply: reply.groups))
                self.albumCounts = grouping == .album ? [:] : GroupSpans.albumCounts(reply.groups)
                self.layoutBasis = LayoutBasis(epoch: target.epoch, revision: reply.revision, refresh: target.refresh)
                self.layoutVersion &+= 1
                if previous != nil, previous != reply.revision { self.facts.invalidate() }
                // The cache may have moved on while the spans were in flight.
                self.syncLayout()
            case .failure:
                // A stale revision is followed by the cache's next pass, which changes the target.
                self.groupsFailed = target
            }
        }
    }

    private func discardGroups() {
        groupsTicket &+= 1
        groupsRequest = nil
        groupsFailed = nil
        facts.reset()
    }

    // MARK: Table

    /// The table's visible display rows. Prefetches the positions they show, records the
    /// anchor row and asks for the facts of the albums in view.
    func visible(_ displayRange: Range<Int>) {
        let layout = self.layout
        visibleDisplay = displayRange
        if let positions = layout.positionRange(forDisplay: displayRange) { cache.visible(positions) }
        recordAnchor()
        guard showsAlbumBlocks else { return }
        var albums = Set<UInt64>()
        for index in displayRange {
            if let album = layout.albumKey(atDisplayIndex: index) { albums.insert(album) }
        }
        facts.want(albums)
    }

    /// The first loaded track in view becomes the anchor. Rows may load after the view stops
    /// scrolling, so this runs again before each revision change. Under a layout older than
    /// the rows, display rows name the wrong positions: the previous anchor stays.
    private func recordAnchor() {
        guard layoutIsCurrent, let range = visibleDisplay else { return }
        let layout = self.layout
        for position in layout.positions(ofDisplayIndexes: IndexSet(integersIn: range)) {
            guard let row = cache.loadedRow(at: position) else { continue }
            guard let display = layout.displayIndex(ofPosition: position) else { return }
            anchorRowID = row.row_id
            anchorOffset = max(0, display - range.lowerBound)
            return
        }
    }

    /// Regular playlists reorder only ungrouped and unsorted (spec 4.3).
    var allowsReorder: Bool { PlaylistReorder.allowed(source: source, grouping: grouping, sort: sort) }

    /// A reorder drop was refused: shows "Set Group to None and clear sorting to reorder".
    func showReorderHint() {
        reorderHintShown = true
        hintTask?.cancel()
        hintTask = Task { [weak self] in
            do { try await Task.sleep(for: .seconds(3)) } catch { return }
            self?.reorderHintShown = false
        }
    }

    var selectionBasis: RowSelection.Basis {
        RowSelection.Basis(epoch: epoch, revision: cache.revision, refresh: cache.refreshes)
    }

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

    private func makeFactsFetch() -> AlbumFactsCache.Fetch {
        { @MainActor [weak self] ids in
            guard let player = self?.player else { throw WindowFetchError.failed("The list closed.") }
            let reply: AlbumSummariesResponse = try await player.send("album_summaries", [
                "album_ids": .uints(ids), "revision": .null])
            return reply.entries
        }
    }

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
/// The Albums grid is kept the same way.
@MainActor
final class TrackLists {
    private var lists: [Destination: ListModel] = [:]
    private var albums: AlbumGridModel?

    func albumGrid(player: PlayerModel, navigation: NavigationModel) -> AlbumGridModel {
        if let albums { return albums }
        let grid = AlbumGridModel(player: player, navigation: navigation)
        albums = grid
        return grid
    }

    func list(for destination: Destination, source: ListSource, player: PlayerModel,
              navigation: NavigationModel) -> ListModel {
        if let list = lists[destination] { return list }
        let list = ListModel(destination: destination, source: source, player: player, navigation: navigation)
        lists[destination] = list
        return list
    }
}
