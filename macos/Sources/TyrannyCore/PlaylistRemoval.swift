import Foundation

/// Removing entries from a regular playlist (spec 4.8): Delete, ⌘Delete and the context menu.
/// The server takes playlist entry IDs (`playlist_remove`), 1 to 500, distinct.
public enum PlaylistRemoval {
    public static let maximumEntries = 500
    public static let notLoadedStatus = "Wait for the playlist to load"
    public static let tooManyStatus = "Select 500 or fewer rows to remove."

    public enum Plan: Equatable, Sendable {
        /// Nothing is selected: do nothing.
        case nothing
        /// Some selected row has not loaded: no partial removal.
        case notLoaded
        case tooMany
        case remove(entryIDs: [UInt64])
    }

    /// The entry IDs of the selected rows, in list order, when all are loaded.
    public static func plan(positions: IndexSet, entryID: (Int) -> UInt64?) -> Plan {
        guard !positions.isEmpty else { return .nothing }
        guard positions.count <= maximumEntries else { return .tooMany }
        let ids = positions.compactMap(entryID)
        guard ids.count == positions.count else { return .notLoaded }
        return .remove(entryIDs: ids)
    }

    /// Delete (51) and forward delete (117), alone or with ⌘. Any other modifier is not removal.
    public static func isRemoveKey(keyCode: UInt16, otherModifiers: Bool) -> Bool {
        (keyCode == 51 || keyCode == 117) && !otherModifiers
    }

    public static func status(count: Int, playlist: String) -> String {
        "Removed \(DisplayFormat.count(count)) from “\(playlist)”"
    }
}
