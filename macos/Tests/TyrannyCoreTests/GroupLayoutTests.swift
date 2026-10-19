import Testing
@testable import TyrannyCore

private func span(_ artist: UInt64, _ album: UInt64?, _ start: UInt64, _ count: UInt64) -> GroupSpan {
    GroupSpan(artist_key: artist, album_key: album, start: start, count: count)
}

@Suite struct GroupLayoutTests {
    @Test func noneIsIdentity() {
        let layout = GroupLayout(grouping: .none, total: 5, spans: [])
        #expect(layout.count == 5)
        for i in 0..<5 {
            #expect(layout.row(at: i) == .track(position: i, blockIndex: nil))
            #expect(layout.displayIndex(ofPosition: i) == i)
            #expect(layout.albumKey(atDisplayIndex: i) == nil)
        }
    }

    @Test func artistGivesHeaderThenTracks() {
        let layout = GroupLayout(grouping: .artist, total: 5,
                                 spans: [span(1, nil, 0, 2), span(0, nil, 2, 3)])
        #expect(layout.count == 7)
        #expect(layout.row(at: 0) == .artistHeader(artistKey: 1, span: 0..<2))
        #expect(layout.row(at: 2) == .track(position: 1, blockIndex: nil))
        #expect(layout.row(at: 3) == .artistHeader(artistKey: 0, span: 2..<5))
        #expect(layout.displayIndex(ofPosition: 2) == 4)
    }

    @Test func albumBlocksPadToSevenRowsAndEndWithGap() {
        let layout = GroupLayout(grouping: .album, total: 10,
                                 spans: [span(1, 10, 0, 2), span(1, 11, 2, 8)])
        // 7 + gap, then 8 + gap
        #expect(layout.count == 8 + 9)
        #expect(layout.row(at: 1) == .track(position: 1, blockIndex: 1))
        #expect(layout.row(at: 2) == .blockFiller(albumKey: UInt64?.some(10), blockIndex: 2))
        #expect(layout.row(at: 6) == .blockFiller(albumKey: UInt64?.some(10), blockIndex: 6))
        #expect(layout.row(at: 7) == .blockGap)
        #expect(layout.row(at: 8) == .track(position: 2, blockIndex: 0))
        #expect(layout.row(at: 15) == .track(position: 9, blockIndex: 7))
        #expect(layout.row(at: 16) == .blockGap)
        #expect(layout.albumKey(atDisplayIndex: 5) == 10)
        #expect(layout.albumKey(atDisplayIndex: 12) == 11)
    }

    @Test func artistAlbumEmitsOneHeaderPerArtistRun() {
        let layout = GroupLayout(grouping: .artistAlbum, total: 9,
                                 spans: [span(1, 10, 0, 3), span(1, 11, 3, 4), span(0, 12, 7, 2)])
        #expect(layout.row(at: 0) == .artistHeader(artistKey: 1, span: 0..<7))
        #expect(layout.row(at: 1) == .track(position: 0, blockIndex: 0))
        // header + 8 + 8 = 17
        #expect(layout.row(at: 17) == .artistHeader(artistKey: 0, span: 7..<9))
        #expect(layout.count == 17 + 1 + 8)
        #expect(layout.albumKey(atDisplayIndex: 0) == nil)
        #expect(layout.albumKey(atDisplayIndex: 9) == 11)
        var headers = 0
        for i in 0..<layout.count { if case .artistHeader? = layout.row(at: i) { headers += 1 } }
        #expect(headers == 2)
    }

    @Test(arguments: [Grouping.artist, .album, .artistAlbum])
    func displayIndexRoundTripsForEveryPosition(grouping: Grouping) {
        var rng = SplitMix(seed: 0x5EED)
        var spans: [GroupSpan] = []
        var start: UInt64 = 0
        var artist: UInt64 = 0
        var album: UInt64 = 100
        while start < 1_000 {
            let n = min(UInt64.random(in: 1...12, using: &rng), 1_000 - start)
            if UInt64.random(in: 0..<3, using: &rng) == 0 { artist += 1 }
            album += 1
            spans.append(span(artist, grouping == .artist ? nil : album, start, n))
            start += n
        }
        if grouping == .artist {
            // artist grouping: one span per artist
            var merged: [GroupSpan] = []
            for s in spans {
                if let last = merged.last, last.artist_key == s.artist_key {
                    merged[merged.count - 1] = span(last.artist_key, nil, last.start, last.count + s.count)
                } else { merged.append(s) }
            }
            spans = merged
        }
        let layout = GroupLayout(grouping: grouping, total: 1_000, spans: spans)
        var last = -1
        for p in 0..<1_000 {
            guard let d = layout.displayIndex(ofPosition: p) else {
                Issue.record("no display index for \(p)"); return
            }
            #expect(d > last)
            last = d
            guard case .track(let back, _)? = layout.row(at: d) else {
                Issue.record("not a track at \(d)"); return
            }
            #expect(back == p)
        }
    }

    @Test(arguments: [Grouping.none, .artist, .album, .artistAlbum])
    func emptyLayoutIsSafe(grouping: Grouping) {
        let layout = GroupLayout(grouping: grouping, total: 0, spans: [])
        #expect(layout.count == 0)
        #expect(layout.row(at: 0) == nil)
        #expect(layout.displayIndex(ofPosition: 0) == nil)
        #expect(layout.albumKey(atDisplayIndex: 0) == nil)
    }

    @Test func outOfRangeLookupsReturnNil() {
        let layout = GroupLayout(grouping: .album, total: 2, spans: [span(1, 10, 0, 2)])
        #expect(layout.row(at: -1) == nil)
        #expect(layout.row(at: layout.count) == nil)
        #expect(layout.albumKey(atDisplayIndex: -1) == nil)
        #expect(layout.albumKey(atDisplayIndex: layout.count) == nil)
        #expect(layout.displayIndex(ofPosition: -1) == nil)
        #expect(layout.displayIndex(ofPosition: 2) == nil)
    }

    @Test func positionGapIsNil() {
        let layout = GroupLayout(grouping: .artist, total: 8,
                                 spans: [span(1, nil, 0, 3), span(2, nil, 5, 3)])
        #expect(layout.displayIndex(ofPosition: 2) != nil)
        #expect(layout.displayIndex(ofPosition: 3) == nil)
        #expect(layout.displayIndex(ofPosition: 4) == nil)
        #expect(layout.displayIndex(ofPosition: 5) != nil)
    }

    @Test func spansAreClampedToTotal() {
        let layout = GroupLayout(grouping: .artist, total: 5,
                                 spans: [span(1, nil, 0, 3), span(1, nil, 2, 2), span(1, nil, 3, 0),
                                         span(2, nil, 3, 10), span(3, nil, 5, 4), span(4, nil, 9, 1)])
        // header+3, header+2 (clamped); overlap, empty, at/after total dropped
        #expect(layout.count == 7)
        #expect(layout.row(at: 4) == .artistHeader(artistKey: 2, span: 3..<5))
        #expect(layout.displayIndex(ofPosition: 4) == 6)
        #expect(layout.displayIndex(ofPosition: 5) == nil)
        let album = GroupLayout(grouping: .album, total: 2, spans: [span(1, 10, 0, 50)])
        #expect(album.count == 8)
        #expect(album.row(at: 1) == .track(position: 1, blockIndex: 1))
    }

    @Test func nilAlbumKeyFillerStaysNil() {
        let layout = GroupLayout(grouping: .album, total: 1, spans: [span(1, nil, 0, 1)])
        #expect(layout.row(at: 1) == .blockFiller(albumKey: nil, blockIndex: 1))
        #expect(layout.albumKey(atDisplayIndex: 1) == nil)
    }

    @Test func mixedLayoutWalk() {
        // artist 1: album 10 (2 tracks), album 11 (8 tracks); artist 0: album 12 (1 track)
        let layout = GroupLayout(grouping: .artistAlbum, total: 11,
                                 spans: [span(1, 10, 0, 2), span(1, 11, 2, 8), span(0, 12, 10, 1)])
        var expected: [DisplayRow] = [.artistHeader(artistKey: 1, span: 0..<10)]
        for i in 0..<2 { expected.append(.track(position: i, blockIndex: i)) }
        for i in 2..<7 { expected.append(.blockFiller(albumKey: 10, blockIndex: i)) }
        expected.append(.blockGap)
        for i in 0..<8 { expected.append(.track(position: 2 + i, blockIndex: i)) }
        expected.append(.blockGap)
        expected.append(.artistHeader(artistKey: 0, span: 10..<11))
        expected.append(.track(position: 10, blockIndex: 0))
        for i in 1..<7 { expected.append(.blockFiller(albumKey: 12, blockIndex: i)) }
        expected.append(.blockGap)
        #expect(layout.count == expected.count)
        for (i, row) in expected.enumerated() { #expect(layout.row(at: i) == row, "row \(i)") }
    }

    @Test func layout_rebuild_keeps_anchor_track_position() {
        let before = GroupLayout(grouping: .artistAlbum, total: 6,
                                 spans: [span(1, 10, 0, 3), span(2, 20, 3, 3)])
        let anchorPosition = 4
        let oldDisplay = before.displayIndex(ofPosition: anchorPosition)!
        // New album (artist 1, 2 tracks) inserted before the anchor's album.
        let after = GroupLayout(grouping: .artistAlbum, total: 8,
                                spans: [span(1, 10, 0, 3), span(1, 15, 3, 2), span(2, 20, 5, 3)])
        let newPosition = anchorPosition + 2
        // header + 8 + 8 + header(artist 2) + offset 1 in album
        let expected = 1 + 8 + 8 + 1 + 1
        #expect(after.displayIndex(ofPosition: newPosition) == expected)
        #expect(after.displayIndex(ofPosition: newPosition)! > oldDisplay)
        #expect(after.row(at: expected) == .track(position: newPosition, blockIndex: 1))
    }
}

private struct SplitMix: RandomNumberGenerator {
    var state: UInt64
    init(seed: UInt64) { state = seed }
    mutating func next() -> UInt64 {
        state &+= 0x9E3779B97F4A7C15
        var z = state
        z = (z ^ (z >> 30)) &* 0xBF58476D1CE4E5B9
        z = (z ^ (z >> 27)) &* 0x94D049BB133111EB
        return z ^ (z >> 31)
    }
}
