import Foundation
import TyrannyCore

/// Double-click, Return and the context menu on a track list. Every one builds a `SelectionRequest`
/// and hands an `EnqueueJob` to `ActivationModel`.
extension ListModel {
    /// The label queue entries from this list carry (spec: Added From).
    var queueSource: QueueSource {
        RunSource.queueSource(list: source, name: displayName ?? destination.title)
    }

    /// Double-click on a row: play the list from that row to its end.
    func activate(position: Int) {
        start(.play, positions: IndexSet(integer: position))
    }

    /// Return, ⌥Return and the context menu on the selected rows.
    func activateSelected(_ action: ActivationAction) {
        start(action, positions: rows.positions)
    }

    private func start(_ action: ActivationAction, positions: IndexSet) {
        guard let player, !positions.isEmpty else { return }
        do {
            // One row played starts a run to the end of the list; several rows play as chosen.
            let request = try selectionRequest(positions: positions, asRun: action == .play)
            player.activation.start(EnqueueJob(action: action, source: queueSource, request: request,
                                               status: status(action, count: positions.count)))
        } catch {
            player.message = (error as? LocalizedError)?.errorDescription ?? "Wait for the list to load, then try again."
        }
    }

    private func status(_ action: ActivationAction, count: Int) -> String? {
        let tracks = "\(DisplayFormat.count(count)) \(count == 1 ? "track" : "tracks")"
        switch action {
        case .play: return nil
        case .addToUpNext: return "Added \(tracks) to Up Next."
        case .playNext: return "Added \(tracks) to play next."
        }
    }

    /// The rows at `positions` as a selection, with the means to find them again. With
    /// `asRun`, a single row starts a run: it and every row after it.
    func selectionRequest(positions: IndexSet, asRun: Bool) throws -> SelectionRequest {
        guard let revision = cache.revision, let first = positions.first,
              let firstRow = cache.loadedRow(at: first) else { throw EmptySelection() }
        let selection = asRun && positions.count == 1
            ? SelectionEncoder.run(list: description, revision: revision, from: first, rowID: firstRow.row_id)
            : try encode(positions)
        let rowID = firstRow.row_id
        return SelectionRequest(selection: selection) { [weak self] in
            guard let self else { throw ListChangedError() }
            return try await self.relocated(selection, rowID: rowID, at: first)
        }
    }

    /// Fetches the window around the clicked row, finds the row by ID and re-encodes the
    /// selection under the library revision of that window.
    private func relocated(_ selection: TrackSelection, rowID: UInt64, at position: Int) async throws -> TrackSelection {
        guard let player else { throw ListChangedError() }
        let window = RowRelocation.window(around: position, size: 200)
        var request = description
        request.source = source
        let reply: ListTracksResponse = try await player.send("list_tracks", [
            "list": try JSONValue.encoding(request), "revision": .null,
            "offset": .int(window.offset), "limit": .int(window.limit)])
        guard case .found(let now) = RowRelocation.locate(rowID: rowID, rowIDs: reply.entries.map(\.row_id),
                                                          offset: reply.offset),
              let fresh = SelectionRetarget.apply(selection, libraryRevision: reply.revision, delta: now - position)
        else { throw ListChangedError() }
        return fresh
    }

    // MARK: Context menu targets

    /// The selected rows' first row, for Show in Album and Show in Artist.
    var firstSelectedTrack: TrackSummary? {
        rows.positions.first.flatMap { cache.loadedRow(at: $0)?.track }
    }

    func showInAlbum() {
        guard let track = firstSelectedTrack else { return }
        navigation.reveal = .album(track.album_id)
    }

    func showInArtist() {
        guard let track = firstSelectedTrack else { return }
        navigation.reveal = .artist(track.album_artist_id)
    }
}
