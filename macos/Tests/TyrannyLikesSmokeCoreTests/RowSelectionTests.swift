import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

/// Loaded rows where row `p` has ID `ids[p]`.
private func rows(_ ids: [Int: UInt64]) -> [(position: Int, id: UInt64)] {
    ids.sorted { $0.key < $1.key }.map { (position: $0.key, id: $0.value) }
}

private func basis(_ epoch: UInt64, _ revision: UInt64?, refresh: UInt64 = 0) -> RowSelection.Basis {
    RowSelection.Basis(epoch: epoch, revision: revision, refresh: refresh)
}

@Suite struct RowSelectionTests {
    @Test func userSelectionHoldsLoadedRowIDs() {
        var selection = RowSelection()
        let loaded: [Int: UInt64] = [0: 10, 1: 11, 2: 12]
        selection.select(IndexSet([0, 2, 5]), rowID: { loaded[$0] }, basis: basis(1, 1))
        #expect(selection.ids == [10, 12])
        #expect(selection.positions == IndexSet([0, 2, 5]))
    }

    @Test func newlyLoadedRowsAtSelectedPositionsJoinTheSelection() {
        var selection = RowSelection()
        // ⌘A over 1,000 rows with only window 0 loaded.
        selection.select(IndexSet(integersIn: 0..<1000), rowID: { $0 < 200 ? UInt64($0 + 1) : nil }, basis: basis(1, 1))
        #expect(selection.ids.count == 200)
        selection.rowsChanged(loaded: rows(Dictionary(uniqueKeysWithValues: (800..<1000).map { ($0, UInt64($0 + 1)) })),
                              total: 1000, basis: basis(1, 1))
        #expect(selection.positions == IndexSet(integersIn: 0..<1000), "a stable reload dropped selected positions")
        #expect(selection.ids.count == 400)
        #expect(selection.ids.contains(1000))
    }

    @Test func revisionChangeRemapsLoadedRowsByID() {
        var selection = RowSelection()
        let before: [Int: UInt64] = [0: 10, 1: 11, 2: 12, 3: 13]
        selection.select(IndexSet([1, 2]), rowID: { before[$0] }, basis: basis(1, 1))
        // Two rows were inserted at the top: 11 and 12 moved down by two.
        selection.rowsChanged(loaded: rows([0: 98, 1: 99, 2: 10, 3: 11, 4: 12, 5: 13]), total: 6, basis: basis(1, 2))
        #expect(selection.positions == IndexSet([3, 4]))
        #expect(selection.ids == [11, 12])
    }

    @Test func refreshAtTheSameRevisionRemapsLoadedRowsByID() {
        // Sorted by Plays: a play moves C above A without a library revision change.
        var selection = RowSelection()
        let before: [Int: UInt64] = [2: 30, 3: 10, 4: 20]
        selection.select(IndexSet([3]), rowID: { before[$0] }, basis: basis(1, 5, refresh: 0))
        #expect(selection.ids == [10])
        selection.rowsChanged(loaded: rows([2: 20, 3: 30, 4: 10]), total: 10, basis: basis(1, 5, refresh: 1))
        #expect(selection.positions == IndexSet([4]), "the row now at the old position joined the selection")
        #expect(selection.ids == [10])
    }

    @Test func revisionChangeKeepsSelectedPlaceholderPositions() {
        var selection = RowSelection()
        selection.select(IndexSet([1, 500]), rowID: { $0 == 1 ? 11 : nil }, basis: basis(1, 1))
        selection.rowsChanged(loaded: rows([0: 10, 1: 11]), total: 1000, basis: basis(1, 2))
        #expect(selection.positions == IndexSet([1, 500]))
    }

    @Test func descriptionChangeDropsPositionsThenReselectsByIDAsRowsLoad() {
        var selection = RowSelection()
        selection.select(IndexSet([0, 1, 700]), rowID: { [0: 10, 1: 11, 700: 710][$0] }, basis: basis(1, 1))
        // A new sort: 710 is now first, 10 is far down and not loaded yet.
        selection.rowsChanged(loaded: rows([0: 710, 1: 50]), total: 1000, basis: basis(2, 1))
        #expect(selection.positions == IndexSet([0]))
        #expect(selection.ids == [10, 11, 710], "unloaded selected IDs were forgotten")
        selection.rowsChanged(loaded: rows([0: 710, 1: 50, 900: 10]), total: 1000, basis: basis(2, 1))
        #expect(selection.positions == IndexSet([0, 900]))
    }

    @Test func shrinkingTotalClipsPositions() {
        var selection = RowSelection()
        selection.select(IndexSet([1, 8, 9]), rowID: { _ in nil }, basis: basis(1, 1))
        selection.rowsChanged(loaded: [], total: 5, basis: basis(1, 2))
        #expect(selection.positions == IndexSet([1]))
    }

    @Test func userChangeForgetsUnloadedIDs() {
        var selection = RowSelection()
        selection.select(IndexSet([0, 700]), rowID: { [0: 10, 700: 710][$0] }, basis: basis(1, 1))
        selection.rowsChanged(loaded: rows([0: 710]), total: 1000, basis: basis(2, 1))
        selection.select(IndexSet([0]), rowID: { $0 == 0 ? 710 : nil }, basis: basis(2, 1))
        #expect(selection.ids == [710])
    }
}

@Suite struct BrowserSelectionTests {
    @Test func emptySelectionIsTheWholeLibrary() {
        let selection = BrowserSelection()
        #expect(selection.browser == Browser())
    }

    @Test func dimensionsMapToBrowserFieldsSorted() {
        var selection = BrowserSelection()
        selection.setGenres([3, 1])
        selection.setArtists([9, BrowserSelection.compilationsID, 4])
        selection.setAlbums([7])
        #expect(selection.browser == Browser(genre_ids: [1, 3], artist_ids: [4, 9], compilations: true, album_ids: [7]))
    }

    @Test func changingAColumnResetsTheColumnsToItsRight() {
        var selection = BrowserSelection()
        selection.setGenres([1])
        selection.setArtists([2])
        selection.setAlbums([3])
        selection.setArtists([5])
        #expect(selection.albums.isEmpty)
        #expect(selection.genres == [1])
        selection.setGenres([2])
        #expect(selection.artists.isEmpty && selection.albums.isEmpty)
    }

    @Test func settingTheSameValueKeepsTheColumnsToItsRight() {
        var selection = BrowserSelection()
        selection.setGenres([1])
        selection.setAlbums([3])
        selection.setGenres([1])
        #expect(selection.albums == [3])
    }

    @Test func narrowingForAlbumsOmitsAlbumIDs() {
        var selection = BrowserSelection()
        selection.setGenres([1])
        selection.setAlbums([3])
        #expect(selection.albumColumnBrowser == Browser(genre_ids: [1], artist_ids: [], compilations: false, album_ids: []))
    }
}

@Suite struct ListFormatTests {
    @Test func browserAllRowCountsItsItems() {
        #expect(DisplayFormat.browserAll(12, singular: "Genre", plural: "Genres") == "All (12 Genres)")
        #expect(DisplayFormat.browserAll(1, singular: "Genre", plural: "Genres") == "All (1 Genre)")
        #expect(DisplayFormat.browserAll(1_204, singular: "Album", plural: "Albums") == "All (1,204 Albums)")
    }

    @Test func songCountUsesSingularForOne() {
        #expect(DisplayFormat.songs(1) == "1 song")
        #expect(DisplayFormat.songs(1_129) == "1,129 songs")
        #expect(DisplayFormat.songs(0) == "0 songs")
    }

    @Test func columnValues() {
        #expect(DisplayFormat.kilobits(nil) == "")
        #expect(DisplayFormat.kilobits(1411) == "1,411 kbps")
        #expect(DisplayFormat.sampleRate(44_100) == "44.1 kHz")
        #expect(DisplayFormat.sampleRate(48_000) == "48 kHz")
        #expect(DisplayFormat.sampleRate(0) == "")
    }
}
