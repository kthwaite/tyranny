import Foundation
import Testing
@testable import TyrannyCore

private func album(id: UInt64 = 7, title: String = "Spirit of Eden", artist: String = "Talk Talk", year: Int32? = 1988,
                   genre: String = "Art Rock", tracks: UInt64 = 6, duration: UInt64 = 2_496_000,
                   format: String = "FLAC 16/44.1", gain: Double? = -6.24) -> AlbumSummary {
    AlbumSummary(id: id, title: title, album_artist: artist, album_artist_id: 3, compilation: false, year: year,
                 genre: genre, track_count: tracks, disc_count: 1, duration_ms: duration, format: format,
                 album_gain_db: gain, date_added: 0, musicbrainz_release_id: nil)
}

private func item(_ id: UInt64, played: Bool) -> QueueItem {
    QueueItem(id: id, path: "/m/\(id).flac", track_id: id, source: nil, run_id: nil, played: played,
              title: "", artist: "", album: "", album_id: nil, duration_ms: nil)
}

@Suite struct AlbumGridGeometryTests {
    @Test func columnsFitTheWidthAndCentreTheGrid() {
        // 1000 wide, 100 pt covers, 16 pt gaps, 20 pt minimum insets: 960 available, 8 fit (8·100 + 7·16 = 912).
        let grid = AlbumGridGeometry(width: 1000, coverSize: 100)
        #expect(grid.columns == 8)
        #expect(grid.horizontalInset == 44)
        // A full row fills the width between the insets exactly, so the flow layout never spreads it.
        let covers: Double = Double(grid.columns) * grid.itemWidth
        let gaps: Double = Double(grid.columns - 1) * AlbumGridGeometry.spacing
        #expect(covers + gaps + 2 * grid.horizontalInset == 1000)
        #expect(grid.trailingInset == 44)
    }

    @Test func oddLeftoverGoesToTheTrailingInset() {
        // 1001 wide: 89 pt left over; 44 leading, 45 trailing, so rows still fill exactly.
        let grid = AlbumGridGeometry(width: 1001, coverSize: 100)
        #expect(grid.horizontalInset == 44)
        #expect(grid.trailingInset == 45)
    }

    @Test func narrowWidthStillShowsOneColumn() {
        let grid = AlbumGridGeometry(width: 90, coverSize: 200)
        #expect(grid.columns == 1)
        #expect(grid.horizontalInset == AlbumGridGeometry.minimumInset)
        #expect(grid.trailingInset == AlbumGridGeometry.minimumInset)
    }

    @Test func coverSizeIsClampedToTheSliderRange() {
        #expect(AlbumGridGeometry(width: 800, coverSize: 20).coverSize == 96)
        #expect(AlbumGridGeometry(width: 800, coverSize: 900).coverSize == 200)
    }

    @Test func caretPointsAtTheCoverCentreFromThePanelEdge() {
        let grid = AlbumGridGeometry(width: 1000, coverSize: 100)
        // The panel starts at the minimum inset; column 2's cover centre is 44 + 2·116 + 50.
        let expected: Double = 44 + 232 + 50 - 20
        #expect(grid.caretX(column: 2) == expected)
        #expect(grid.panelWidth < 1000 - 40)
    }
}

@Suite struct AlbumGridLayoutTests {
    @Test func collapsedGridIsOneRunOfAlbums() {
        let layout = AlbumGridLayout(count: 10, columns: 4, expanded: nil)
        #expect(layout.split == 10)
        #expect((0..<AlbumGridLayout.sections).map(layout.numberOfItems(inSection:)) == [10, 0, 0])
        #expect(layout.item(section: 0, item: 9) == .album(9))
        #expect(layout.item(section: 1, item: 0) == nil)
        #expect(layout.location(ofAlbum: 9) == AlbumGridLayout.Location(section: 0, item: 9))
    }

    @Test func panelFollowsTheLastCellOfTheClickedRow() {
        let layout = AlbumGridLayout(count: 10, columns: 4, expanded: 5)
        #expect(layout.split == 8)
        #expect((0..<AlbumGridLayout.sections).map(layout.numberOfItems(inSection:)) == [8, 1, 2])
        #expect(layout.item(section: 0, item: 7) == .album(7))
        #expect(layout.item(section: 1, item: 0) == .expansion)
        #expect(layout.item(section: 2, item: 0) == .album(8))
        #expect(layout.item(section: 2, item: 2) == nil)
        #expect(layout.location(ofAlbum: 9) == AlbumGridLayout.Location(section: 2, item: 1))
        #expect(layout.expandedColumn == 1)
    }

    @Test func panelOnThePartialLastRowEndsTheGrid() {
        let layout = AlbumGridLayout(count: 10, columns: 4, expanded: 9)
        #expect(layout.split == 10)
        #expect((0..<AlbumGridLayout.sections).map(layout.numberOfItems(inSection:)) == [10, 1, 0])
    }

    @Test func albumsBeyondTheCountHaveNoLocation() {
        let layout = AlbumGridLayout(count: 3, columns: 4, expanded: nil)
        #expect(layout.location(ofAlbum: 3) == nil)
        #expect(layout.location(ofAlbum: -1) == nil)
        // An expanded position past the end (the list shrank) shows no panel.
        #expect(AlbumGridLayout(count: 3, columns: 4, expanded: 5).numberOfItems(inSection: 1) == 0)
    }

    @Test func albumRangeCoversVisibleLocations() {
        let layout = AlbumGridLayout(count: 10, columns: 4, expanded: 1)
        let locations = [AlbumGridLayout.Location(section: 0, item: 2), .init(section: 1, item: 0),
                         .init(section: 2, item: 3)]
        #expect(layout.albumRange(of: locations) == 2..<8)
        #expect(layout.albumRange(of: [.init(section: 1, item: 0)]) == nil)
    }
}

@Suite struct AlbumTextTests {
    @Test func cellSubtitleIsArtistAndYear() {
        #expect(DisplayFormat.albumSubtitle(album()) == "Talk Talk · 1988")
        #expect(DisplayFormat.albumSubtitle(album(year: nil)) == "Talk Talk")
        #expect(DisplayFormat.albumSubtitle(album(artist: "", year: nil)) == "Unknown Artist")
    }

    @Test func expansionFactsLineJoinsEveryKnownFact() {
        #expect(DisplayFormat.albumExpansionFacts(album())
                == "1988 · Art Rock · 6 tracks · 41:36 · FLAC 16/44.1 · RG album \u{2212}6.2 dB")
        #expect(DisplayFormat.albumExpansionFacts(album(year: nil, genre: "", tracks: 1, duration: 61_000,
                                                        format: "", gain: nil)) == "1 track · 1:01")
    }
}

@Suite struct AlbumPlaybackTests {
    private let list = ListDescription(source: .album(id: 7))

    @Test func playSendsTheWholeAlbumWithoutAStartRow() {
        let request = AlbumPlayback.enqueue(.play, albumID: 7, title: "Spirit of Eden", libraryRevision: 12,
                                            policy: .ask, hasUpcoming: false)
        #expect(request.selection == .list(list: list, libraryRevision: 12, ranges: [[0, UInt64.max]],
                                           excluded: [], firstRowID: nil))
        #expect(request.placement == .replaceUpcoming)
        #expect(request.play)
        #expect(request.source == QueueSource(kind: .album, name: "Spirit of Eden"))
    }

    @Test func playFollowsTheUpNextPolicy() {
        func placement(_ policy: UpNextPolicy) -> Placement {
            AlbumPlayback.enqueue(.play, albumID: 7, title: "", libraryRevision: 1, policy: policy,
                                  hasUpcoming: true).placement
        }
        #expect(placement(.replace) == .replaceUpcoming)
        #expect(placement(.keep) == .next)
        // Ask has no sheet yet (Task 13): it keeps Up Next.
        #expect(placement(.ask) == .next)
    }

    @Test func addToUpNextAppendsWithoutPlaying() {
        let request = AlbumPlayback.enqueue(.addToUpNext, albumID: 7, title: "Spirit of Eden", libraryRevision: 3,
                                            policy: .replace, hasUpcoming: true)
        #expect(request.placement == .append)
        #expect(!request.play)
        #expect(request.selection == SelectionEncoder.source(list: list, revision: 3))
    }

    @Test func requestFieldsCarryTheQueueRevision() throws {
        let request = AlbumPlayback.enqueue(.play, albumID: 7, title: "T", libraryRevision: 3, policy: .ask,
                                            hasUpcoming: false)
        let fields = try request.fields(expectedRevision: 41)
        #expect(fields["expected_revision"] == .uint(41))
        #expect(fields["placement"] == .string("replace_upcoming"))
        #expect(fields["play"] == .bool(true))
        #expect(fields["source"] == .object(["kind": .string("album"), "name": .string("T")]))
    }
}

@Suite struct UpNextTests {
    @Test func entriesAfterTheCurrentOneAreUpcoming() {
        let page = Page(revision: 4, total: 3, offset: 0, entries: [item(1, played: true), item(2, played: false),
                                                                    item(3, played: false)])
        #expect(UpNext.hasUpcoming(queueLength: 3, currentID: 2, queueRevision: 4, page: page))
        #expect(!UpNext.hasUpcoming(queueLength: 3, currentID: 3, queueRevision: 4, page: page))
    }

    @Test func withoutACurrentEntryUnplayedEntriesAreUpcoming() {
        let played = Page(revision: 4, total: 2, offset: 0, entries: [item(1, played: true), item(2, played: true)])
        #expect(!UpNext.hasUpcoming(queueLength: 2, currentID: nil, queueRevision: 4, page: played))
        let fresh = Page(revision: 4, total: 2, offset: 0, entries: [item(1, played: true), item(2, played: false)])
        #expect(UpNext.hasUpcoming(queueLength: 2, currentID: nil, queueRevision: 4, page: fresh))
        #expect(!UpNext.hasUpcoming(queueLength: 0, currentID: nil, queueRevision: 4, page: nil))
    }

    @Test func unknownQueueContentsCountAsUpcoming() {
        // A stale page, or a current entry outside the loaded page, cannot rule upcoming entries out.
        let stale = Page(revision: 3, total: 2, offset: 0, entries: [item(1, played: true), item(2, played: false)])
        #expect(UpNext.hasUpcoming(queueLength: 2, currentID: 2, queueRevision: 4, page: stale))
        let partial = Page(revision: 4, total: 300, offset: 0, entries: [item(1, played: true)])
        #expect(UpNext.hasUpcoming(queueLength: 300, currentID: 250, queueRevision: 4, page: partial))
        #expect(UpNext.hasUpcoming(queueLength: 300, currentID: nil, queueRevision: 4, page: partial))
        #expect(!UpNext.hasUpcoming(queueLength: 1, currentID: 1, queueRevision: 4, page: nil))
    }
}

@Suite struct QueueSourceNameTests {
    @Test func longNamesAreCutToTheServerLimit() {
        let name = String(repeating: "a", count: 300)
        #expect(QueueSource(kind: .album, name: name).name == String(repeating: "a", count: 256))
        #expect(QueueSource(kind: .album, name: "Spirit of Eden").name == "Spirit of Eden")
    }

    @Test func aMultiByteCharacterStraddlingTheLimitIsDropped() {
        // 255 bytes of ASCII, then "é" (2 bytes) would end at byte 257: it is dropped whole.
        let name = String(repeating: "a", count: 255) + "é" + "z"
        let cut = QueueSource(kind: .album, name: name).name
        #expect(cut == String(repeating: "a", count: 255))
        #expect(cut.utf8.count <= QueueSource.maximumNameBytes)
        // A character that fits exactly is kept.
        let exact = String(repeating: "a", count: 254) + "é" + "z"
        #expect(QueueSource(kind: .album, name: exact).name == String(repeating: "a", count: 254) + "é")
    }

    @Test func decodedNamesAreNotRewritten() throws {
        let json = Data(#"{"kind":"album","name":"Spirit of Eden"}"#.utf8)
        #expect(try JSONDecoder().decode(QueueSource.self, from: json).name == "Spirit of Eden")
    }
}

@Suite struct AlbumPanelTests {
    @Test func panelLoadsOnceAndAgainOnlyWhenTheAlbumChanged() {
        let shown = album()
        #expect(AlbumPanel.needsReload(loaded: nil, current: shown, revision: 4, scanning: true))
        let loaded = AlbumPanel.Loaded(album: shown, revision: 4)
        // A scan batch bumps the revision without touching this album: no refetch.
        #expect(!AlbumPanel.needsReload(loaded: loaded, current: shown, revision: 5, scanning: true))
        // The album's summary changed: refetch, even mid-scan.
        #expect(AlbumPanel.needsReload(loaded: loaded, current: album(tracks: 7), revision: 5, scanning: true))
        // Once the scan settles, one refetch picks up retags that keep the summary.
        #expect(AlbumPanel.needsReload(loaded: loaded, current: shown, revision: 5, scanning: false))
        #expect(!AlbumPanel.needsReload(loaded: loaded, current: shown, revision: 4, scanning: false))
    }

    @Test func longAlbumsShowTheFirstTracksAndACount() {
        #expect(AlbumPanel.trackLimit == 200)
        #expect(AlbumPanel.moreText(total: 1024, shown: 200) == "and 824 more…")
        #expect(AlbumPanel.moreText(total: 1201, shown: 200) == "and 1,001 more…")
        #expect(AlbumPanel.moreText(total: 7, shown: 7) == nil)
    }
}

@Suite struct AlbumGridReloadTests {
    @Test func coverSizeAloneKeepsTheItems() {
        // Without a panel, a column change moves cells but keeps every item: re-layout only.
        let before = AlbumGridLayout(count: 10, columns: 4, expanded: nil)
        #expect(before.hasSameItems(as: AlbumGridLayout(count: 10, columns: 6, expanded: nil)))
        // With a panel, the row end moves and items change sections: reload.
        let open = AlbumGridLayout(count: 10, columns: 4, expanded: 5)
        #expect(!open.hasSameItems(as: AlbumGridLayout(count: 10, columns: 3, expanded: 5)))
        // The same row end at another width (8 = 2·4 = 1·8) keeps the items; only the caret moves.
        #expect(open.hasSameItems(as: AlbumGridLayout(count: 10, columns: 8, expanded: 5)))
        #expect(open.hasSameItems(as: AlbumGridLayout(count: 10, columns: 4, expanded: 6)) == false)
        #expect(open.hasSameItems(as: AlbumGridLayout(count: 10, columns: 4, expanded: 5)))
        #expect(!before.hasSameItems(as: AlbumGridLayout(count: 11, columns: 4, expanded: nil)))
    }
}
