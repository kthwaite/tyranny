import Foundation
import Observation
import TyrannyCore

/// The Albums grid (spec 4.4): a window cache over `list_albums` for the library, narrowed by
/// the filter and ordered by `sort`; the selected album; the expanded album and its tracks;
/// and the album actions.
///
/// `sync()` follows the connection, the library revision and the filter. The view calls it
/// when any of them changes, and `refreshed()` when the cache commits a refresh pass.
@MainActor @Observable
final class AlbumGridModel {
    let cache: ListWindowCache<AlbumSummary>
    private(set) var sort: AlbumSort = .albumArtist
    /// Bumps when the list description or sort changes; positions from an older epoch mean nothing.
    private(set) var epoch: UInt64 = 0
    private(set) var filterError: String?
    var selectedID: UInt64?
    /// The album whose panel is open, with its tracks.
    private(set) var expansion: AlbumExpansion?

    /// Date Added lists the newest albums first; the other sorts ascend.
    var descending: Bool { sort == .dateAdded }
    var isNarrowed: Bool { !query.isEmpty }

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private let navigation: NavigationModel
    @ObservationIgnored private var query = ""
    @ObservationIgnored private var pendingFilter: String?
    @ObservationIgnored private var filterTask: Task<Void, Never>?
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private var needsReset = true
    @ObservationIgnored private var libraryRevision: UInt64?
    /// The first album in view, kept in view across revision changes.
    @ObservationIgnored private var anchorID: UInt64?
    @ObservationIgnored private var visibleRange: Range<Int>?

    init(player: PlayerModel, navigation: NavigationModel) {
        self.player = player
        self.navigation = navigation
        // Replaced by `reset` on the first sync.
        cache = ListWindowCache { _, _, _ in throw WindowFetchError.failed("not started") }
    }

    // MARK: Following state

    func sync() {
        guard let player else { return }
        let session = player.playlistConnectionGeneration
        guard player.connected, let snapshot = player.snapshot else {
            if self.session != session {
                self.session = session
                cache.discard(session: session)
            }
            needsReset = true
            return
        }
        if self.session != session {
            cache.discard(session: session)
        }
        self.session = session
        let filter = navigation.filter.trimmingCharacters(in: .whitespacesAndNewlines)
        var queryChanged = false
        if filter != query {
            if filter.isEmpty || needsReset {
                cancelFilter()
                query = filter
                queryChanged = true
            } else if filter != pendingFilter {
                // Revision changes during a scan call sync() too; they must not restart the wait.
                scheduleFilter(filter)
            }
        } else if pendingFilter != nil {
            cancelFilter()
        }
        let library = snapshot.library.revision
        if needsReset || queryChanged {
            needsReset = false
            libraryRevision = library
            reset()
        } else if library != libraryRevision {
            libraryRevision = library
            recordAnchor()
            cache.serverRevisionChanged(library, anchor: anchorID)
        }
    }

    func setSort(_ sort: AlbumSort) {
        guard sort != self.sort else { return }
        self.sort = sort
        reset()
    }

    /// A new filter, sort or session: positions start over, so the panel closes.
    private func reset() {
        filterError = nil
        expansion = nil
        epoch &+= 1
        anchorID = nil
        visibleRange = nil
        cache.reset(fetch: makeFetch())
    }

    /// Typing waits 250 ms before the grid refetches.
    private func scheduleFilter(_ filter: String) {
        filterTask?.cancel()
        pendingFilter = filter
        filterTask = Task { [weak self] in
            do { try await Task.sleep(for: .milliseconds(250)) } catch { return }
            guard let self, self.pendingFilter == filter else { return }
            self.pendingFilter = nil
            self.query = filter
            self.needsReset = true
            self.sync()
        }
    }

    private func cancelFilter() {
        filterTask?.cancel()
        filterTask = nil
        pendingFilter = nil
    }

    /// The cache committed a refresh pass, or a scan started or ended: the panel's album may
    /// have changed. Its tracks are fetched again only when `AlbumPanel.needsReload` says so.
    func refreshed() {
        guard let expansion, let player, let revision = player.snapshot?.library.revision else { return }
        if let position = cache.position(of: expansion.albumID), let album = cache.loadedRow(at: position) {
            expansion.album = album
        }
        guard AlbumPanel.needsReload(loaded: expansion.basis, current: expansion.album, revision: revision,
                                     scanning: player.scanActive) else { return }
        loadTracks(expansion)
    }

    // MARK: Grid

    /// The album positions in view. Prefetches them and records the anchor album.
    func visible(_ range: Range<Int>) {
        visibleRange = range
        cache.visible(range)
        recordAnchor()
    }

    private func recordAnchor() {
        guard let range = visibleRange else { return }
        for position in range {
            if let album = cache.loadedRow(at: position) {
                anchorID = album.id
                return
            }
        }
    }

    /// The album shown at `position`, if loaded.
    func album(at position: Int) -> AlbumSummary? { cache.loadedRow(at: position) }

    /// Opens the panel for `album`, or closes it when it is already open.
    func toggleExpansion(_ album: AlbumSummary) {
        if expansion?.albumID == album.id {
            expansion = nil
        } else {
            let next = AlbumExpansion(album: album)
            expansion = next
            loadTracks(next)
        }
    }

    func collapse() { expansion = nil }

    // MARK: Tracks

    /// Fetches the album's first `AlbumPanel.trackLimit` tracks in one request. One fetch per
    /// panel is in flight; a reload asked for meanwhile runs after it. A reply for a panel that
    /// has closed or moved is dropped.
    private func loadTracks(_ expansion: AlbumExpansion) {
        guard !expansion.fetching else {
            expansion.refetch = true
            return
        }
        guard let player, let revision = player.snapshot?.library.revision else { return }
        expansion.fetching = true
        let basis = AlbumPanel.Loaded(album: expansion.album, revision: revision)
        let list = AlbumPlayback.list(albumID: expansion.albumID)
        // A cancelled request would close the shared connection: send it unstructured.
        Task { @MainActor [weak self] in
            let result: Result<ListTracksResponse, any Error>
            do {
                result = .success(try await player.send("list_tracks", [
                    "list": try JSONValue.encoding(list), "revision": .null,
                    "offset": .int(0), "limit": .int(AlbumPanel.trackLimit)]))
            } catch {
                result = .failure(error)
            }
            expansion.fetching = false
            guard let self, self.expansion === expansion else { return }
            switch result {
            case .success(let page):
                expansion.loaded(page.entries, total: Int(page.total), basis: basis)
            case .failure(let error):
                // The next refresh pass, or the end of the scan, fetches again.
                expansion.failed(error.localizedDescription)
            }
            if expansion.refetch {
                expansion.refetch = false
                self.refreshed()
            }
        }
    }

    // MARK: Actions

    /// Play: the whole album, placed by the Up Next policy. Shuffle turns shuffle on first, so
    /// the server starts at a shuffle pick.
    func play(_ album: AlbumSummary, shuffle: Bool = false) {
        start(.play, album, shuffle: shuffle)
    }

    /// ⌥Return and Add to Up Next: appends the album.
    func addToUpNext(_ album: AlbumSummary) {
        start(.addToUpNext, album)
    }

    /// The album is a source selection, so it has no row to relocate: a stale library revision
    /// only needs the fresh revision.
    private func start(_ action: ActivationAction, _ album: AlbumSummary, shuffle: Bool = false) {
        guard let player, let revision = player.snapshot?.library.revision else { return }
        let selection = AlbumPlayback.selection(albumID: album.id, libraryRevision: revision)
        let noun = album.track_count == 1 ? "track" : "tracks"
        player.activation.start(EnqueueJob(
            action: action, source: AlbumPlayback.source(title: album.title),
            request: .source(selection, player: player), shuffleFirst: shuffle,
            status: action == .addToUpNext ? "Added \(DisplayFormat.count(album.track_count)) \(noun) to Up Next." : nil))
    }

    /// Show in Album: selects the album and opens its panel.
    func reveal(albumID: UInt64) {
        guard let player else { return }
        selectedID = albumID
        Task { @MainActor [weak self] in
            let reply: AlbumSummariesResponse? = try? await player.send("album_summaries", [
                "album_ids": .uints([albumID]), "revision": .null])
            guard let self, selectedID == albumID, expansion?.albumID != albumID,
                  let album = reply?.entries.first else { return }
            toggleExpansion(album)
        }
    }

    /// Adds the album to the end of a regular playlist. A stale playlist revision refetches
    /// the catalog page and retries once; a stale library revision refetches the snapshot.
    func addToPlaylist(_ album: AlbumSummary, playlist: Playlist) {
        guard let player else { return }
        player.mutate {
            var target = playlist
            var retried = false
            while true {
                guard player.connected, let state = player.snapshot else {
                    throw ServerFailure(code: "connection_changed", message: "Disconnected. Nothing was added.")
                }
                let selection = SelectionEncoder.source(list: AlbumPlayback.list(albumID: album.id),
                                                        revision: state.library.revision)
                do {
                    let _: PlaylistApplied = try await player.send("playlist_add_selection", [
                        "playlist_id": .uint(target.id), "expected_revision": .uint(target.revision),
                        "selection": try JSONValue.encoding(selection), "before": .null])
                    player.message = "Added \(album.title.isEmpty ? "the album" : album.title) to \(target.name)."
                    return
                } catch let failure as ServerFailure
                    where ["stale_revision", "stale_library_revision"].contains(failure.code) && !retried {
                    retried = true
                    try await player.refreshSnapshot()
                    if failure.code == "stale_revision" {
                        let page = try await player.regularPlaylistPage(query: "", offset: 0)
                        guard let fresh = page.entries.first(where: { $0.id == target.id }) else {
                            throw ServerFailure(code: "playlist_missing", message: "\(target.name) no longer exists.")
                        }
                        target = fresh
                    }
                }
            }
        }
    }

    /// Regular playlists from the loaded catalog.
    var regularPlaylists: [Playlist] { player?.savedPlaylists.rows.filter { !$0.isSmart } ?? [] }

    // MARK: Fetching

    private func makeFetch() -> ListWindowCache<AlbumSummary>.Fetch {
        let list = ListDescription(source: .library, query: query)
        let sort = self.sort, descending = self.descending, epoch = self.epoch
        return { @MainActor [weak self] offset, limit, revision in
            guard let self, let player = self.player else { throw WindowFetchError.failed("The grid closed.") }
            do {
                let reply: ListAlbumsResponse = try await player.send("list_albums", [
                    "list": try JSONValue.encoding(list), "sort": .string(sort.rawValue),
                    "descending": .bool(descending), "revision": revision.map(JSONValue.uint) ?? .null,
                    "offset": .int(offset), "limit": .int(limit)])
                if epoch == self.epoch { self.filterError = nil }
                return WindowPage(revision: reply.revision, total: Int(reply.total), offset: reply.offset,
                                  rows: reply.entries)
            } catch let failure as ServerFailure {
                switch failure.code {
                case "stale_library_revision":
                    throw WindowFetchError.stale
                case "invalid_filter":
                    if epoch == self.epoch { self.filterError = failure.message }
                    throw WindowFetchError.failed(failure.message)
                default:
                    throw WindowFetchError.failed(failure.localizedDescription)
                }
            }
        }
    }
}

/// The open album panel: the album and its first tracks in album order.
@MainActor @Observable
final class AlbumExpansion {
    let albumID: UInt64
    var album: AlbumSummary
    /// At most `AlbumPanel.trackLimit` tracks.
    private(set) var tracks: [ListRow] = []
    /// The album's track count from the same reply as `tracks`.
    private(set) var total = 0
    /// The tracks match the current album; false while the first fetch is in flight.
    private(set) var isLoaded = false
    private(set) var failure: String?
    /// What `tracks` were fetched for.
    @ObservationIgnored private(set) var basis: AlbumPanel.Loaded?
    @ObservationIgnored var fetching = false
    @ObservationIgnored var refetch = false

    init(album: AlbumSummary) {
        albumID = album.id
        self.album = album
    }

    func loaded(_ rows: [ListRow], total: Int, basis: AlbumPanel.Loaded) {
        tracks = rows
        self.total = total
        self.basis = basis
        isLoaded = true
        failure = nil
    }

    func failed(_ message: String) {
        // Keep the tracks shown; a later refresh replaces them.
        if !isLoaded { failure = message }
    }

    /// "and N more…" under a tracklist cut at `AlbumPanel.trackLimit`.
    var moreText: String? { AlbumPanel.moreText(total: total, shown: tracks.count) }
}
