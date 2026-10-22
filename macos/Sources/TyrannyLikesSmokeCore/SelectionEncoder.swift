import Foundation

public enum UpNextPolicy: String, Sendable { case ask, replace, keep }

public enum RunDecision: Equatable, Sendable {
    case enqueue(Placement)
    case ask
}

public struct SelectionTooScattered: Error, LocalizedError, Equatable {
    public init() {}
    public var errorDescription: String? { "Selection is too scattered; select fewer separate ranges." }
}

public struct EmptySelection: Error, Equatable {
    public init() {}
}

public enum SelectionEncoder {
    static let maxIDs = 500
    static let maxRanges = 64
    static let maxExcluded = 500

    /// Play from `position` to the end of the list, starting at the clicked row.
    public static func run(list: ListDescription, revision: UInt64, from position: Int, rowID: UInt64) -> TrackSelection {
        .list(list: list, libraryRevision: revision, ranges: [[UInt64(max(position, 0)), UInt64.max]],
              excluded: [], firstRowID: rowID)
    }

    /// Play a whole source. `first_row_id` is nil so the server picks the start (shuffle pick under shuffle).
    public static func source(list: ListDescription, revision: UInt64) -> TrackSelection {
        .list(list: list, libraryRevision: revision, ranges: [[0, UInt64.max]], excluded: [], firstRowID: nil)
    }

    public static func selection(
        list: ListDescription, revision: UInt64, positions: IndexSet,
        rowID: (Int) -> UInt64?, trackID: (Int) -> UInt64?
    ) throws -> TrackSelection {
        let sorted = Array(positions)
        guard let first = sorted.first, let last = sorted.last else { throw EmptySelection() }

        if sorted.count <= maxIDs, !isPlaylist(list.source) {
            let ids = sorted.compactMap(trackID)
            if ids.count == sorted.count {
                return .ids(libraryRevision: revision, trackIDs: ids)
            }
        }

        let firstRowID = rowID(first)
        let runs = positions.rangeView
        if runs.count <= maxRanges {
            let ranges = runs.map { [UInt64($0.lowerBound), UInt64($0.upperBound)] }
            return .list(list: list, libraryRevision: revision, ranges: ranges, excluded: [], firstRowID: firstRowID)
        }

        // Too many runs: send the bounding range and exclude the gap rows.
        var excluded: [UInt64] = []
        var cursor = first
        for run in runs {
            for gap in cursor..<run.lowerBound {
                guard excluded.count < maxExcluded, let id = rowID(gap) else { throw SelectionTooScattered() }
                excluded.append(id)
            }
            cursor = run.upperBound
        }
        return .list(list: list, libraryRevision: revision, ranges: [[UInt64(first), UInt64(last + 1)]],
                     excluded: excluded, firstRowID: firstRowID)
    }

    /// The rows an activation sends. Double-click and Return (`asRun`) on one row play from that
    /// row to the end of the list. The context menu's Play and every other action send exactly the
    /// rows chosen.
    public static func activation(
        list: ListDescription, revision: UInt64, positions: IndexSet, asRun: Bool,
        rowID: (Int) -> UInt64?, trackID: (Int) -> UInt64?
    ) throws -> TrackSelection {
        guard let first = positions.first, let id = rowID(first) else { throw EmptySelection() }
        if asRun, positions.count == 1 {
            return run(list: list, revision: revision, from: first, rowID: id)
        }
        return try selection(list: list, revision: revision, positions: positions, rowID: rowID, trackID: trackID)
    }

    public static func decide(policy: UpNextPolicy, hasUpcoming: Bool) -> RunDecision {
        guard hasUpcoming else { return .enqueue(.replaceUpcoming) }
        switch policy {
        case .replace: return .enqueue(.replaceUpcoming)
        case .keep: return .enqueue(.next)
        case .ask: return .ask
        }
    }

    private static func isPlaylist(_ source: ListSource) -> Bool {
        if case .playlist = source { return true }
        return false
    }
}
