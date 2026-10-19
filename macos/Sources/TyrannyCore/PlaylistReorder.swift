/// When a regular playlist's rows can be dragged into a new order (spec 4.3, 4.8).
/// Grouping and sorting change the display only, so the shown order is the playlist's
/// own order only when the list is ungrouped and unsorted.
public enum PlaylistReorder {
    public static let hint = "Set Group to None and clear sorting to reorder"

    public static func allowed(source: ListSource, grouping: Grouping, sort: [SortKey]) -> Bool {
        guard case .playlist = source else { return false }
        return grouping == .none && sort.isEmpty
    }
}
