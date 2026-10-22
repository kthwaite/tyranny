import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct RecentKeysTests {
    /// The current key plus the four most recent stay; older ones are named for eviction.
    @Test func keepsTheCurrentAndTheFourMostRecent() {
        var recent = RecentKeys<Int>(capacity: 5)
        for key in 1...5 { #expect(recent.touch(key).isEmpty) }
        #expect(recent.touch(6) == [1])
        #expect(recent.keys == [2, 3, 4, 5, 6])
    }

    @Test func touchingAKeptKeyMakesItMostRecent() {
        var recent = RecentKeys<Int>(capacity: 3)
        _ = recent.touch(1); _ = recent.touch(2); _ = recent.touch(3)
        #expect(recent.touch(1).isEmpty)
        #expect(recent.touch(4) == [2])
        #expect(recent.keys == [3, 1, 4])
    }

    /// The key just touched is the one on screen: it is never evicted, even at capacity 1.
    @Test func neverEvictsTheTouchedKey() {
        var recent = RecentKeys<Int>(capacity: 1)
        #expect(recent.touch(1).isEmpty)
        #expect(recent.touch(2) == [1])
        #expect(recent.keys == [2])
    }

    @Test func removedKeysAreForgotten() {
        var recent = RecentKeys<Int>(capacity: 2)
        _ = recent.touch(1); _ = recent.touch(2)
        recent.remove(1)
        #expect(recent.touch(3).isEmpty)
        #expect(recent.keys == [2, 3])
    }
}
