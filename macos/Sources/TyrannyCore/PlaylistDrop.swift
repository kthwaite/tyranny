import Foundation

/// What a drop of tracks does on the source list and inside a playlist (spec 4.8, 5.2).
public enum PlaylistDrop {
    /// A source-list row that accepts drops.
    public enum Target: Equatable, Sendable { case regularPlaylist, smartPlaylist, upNext }

    public enum RowAction: Equatable, Sendable {
        /// `playlist_add_selection`, appended.
        case addSelection
        /// `enqueue_tracks` `.append` with source `.drag`.
        case enqueueAppend
        case refuse
    }

    public static func onRow(_ target: Target) -> RowAction {
        switch target {
        case .regularPlaylist: .addSelection
        case .upNext: .enqueueAppend
        case .smartPlaylist: .refuse
        }
    }

    /// The most loaded IDs `playlist_add` takes in one command (the server's page limit).
    public static let maximumInsertedIDs = 500
    public static let appendedStatus = "Added at the end"

    public enum InsertionAction: Equatable, Sendable {
        /// `playlist_add` with `before`.
        case insert(libraryRevision: UInt64, trackIDs: [UInt64])
        /// `playlist_add_selection` at the end; the status bar says so.
        case append
    }

    /// A drop at an insertion point. Only a selection of loaded IDs can be placed.
    public static func atInsertion(_ selection: TrackSelection) -> InsertionAction {
        if case .ids(let revision, let ids) = selection, ids.count <= maximumInsertedIDs {
            return .insert(libraryRevision: revision, trackIDs: ids)
        }
        return .append
    }

    public enum Reorder: Equatable, Sendable {
        /// `playlist_move` before this entry (nil: to the end).
        case move(before: UInt64?)
        case noChange
        /// The entry the drop lands before is not loaded.
        case unavailable
    }

    /// Moves the rows at `dragged` before the row at `dropPosition` (`total` for the end).
    public static func reorder(dragged: IndexSet, dropPosition: Int, total: Int,
                               rowID: (Int) -> UInt64?) -> Reorder {
        guard let first = dragged.first, let last = dragged.last else { return .noChange }
        // Dropping on a row that moves is ambiguous: nothing happens.
        if dragged.contains(dropPosition) { return .noChange }
        // A contiguous block dropped on or next to itself keeps its place.
        if dragged.count == last - first + 1, (first...(last + 1)).contains(dropPosition) { return .noChange }
        if dropPosition >= total { return .move(before: nil) }
        guard let before = rowID(dropPosition) else { return .unavailable }
        return .move(before: before)
    }

    /// Playlist edits that get these codes refetch the playlist and retry once.
    public static func isStale(_ code: String) -> Bool {
        code == "stale_revision" || code == "stale_playlist_revision"
    }
}
