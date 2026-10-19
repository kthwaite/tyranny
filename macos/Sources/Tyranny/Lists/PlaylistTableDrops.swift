import AppKit
import TyrannyCore

/// Drops inside a regular playlist's table (spec 4.3, 4.8). Rows of the table itself are
/// reordered, with `playlist_move`, when the list is ungrouped and unsorted. Tracks from
/// another list are inserted at the insertion point with `playlist_add`; a selection that
/// cannot be placed is appended and the status bar says "Added at the end".
@MainActor
extension ListModel {
    enum DropProposal: Equatable {
        case refuse
        /// An insertion line before this row.
        case row(Int)
        /// The whole table is the target: the list has no insertion points.
        case table
    }

    /// The table's own drag started on `positions`.
    func dragBegan(positions: IndexSet) {
        guard case .playlist = source else { return }
        ownDrag = (positions, selectionBasis)
    }

    func dragEnded() { ownDrag = nil }

    /// The rows' track IDs in order, when there are at most 500 and all are loaded.
    func loadedTrackIDs(_ positions: IndexSet) -> [UInt64]? {
        guard positions.count <= PlaylistDrop.maximumInsertedIDs else { return nil }
        let ids = positions.compactMap { cache.loadedRow(at: $0)?.track.id }
        return ids.count == positions.count ? ids : nil
    }

    private var playlist: Playlist? {
        guard case .playlist(let id, _) = source else { return nil }
        return player?.savedPlaylists.playlist(id: id)
    }

    func proposeDrop(token: String, own: Bool, row: Int) -> DropProposal {
        guard let player, let playlist, !playlist.isSmart,
              player.validateTrackDrop(token: token, destination: .playlist(playlist.id)) else { return .refuse }
        let at = min(max(row, 0), cache.total)
        if own {
            // Grouped or sorted, the shown order is not the playlist's.
            guard allowsReorder else { showReorderHint(); return .refuse }
            return ownDrag == nil ? .refuse : .row(at)
        }
        return allowsReorder ? .row(at) : .table
    }

    func acceptDrop(token: String, own: Bool, row: Int) -> Bool {
        guard let player, let playlist, proposeDrop(token: token, own: own, row: row) != .refuse,
              let drag = player.activeDrag, drag.token == token, let selection = drag.selection else { return false }
        let at = min(max(row, 0), cache.total)
        return own ? reorder(playlist, at: at, player: player)
                   : insert(playlist, selection: selection, trackIDs: drag.trackIDs, count: drag.count,
                         at: allowsReorder ? at : nil, player: player)
    }

    private func reorder(_ playlist: Playlist, at row: Int, player: PlayerModel) -> Bool {
        guard let (positions, basis) = ownDrag, basis == selectionBasis else {
            player.message = "The playlist changed during this drag. Start a new drag."
            player.activeDrag = nil
            return false
        }
        guard positions.count <= PlaylistDrop.maximumInsertedIDs else {
            player.message = "Select 500 or fewer rows to reorder."
            player.activeDrag = nil
            return false
        }
        let ids = positions.compactMap { cache.loadedRow(at: $0)?.row_id }
        guard ids.count == positions.count else {
            player.message = "Wait for the playlist to load, then drag again."
            player.activeDrag = nil
            return false
        }
        let plan = PlaylistDrop.reorder(dragged: positions, dropPosition: row, total: cache.total,
                                        rowID: { [cache] in cache.loadedRow(at: $0)?.row_id })
        switch plan {
        case .noChange:
            player.activeDrag = nil
            return true
        case .unavailable:
            player.message = "Wait for the rows to load, then drop again."
            return false
        case .move(let before):
            player.activeDrag = nil
            player.mutate { [player] in
                try await player.savedPlaylists.move(in: playlist, entryIDs: ids, before: before)
            }
            return true
        }
    }

    private func insert(_ playlist: Playlist, selection: TrackSelection, trackIDs: [UInt64]?, count: Int, at row: Int?,
                        player: PlayerModel) -> Bool {
        var before: UInt64?
        if let row, row < cache.total {
            guard let id = cache.loadedRow(at: row)?.row_id else {
                player.message = "Wait for the rows to load, then drop again."
                return false
            }
            before = id
        }
        player.activeDrag = nil
        player.mutate { [player, before] in
            try await player.savedPlaylists.addAtInsertion(
                to: playlist, selection: selection, trackIDs: trackIDs, count: count, before: before, atEnd: row == nil)
        }
        return true
    }

    // MARK: Remove

    /// A regular playlist's list: Delete, ⌘Delete and "Remove from Playlist" apply.
    var canRemoveEntries: Bool {
        guard case .playlist = source, let playlist, !playlist.isSmart else { return false }
        return true
    }

    /// Removes the selected entries, by entry ID, under any sort, grouping or filter. Only when
    /// every selected row is loaded.
    func removeSelected() {
        guard canRemoveEntries, let player, let playlist else { return }
        switch PlaylistRemoval.plan(positions: selectedPositions, entryID: { [cache] in cache.loadedRow(at: $0)?.row_id }) {
        case .nothing:
            return
        case .notLoaded:
            player.message = PlaylistRemoval.notLoadedStatus
        case .tooMany:
            player.message = PlaylistRemoval.tooManyStatus
        case .remove(let ids):
            guard player.canMutate else { return }
            player.mutate { [weak self, player] in
                try await player.savedPlaylists.remove(in: playlist, entryIDs: ids)
                self?.userSelected(IndexSet())
            }
        }
    }
}
