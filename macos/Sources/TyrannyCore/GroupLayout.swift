import Foundation

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
    /// The gutter's 104 pt cover and its four facts lines end within eight 20 pt rows, so the
    /// gap row after a block stays clear.
    public static let minimumBlockRows = 8

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

    public let grouping: Grouping
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
        self.grouping = grouping
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

    /// The row's offset within its album block: tracks, fillers and the gap. nil outside
    /// album blocks.
    public func blockIndex(atDisplayIndex displayIndex: Int) -> Int? {
        guard displayIndex >= 0, displayIndex < count else { return nil }
        let block = blocks[blockIndex(containingDisplay: displayIndex)]
        guard case .album = block.kind else { return nil }
        return displayIndex - block.displayStart
    }

    /// The list positions of the track rows among `displayIndexes`. O(ranges · log n + blocks hit).
    public func positions(ofDisplayIndexes displayIndexes: IndexSet) -> IndexSet {
        var result = IndexSet()
        for range in displayIndexes.rangeView {
            let lower = max(range.lowerBound, 0), upper = min(range.upperBound, count)
            guard lower < upper else { continue }
            var b = blockIndex(containingDisplay: lower)
            while b < blocks.count, blocks[b].displayStart < upper {
                let block = blocks[b]
                let start = max(lower, block.displayStart)
                let end = min(upper, block.displayStart + block.trackCount)
                if start < end {
                    let shift = block.positionStart - block.displayStart
                    result.insert(integersIn: (start + shift)..<(end + shift))
                }
                b += 1
            }
        }
        return result
    }

    /// The display indices of the track rows at `positions`. Positions without a row are left out.
    public func displayIndexes(ofPositions positions: IndexSet) -> IndexSet {
        var result = IndexSet()
        for range in positions.rangeView {
            guard range.upperBound > 0 else { continue }
            var t = trackBlockIndex(containingPosition: max(range.lowerBound, 0))
            while t < trackBlocks.count, blocks[trackBlocks[t]].positionStart < range.upperBound {
                let block = blocks[trackBlocks[t]]
                let start = max(range.lowerBound, block.positionStart)
                let end = min(range.upperBound, block.positionStart + block.trackCount)
                if start < end {
                    let shift = block.displayStart - block.positionStart
                    result.insert(integersIn: (start + shift)..<(end + shift))
                }
                t += 1
            }
        }
        return result
    }

    /// The list positions that rows in `range` show: their tracks, and the first track of
    /// each artist header (the header names its artist from it). nil when there are none.
    public func positionRange(forDisplay range: Range<Int>) -> Range<Int>? {
        let lower = max(range.lowerBound, 0), upper = min(range.upperBound, count)
        guard lower < upper else { return nil }
        var low = Int.max, high = Int.min
        var b = blockIndex(containingDisplay: lower)
        while b < blocks.count, blocks[b].displayStart < upper {
            let block = blocks[b]
            if case .header(_, let span) = block.kind, !span.isEmpty {
                low = min(low, span.lowerBound)
                high = max(high, span.lowerBound + 1)
            }
            let start = max(lower, block.displayStart)
            let end = min(upper, block.displayStart + block.trackCount)
            if start < end {
                let shift = block.positionStart - block.displayStart
                low = min(low, start + shift)
                high = max(high, end + shift)
            }
            b += 1
        }
        return low < high ? low..<high : nil
    }

    /// The nearest track row at or after (`forward`) or at or before `displayIndex`.
    public func nearestTrack(from displayIndex: Int, forward: Bool) -> Int? {
        guard displayIndex >= 0, displayIndex < count else { return nil }
        let b = blockIndex(containingDisplay: displayIndex)
        let block = blocks[b]
        let offset = displayIndex - block.displayStart
        if offset < block.trackCount { return displayIndex }
        if !forward, block.trackCount > 0 { return block.displayStart + block.trackCount - 1 }
        // trackBlocks ascend in display order too: the first one after (or the last one before) b.
        var lo = 0, hi = trackBlocks.count
        while lo < hi {
            let mid = (lo + hi) / 2
            if trackBlocks[mid] <= b { lo = mid + 1 } else { hi = mid }
        }
        if forward {
            return lo < trackBlocks.count ? blocks[trackBlocks[lo]].displayStart : nil
        }
        // lo is the first track block after b; b itself has no tracks here.
        guard lo > 0 else { return nil }
        let previous = blocks[trackBlocks[lo - 1]]
        return previous.displayStart + previous.trackCount - 1
    }

    /// The rows to select for a proposed selection: track rows only. A keyboard proposal that
    /// adds only rows without tracks (an arrow or shift-arrow onto a header, filler or gap)
    /// goes on to the nearest track that way: an arrow moves there, a shift-arrow extends to it.
    /// With no track that way the selection stays. Clicks keep the track rows only.
    public func selection(proposed: IndexSet, current: IndexSet, keyboard: Bool) -> IndexSet {
        let tracks = displayIndexes(ofPositions: positions(ofDisplayIndexes: proposed))
        guard grouping != .none, keyboard else { return tracks }
        let added = proposed.subtracting(current)
        guard let first = added.first, let last = added.last,
              positions(ofDisplayIndexes: added).isEmpty else { return tracks }
        let forward = first > (current.last ?? -1)
        guard let target = nearestTrack(from: forward ? last : first, forward: forward) else {
            return proposed.count == 1 ? displayIndexes(ofPositions: positions(ofDisplayIndexes: current)) : tracks
        }
        if proposed.count == 1 { return IndexSet(integer: target) }
        var extended = tracks
        extended.insert(target)
        return extended
    }

    /// The index into `trackBlocks` of the last block starting at or before `position`, or 0.
    private func trackBlockIndex(containingPosition position: Int) -> Int {
        var lo = 0, hi = trackBlocks.count - 1
        guard hi > 0 else { return 0 }
        while lo < hi {
            let mid = (lo + hi + 1) / 2
            if blocks[trackBlocks[mid]].positionStart <= position { lo = mid } else { hi = mid - 1 }
        }
        return lo
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

/// Turns `list_groups` replies into layout spans.
///
/// Artist grouping asks the server for artist › album spans. Both groupings order artists by
/// their first row, so one artist's rows take the same positions in both, and merging an
/// artist's consecutive album spans gives the artist span. The album spans also give each
/// artist header its album count.
public enum GroupSpans {
    /// The grouping to send in `list_groups` for a list shown with `grouping`.
    public static func requestGrouping(for grouping: Grouping) -> Grouping {
        grouping == .artist ? .artistAlbum : grouping
    }

    /// The spans `GroupLayout` takes for `grouping`, from a reply to `requestGrouping(for:)`.
    public static func layoutSpans(grouping: Grouping, reply: [GroupSpan]) -> [GroupSpan] {
        grouping == .artist ? artistSpans(fromArtistAlbum: reply) : reply
    }

    /// Consecutive spans of one artist merged into one span with no album key.
    public static func artistSpans(fromArtistAlbum spans: [GroupSpan]) -> [GroupSpan] {
        var merged: [GroupSpan] = []
        for span in spans {
            if let last = merged.last, last.artist_key == span.artist_key, last.start + last.count == span.start {
                merged[merged.count - 1] = GroupSpan(artist_key: last.artist_key, album_key: nil,
                                                     start: last.start, count: last.count + span.count)
            } else {
                merged.append(GroupSpan(artist_key: span.artist_key, album_key: nil, start: span.start, count: span.count))
            }
        }
        return merged
    }

    /// Albums per artist key, from artist › album spans.
    public static func albumCounts(_ spans: [GroupSpan]) -> [UInt64: Int] {
        var counts: [UInt64: Int] = [:]
        for span in spans where span.album_key != nil { counts[span.artist_key, default: 0] += 1 }
        return counts
    }
}
