/// One row of a grouped track table, in display order.
public enum DisplayRow: Equatable, Sendable {
    /// `span` = list positions in this artist group.
    case artistHeader(artistKey: UInt64, span: Range<Int>)
    /// `blockIndex` = row index within its album block (0-based), nil outside album blocks.
    case track(position: Int, blockIndex: Int?)
    /// Pads a short album to the cover's height.
    case blockFiller(albumKey: UInt64?, blockIndex: Int)
    case blockGap
}

/// Maps list positions to display rows and back, from server group spans.
/// Memory is O(groups). Lookups are O(log n) binary searches over prefix sums.
public struct GroupLayout: Sendable {
    /// 104 pt cover + facts at 20 pt rows.
    public static let minimumBlockRows = 7

    private enum Kind: Sendable {
        case header(artistKey: UInt64, span: Range<Int>)
        /// Tracks only, no cover (artist grouping).
        case plain(albumKey: UInt64?)
        /// Tracks, fillers, then one gap.
        case album(albumKey: UInt64?)
    }

    private struct Block: Sendable {
        let displayStart: Int
        let positionStart: Int
        let trackCount: Int
        let rowCount: Int
        let kind: Kind
    }

    private var blocks: [Block] = []
    /// Indices into `blocks` of blocks holding tracks, ascending by `positionStart`.
    private var trackBlocks: [Int] = []
    public private(set) var count: Int = 0

    /// Spans are clamped to `total`: zero-count, overlapping and non-ascending spans are
    /// dropped, and portions at or after `total` are cut. Gaps between spans stay uncovered.
    private static func sanitized(_ spans: [GroupSpan], total: Int) -> [GroupSpan] {
        var result: [GroupSpan] = []
        var cursor = 0
        for span in spans {
            guard span.start < UInt64(max(total, 0)) else { continue }
            let start = Int(span.start)
            guard start >= cursor else { continue }
            let n = Int(min(span.count, UInt64(total - start)))
            guard n > 0 else { continue }
            result.append(GroupSpan(artist_key: span.artist_key, album_key: span.album_key,
                                    start: span.start, count: UInt64(n)))
            cursor = start + n
        }
        return result
    }

    public init(grouping: Grouping, total: Int, spans rawSpans: [GroupSpan]) {
        let spans = grouping == .none ? [] : Self.sanitized(rawSpans, total: total)
        var display = 0
        func append(_ position: Int, _ tracks: Int, _ rows: Int, _ kind: Kind) {
            if tracks > 0 { trackBlocks.append(blocks.count) }
            blocks.append(Block(displayStart: display, positionStart: position,
                                trackCount: tracks, rowCount: rows, kind: kind))
            display += rows
        }

        switch grouping {
        case .none:
            if total > 0 { append(0, total, total, .plain(albumKey: nil)) }
        case .artist:
            for span in spans {
                let start = Int(span.start), n = Int(span.count)
                append(start, 0, 1, .header(artistKey: span.artist_key, span: start..<(start + n)))
                if n > 0 { append(start, n, n, .plain(albumKey: span.album_key)) }
            }
        case .album:
            for span in spans { Self.appendAlbum(span, append) }
        case .artistAlbum:
            var i = 0
            while i < spans.count {
                let key = spans[i].artist_key
                var j = i
                while j < spans.count, spans[j].artist_key == key {
                    j += 1
                }
                let start = Int(spans[i].start)
                let end = Int(spans[j - 1].start + spans[j - 1].count)
                append(start, 0, 1, .header(artistKey: key, span: start..<end))
                for k in i..<j { Self.appendAlbum(spans[k], append) }
                i = j
            }
        }
        count = display
    }

    private static func appendAlbum(_ span: GroupSpan, _ append: (Int, Int, Int, Kind) -> Void) {
        let n = Int(span.count)
        append(Int(span.start), n, max(n, minimumBlockRows) + 1, .album(albumKey: span.album_key))
    }

    public func row(at displayIndex: Int) -> DisplayRow? {
        guard displayIndex >= 0, displayIndex < count else { return nil }
        let block = blocks[blockIndex(containingDisplay: displayIndex)]
        let offset = displayIndex - block.displayStart
        switch block.kind {
        case .header(let artistKey, let span):
            return .artistHeader(artistKey: artistKey, span: span)
        case .plain:
            return .track(position: block.positionStart + offset, blockIndex: nil)
        case .album(let albumKey):
            if offset < block.trackCount {
                return .track(position: block.positionStart + offset, blockIndex: offset)
            }
            return offset == block.rowCount - 1
                ? .blockGap
                : .blockFiller(albumKey: albumKey, blockIndex: offset)
        }
    }

    public func displayIndex(ofPosition position: Int) -> Int? {
        var lo = 0, hi = trackBlocks.count - 1
        guard hi >= 0, position >= 0 else { return nil }
        while lo < hi {
            let mid = (lo + hi + 1) / 2
            if blocks[trackBlocks[mid]].positionStart <= position { lo = mid } else { hi = mid - 1 }
        }
        let block = blocks[trackBlocks[lo]]
        let offset = position - block.positionStart
        guard offset >= 0, offset < block.trackCount else { return nil }
        return block.displayStart + offset
    }

    public func albumKey(atDisplayIndex displayIndex: Int) -> UInt64? {
        guard displayIndex >= 0, displayIndex < count else { return nil }
        switch blocks[blockIndex(containingDisplay: displayIndex)].kind {
        case .album(let key): return key
        case .plain, .header: return nil
        }
    }

    private func blockIndex(containingDisplay displayIndex: Int) -> Int {
        var lo = 0, hi = blocks.count - 1
        while lo < hi {
            let mid = (lo + hi + 1) / 2
            if blocks[mid].displayStart <= displayIndex { lo = mid } else { hi = mid - 1 }
        }
        return lo
    }
}
