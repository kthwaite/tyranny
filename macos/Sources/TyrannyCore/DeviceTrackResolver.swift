import Foundation

/// Resolve the same revision-pinned selection as playback, without mutating the queue.
/// All rows are resolved before the first USB write, so stale or unavailable rows fail atomically.
public enum DeviceTrackResolver {
    public static func paths(
        selection: TrackSelection,
        isolation: isolated (any Actor)? = #isolation,
        fetch: (ListDescription, UInt64, Int, Int) async throws -> ListTracksResponse
    ) async throws -> [String] {
        switch selection {
        case .ids(let revision, let ids):
            var remaining = Set(ids)
            var found: [UInt64: String] = [:]
            var offset = 0
            while !remaining.isEmpty {
                let page = try await fetch(ListDescription(source: .library), revision, offset, 500)
                try validate(page, revision: revision, offset: offset)
                for row in page.entries where remaining.contains(row.track.id) {
                    try available(row)
                    found[row.track.id] = row.track.path
                    remaining.remove(row.track.id)
                }
                offset += page.entries.count
                if UInt64(offset) >= page.total { break }
                guard !page.entries.isEmpty else { throw DeviceSyncFailure("The library returned an incomplete page. Nothing was copied.") }
            }
            guard remaining.isEmpty else { throw DeviceSyncFailure("Some selected tracks disappeared. Start a new drag.") }
            var seen = Set<String>()
            return ids.compactMap { found[$0] }.filter { seen.insert($0).inserted }
        case .list(let list, let revision, let ranges, let excluded, let firstRowID):
            let excluded = Set(excluded)
            var paths: [String] = []
            var seen = Set<String>()
            for (index, range) in ranges.enumerated() {
                guard range.count == 2, range[0] < range[1], range[0] <= UInt64(Int.max) else {
                    throw DeviceSyncFailure("Invalid track selection. Start a new drag.")
                }
                var offset = Int(range[0])
                var end = range[1]
                var first = true
                while UInt64(offset) < end {
                    let limit = Int(min(500, end - UInt64(offset)))
                    let page = try await fetch(list, revision, offset, limit)
                    try validate(page, revision: revision, offset: offset)
                    end = min(end, page.total)
                    if first, index == 0, let firstRowID, page.entries.first?.row_id != firstRowID {
                        throw DeviceSyncFailure("The selected rows moved. Start a new drag.")
                    }
                    first = false
                    for row in page.entries.prefix(Int(min(UInt64(page.entries.count), end > UInt64(offset) ? end - UInt64(offset) : 0))) {
                        guard !excluded.contains(row.row_id) else { continue }
                        try available(row)
                        if seen.insert(row.track.path).inserted { paths.append(row.track.path) }
                    }
                    offset += page.entries.count
                    if UInt64(offset) >= end { break }
                    guard !page.entries.isEmpty else { throw DeviceSyncFailure("The library returned an incomplete page. Nothing was copied.") }
                }
            }
            return paths
        }
    }

    private static func validate(_ page: ListTracksResponse, revision: UInt64, offset: Int) throws {
        guard page.revision == revision, page.offset == offset else {
            throw DeviceSyncFailure("The library changed during this drag. Start a new drag; nothing was copied.")
        }
    }

    private static func available(_ row: ListRow) throws {
        guard row.available, row.track.path.hasPrefix("/") else {
            throw DeviceSyncFailure("“\(row.track.title)” is not an available local file. Nothing was copied.")
        }
    }
}
