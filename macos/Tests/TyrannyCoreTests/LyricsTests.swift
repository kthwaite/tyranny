import Foundation
import Testing
@testable import TyrannyCore

@Suite struct LyricsTests {
    @Test func fractionsMultipleTagsAndStableDuplicateOrder() {
        let timeline = LyricsTimeline("[00:03.125]Third\n[00:01.5][00:03.125]Repeated\n[00:01.50]Translation\n[01:02:03.001]Hour")
        #expect(timeline.lines.map(\.seconds) == [1.5, 1.5, 3.125, 3.125, 3723.001])
        #expect(timeline.lines.map(\.text) == ["Repeated", "Translation", "Third", "Repeated", "Hour"])
        #expect(Set(timeline.lines.map(\.id)).count == 5)
        #expect(timeline.activeRange(at: 1.499).isEmpty)
        #expect(timeline.activeRange(at: 1.5) == 0..<2)
        #expect(timeline.activeRange(at: 3.125) == 2..<4)
    }

    @Test func finalOffsetAppliesToEntireDocumentAndKeepsEarlyLines() {
        let early = LyricsTimeline("[offset:-200]\n[00:00]Opening\n[00:02.5]Later\n[offset:+500]")
        #expect(early.lines.map(\.seconds) == [-0.5, 2])
        #expect(early.activeRange(at: 0) == 0..<1)
        #expect(early.activeRange(at: 2) == 1..<2)
        let late = LyricsTimeline("[00:00]Opening\n[offset:-750]")
        #expect(late.activeRange(at: 0.749).isEmpty)
        #expect(late.activeRange(at: 0.75) == 0..<1)
    }

    @Test func malformedMetadataAndTimesAreNotLyrics() {
        let timeline = LyricsTimeline("[ar:Artist]\r\n[ti:Title]\n[00:61]Bad\n[NaN:02]Bad\n[-1:02]Bad\n[00:01.]Bad\n[00:02.1234567890]Bad\n[00:03 Missing bracket\n[offset:NaN]\n[offset:inf]\n[00:04.25]Good\n[00:05]A [literal] lyric")
        #expect(timeline.lines.map(\.text) == ["Good", "A [literal] lyric"])
        #expect(timeline.lines.map(\.seconds) == [4.25, 5])
        #expect(LyricsTimeline("plain unsynchronized lyrics").lines.isEmpty)
    }

    @Test func pauseAndSeekUseAuthoritativePositionWithoutWallClock() {
        let timeline = LyricsTimeline("[00:00]Opening\n[00:02]Second\n[00:04]Third\n[00:06]")
        #expect(timeline.activeRange(at: 3) == 1..<2)
        #expect(timeline.activeRange(at: 5) == 2..<3) // Forward seek.
        #expect(timeline.activeRange(at: 0) == 0..<1) // Backward seek.
        #expect(timeline.activeRange(at: -1).isEmpty)
        #expect(timeline.activeRange(at: .nan).isEmpty)
        #expect(timeline.activeRange(at: .infinity).isEmpty)
        #expect(timeline.lines[timeline.activeRange(at: 6)].map(\.text) == [""])
    }

    @Test func decodesWireStatusesAndAttribution() throws {
        let ready = try response(status: "ready", source: "lrclib", plain: "Plain lyrics", synced: "[00:00]Sung", url: "https://lrclib.net/api/get/42")
        #expect(ready.lyrics.status == .ready)
        #expect(ready.lyrics.plain_text == "Plain lyrics")
        #expect(ready.lyrics.synced_lrc == "[00:00]Sung")
        #expect(ready.lyrics.attributionURL?.absoluteString == "https://lrclib.net/api/get/42")
        for status in ["missing", "unavailable", "instrumental"] {
            let decoded = try response(status: status)
            #expect(decoded.lyrics.status.rawValue == status)
            #expect(decoded.lyrics.plain_text == nil)
        }
        #expect(throws: (any Error).self) { try response(status: "unknown") }
        #expect(throws: (any Error).self) { try response(status: "ready", kind: "artwork") }
    }

    @Test func providerLinksDoNotOpenArbitrarySchemesOrHosts() throws {
        for url in ["file:///tmp/lyrics", "javascript:alert(1)", "https://example.com/", "https://lrclib.net.evil.example/", "https://user@lrclib.net/"] {
            #expect(try response(status: "ready", source: "lrclib", url: url).lyrics.attributionURL == nil)
        }
        #expect(try response(status: "ready", source: "embedded", url: "https://lrclib.net/").lyrics.attributionURL == nil)
    }

    private func response(status: String, source: String? = nil, plain: String? = nil,
                          synced: String? = nil, url: String? = nil, kind: String = "lyrics") throws -> LyricsResponse {
        let payload: [String: Any] = ["status": status, "source": source as Any? ?? NSNull(),
                                      "plain_text": plain as Any? ?? NSNull(), "synced_lrc": synced as Any? ?? NSNull(),
                                      "attribution_url": url as Any? ?? NSNull(), "message": NSNull()]
        let data = try JSONSerialization.data(withJSONObject: ["kind": kind, "lyrics": payload])
        return try JSONDecoder().decode(LyricsResponse.self, from: data)
    }
}
