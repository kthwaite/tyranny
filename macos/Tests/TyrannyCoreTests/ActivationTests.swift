import Foundation
import Testing
@testable import TyrannyCore

private func entry(_ id: UInt64, played: Bool = false) -> QueueItem {
    QueueItem(id: id, path: "/m/\(id).flac", track_id: id, source: nil, run_id: nil, played: played,
              title: "", artist: "", album: "", album_id: nil, duration_ms: nil)
}

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
    @Test func theSnapshotAnswersEmptyAndLoneCurrentQueues() {
        #expect(UpNext.quickAnswer(queueLength: 0, currentID: nil) == false)
        #expect(UpNext.quickAnswer(queueLength: 1, currentID: 5) == false)
        #expect(UpNext.quickAnswer(queueLength: 1, currentID: nil) == nil)
        #expect(UpNext.quickAnswer(queueLength: 3, currentID: 5) == nil)
    }

    @Test func entriesAfterTheCurrentOneAreUpcoming() {
        let queue = [entry(1, played: true), entry(2), entry(3)]
        #expect(UpNext.hasUpcoming(total: 3, currentID: 2, loaded: queue) == true)
        #expect(UpNext.hasUpcoming(total: 3, currentID: 3, loaded: queue) == false)
    }

    @Test func theCurrentEntryDecidesEvenWithUnplayedEntriesBeforeIt() {
        // Shuffle can leave unplayed entries before the current one; the server counts only later ones.
        let queue = [entry(1), entry(2, played: true), entry(3)]
        #expect(UpNext.hasUpcoming(total: 3, currentID: 3, loaded: queue) == false)
    }

    @Test func withoutACurrentEntryUnplayedEntriesAreUpcoming() {
        #expect(UpNext.hasUpcoming(total: 2, currentID: nil, loaded: [entry(1, played: true), entry(2, played: true)]) == false)
        #expect(UpNext.hasUpcoming(total: 2, currentID: nil, loaded: [entry(1, played: true), entry(2)]) == true)
        #expect(UpNext.hasUpcoming(total: 0, currentID: nil, loaded: []) == false)
    }

    @Test func aLongQueueNeedsMoreEntriesUntilItCanTell() {
        // 600 entries, the current one at 550: the first 500 cannot tell.
        let first = (1...500).map { entry(UInt64($0), played: true) }
        #expect(UpNext.hasUpcoming(total: 600, currentID: 550, loaded: first) == nil)
        #expect(UpNext.hasUpcoming(total: 600, currentID: nil, loaded: first) == nil)
        let all = first + (501...600).map { entry(UInt64($0), played: $0 < 550) }
        #expect(UpNext.hasUpcoming(total: 600, currentID: 550, loaded: all) == true)
        #expect(UpNext.hasUpcoming(total: 600, currentID: 600, loaded: all) == false)
        // An unplayed entry among the first is enough without a current entry.
        #expect(UpNext.hasUpcoming(total: 600, currentID: nil, loaded: [entry(1, played: true), entry(2)]) == true)
    }

    @Test func aCurrentEntryOutsideTheQueueFallsBackToPlayedMarks() {
        let queue = [entry(1, played: true), entry(2)]
        #expect(UpNext.hasUpcoming(total: 2, currentID: 99, loaded: queue) == true)
        #expect(UpNext.hasUpcoming(total: 2, currentID: 99, loaded: [entry(1, played: true), entry(2, played: true)]) == false)
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
