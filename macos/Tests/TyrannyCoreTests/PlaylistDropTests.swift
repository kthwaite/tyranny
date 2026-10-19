import Foundation
import Testing
@testable import TyrannyCore

@Suite struct PlaylistDropTests {
    // MARK: Dropping on a source-list row

    @Test func regularPlaylistRowAddsTheSelection() {
        #expect(PlaylistDrop.onRow(.regularPlaylist) == .addSelection)
    }

    @Test func smartPlaylistRowRefusesTheDrop() {
        #expect(PlaylistDrop.onRow(.smartPlaylist) == .refuse)
    }

    @Test func upNextRowEnqueuesAtTheEnd() {
        #expect(PlaylistDrop.onRow(.upNext) == .enqueueAppend)
    }

    // MARK: Dropping at an insertion point

    @Test func smallLoadedSelectionInsertsBeforeTheRow() {
        let selection = TrackSelection.ids(libraryRevision: 7, trackIDs: [3, 1, 2])
        #expect(PlaylistDrop.atInsertion(selection) == .insert(libraryRevision: 7, trackIDs: [3, 1, 2]))
    }

    @Test func fiveHundredIDsStillInsert() {
        let ids = (1...500).map { UInt64($0) }
        #expect(PlaylistDrop.atInsertion(.ids(libraryRevision: 1, trackIDs: ids))
                == .insert(libraryRevision: 1, trackIDs: ids))
    }

    @Test func moreThanFiveHundredIDsAppend() {
        let ids = (1...501).map { UInt64($0) }
        #expect(PlaylistDrop.atInsertion(.ids(libraryRevision: 1, trackIDs: ids)) == .append)
    }

    @Test func listSelectionAppends() {
        let selection = TrackSelection.list(
            list: ListDescription(source: .library), libraryRevision: 4, ranges: [[0, 40]], excluded: [], firstRowID: 9)
        #expect(PlaylistDrop.atInsertion(selection) == .append)
    }

    @Test func playlistRowsWithLoadedTrackIDsInsert() {
        let selection = TrackSelection.list(
            list: ListDescription(source: .playlist(id: 1, revision: 2)), libraryRevision: 4,
            ranges: [[0, 3]], excluded: [], firstRowID: 9)
        #expect(PlaylistDrop.atInsertion(selection, loadedTrackIDs: [5, 6, 7])
                == .insert(libraryRevision: 4, trackIDs: [5, 6, 7]))
        #expect(PlaylistDrop.atInsertion(selection, loadedTrackIDs: nil) == .append)
        let many = (1...501).map { UInt64($0) }
        #expect(PlaylistDrop.atInsertion(selection, loadedTrackIDs: many) == .append)
        #expect(PlaylistDrop.embedsPlaylistSource(selection))
        #expect(!PlaylistDrop.embedsPlaylistSource(.ids(libraryRevision: 1, trackIDs: [1])))
    }

    @Test func appendedStatusText() {
        #expect(PlaylistDrop.appendedStatus == "Added at the end")
    }

    // MARK: Reordering inside a playlist

    private func rows(_ ids: [UInt64]) -> (Int) -> UInt64? { { ids.indices.contains($0) ? ids[$0] : nil } }

    @Test func movingRowsBeforeALoadedRowNamesThatRow() {
        let plan = PlaylistDrop.reorder(dragged: IndexSet([0, 1]), dropPosition: 4, total: 6,
                                        rowID: rows([10, 11, 12, 13, 14, 15]))
        #expect(plan == .move(before: 14))
    }

    @Test func droppingPastTheEndMovesToTheEnd() {
        let plan = PlaylistDrop.reorder(dragged: IndexSet([1]), dropPosition: 3, total: 3, rowID: rows([10, 11, 12]))
        #expect(plan == .move(before: nil))
    }

    @Test func droppingNextToAContiguousBlockChangesNothing() {
        let ids: [UInt64] = [10, 11, 12, 13, 14]
        #expect(PlaylistDrop.reorder(dragged: IndexSet([1, 2]), dropPosition: 1, total: 5, rowID: rows(ids)) == .noChange)
        #expect(PlaylistDrop.reorder(dragged: IndexSet([1, 2]), dropPosition: 2, total: 5, rowID: rows(ids)) == .noChange)
        #expect(PlaylistDrop.reorder(dragged: IndexSet([1, 2]), dropPosition: 3, total: 5, rowID: rows(ids)) == .noChange)
    }

    @Test func droppingOnADraggedRowChangesNothing() {
        let ids: [UInt64] = [10, 11, 12, 13, 14]
        #expect(PlaylistDrop.reorder(dragged: IndexSet([0, 2]), dropPosition: 2, total: 5, rowID: rows(ids)) == .noChange)
    }

    @Test func aScatteredSelectionGathersAtTheDrop() {
        let ids: [UInt64] = [10, 11, 12, 13, 14]
        let plan = PlaylistDrop.reorder(dragged: IndexSet([0, 2]), dropPosition: 1, total: 5, rowID: rows(ids))
        #expect(plan == .move(before: 11))
    }

    @Test func anUnloadedDropRowIsUnavailable() {
        let plan = PlaylistDrop.reorder(dragged: IndexSet([0]), dropPosition: 3, total: 6, rowID: rows([10, 11, 12]))
        #expect(plan == .unavailable)
    }

    // MARK: Reorder gate and stale codes

    @Test func reorderIsAllowedOnlyUngroupedAndUnsorted() {
        let playlist = ListSource.playlist(id: 1, revision: 2)
        #expect(PlaylistReorder.allowed(source: playlist, grouping: .none, sort: []))
        #expect(!PlaylistReorder.allowed(source: playlist, grouping: .album, sort: []))
        #expect(!PlaylistReorder.allowed(source: playlist, grouping: .none, sort: [SortKey(field: .title, descending: false)]))
        #expect(!PlaylistReorder.allowed(source: .smart(id: 1, revision: 2), grouping: .none, sort: []))
        #expect(!PlaylistReorder.allowed(source: .library, grouping: .none, sort: []))
    }

    @Test func staleCodesAreRetriedOnce() {
        #expect(PlaylistDrop.isStale("stale_revision"))
        #expect(PlaylistDrop.isStale("stale_playlist_revision"))
        #expect(!PlaylistDrop.isStale("stale_library_revision"))
        #expect(!PlaylistDrop.isStale("invalid_command"))
    }
}
