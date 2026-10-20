import Foundation
import Observation
import TyrannyCore

/// The queue as the Up Next view and the drawer show it.
///
/// Two window caches read `queue_page`: `cache` for the Up Next view (only while it is shown) and
/// `drawer` for the drawer (always: after the current entry with shuffle off, else from the start
/// of the queue). Sections follow the server's shuffle-aware rule (`UpNextSections`); the counts
/// come from the snapshot's `upcoming_count`.
///
/// `sync()` follows the connection and three counters. A change of the queue revision, the
/// library revision (display fields come from the library) or `current_id` (played marks and
/// Repeat All wrap clears do not bump the queue revision) refetches the visible windows.
///
/// Queue edits (move, remove, clear, shuffle, insert, save) that get `stale_revision` refetch the
/// snapshot and retry once. They name entries by ID, so a retry is safe.
@MainActor @Observable
final class UpNextModel {
    /// Windows for the Up Next view.
    let cache: ListWindowCache<QueueItem>
    /// Windows for the drawer and the count.
    let drawer: ListWindowCache<QueueItem>
    /// Album facts for the drawer's "album · year" line.
    let facts: AlbumFactsCache
    /// Bumps when `cache` is reset; table positions from an older epoch mean nothing.
    private(set) var epoch: UInt64 = 0
    /// The first Up Next row in view, kept in view across refreshes.
    @ObservationIgnored private(set) var anchorID: UInt64?

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private var needsReset = true
    @ObservationIgnored private var viewShown = false
    /// `cache` missed a change while the view was hidden.
    @ObservationIgnored private var viewStale = true
    @ObservationIgnored private var queueRevision: UInt64?
    @ObservationIgnored private var libraryRevision: UInt64?
    @ObservationIgnored private var currentID: UInt64?
    /// The queue positions the drawer last asked for.
    @ObservationIgnored private var drawerRange: Range<Int>?

    /// Upcoming rows the drawer lists; the rest show as "and N more".
    static let drawerLimit = 100

    init() {
        cache = ListWindowCache { _, _, _ in throw WindowFetchError.failed("not started") }
        drawer = ListWindowCache { _, _, _ in throw WindowFetchError.failed("not started") }
        facts = AlbumFactsCache { _ in throw WindowFetchError.failed("not started") }
    }

    func attach(_ player: PlayerModel) {
        self.player = player
        facts.reset(fetch: { @MainActor [weak player] ids in
            guard let player else { throw WindowFetchError.failed("Up Next closed.") }
            let reply: AlbumSummariesResponse = try await player.send("album_summaries", [
                "album_ids": .uints(ids), "revision": .null])
            return reply.entries
        })
    }

    // MARK: Following state

    func sync() {
        guard let player else { return }
        let generation = player.playlistConnectionGeneration
        guard player.connected, let snapshot = player.snapshot else {
            if session != generation {
                session = generation
                cache.discard(session: generation)
                drawer.discard(session: generation)
                facts.reset()
                queueRevision = nil
                selection = []
            }
            needsReset = true
            return
        }
        if session != generation {
            cache.discard(session: generation)
            drawer.discard(session: generation)
        }
        session = generation
        let queue = snapshot.queue_revision
        let library = snapshot.library.revision
        let current = snapshot.current_id
        if needsReset {
            needsReset = false
            queueRevision = queue; libraryRevision = library; currentID = current
            drawer.reset(fetch: makeFetch())
            drawerRange = nil
            viewStale = true
            if viewShown { resetView() }
        } else if queue != queueRevision || library != libraryRevision || current != currentID {
            queueRevision = queue; libraryRevision = library; currentID = current
            drawer.serverRevisionChanged(queue, anchor: nil)
            if viewShown { cache.serverRevisionChanged(queue, anchor: anchorID) } else { viewStale = true }
        }
        let range = UpNextSections.drawerRange(in: QueueContext(snapshot), limit: Self.drawerLimit)
        if range != drawerRange {
            drawerRange = range
            drawer.visible(range)
        }
    }

    /// The Up Next view appeared or left.
    func setViewShown(_ shown: Bool) {
        guard shown != viewShown else { return }
        viewShown = shown
        if shown, !needsReset, viewStale { resetView() }
    }

    private func resetView() {
        viewStale = false
        epoch &+= 1
        anchorID = nil
        cache.reset(fetch: makeFetch())
    }

    /// The table reports the rows in view.
    func visible(_ range: Range<Int>) {
        cache.visible(range)
        anchorID = range.lazy.compactMap { [cache] in cache.loadedRow(at: $0)?.id }.first
    }

    /// Fetches queue windows. A pinned revision the server dropped is `stale`.
    private func makeFetch() -> ListWindowCache<QueueItem>.Fetch {
        { @MainActor [weak self] offset, limit, revision in
            guard let player = self?.player else { throw WindowFetchError.failed("Up Next closed.") }
            do {
                let page: Page<QueueItem> = try await player.send("queue_page", [
                    "revision": revision.map(JSONValue.uint) ?? .null,
                    "offset": .int(offset), "limit": .int(limit)])
                return WindowPage(revision: page.revision, total: page.total, offset: page.offset, rows: page.entries)
            } catch let failure as ServerFailure {
                if failure.code == "stale_revision" { throw WindowFetchError.stale }
                throw WindowFetchError.failed(failure.message)
            }
        }
    }

    // MARK: Reading

    /// The current entry, its queue position and the shuffle state, from the snapshot.
    var context: QueueContext { QueueContext(player?.snapshot) }

    /// Entries still to play, as the server counts them. nil without a snapshot.
    var upcomingCount: Int? { player?.snapshot?.upcoming_count }

    /// The drawer's upcoming entries among the loaded windows, in queue order.
    func upcoming(limit: Int) -> [QueueItem] {
        UpNextSections.upcoming(drawer.loadedRows(), in: context, limit: limit)
    }

    // MARK: Selection

    /// Selected entry IDs in position order.
    @ObservationIgnored var selection: Set<UInt64> = []

    func selectedIDs() -> [UInt64] {
        cache.loadedRows().filter { selection.contains($0.row.id) }.map(\.row.id)
    }

    // MARK: Commands

    func play(_ id: UInt64) {
        player?.mutate { [weak self] in
            try await self?.run("play", fields: { _ in ["entry_id": .uint(id)] })
        }
    }

    func playSelection() {
        guard let id = selectedIDs().first else { return }
        play(id)
    }

    func removeSelection() {
        let ids = selectedIDs()
        guard !ids.isEmpty else { return }
        player?.mutate { [weak self] in
            guard let self else { return }
            for id in ids {
                try await self.run("remove", fields: { state in
                    ["expected_revision": .uint(state.queue_revision), "entry_id": .uint(id)] })
            }
        }
    }

    func shuffleUpcoming() {
        player?.mutate { [weak self] in
            guard let self else { return }
            try await self.run("shuffle_upcoming", fields: { state in
                ["expected_revision": .uint(state.queue_revision)] })
        }
    }

    /// Removes the entries after the current one. The current track keeps playing.
    func clearUpcoming() {
        player?.mutate { [weak self] in
            guard let self else { return }
            try await self.run("clear_upcoming", fields: { state in
                ["expected_revision": .uint(state.queue_revision)] })
        }
    }

    /// Saves the upcoming entries as a regular playlist. Returns the server's failure text, or
    /// nil on success. The caller (the sheet) shows it.
    func savePlaylist(name: String) async -> String? {
        guard let player, player.canMutate else { return "Wait for the current command to finish." }
        // Through `mutate`, so the model is busy and Clear and Shuffle are disabled meanwhile.
        return await withCheckedContinuation { continuation in
            player.mutate { [weak self] in
                do {
                    guard let self else { throw CancellationError() }
                    let _: PlaylistApplied = try await self.edit("save_upcoming", fields: { state in
                        ["expected_revision": .uint(state.queue_revision),
                         "expected_playlists_revision": .uint(state.playlists_revision), "name": .string(name)] })
                    player.message = "Saved \"\(name)\" as a playlist."
                    continuation.resume(returning: nil)
                } catch {
                    continuation.resume(returning: error.localizedDescription)
                }
            }
        }
    }

    /// Append is revision-checked; play is sent only after a durable append succeeds.
    /// A play failure never invites retrying the already successful append.
    func addStream(url: String, title: String, play: Bool) async -> String? {
        guard let player, player.canMutate else { return "Wait for the current command to finish." }
        return await withCheckedContinuation { continuation in
            player.mutate { [weak self] in
                do {
                    guard let self else { throw CancellationError() }
                    let connection = player.playlistConnectionGeneration
                    let added: Applied = try await self.edit("append_stream", fields: { state in
                        ["expected_revision": .uint(state.queue_revision),
                         "url": .string(url), "title": .string(title)] })
                    player.message = "Added stream to Up Next."
                    if play {
                        do {
                            guard player.connected, player.playlistConnectionGeneration == connection else {
                                throw ServerFailure(code: "connection_changed", message: "Connection changed. Select the stream in Up Next to play it.")
                            }
                            guard let id = added.entry_id else {
                                throw ServerFailure(code: "missing_entry", message: "The server did not return the stream entry.")
                            }
                            try await self.run("play", fields: { _ in ["entry_id": .uint(id)] })
                        } catch {
                            player.message = "Stream added, but playback could not start: \(error.localizedDescription)"
                        }
                    }
                    continuation.resume(returning: nil)
                } catch {
                    continuation.resume(returning: error.localizedDescription)
                }
            }
        }
    }

    private func run(_ command: String, fields: (ControlState) -> [String: JSONValue]) async throws {
        let _: Applied = try await edit(command, fields: fields)
    }

    /// Sends one queue command with the current revision. `stale_revision` refetches the
    /// snapshot and retries once.
    private func edit<T: Decodable>(_ command: String, fields: (ControlState) -> [String: JSONValue]) async throws -> T {
        guard let player else { throw CancellationError() }
        let connection = player.playlistConnectionGeneration
        var retried = false
        while true {
            guard player.connected, player.playlistConnectionGeneration == connection, let state = player.snapshot else {
                throw ServerFailure(code: "connection_changed", message: "Disconnected. Nothing was changed.")
            }
            do {
                return try await player.send(command, fields(state))
            } catch let failure as ServerFailure where failure.code == "stale_revision" && !retried {
                retried = true
                try await player.refreshSnapshot()
            }
        }
    }

    // MARK: Drag and drop

    /// Starts a drag of the selected rows (or the dragged row when it is not selected).
    func beginDrag(ids: [UInt64]) -> String? {
        guard let player else { return nil }
        guard ids.count <= 500 else {
            player.message = "Select 500 or fewer rows to drag."
            return nil
        }
        guard !ids.isEmpty, let state = player.snapshot else {
            player.message = "Wait for Up Next to load, then drag again."
            return nil
        }
        guard player.canMutate else {
            player.message = "Wait for the current command to finish, then drag again."
            return nil
        }
        let drag = TrackDrag(token: UUID().uuidString, connection: player.playlistConnectionGeneration,
                             source: .queue, ids: ids, positions: [], revision: state.queue_revision,
                             libraryRevision: state.library.revision)
        player.activeDrag = drag
        return drag.token
    }

    /// Whether a drop at `position` may happen: the drag is valid and the entry the drop lands
    /// before is loaded (or the drop is at the end).
    func canDrop(token: String, at position: Int) -> Bool {
        guard let player, player.validateTrackDrop(token: token, destination: .queue) else { return false }
        return position >= cache.total || cache.loadedRow(at: position) != nil
    }

    /// Drops the active drag before the entry at `position` (the end when past the last row).
    ///
    /// Up Next rows move. A library drag of loaded IDs (500 at most) is inserted there. Any
    /// other selection is too large to place and is appended instead.
    func drop(token: String, at position: Int) -> Bool {
        guard canDrop(token: token, at: position), let player, let drag = player.activeDrag else { return false }
        let before = position < cache.total ? cache.loadedRow(at: position)?.id : nil
        if drag.source == .queue {
            let ids = drag.ids
            guard !ids.contains(before ?? 0) else { player.activeDrag = nil; return true }
            player.activeDrag = nil
            player.mutate { [weak self] in
                try await self?.run("move", fields: { state in
                    ["expected_revision": .uint(state.queue_revision), "entry_ids": .uints(ids),
                     "before": before.map(JSONValue.uint) ?? .null] })
            }
            return true
        }
        guard let selection = drag.selection else { return false }
        if case .ids(let libraryRevision, let trackIDs) = selection, trackIDs.count <= 500 {
            player.activeDrag = nil
            let count = trackIDs.count
            player.mutate { [weak self, weak player] in
                try await self?.run("insert_tracks", fields: { state in
                    ["expected_revision": .uint(state.queue_revision), "library_revision": .uint(libraryRevision),
                     "track_ids": .uints(trackIDs), "before": before.map(JSONValue.uint) ?? .null] })
                player?.message = "Added \(DisplayFormat.count(count)) \(count == 1 ? "track" : "tracks") to Up Next."
            }
            return true
        }
        player.activeDrag = nil
        let label = drag.label, connection = drag.connection
        player.mutate { [weak player] in
            guard let player else { return }
            try await player.appendSelection(selection, label: label, connection: connection)
            player.message = "Added at the end"
        }
        return true
    }
}
