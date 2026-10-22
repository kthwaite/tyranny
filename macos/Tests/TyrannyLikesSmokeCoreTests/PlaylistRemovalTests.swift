import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct PlaylistRemovalTests {
    private func entry(_ position: Int) -> UInt64? { UInt64(100 + position) }

    @Test func removesLoadedEntriesInListOrder() {
        #expect(PlaylistRemoval.plan(positions: IndexSet([4, 1, 2]), entryID: entry)
                == .remove(entryIDs: [101, 102, 104]))
    }

    @Test func emptySelectionDoesNothing() {
        #expect(PlaylistRemoval.plan(positions: IndexSet(), entryID: entry) == .nothing)
    }

    @Test func anUnloadedRowRemovesNothing() {
        let plan = PlaylistRemoval.plan(positions: IndexSet([1, 2, 3])) { $0 == 2 ? nil : UInt64($0) }
        #expect(plan == .notLoaded)
        #expect(PlaylistRemoval.notLoadedStatus == "Wait for the playlist to load")
    }

    @Test func fiveHundredRowsRemoveAndMoreAreRefused() {
        #expect(PlaylistRemoval.plan(positions: IndexSet(0..<500), entryID: entry) != .tooMany)
        #expect(PlaylistRemoval.plan(positions: IndexSet(0..<501), entryID: entry) == .tooMany)
    }

    @Test func deleteKeysRemoveAloneOrWithCommand() {
        #expect(PlaylistRemoval.isRemoveKey(keyCode: 51, otherModifiers: false))
        #expect(PlaylistRemoval.isRemoveKey(keyCode: 117, otherModifiers: false))
        #expect(!PlaylistRemoval.isRemoveKey(keyCode: 51, otherModifiers: true))
        #expect(!PlaylistRemoval.isRemoveKey(keyCode: 36, otherModifiers: false))
    }

    @Test func statusNamesTheCountAndPlaylist() {
        #expect(PlaylistRemoval.status(count: 3, playlist: "Road") == "Removed 3 from “Road”")
    }
}
