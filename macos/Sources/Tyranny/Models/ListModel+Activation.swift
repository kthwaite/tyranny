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
    func activateSelected(_ action: ActivationAction, asRun: Bool = false) {
        start(action, positions: rows.positions, asRun: asRun)
    }

    private func start(_ action: ActivationAction, positions: IndexSet, asRun: Bool = true) {
        guard let player, !positions.isEmpty else { return }
        do {
            // A double-click and Return play a run to the end of the list; the rest send the rows chosen.
            let request = try selectionRequest(positions: positions, asRun: asRun && action == .play)
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
        let selection = try SelectionEncoder.activation(
            list: description, revision: revision, positions: positions, asRun: asRun,
            rowID: { cache.loadedRow(at: $0)?.row_id }, trackID: { cache.loadedRow(at: $0)?.track.id })
        if case .ids = selection, let player {
            // Track IDs do not move with the list: only the revision needs renewing.
            return .source(selection, player: player)
        }
        let rowID = firstRow.row_id
        return SelectionRequest(selection: selection) { [weak self] in
            guard let self else { throw ListChangedError() }
            return try await self.relocated(selection, rowID: rowID, at: first)
        }
    }

    /// Finds the clicked row again in the list as it is now.
    private func relocated(_ selection: TrackSelection, rowID: UInt64, at position: Int) async throws -> TrackSelection {
        guard let player else { throw ListChangedError() }
        var request = description
        request.source = source
        return try await player.relocated(selection, rowID: rowID, at: position, in: request)
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
