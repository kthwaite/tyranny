import Foundation
import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct ListSearchTests {
    @Test func forwardStartsAfterTheLastSelectedRow() {
        #expect(ListSearch.from(selection: [4, 9], backward: false) == 9)
    }

    @Test func backwardStartsBeforeTheFirstSelectedRow() {
        #expect(ListSearch.from(selection: [4, 9], backward: true) == 4)
    }

    @Test func noSelectionStartsAtAnEnd() {
        #expect(ListSearch.from(selection: [], backward: false) == nil)
        #expect(ListSearch.from(selection: [], backward: true) == nil)
    }

    @Test func fieldsCarryTermAndDirection() throws {
        let list = ListDescription(source: .library)
        let fields = try ListSearch.fields(list: list, revision: 3, term: "beatles", from: 41, backward: true)
        #expect(fields["term"] == .string("beatles"))
        #expect(fields["backward"] == .bool(true))
        #expect(fields["from"] == .int(41))
        #expect(fields["revision"] == .uint(3))
        #expect(fields["list"] == (try JSONValue.encoding(list)))
    }

    @Test func fieldsUseNullForMissingFromAndRevision() throws {
        let fields = try ListSearch.fields(list: ListDescription(source: .library), revision: nil, term: "x", from: nil, backward: false)
        #expect(fields["from"] == .null)
        #expect(fields["revision"] == .null)
        #expect(fields["backward"] == .bool(false))
    }

    @Test func messages() {
        #expect(ListSearch.wrappedMessage(backward: false) == "Search wrapped to the top.")
        #expect(ListSearch.wrappedMessage(backward: true) == "Search wrapped to the bottom.")
        #expect(ListSearch.noMatch("x") == "No match for “x”.")
    }

    @Test func invalidTermShowsTheServerMessage() {
        let error: Error = ListSearch.InvalidTerm(message: "Bad filter near “&”.")
        #expect(error.localizedDescription == "Bad filter near “&”.")
    }

    @Test func hintNamesTheBoundChords() {
        let n = KeyChord("n"), upperN = KeyChord("N")
        #expect(ListSearch.hint(next: n, previous: upperN) == "Jump to a match. Uses filter syntax. n and N repeat.")
        #expect(ListSearch.hint(next: n, previous: nil) == "Jump to a match. Uses filter syntax. n repeats.")
        #expect(ListSearch.hint(next: nil, previous: upperN) == "Jump to a match. Uses filter syntax. N repeats.")
        #expect(ListSearch.hint(next: nil, previous: nil) == "Jump to a match. Uses filter syntax.")
        let rebound = KeyChord("ctrl+j")!
        #expect(ListSearch.hint(next: rebound, previous: n) == "Jump to a match. Uses filter syntax. \(rebound.display) and n repeat.")
    }
}
