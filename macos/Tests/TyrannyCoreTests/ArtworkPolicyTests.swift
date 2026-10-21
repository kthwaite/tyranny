import Foundation
import Testing
@testable import TyrannyCore

@Suite struct ArtworkPolicyTests {
    @Test func defaultsRequireNewProviderOptIn() {
        let preferences = ArtworkPreferences()
        #expect(preferences.order == [.coverArtArchive, .deezer, .discogs])
        #expect(preferences.providers == [.coverArtArchive])
    }

    @Test func reorderPreservesDisabledProvidersAndRoundTrips() throws {
        var preferences = ArtworkPreferences(order: [.discogs, .discogs])
        #expect(preferences.order == [.discogs, .coverArtArchive, .deezer])
        preferences.setEnabled(.deezer, true)
        preferences.move(.deezer, by: -1)
        #expect(preferences.providers == [.deezer, .coverArtArchive])
        preferences.move(.discogs, by: -1)
        #expect(preferences.order == [.discogs, .deezer, .coverArtArchive])
        let data = try JSONEncoder().encode(preferences)
        #expect(try JSONDecoder().decode(ArtworkPreferences.self, from: data) == preferences)
        preferences.setEnabled(.deezer, false)
        preferences.setEnabled(.coverArtArchive, false)
        #expect(preferences.providers.isEmpty)
    }

    @Test func tokenValidationRejectsEmptyOversizedAndHeaderControlValues() {
        #expect(ArtworkPreferences.tokenError("") != nil)
        #expect(ArtworkPreferences.tokenError(String(repeating: "x", count: 513)) != nil)
        #expect(ArtworkPreferences.tokenError("token\r\nAuthorization: other") != nil)
        #expect(ArtworkPreferences.tokenError("token with space") != nil)
        #expect(ArtworkPreferences.tokenError(String(repeating: "x", count: 512)) == nil)
    }

    @Test func displayOnlyImagesCannotOutliveDemandOrEnterAttachments() {
        #expect(ArtworkRetention.mayRedisplay(policy: .displayOnly, consumers: 1))
        #expect(!ArtworkRetention.mayRedisplay(policy: .displayOnly, consumers: 0))
        #expect(ArtworkRetention.mayRedisplay(policy: .persistent, consumers: 0))
        #expect(!ArtworkRetention.permitsAttachment(source: "deezer"))
        #expect(!ArtworkRetention.permitsAttachment(source: "discogs"))
        #expect(!ArtworkRetention.permitsAttachment(source: "unknown"))
        #expect(ArtworkRetention.permitsAttachment(source: "embedded"))
        #expect(ArtworkRetention.permitsAttachment(source: "folder"))
        #expect(ArtworkRetention.permitsAttachment(source: "cover_art_archive"))
    }

    @Test func discogsRequiresSafeProvenanceAndFutureExpiryWithinSixHours() {
        let now = Date(timeIntervalSince1970: 1_000_000)
        func valid(_ expiry: Date?, url: URL? = URL(string: "https://www.discogs.com/release/123-Album"),
                   policy: ArtworkCachePolicy = .persistent) -> Bool {
            ArtworkRetention.valid(source: "discogs", policy: policy, sourceURL: url, expiresAt: expiry, now: now)
        }
        #expect(valid(now.addingTimeInterval(21_600)))
        #expect(!valid(now.addingTimeInterval(21_601)))
        #expect(!valid(now))
        #expect(!valid(now.addingTimeInterval(-1)))
        #expect(!valid(nil))
        #expect(!valid(now.addingTimeInterval(60), url: URL(string: "https://www.discogs.com.evil.test/release/123")))
        #expect(!valid(now.addingTimeInterval(60), url: URL(string: "https://user@www.discogs.com/release/123")))
        #expect(!valid(now.addingTimeInterval(60), policy: .displayOnly))
    }

    @Test func deezerRequiresDisplayOnlyAndLinkedAlbum() {
        let now = Date()
        let url = URL(string: "https://www.deezer.com/album/302127")!
        #expect(ArtworkRetention.valid(source: "deezer", policy: .displayOnly, sourceURL: url, expiresAt: nil, now: now))
        #expect(!ArtworkRetention.valid(source: "deezer", policy: .persistent, sourceURL: url, expiresAt: nil, now: now))
        #expect(!ArtworkRetention.valid(source: "deezer", policy: .displayOnly, sourceURL: nil, expiresAt: nil, now: now))
    }
}
