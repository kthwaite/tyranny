import Testing
@testable import TyrannyLikesSmokeCore

@Suite struct PlaylistSeedTests {
    @Test func seedHoldsUntilTheCacheReachesItsRevision() {
        #expect(PlaylistSeed.isActive(seedRevision: 5, loadedRevision: nil))
        #expect(PlaylistSeed.isActive(seedRevision: 5, loadedRevision: 4))
        #expect(!PlaylistSeed.isActive(seedRevision: 5, loadedRevision: 5))
        #expect(!PlaylistSeed.isActive(seedRevision: 5, loadedRevision: 9))
    }
}
