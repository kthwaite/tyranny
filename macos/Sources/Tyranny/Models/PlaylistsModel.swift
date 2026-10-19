import Foundation
import Observation
import TyrannyCore

/// The saved playlists as the source list shows them, and the edits made to them.
///
/// `cache` reads `saved_playlists` (regular and smart, by name). The source list loads every
/// playlist, up to `limit`. `sync()` follows the connection and the playlists revision: when
/// it changes the cache refreshes, so a playlist's revision (the one a list source names)
/// is current within a refresh pass.
///
/// Edits name the playlist's revision. One that gets `stale_revision` or
/// `stale_playlist_revision` refetches the snapshot and the playlists and retries once.
/// A drop the server rejected because the rows it names moved on. Nothing was added.
struct ListChangedError: LocalizedError {
    var errorDescription: String? { "The list changed; try again" }
}

@MainActor @Observable
final class PlaylistsModel {
    /// The most playlists the sidebar loads (ten windows).
    static let limit = 2000

    let cache: ListWindowCache<Playlist>

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private var needsReset = true
    @ObservationIgnored private var catalogRevision: UInt64?
    /// Playlists fetched by a retry, newer than the cache's rows until it refreshes.
    @ObservationIgnored private var fetched: [UInt64: Playlist] = [:]
    @ObservationIgnored private var memo: (generation: UInt64, byID: [UInt64: Playlist], rows: [Playlist])?

    init() {
        cache = ListWindowCache { _, _, _ in throw WindowFetchError.failed("not started") }
    }

    func attach(_ player: PlayerModel) { self.player = player }

    // MARK: Following state

    func sync() {
        guard let player else { return }
        let generation = player.playlistConnectionGeneration
        guard player.connected, let snapshot = player.snapshot else {
            if session != generation {
                session = generation
                cache.discard(session: generation)
                fetched = [:]
                catalogRevision = nil
            }
            needsReset = true
            return
        }
        if session != generation {
            cache.discard(session: generation)
            fetched = [:]
        }
        session = generation
        let revision = snapshot.playlists_revision
        if needsReset {
            needsReset = false
            catalogRevision = revision
            fetched = [:]
            cache.reset(fetch: makeFetch())
            cache.visible(0..<1)
        } else if revision != catalogRevision {
            catalogRevision = revision
            // A retry's fetch is older than this revision: a playlist may be gone since.
            fetched = [:]
            cache.serverRevisionChanged(revision, anchor: nil)
        }
    }

    /// Asks for every window the catalog needs. The source list calls it as the cache changes.
    func loadAll() {
        let total = min(max(cache.total, 1), Self.limit)
        cache.visible(0..<total)
    }

    private func makeFetch() -> ListWindowCache<Playlist>.Fetch {
        { @MainActor [weak self] offset, limit, revision in
            guard let player = self?.player else { throw WindowFetchError.failed("Playlists closed.") }
            do {
                let page: Page<Playlist> = try await player.send("saved_playlists", [
                    "revision": revision.map(JSONValue.uint) ?? .null,
                    "offset": .int(offset), "limit": .int(limit), "query": .string(""),
                ])
                return WindowPage(revision: page.revision, total: page.total, offset: page.offset, rows: page.entries)
            } catch let failure as ServerFailure {
                if failure.code == "stale_playlist_revision" { throw WindowFetchError.stale }
                throw WindowFetchError.failed(failure.message)
            }
        }
    }

    // MARK: Reading

    private var table: (byID: [UInt64: Playlist], rows: [Playlist]) {
        let generation = cache.generation
        if let memo, memo.generation == generation { return (memo.byID, memo.rows) }
        let rows = cache.loadedRows().map(\.row)
        var byID: [UInt64: Playlist] = [:]
        for row in rows { byID[row.id] = row }
        memo = (generation, byID, rows)
        return (byID, rows)
    }

    /// The loaded playlists in name order.
    var rows: [Playlist] { table.rows }

    /// Every playlist has loaded (or the catalog is empty).
    var isLoaded: Bool { cache.revision != nil && table.rows.count >= min(cache.total, Self.limit) }

    /// The playlist's latest known state: the loaded row, or a newer one a retry fetched.
    func playlist(id: UInt64) -> Playlist? {
        let loaded = table.byID[id]
        guard let newer = fetched[id] else { return loaded }
        guard let loaded else { return newer }
        return newer.revision > loaded.revision ? newer : loaded
    }

    /// The list source a playlist's tracks are read through.
    static func listSource(_ playlist: Playlist) -> ListSource {
        playlist.isSmart ? .smart(id: playlist.id, revision: playlist.revision)
                         : .playlist(id: playlist.id, revision: playlist.revision)
    }

    // MARK: Edits

    /// Fetches the whole catalog without a pinned revision, for the retry after a stale reply.
    private func refetch() async throws {
        guard let player else { throw CancellationError() }
        try await player.refreshSnapshot()
        var offset = 0
        var fresh: [UInt64: Playlist] = [:]
        while offset < Self.limit {
            let page: Page<Playlist> = try await player.send("saved_playlists", [
                "revision": .null, "offset": .int(offset), "limit": .int(500), "query": .string("")])
            for entry in page.entries { fresh[entry.id] = entry }
            offset += page.entries.count
            if page.entries.isEmpty || offset >= page.total { break }
        }
        fetched = fresh
    }

    /// Sends one playlist command. `fields` gets the playlist's latest state (nil for commands
    /// that name none) and the snapshot. `stale_revision` and `stale_playlist_revision`
    /// refetch and retry once, and so does `stale_library_revision` when `retryLibrary` is set
    /// (the fields then read the library revision again).
    private func edit<T: Decodable>(
        _ command: String, playlist id: UInt64? = nil, seed: Playlist? = nil, retryLibrary: Bool = false,
        embedsPlaylistSource: Bool = false,
        fields: (Playlist?, ControlState) throws -> [String: JSONValue]
    ) async throws -> T {
        guard let player else { throw CancellationError() }
        var retried = false
        var known = seed
        while true {
            guard player.connected, let state = player.snapshot else {
                throw ServerFailure(code: "connection_changed", message: "Disconnected. Nothing was changed.")
            }
            let current: Playlist?
            if let id {
                guard let found = known ?? playlist(id: id) else {
                    throw ServerFailure(code: "invalid_command", message: "That playlist no longer exists.")
                }
                current = found
            } else {
                current = nil
            }
            do {
                return try await player.send(command, try fields(current, state))
            } catch let failure as ServerFailure
                where embedsPlaylistSource && failure.code == "stale_playlist_revision" {
                // The selection names a playlist's revision; a refetch cannot update it.
                throw ListChangedError()
            } catch let failure as ServerFailure
                where !retried && (PlaylistDrop.isStale(failure.code)
                                   || (retryLibrary && failure.code == "stale_library_revision")) {
                retried = true
                try await refetch()
                known = nil
            }
        }
    }

    /// Creates a regular playlist. Returns it.
    func create(name: String) async throws -> Playlist {
        let reply: PlaylistApplied = try await edit("create_playlist") { _, state in
            ["expected_revision": .uint(state.playlists_revision), "name": .string(name)]
        }
        guard let playlist = reply.playlist else {
            throw ServerFailure(code: "invalid_reply", message: "Creation reply omitted the playlist.")
        }
        player?.message = "Saved playlist “\(playlist.name)”."
        return playlist
    }

    func rename(_ playlist: Playlist, name: String) async throws {
        let _: PlaylistApplied = try await edit("rename_playlist", playlist: playlist.id) { current, _ in
            ["playlist_id": .uint(playlist.id), "expected_revision": .uint(current?.revision ?? playlist.revision),
             "name": .string(name)]
        }
    }

    func delete(_ playlist: Playlist) async throws {
        let _: PlaylistApplied = try await edit("delete_playlist", playlist: playlist.id) { current, _ in
            ["playlist_id": .uint(playlist.id), "expected_revision": .uint(current?.revision ?? playlist.revision)]
        }
    }

    /// Adds a selection after the playlist's last entry (`playlist_add_selection`). Throws
    /// `ListChangedError` when the rows moved on; the success status is set only on success.
    func addSelection(to playlist: Playlist, selection: TrackSelection, count: Int, seed: Playlist? = nil) async throws {
        let encoded = try JSONValue.encoding(selection)
        do {
            let _: PlaylistApplied = try await edit(
                "playlist_add_selection", playlist: playlist.id, seed: seed,
                embedsPlaylistSource: PlaylistDrop.embedsPlaylistSource(selection)
            ) { current, _ in
                ["playlist_id": .uint(playlist.id), "expected_revision": .uint(current?.revision ?? playlist.revision),
                 "selection": encoded, "before": .null]
            }
        } catch let failure as ServerFailure where failure.code == "stale_library_revision" {
            throw ListChangedError()
        }
        player?.message = "Added \(DisplayFormat.count(count)) \(count == 1 ? "track" : "tracks") to \(playlist.name)."
    }

    /// Drops a selection at an insertion point: loaded IDs go before `before`, anything else is
    /// appended. An append, or a drop on a list with no insertion points (`atEnd`), says so in
    /// the status bar. Throws `ListChangedError` when the rows moved on.
    func addAtInsertion(to playlist: Playlist, selection: TrackSelection, trackIDs: [UInt64]?, count: Int,
                        before: UInt64?, atEnd: Bool = false) async throws {
        switch PlaylistDrop.atInsertion(selection, loadedTrackIDs: trackIDs) {
        case .append:
            try await addSelection(to: playlist, selection: selection, count: count)
            player?.message = PlaylistDrop.appendedStatus
        case .insert(let libraryRevision, let trackIDs):
            do {
                let _: PlaylistApplied = try await edit("playlist_add", playlist: playlist.id) { current, _ in
                    ["playlist_id": .uint(playlist.id), "expected_revision": .uint(current?.revision ?? playlist.revision),
                     "library_revision": .uint(libraryRevision), "track_ids": .uints(trackIDs),
                     "before": before.map(JSONValue.uint) ?? .null]
                }
            } catch let failure as ServerFailure where failure.code == "stale_library_revision" {
                throw ListChangedError()
            }
            player?.message = atEnd ? PlaylistDrop.appendedStatus
                : "Added \(DisplayFormat.count(count)) \(count == 1 ? "track" : "tracks") to \(playlist.name)."
        }
    }

    /// Moves entries before `before` (nil: to the end), in the order given.
    func move(in playlist: Playlist, entryIDs: [UInt64], before: UInt64?) async throws {
        let _: PlaylistApplied = try await edit("playlist_move", playlist: playlist.id) { current, _ in
            ["playlist_id": .uint(playlist.id), "expected_revision": .uint(current?.revision ?? playlist.revision),
             "entry_ids": .uints(entryIDs), "before": before.map(JSONValue.uint) ?? .null]
        }
    }

    /// Creates a regular playlist and adds a selection to it. If the add fails the playlist
    /// exists and the error says so.
    func createAdding(name: String, selection: TrackSelection, count: Int) async throws -> Playlist {
        let playlist = try await create(name: name)
        do {
            try await addSelection(to: playlist, selection: selection, count: count, seed: playlist)
        } catch {
            throw ServerFailure(code: "partial", message:
                "Playlist “\(playlist.name)” was created, but adding tracks failed: \(error.localizedDescription)")
        }
        return playlist
    }

    /// Saves a smart playlist: a new one named `name`, or the new rule of `playlist`.
    func saveSmart(_ playlist: Playlist?, name: String, rule: String) async throws -> Playlist? {
        let reply: PlaylistApplied
        if let playlist {
            reply = try await edit("update_smart_playlist", playlist: playlist.id) { current, _ in
                ["playlist_id": .uint(playlist.id), "expected_revision": .uint(current?.revision ?? playlist.revision),
                 "query": .string(rule)]
            }
        } else {
            reply = try await edit("create_smart_playlist") { _, state in
                ["expected_revision": .uint(state.playlists_revision), "name": .string(name), "query": .string(rule)]
            }
        }
        return reply.playlist
    }

    /// Copies a smart playlist's current results into a new regular playlist.
    func snapshotSmart(_ playlist: Playlist, name: String) async throws -> Playlist? {
        let reply: PlaylistApplied = try await edit(
            "snapshot_smart_playlist", playlist: playlist.id, retryLibrary: true
        ) { current, state in
            ["playlist_id": .uint(playlist.id), "playlist_revision": .uint(current?.revision ?? playlist.revision),
             "library_revision": .uint(state.library.revision), "expected_revision": .uint(state.playlists_revision),
             "name": .string(name)]
        }
        return reply.playlist
    }

    /// Enqueues the whole playlist, or plays it from the start.
    func enqueue(_ playlist: Playlist, play: Bool) async throws {
        let _: Applied = try await edit("enqueue_playlist", playlist: playlist.id, retryLibrary: true) { current, state in
            ["playlist_id": .uint(playlist.id), "playlist_revision": .uint(current?.revision ?? playlist.revision),
             "library_revision": .uint(state.library.revision), "expected_revision": .uint(state.queue_revision),
             "play": .bool(play)]
        }
    }

    // MARK: Smart rule preview

    struct RulePreview {
        let revision: UInt64
        let total: Int
        let rows: [ListRow]
    }

    /// The first 100 tracks of the whole library that match `rule`, with the match count.
    /// Never narrowed by a list's browser or filter.
    func preview(rule: String) async throws -> RulePreview {
        guard let player else { throw CancellationError() }
        let generation = player.playlistConnectionGeneration
        let description = ListDescription(source: .library, query: rule)
        // A cancelled request would close the shared connection: send it unstructured.
        let reply: ListTracksResponse = try await Task { @MainActor in
            try await player.send("list_tracks", [
                "list": try JSONValue.encoding(description), "revision": .null,
                "offset": .int(0), "limit": .int(100)])
        }.value
        guard generation == player.playlistConnectionGeneration, player.connected else { throw CancellationError() }
        return RulePreview(revision: reply.revision, total: Int(reply.total), rows: reply.entries)
    }

    // MARK: Running from a sheet

    /// Runs `operation` as a user mutation and returns its failure text, or nil on success.
    /// The sheet shows the text. The model is busy meanwhile.
    func run(_ operation: @escaping @MainActor () async throws -> Void) async -> String? {
        guard let player, player.canMutate else { return "Wait for the current command to finish." }
        return await withCheckedContinuation { continuation in
            player.mutate {
                do {
                    try await operation()
                    continuation.resume(returning: nil)
                } catch {
                    continuation.resume(returning: error.localizedDescription)
                }
            }
        }
    }
}
