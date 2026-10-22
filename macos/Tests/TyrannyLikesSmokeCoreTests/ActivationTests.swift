import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct ActivationPlanTests {
    @Test func addToUpNextAppendsAndPlayNextInsertsWithoutPlaying() {
        for policy in [UpNextPolicy.ask, .replace, .keep] {
            #expect(ActivationPlan.make(.addToUpNext, policy: policy, hasUpcoming: true)
                    == .enqueue(placement: .append, play: false))
            #expect(ActivationPlan.make(.playNext, policy: policy, hasUpcoming: true)
                    == .enqueue(placement: .next, play: false))
        }
    }

    @Test func playReplacesWhenNothingIsUpcomingWhateverThePolicy() {
        for policy in [UpNextPolicy.ask, .replace, .keep] {
            #expect(ActivationPlan.make(.play, policy: policy, hasUpcoming: false)
                    == .enqueue(placement: .replaceUpcoming, play: true))
        }
    }

    @Test func playFollowsThePolicyWhenEntriesAreUpcoming() {
        #expect(ActivationPlan.make(.play, policy: .replace, hasUpcoming: true)
                == .enqueue(placement: .replaceUpcoming, play: true))
        #expect(ActivationPlan.make(.play, policy: .keep, hasUpcoming: true) == .enqueue(placement: .next, play: true))
        #expect(ActivationPlan.make(.play, policy: .ask, hasUpcoming: true) == .ask)
    }

    @Test func theSheetChoicesMapToPlacementsAndRememberedPolicies() {
        #expect(UpNextChoice.clear.placement == .replaceUpcoming)
        #expect(UpNextChoice.clear.remembered == .replace)
        #expect(UpNextChoice.keep.placement == .next)
        #expect(UpNextChoice.keep.remembered == .keep)
    }

    @Test func returnPlaysOptionReturnAddsAndOtherModifiersAreIgnored() {
        #expect(ActivationKey.action(option: false, otherModifiers: false) == .play)
        #expect(ActivationKey.action(option: true, otherModifiers: false) == .addToUpNext)
        #expect(ActivationKey.action(option: false, otherModifiers: true) == nil)
        #expect(ActivationKey.action(option: true, otherModifiers: true) == nil)
    }

    @Test func requestFieldsCarryTheQueueRevision() throws {
        let request = EnqueueRequest(selection: .ids(libraryRevision: 3, trackIDs: [1]), placement: .replaceUpcoming,
                                     play: true, source: QueueSource(kind: .album, name: "T"))
        let fields = try request.fields(expectedRevision: 41)
        #expect(fields["expected_revision"] == .uint(41))
        #expect(fields["placement"] == .string("replace_upcoming"))
        #expect(fields["play"] == .bool(true))
        #expect(fields["source"] == .object(["kind": .string("album"), "name": .string("T")]))
    }

    @Test func listsStampTheirKindAndName() {
        #expect(RunSource.queueSource(list: .library, name: "Songs") == QueueSource(kind: .songs, name: "Songs"))
        #expect(RunSource.queueSource(list: .album(id: 1), name: "A").kind == .album)
        #expect(RunSource.queueSource(list: .playlist(id: 1, revision: 2), name: "P") == QueueSource(kind: .playlist, name: "P"))
        #expect(RunSource.queueSource(list: .smart(id: 1, revision: 2), name: "S").kind == .smart)
        #expect(RunSource.queueSource(list: .genre(id: 1), name: "G").kind == .genre)
        #expect(RunSource.queueSource(list: .recent, name: "Recently Added").kind == .recent)
        #expect(RunSource.queueSource(list: .artist(id: 1), name: "X").kind == .artist)
        #expect(RunSource.queueSource(list: .appearsOn(artistID: 1), name: "X").kind == .artist)
        #expect(RunSource.queueSource(list: .compilations, name: "Compilations").kind == .compilations)
    }
}

@Suite struct UpNextUpcomingTests {
    /// The server counts upcoming entries with the shuffle-aware rule; the snapshot answers.
    @Test func theSnapshotCountAnswersForBothShuffleStates() throws {
        // Shuffle off, the current entry last: a skipped entry before it is not upcoming.
        #expect(UpNext.hasUpcoming(try snapshot(currentID: 3, currentIndex: 2, upcoming: 0, shuffle: false,
                                                queueLength: 3)) == false)
        // Shuffle on, the same queue: the skipped entry is upcoming.
        #expect(UpNext.hasUpcoming(try snapshot(currentID: 3, currentIndex: 2, upcoming: 1, shuffle: true,
                                                queueLength: 3)) == true)
        #expect(UpNext.hasUpcoming(try snapshot(currentID: nil, currentIndex: nil, upcoming: 2, shuffle: false)))
        #expect(!UpNext.hasUpcoming(try snapshot(currentID: nil, currentIndex: nil, upcoming: 0, shuffle: true,
                                                 queueLength: 0)))
    }

    @Test func snapshotDecodesTheUpcomingFields() throws {
        let state = try snapshot(currentID: 4, currentIndex: 7, upcoming: 12, shuffle: true)
        #expect(state.current_index == 7)
        #expect(state.upcoming_count == 12)
        let idle = try snapshot(currentID: nil, currentIndex: nil, upcoming: 0, shuffle: false)
        #expect(idle.current_index == nil)
    }
}

@Suite struct RowRelocationTests {
    @Test func aRowThatMovedIsFoundAtItsNewPosition() {
        #expect(RowRelocation.locate(rowID: 30, rowIDs: [10, 20, 30, 40], offset: 100) == .found(102))
    }

    @Test func aRowThatLeftTheWindowIsGone() {
        #expect(RowRelocation.locate(rowID: 99, rowIDs: [10, 20], offset: 0) == .gone)
        #expect(RowRelocation.locate(rowID: 1, rowIDs: [], offset: 0) == .gone)
    }

    @Test func theWindowIsCentredAndStartsNoEarlierThanTheFirstRow() {
        #expect(RowRelocation.window(around: 500, size: 200) == (400, 200))
        #expect(RowRelocation.window(around: 30, size: 200) == (0, 200))
    }

    private let list = ListDescription(source: .library)

    @Test func idSelectionsKeepTheirTracksUnderTheFreshRevision() {
        #expect(SelectionRetarget.apply(.ids(libraryRevision: 1, trackIDs: [4, 5]), libraryRevision: 9, delta: 7)
                == .ids(libraryRevision: 9, trackIDs: [4, 5]))
    }

    @Test func aRunMovesWithItsStartRowAndKeepsItsOpenEnd() {
        let run = SelectionEncoder.run(list: list, revision: 1, from: 10, rowID: 77)
        #expect(SelectionRetarget.apply(run, libraryRevision: 2, delta: 3)
                == .list(list: list, libraryRevision: 2, ranges: [[13, UInt64.max]], excluded: [], firstRowID: 77))
        #expect(SelectionRetarget.apply(run, libraryRevision: 2, delta: -4)
                == .list(list: list, libraryRevision: 2, ranges: [[6, UInt64.max]], excluded: [], firstRowID: 77))
    }

    @Test func aMovedBoundedRangeShiftsBothEnds() {
        let selection = TrackSelection.list(list: list, libraryRevision: 1, ranges: [[4, 9]], excluded: [3], firstRowID: 5)
        #expect(SelectionRetarget.apply(selection, libraryRevision: 2, delta: 2)
                == .list(list: list, libraryRevision: 2, ranges: [[6, 11]], excluded: [3], firstRowID: 5))
    }

    @Test func severalRangesOnlyFollowWhenNothingMoved() {
        let selection = TrackSelection.list(list: list, libraryRevision: 1, ranges: [[1, 2], [5, 6]],
                                            excluded: [], firstRowID: 1)
        #expect(SelectionRetarget.apply(selection, libraryRevision: 2, delta: 1) == nil)
        #expect(SelectionRetarget.apply(selection, libraryRevision: 2, delta: 0)
                == .list(list: list, libraryRevision: 2, ranges: [[1, 2], [5, 6]], excluded: [], firstRowID: 1))
    }
}

@Suite struct StaleRunTests {
    @Test func aRetryDecidesAgainAndAsksWhenEntriesArrivedMeanwhile() {
        // The snapshot lagged: Up Next looked empty, so Play replaced. After the refetch entries are upcoming.
        for policy in [UpNextPolicy.ask] {
            #expect(ActivationPlan.make(.play, policy: policy, hasUpcoming: false)
                    == .enqueue(placement: .replaceUpcoming, play: true))
            #expect(ActivationPlan.make(.play, policy: policy, hasUpcoming: true) == .ask)
        }
        #expect(ActivationPlan.make(.play, policy: .keep, hasUpcoming: true) == .enqueue(placement: .next, play: true))
    }
}

@Suite struct ActivationSelectionTests {
    private let list = ListDescription(source: .library)
    private func encode(_ positions: IndexSet, asRun: Bool) throws -> TrackSelection {
        try SelectionEncoder.activation(list: list, revision: 5, positions: positions, asRun: asRun,
                                        rowID: { UInt64($0) + 100 }, trackID: { UInt64($0) + 1000 })
    }

    @Test func doubleClickAndReturnRunToTheEndOfTheList() throws {
        #expect(try encode(IndexSet(integer: 3), asRun: true)
                == .list(list: list, libraryRevision: 5, ranges: [[3, UInt64.max]], excluded: [], firstRowID: 103))
    }

    @Test func theMenuPlaysOnlyTheSelectedRows() throws {
        #expect(try encode(IndexSet(integer: 3), asRun: false) == .ids(libraryRevision: 5, trackIDs: [1003]))
        #expect(try encode(IndexSet([3, 4]), asRun: false) == .ids(libraryRevision: 5, trackIDs: [1003, 1004]))
        // Several rows are never a run, even for Return.
        #expect(try encode(IndexSet([3, 4]), asRun: true) == .ids(libraryRevision: 5, trackIDs: [1003, 1004]))
    }

    @Test func aRetargetNamesTheListTheRowsWereFetchedFor() {
        let run = SelectionEncoder.run(list: list, revision: 1, from: 2, rowID: 9)
        let fetched = ListDescription(source: .playlist(id: 4, revision: 8), query: "x")
        #expect(SelectionRetarget.apply(run, libraryRevision: 2, delta: 1, list: fetched)
                == .list(list: fetched, libraryRevision: 2, ranges: [[3, UInt64.max]], excluded: [], firstRowID: 9))
    }
}
