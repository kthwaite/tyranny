import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

private func album(id: UInt64 = 7, title: String = "Spirit of Eden", artist: String = "Talk Talk", year: Int32? = 1988,
                   genre: String = "Art Rock", tracks: UInt64 = 6, duration: UInt64 = 2_496_000,
                   format: String = "FLAC 16/44.1", gain: Double? = -6.24) -> AlbumSummary {
    AlbumSummary(id: id, title: title, album_artist: artist, album_artist_id: 3, compilation: false, year: year,
                 genre: genre, track_count: tracks, disc_count: 1, duration_ms: duration, format: format,
                 album_gain_db: gain, date_added: 0, musicbrainz_release_id: nil)
}

private func rowID(_ p: Int) -> UInt64? { UInt64(1000 + p) }
private func trackID(_ p: Int) -> UInt64? { UInt64(5000 + p) }

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

    @Test func theWholeAlbumIsASourceSelectionWithoutAStartRow() {
        let selection = AlbumPlayback.selection(albumID: 7, libraryRevision: 12)
        #expect(selection == .list(list: list, libraryRevision: 12, ranges: [[0, UInt64.max]],
                                   excluded: [], firstRowID: nil))
        #expect(AlbumPlayback.source(title: "Spirit of Eden") == QueueSource(kind: .album, name: "Spirit of Eden"))
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

    @Test func noChosenRowsPlaysTheWholeAlbum() throws {
        for asRun in [true, false] {
            let s = try AlbumPanel.selection(albumID: 7, revision: 12, positions: IndexSet(), asRun: asRun,
                                             rowID: rowID, trackID: trackID)
            #expect(s == AlbumPlayback.selection(albumID: 7, libraryRevision: 12))
        }
    }

    @Test func oneRowAsARunPlaysFromItToTheAlbumEnd() throws {
        let s = try AlbumPanel.selection(albumID: 7, revision: 12, positions: IndexSet([2]), asRun: true,
                                         rowID: rowID, trackID: trackID)
        #expect(s == .list(list: AlbumPlayback.list(albumID: 7), libraryRevision: 12, ranges: [[2, UInt64.max]],
                           excluded: [], firstRowID: 1002))
    }

    @Test func aRunFollowsItsFirstTrackAfterTheLibraryMovedOn() throws {
        // A scan added a track before the clicked one; the panel still shows the old order.
        let run = try AlbumPanel.selection(albumID: 7, revision: 12, positions: IndexSet([2]), asRun: true,
                                           rowID: rowID, trackID: trackID)
        let list = AlbumPlayback.list(albumID: 7)
        #expect(SelectionRetarget.relocate(run, rowID: 1002, from: 2, rowIDs: [1000, 1001, 999, 1002], offset: 0,
                                           libraryRevision: 13, list: list)
                == .list(list: list, libraryRevision: 13, ranges: [[3, UInt64.max]], excluded: [], firstRowID: 1002))
        #expect(SelectionRetarget.relocate(run, rowID: 1002, from: 2, rowIDs: [1000, 1001], offset: 0,
                                           libraryRevision: 13, list: list) == nil)
    }

    @Test func otherChoicesSendExactlyTheChosenTracks() throws {
        // Several rows, even as a run, and one row not as a run: their track IDs, in album order.
        let several = try AlbumPanel.selection(albumID: 7, revision: 12, positions: IndexSet([3, 1]), asRun: true,
                                               rowID: rowID, trackID: trackID)
        #expect(several == .ids(libraryRevision: 12, trackIDs: [5001, 5003]))
        let one = try AlbumPanel.selection(albumID: 7, revision: 12, positions: IndexSet([2]), asRun: false,
                                           rowID: rowID, trackID: trackID)
        #expect(one == .ids(libraryRevision: 12, trackIDs: [5002]))
    }

    @Test func tableShowsAtMostTwelveRowsAndAtLeastOne() {
        #expect(AlbumPanel.maximumRows == 12)
        #expect(AlbumPanel.visibleRows(trackCount: 0) == 1)
        #expect(AlbumPanel.visibleRows(trackCount: 7) == 7)
        #expect(AlbumPanel.visibleRows(trackCount: 12) == 12)
        #expect(AlbumPanel.visibleRows(trackCount: 1024) == 12)
    }

    @Test func moreLineShowsOnlyPastTheTrackLimit() {
        #expect(!AlbumPanel.showsMore(trackCount: 12))
        #expect(!AlbumPanel.showsMore(trackCount: 200))
        #expect(AlbumPanel.showsMore(trackCount: 201))
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
