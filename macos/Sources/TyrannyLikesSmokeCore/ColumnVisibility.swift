/// Which track-table columns are shown (spec 4.2). A column in the hidden set is hidden unless
/// the list sorts by it and the user has never chosen its visibility: Recently Added sorts by
/// Date Added, which is hidden by default, so the sort arrow needs the column. An explicit
/// choice always wins.
public enum ColumnVisibility {
    public static func isShown(_ id: String, hidden: Set<String>, explicit: Set<String>, sortedBy: Bool) -> Bool {
        guard hidden.contains(id) else { return true }
        return sortedBy && !explicit.contains(id)
    }
}
