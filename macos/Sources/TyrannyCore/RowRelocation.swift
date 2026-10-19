import Foundation

/// A row the user acted on, found again after the library revision changed.
public enum RowRelocation: Equatable, Sendable {
    case found(Int)
    case gone

    /// `rowIDs` are the rows of a freshly fetched window that starts at list position `offset`.
    public static func locate(rowID: UInt64, rowIDs: [UInt64], offset: Int) -> RowRelocation {
        rowIDs.firstIndex(of: rowID).map { .found(offset + $0) } ?? .gone
    }

    /// The window to fetch around `position`: `size` rows, centred, never before the first row.
    public static func window(around position: Int, size: Int) -> (offset: Int, limit: Int) {
        (max(0, position - size / 2), size)
    }
}

public enum SelectionRetarget {
    /// The selection again under a fresh library revision, with its ranges moved by `delta`
    /// (where the first row went). Ranges that run to the end of the list keep their end.
    /// nil when the move cannot be applied: several ranges moved by different amounts.
    public static func apply(_ selection: TrackSelection, libraryRevision: UInt64, delta: Int) -> TrackSelection? {
        switch selection {
        case .ids(_, let trackIDs):
            return .ids(libraryRevision: libraryRevision, trackIDs: trackIDs)
        case .list(let list, _, let ranges, let excluded, let firstRowID):
            if delta != 0, ranges.count > 1 { return nil }
            let moved = ranges.map { range -> [UInt64] in
                range.enumerated().map { index, value in
                    if index == 1, value == UInt64.max { return value }
                    return UInt64(max(0, Int(clamping: value) + delta))
                }
            }
            return .list(list: list, libraryRevision: libraryRevision, ranges: moved, excluded: excluded,
                         firstRowID: firstRowID)
        }
    }
}
