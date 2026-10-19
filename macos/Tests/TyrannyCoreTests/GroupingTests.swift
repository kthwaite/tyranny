import Foundation
import Testing
@testable import TyrannyCore

private func span(_ artist: UInt64, _ album: UInt64?, _ start: UInt64, _ count: UInt64) -> GroupSpan {
    GroupSpan(artist_key: artist, album_key: album, start: start, count: count)
}

/// artist 1: album 10 (2 tracks), album 11 (8 tracks); artist 0: album 12 (1 track).
/// Display: 0 header | 1–2 tracks 0–1 | 3–8 fillers | 9 gap | 10–17 tracks 2–9 | 18 gap
/// | 19 header | 20 track 10 | 21–27 fillers | 28 gap.
private let mixed = GroupLayout(grouping: .artistAlbum, total: 11,
                                spans: [span(1, 10, 0, 2), span(1, 11, 2, 8), span(0, 12, 10, 1)])

@Suite struct GroupLayoutSetTests {
    @Test func albumBlocksLeaveTheGapRowBelowTheGutterContent() {
        // Cover 104 pt plus four facts lines need eight 20 pt rows; the gap is never drawn on.
        #expect(GroupLayout.minimumBlockRows == 8)
        #expect(mixed.count == 29)
        #expect(mixed.row(at: 9) == .blockGap)
        #expect(mixed.blockIndex(atDisplayIndex: 9) == 8)
    }

    @Test func positionsOfDisplayIndexesSkipHeadersFillersAndGaps() {
        #expect(mixed.positions(ofDisplayIndexes: IndexSet(0..<mixed.count)) == IndexSet(0..<11))
        #expect(mixed.positions(ofDisplayIndexes: IndexSet([0, 3, 9, 18, 19, 21, 28])) == IndexSet())
        #expect(mixed.positions(ofDisplayIndexes: IndexSet([2, 10, 20])) == IndexSet([1, 2, 10]))
        #expect(mixed.positions(ofDisplayIndexes: IndexSet(5..<13)) == IndexSet(2..<5))
        #expect(mixed.positions(ofDisplayIndexes: IndexSet([-1, 400])) == IndexSet())
    }

    @Test func displayIndexesOfPositionsRoundTrip() {
        #expect(mixed.displayIndexes(ofPositions: IndexSet([0, 1, 2, 10])) == IndexSet([1, 2, 10, 20]))
        #expect(mixed.displayIndexes(ofPositions: IndexSet(0..<11))
            == IndexSet(integersIn: 1...2).union(IndexSet(integersIn: 10...17)).union(IndexSet([20])))
        #expect(mixed.displayIndexes(ofPositions: IndexSet([11, 50])) == IndexSet())
        let none = GroupLayout(grouping: .none, total: 50_000, spans: [])
        #expect(none.displayIndexes(ofPositions: IndexSet(0..<50_000)) == IndexSet(0..<50_000))
        #expect(none.positions(ofDisplayIndexes: IndexSet(10..<20)) == IndexSet(10..<20))
    }

    @Test func positionRangeCoversTracksAndHeaderFirstRows() {
        #expect(mixed.positionRange(forDisplay: 0..<1) == 0..<1)
        #expect(mixed.positionRange(forDisplay: 3..<9) == nil)
        #expect(mixed.positionRange(forDisplay: 5..<13) == 2..<5)
        #expect(mixed.positionRange(forDisplay: 18..<20) == 10..<11)
        #expect(mixed.positionRange(forDisplay: 0..<mixed.count) == 0..<11)
        #expect(mixed.positionRange(forDisplay: 21..<28) == nil)
        #expect(mixed.positionRange(forDisplay: 30..<40) == nil)
    }

    @Test func blockIndexCoversTracksFillersAndGap() {
        #expect(mixed.blockIndex(atDisplayIndex: 0) == nil)
        #expect(mixed.blockIndex(atDisplayIndex: 1) == 0)
        #expect(mixed.blockIndex(atDisplayIndex: 7) == 6)
        #expect(mixed.blockIndex(atDisplayIndex: 18) == 8)
        #expect(mixed.blockIndex(atDisplayIndex: 28) == 8)
        #expect(mixed.blockIndex(atDisplayIndex: 29) == nil)
        let artist = GroupLayout(grouping: .artist, total: 2, spans: [span(1, nil, 0, 2)])
        #expect(artist.blockIndex(atDisplayIndex: 1) == nil)
    }

    @Test func nearestTrackSkipsRowsWithoutTracks() {
        #expect(mixed.nearestTrack(from: 3, forward: true) == 10)
        #expect(mixed.nearestTrack(from: 3, forward: false) == 2)
        #expect(mixed.nearestTrack(from: 0, forward: true) == 1)
        #expect(mixed.nearestTrack(from: 0, forward: false) == nil)
        #expect(mixed.nearestTrack(from: 19, forward: false) == 17)
        #expect(mixed.nearestTrack(from: 21, forward: true) == nil)
        #expect(mixed.nearestTrack(from: 10, forward: true) == 10)
    }

    @Test func clicksOnRowsWithoutTracksSelectNothing() {
        #expect(mixed.selection(proposed: IndexSet([3]), current: IndexSet([1]), keyboard: false) == IndexSet())
        #expect(mixed.selection(proposed: IndexSet(0..<29), current: IndexSet(), keyboard: false)
            == mixed.displayIndexes(ofPositions: IndexSet(0..<11)))
        #expect(mixed.selection(proposed: IndexSet(1...12), current: IndexSet([1]), keyboard: false)
            == IndexSet([1, 2, 10, 11, 12]))
    }

    @Test func arrowKeysMoveOnToTheNextTrack() {
        #expect(mixed.selection(proposed: IndexSet([3]), current: IndexSet([2]), keyboard: true) == IndexSet([10]))
        #expect(mixed.selection(proposed: IndexSet([9]), current: IndexSet([10]), keyboard: true) == IndexSet([2]))
        #expect(mixed.selection(proposed: IndexSet([18]), current: IndexSet([17]), keyboard: true) == IndexSet([20]))
        #expect(mixed.selection(proposed: IndexSet([0]), current: IndexSet([1]), keyboard: true) == IndexSet([1]))
        #expect(mixed.selection(proposed: IndexSet([21]), current: IndexSet([20]), keyboard: true) == IndexSet([20]))
    }

    @Test func shiftArrowExtendsAcrossFillersGapsAndHeaders() {
        // Down from the end of a block: past fillers and the gap into the next block.
        #expect(mixed.selection(proposed: IndexSet([1, 2, 3]), current: IndexSet([1, 2]), keyboard: true)
            == IndexSet([1, 2, 10]))
        // Down past a gap and an artist header.
        #expect(mixed.selection(proposed: IndexSet([16, 17, 18]), current: IndexSet([16, 17]), keyboard: true)
            == IndexSet([16, 17, 20]))
        // Up past the gap and fillers of the previous block.
        #expect(mixed.selection(proposed: IndexSet([9, 10, 11]), current: IndexSet([10, 11]), keyboard: true)
            == IndexSet([2, 10, 11]))
        // Nothing further that way: the selection stays.
        #expect(mixed.selection(proposed: IndexSet([20, 21]), current: IndexSet([20]), keyboard: true)
            == IndexSet([20]))
        // Shrinking drops rows as proposed.
        #expect(mixed.selection(proposed: IndexSet([1, 2]), current: IndexSet([1, 2, 10]), keyboard: true)
            == IndexSet([1, 2]))
        // Select All by keyboard keeps the tracks only.
        #expect(mixed.selection(proposed: IndexSet(0..<29), current: IndexSet(), keyboard: true)
            == mixed.displayIndexes(ofPositions: IndexSet(0..<11)))
        let none = GroupLayout(grouping: .none, total: 5, spans: [])
        #expect(none.selection(proposed: IndexSet([1, 2]), current: IndexSet([1]), keyboard: true) == IndexSet([1, 2]))
    }
}

@Suite struct GroupSpansTests {
    @Test func artistSpansMergeConsecutiveAlbumsOfOneArtist() {
        let spans = [span(1, 10, 0, 2), span(1, 11, 2, 8), span(0, 12, 10, 1), span(3, 13, 11, 4)]
        #expect(GroupSpans.artistSpans(fromArtistAlbum: spans)
            == [span(1, nil, 0, 10), span(0, nil, 10, 1), span(3, nil, 11, 4)])
        #expect(GroupSpans.albumCounts(spans) == [1: 2, 0: 1, 3: 1])
    }

    @Test func artistGroupingAsksForArtistAlbumSpans() {
        #expect(GroupSpans.requestGrouping(for: .artist) == .artistAlbum)
        #expect(GroupSpans.requestGrouping(for: .album) == .album)
        #expect(GroupSpans.requestGrouping(for: .artistAlbum) == .artistAlbum)
        let merged = GroupSpans.layoutSpans(grouping: .artist, reply: [span(1, 10, 0, 2), span(1, 11, 2, 3)])
        #expect(merged == [span(1, nil, 0, 5)])
        let kept = [span(1, 10, 0, 2), span(1, 11, 2, 3)]
        #expect(GroupSpans.layoutSpans(grouping: .artistAlbum, reply: kept) == kept)
    }
}

@Suite struct AlbumFactsFormatTests {
    private func album(year: Int32?, genre: String, tracks: UInt64, ms: UInt64, format: String) -> AlbumSummary {
        AlbumSummary(id: 1, title: "Kind of Blue", album_artist: "Miles Davis", album_artist_id: 2, compilation: false,
                     year: year, genre: genre, track_count: tracks, disc_count: 1, duration_ms: ms, format: format,
                     album_gain_db: nil, date_added: 0, musicbrainz_release_id: nil)
    }

    @Test func factsLinesFollowTheMockup() {
        #expect(DisplayFormat.albumFacts(album(year: 1959, genre: "Jazz", tracks: 5, ms: 2_744_000, format: "FLAC 16/44.1"))
            == ["Kind of Blue", "1959 · Jazz", "5 tracks · 45:44", "FLAC 16/44.1"])
        #expect(DisplayFormat.albumFacts(album(year: nil, genre: "", tracks: 1, ms: 3_725_000, format: ""))
            == ["Kind of Blue", "1 track · 1:02:05"])
        #expect(DisplayFormat.albumFacts(album(year: nil, genre: "Folk", tracks: 2, ms: 1_000, format: "MP3 320"))
            == ["Kind of Blue", "Folk", "2 tracks · 0:01", "MP3 320"])
    }

    @Test func artistHeaderCountsAlbumsAndTracks() {
        #expect(DisplayFormat.artistHeaderCounts(albums: 1, tracks: 5) == "1 album · 5 tracks")
        #expect(DisplayFormat.artistHeaderCounts(albums: 12, tracks: 1_129) == "12 albums · 1,129 tracks")
        #expect(DisplayFormat.artistHeaderCounts(albums: nil, tracks: 1) == "1 track")
    }
}

@Suite struct PlaylistReorderTests {
    @Test func reorderNeedsARegularPlaylistUngroupedAndUnsorted() {
        let playlist = ListSource.playlist(id: 4, revision: 9)
        #expect(PlaylistReorder.allowed(source: playlist, grouping: .none, sort: []))
        #expect(!PlaylistReorder.allowed(source: playlist, grouping: .album, sort: []))
        #expect(!PlaylistReorder.allowed(source: playlist, grouping: .none, sort: [SortKey(field: .title)]))
        #expect(!PlaylistReorder.allowed(source: .smart(id: 4, revision: 9), grouping: .none, sort: []))
        #expect(!PlaylistReorder.allowed(source: .library, grouping: .none, sort: []))
        #expect(PlaylistReorder.hint == "Set Group to None and clear sorting to reorder")
    }
}

/// Album summaries served from a dictionary. Requests wait until the test completes them.
@MainActor
private final class FakeSummaries {
    var albums: [UInt64: String]
    private(set) var log: [[UInt64]] = []
    private var pending: [(ids: [UInt64], continuation: CheckedContinuation<[AlbumSummary], any Error>)] = []

    init(_ albums: [UInt64: String]) { self.albums = albums }

    var fetch: AlbumFactsCache.Fetch { { ids in try await self.serve(ids) } }
    var pendingCount: Int { pending.count }

    private func serve(_ ids: [UInt64]) async throws -> [AlbumSummary] {
        log.append(ids)
        return try await withCheckedThrowingContinuation { pending.append((ids, $0)) }
    }

    func completeAll() {
        let batch = pending
        pending.removeAll()
        for item in batch {
            item.continuation.resume(returning: item.ids.compactMap { id in albums[id].map { summary(id, $0) } })
        }
    }

    func failAll() {
        let batch = pending
        pending.removeAll()
        for item in batch { item.continuation.resume(throwing: WindowFetchError.failed("test")) }
    }

    func summary(_ id: UInt64, _ title: String) -> AlbumSummary {
        AlbumSummary(id: id, title: title, album_artist: "", album_artist_id: 1, compilation: false, year: nil,
                     genre: "", track_count: 1, disc_count: 1, duration_ms: 0, format: "", album_gain_db: nil,
                     date_added: 0, musicbrainz_release_id: nil)
    }
}

@MainActor
private func drain() async {
    for _ in 0..<200 { await Task.yield() }
}

@MainActor @Suite struct AlbumFactsCacheTests {
    @Test func wantRequestsOnlyMissingAlbumsInBatchesOfFiveHundred() async {
        let server = FakeSummaries(Dictionary(uniqueKeysWithValues: (1...700).map { (UInt64($0), "A\($0)") }))
        let cache = AlbumFactsCache(fetch: server.fetch)
        cache.want(Set((1...700).map(UInt64.init)))
        await drain()
        #expect(server.log.map(\.count).sorted() == [200, 500])
        #expect(Set(server.log.flatMap { $0 }) == Set((1...700).map(UInt64.init)))
        server.completeAll()
        await drain()
        #expect(cache.summary(1)?.title == "A1")
        #expect(cache.summary(700)?.title == "A700")
        cache.want([1, 2, 3])
        await drain()
        #expect(server.log.count == 2)
    }

    @Test func inFlightAlbumsAreNotRequestedTwice() async {
        let server = FakeSummaries([1: "One", 2: "Two"])
        let cache = AlbumFactsCache(fetch: server.fetch)
        cache.want([1])
        cache.want([1, 2])
        await drain()
        #expect(server.log == [[1], [2]])
        let before = cache.generation
        server.completeAll()
        await drain()
        #expect(cache.summary(2)?.title == "Two")
        #expect(cache.generation != before)
    }

    @Test func albumsTheServerDoesNotKnowAreNotRetried() async {
        let server = FakeSummaries([1: "One"])
        let cache = AlbumFactsCache(fetch: server.fetch)
        cache.want([1, 9])
        await drain()
        server.completeAll()
        await drain()
        cache.want([1, 9])
        await drain()
        #expect(server.log == [[1, 9]])
        #expect(cache.summary(9) == nil)
    }

    @Test func invalidateKeepsShownFactsAndRefetchesWantedAlbums() async {
        let server = FakeSummaries([1: "One", 2: "Two"])
        let cache = AlbumFactsCache(fetch: server.fetch)
        cache.want([1, 2])
        await drain()
        server.completeAll()
        await drain()
        server.albums[1] = "One (Remaster)"
        cache.want([1])
        cache.invalidate()
        #expect(cache.summary(1)?.title == "One")
        await drain()
        #expect(server.log == [[1, 2], [1]])
        server.completeAll()
        await drain()
        #expect(cache.summary(1)?.title == "One (Remaster)")
        // Album 2 is out of view: it is refetched when wanted again.
        cache.want([2])
        await drain()
        #expect(server.log.last == [2])
    }

    @Test func invalidateDuringAFetchRefetchesAfterIt() async {
        let server = FakeSummaries([1: "One"])
        let cache = AlbumFactsCache(fetch: server.fetch)
        cache.want([1])
        await drain()
        server.albums[1] = "One v2"
        cache.invalidate()
        await drain()
        #expect(server.log == [[1]])
        server.completeAll()
        await drain()
        #expect(server.log == [[1], [1]])
        server.completeAll()
        await drain()
        #expect(cache.summary(1)?.title == "One v2")
    }

    @Test func failedAlbumsWaitUntilTheyComeIntoViewAgainOrInvalidate() async {
        let server = FakeSummaries([1: "One", 2: "Two"])
        let cache = AlbumFactsCache(fetch: server.fetch)
        cache.want([1])
        await drain()
        server.failAll()
        await drain()
        cache.want([1])
        cache.want([1, 2])
        await drain()
        #expect(server.log == [[1], [2]])
        server.completeAll()
        cache.want([2])
        cache.want([1, 2])
        await drain()
        #expect(server.log == [[1], [2], [1]])
        server.failAll()
        await drain()
        cache.invalidate()
        await drain()
        #expect(server.log == [[1], [2], [1], [1, 2]])
    }

    @Test func resetDropsFactsAndInFlightRepliesAndRequestsTheAlbumsInView() async {
        let server = FakeSummaries([1: "One"])
        let cache = AlbumFactsCache(fetch: server.fetch)
        cache.want([1])
        await drain()
        server.albums[1] = "One, new session"
        cache.reset()
        await drain()
        #expect(server.log == [[1], [1]])
        #expect(server.pendingCount == 2)
        server.completeAll()
        await drain()
        #expect(cache.summary(1)?.title == "One, new session")
        cache.want([1])
        await drain()
        #expect(server.log.count == 2)
    }

    @Test func cacheStaysBoundedByEvictingAlbumsOutOfView() async {
        let server = FakeSummaries(Dictionary(uniqueKeysWithValues: (1...30).map { (UInt64($0), "A\($0)") }))
        let cache = AlbumFactsCache(maximumEntries: 10, fetch: server.fetch)
        for id in 1...30 {
            cache.want([UInt64(id)])
            await drain()
            server.completeAll()
            await drain()
        }
        #expect(cache.count <= 10)
        #expect(cache.summary(30)?.title == "A30")
    }
}
