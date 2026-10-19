import Foundation
import Testing
@testable import TyrannyCore

private let library = ListDescription(source: .library)
private let playlist = ListDescription(source: .playlist(id: 3, revision: 9))
private func rowID(_ p: Int) -> UInt64? { UInt64(1000 + p) }
private func trackID(_ p: Int) -> UInt64? { UInt64(5000 + p) }

@Suite struct SelectionEncoderTests {
    @Test func runStartsAtClickedPositionToEnd() {
        let s = SelectionEncoder.run(list: library, revision: 7, from: 12, rowID: 42)
        #expect(s == .list(list: library, libraryRevision: 7, ranges: [[12, UInt64.max]], excluded: [], firstRowID: 42))
    }

    @Test func sourceHasNoFirstRow() {
        let s = SelectionEncoder.source(list: library, revision: 7)
        #expect(s == .list(list: library, libraryRevision: 7, ranges: [[0, UInt64.max]], excluded: [], firstRowID: nil))
    }

    @Test func smallLoadedLibrarySelectionUsesIDs() throws {
        let s = try SelectionEncoder.selection(list: library, revision: 2, positions: IndexSet([4, 1, 9]),
                                               rowID: rowID, trackID: trackID)
        #expect(s == .ids(libraryRevision: 2, trackIDs: [5001, 5004, 5009]))
    }

    @Test func unloadedTrackFallsBackToList() throws {
        let s = try SelectionEncoder.selection(list: library, revision: 2, positions: IndexSet([1, 2]),
                                               rowID: rowID, trackID: { $0 == 2 ? nil : trackID($0) })
        #expect(s == .list(list: library, libraryRevision: 2, ranges: [[1, 3]], excluded: [], firstRowID: 1001))
    }

    @Test func playlistSelectionAlwaysUsesList() throws {
        let s = try SelectionEncoder.selection(list: playlist, revision: 2, positions: IndexSet([1, 5]),
                                               rowID: rowID, trackID: trackID)
        #expect(s == .list(list: playlist, libraryRevision: 2, ranges: [[1, 2], [5, 6]], excluded: [], firstRowID: 1001))
    }

    @Test func select_all_encodes_one_full_range() throws {
        let s = try SelectionEncoder.selection(list: library, revision: 2, positions: IndexSet(integersIn: 0..<5000),
                                               rowID: rowID, trackID: trackID)
        #expect(s == .list(list: library, libraryRevision: 2, ranges: [[0, 5000]], excluded: [], firstRowID: 1000))
    }

    @Test func fragmentedSelectionUsesBoundingRangeWithExclusions() throws {
        // 70 single-row runs at even positions 0,2,...,138; 69 gap rows.
        let positions = IndexSet(stride(from: 0, to: 140, by: 2))
        let s = try SelectionEncoder.selection(list: playlist, revision: 2, positions: positions,
                                               rowID: rowID, trackID: trackID)
        let expected = (0..<139).filter { $0 % 2 == 1 }.map { UInt64(1000 + $0) }
        #expect(s == .list(list: playlist, libraryRevision: 2, ranges: [[0, 139]], excluded: expected, firstRowID: 1000))
    }

    @Test func fragmentedSelectionWithUnloadedGapsIsRefused() {
        let positions = IndexSet(stride(from: 0, to: 140, by: 2))
        #expect(throws: SelectionTooScattered.self) {
            try SelectionEncoder.selection(list: playlist, revision: 2, positions: positions,
                                           rowID: { $0 == 51 ? nil : rowID($0) }, trackID: trackID)
        }
    }

    @Test func fragmentedSelectionWithTooManyGapsIsRefused() {
        let positions = IndexSet(stride(from: 0, to: 2000, by: 3)) // ~1333 gap rows
        #expect(throws: SelectionTooScattered.self) {
            try SelectionEncoder.selection(list: playlist, revision: 2, positions: positions,
                                           rowID: rowID, trackID: trackID)
        }
    }

    @Test func emptySelectionThrows() {
        #expect(throws: EmptySelection.self) {
            try SelectionEncoder.selection(list: library, revision: 2, positions: IndexSet(),
                                           rowID: rowID, trackID: trackID)
        }
    }

    @Test func decideCoversEveryPolicy() {
        for policy in [UpNextPolicy.ask, .replace, .keep] {
            #expect(SelectionEncoder.decide(policy: policy, hasUpcoming: false) == .enqueue(.replaceUpcoming))
        }
        #expect(SelectionEncoder.decide(policy: .replace, hasUpcoming: true) == .enqueue(.replaceUpcoming))
        #expect(SelectionEncoder.decide(policy: .keep, hasUpcoming: true) == .enqueue(.next))
        #expect(SelectionEncoder.decide(policy: .ask, hasUpcoming: true) == .ask)
    }
}
