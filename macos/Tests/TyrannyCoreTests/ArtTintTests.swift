import Testing
@testable import TyrannyCore

@Suite struct ArtTintTests {
    let red = RGB(red: 1, green: 0, blue: 0)
    let blue = RGB(red: 0, green: 0, blue: 1)

    @Test func artworkMapping() {
        func map(_ context: Bool, _ playing: Bool, _ state: String, _ key: String?) -> ArtTint.Artwork {
            ArtTint.artwork(hasContext: context, playing: playing, state: state, cacheKey: key)
        }
        #expect(map(false, true, "embedded", "k") == .noContext)
        #expect(map(true, false, "embedded", "k") == .nothingPlaying)
        #expect(map(true, true, "loading", "k") == .loading)
        #expect(map(true, true, "missing", "k") == .missing)
        #expect(map(true, true, "unavailable", "k") == .unavailable)
        #expect(map(true, true, "embedded", "k") == .image(key: "k"))
        #expect(map(true, true, "folder", nil) == .loading)
    }

    @Test func missReturnsKeyOnceThenAppliesResult() {
        var tint = ArtTint()
        #expect(tint.observe(.image(key: "k1")) == "k1")
        #expect(tint.observe(.image(key: "k1")) == nil)
        #expect(tint.colour == nil)
        tint.extracted(red, for: "k1")
        #expect(tint.colour == red)
    }

    @Test func staleExtractionIsCachedButNotApplied() {
        var tint = ArtTint()
        _ = tint.observe(.image(key: "k1"))
        tint.extracted(red, for: "k1")
        #expect(tint.colour == red)
        _ = tint.observe(.image(key: "k2"))
        _ = tint.observe(.loading)
        _ = tint.observe(.image(key: "k3"))
        tint.extracted(blue, for: "k2")
        #expect(tint.colour == red)
        #expect(tint.observe(.image(key: "k2")) == nil)
        #expect(tint.colour == blue)
    }

    @Test func noContextAndLoadingKeepTheColour() {
        var tint = ArtTint()
        _ = tint.observe(.image(key: "k1"))
        tint.extracted(red, for: "k1")
        _ = tint.observe(.noContext)
        #expect(tint.colour == red)
        _ = tint.observe(.loading)
        #expect(tint.colour == red)
    }

    @Test func returningToACachedCoverNeedsNoExtraction() {
        var tint = ArtTint()
        _ = tint.observe(.image(key: "k1"))
        tint.extracted(red, for: "k1")
        _ = tint.observe(.noContext)
        #expect(tint.observe(.image(key: "k1")) == nil)
        #expect(tint.colour == red)
    }

    @Test func stopMissingAndUnavailableClear() {
        for artwork in [ArtTint.Artwork.nothingPlaying, .missing, .unavailable] {
            var tint = ArtTint()
            _ = tint.observe(.image(key: "k1"))
            tint.extracted(red, for: "k1")
            _ = tint.observe(artwork)
            #expect(tint.colour == nil)
        }
    }

    @Test func greyCoverClearsTheColour() {
        var tint = ArtTint()
        _ = tint.observe(.image(key: "k1"))
        tint.extracted(red, for: "k1")
        _ = tint.observe(.image(key: "k2"))
        tint.extracted(nil, for: "k2")
        #expect(tint.colour == nil)
        #expect(tint.observe(.image(key: "k2")) == nil)
    }

    @Test func cacheEvictsLeastRecentlyUsed() {
        var tint = ArtTint(capacity: 2)
        tint.extracted(red, for: "k1")
        tint.extracted(blue, for: "k2")
        tint.extracted(red, for: "k3")
        #expect(tint.observe(.image(key: "k1")) == "k1")
        #expect(tint.observe(.image(key: "k3")) == nil)
    }
}
