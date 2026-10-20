import Foundation
import Testing
@testable import TyrannyCore

@Suite struct LaunchPrefetchTests {
    private let r: [String: JSONValue] = ["list": .string("songs"), "revision": .null,
                                          "offset": .int(0), "limit": .int(200)]
    private let data = Data("reply".utf8)
    private var soon: ContinuousClock.Instant { ContinuousClock.now + .seconds(1) }

    private func seconds(_ d: Duration) -> Double {
        Double(d.components.seconds) + Double(d.components.attoseconds) / 1e18
    }

    @Test func fulfilledEqualRequestIsClaimed() async {
        let slot = LaunchPrefetchSlot(request: r)
        await slot.fulfil(data)
        #expect(await slot.claim(r, until: soon) == data)
    }

    @Test func differentFieldsFallBackAndEmptyTheSlot() async {
        let slot = LaunchPrefetchSlot(request: r)
        await slot.fulfil(data)
        var other = r
        other["limit"] = .int(100)
        #expect(await slot.claim(other, until: soon) == nil)
        #expect(await slot.claim(r, until: soon) == nil)
    }

    @Test func onlyTheFirstClaimCanSucceed() async {
        let slot = LaunchPrefetchSlot(request: r)
        await slot.fulfil(data)
        #expect(await slot.claim(r, until: soon) == data)
        #expect(await slot.claim(r, until: soon) == nil)
    }

    @Test func inFlightPrefetchIsAwaited() async throws {
        let slot = LaunchPrefetchSlot(request: r)
        let request = r
        let deadline = soon
        let claim = Task { await slot.claim(request, until: deadline) }
        try await Task.sleep(for: .milliseconds(20))
        await slot.fulfil(data)
        #expect(await claim.value == data)
    }

    @Test func failedPrefetchReturnsNilAtOnce() async {
        let slot = LaunchPrefetchSlot(request: r)
        await slot.fulfil(nil)
        let clock = ContinuousClock()
        var result: Data?
        let elapsed = await clock.measure { result = await slot.claim(r, until: clock.now + .seconds(5)) }
        #expect(result == nil)
        #expect(seconds(elapsed) < 0.5)
    }

    @Test func inFlightFailureReturnsNilPromptly() async throws {
        let slot = LaunchPrefetchSlot(request: r)
        let request = r
        let clock = ContinuousClock()
        let start = clock.now
        let claim = Task { await slot.claim(request, until: clock.now + .seconds(5)) }
        try await Task.sleep(for: .milliseconds(20))
        await slot.fulfil(nil)
        #expect(await claim.value == nil)
        #expect(seconds(clock.now - start) < 0.5)
    }

    @Test func claimGivesUpAtTheDeadline() async {
        let slot = LaunchPrefetchSlot(request: r)
        let clock = ContinuousClock()
        var result: Data?
        let elapsed = await clock.measure { result = await slot.claim(r, until: clock.now + .milliseconds(50)) }
        #expect(result == nil)
        #expect(seconds(elapsed) >= 0.04)
        #expect(seconds(elapsed) < 1)
    }

    @Test func fulfilAfterTheClaimIsIgnored() async {
        let slot = LaunchPrefetchSlot(request: r)
        let clock = ContinuousClock()
        #expect(await slot.claim(r, until: clock.now + .milliseconds(30)) == nil)
        await slot.fulfil(data)
        #expect(await slot.claim(r, until: soon) == nil)
    }

    @Test func equalityIsStructural() async {
        var a: [String: JSONValue] = [:]
        a["list"] = .string("songs"); a["revision"] = .null; a["offset"] = .int(0); a["limit"] = .int(200)
        var b: [String: JSONValue] = [:]
        b["limit"] = .int(200); b["offset"] = .int(0); b["revision"] = .null; b["list"] = .string("songs")
        let slot = LaunchPrefetchSlot(request: a)
        await slot.fulfil(data)
        #expect(await slot.claim(b, until: soon) == data)
    }
}

/// The launch prefetch's request must equal the Songs list's first fetch, field for field.
@MainActor @Suite struct LaunchSongsRequestTests {
    private struct Row: Identifiable, Sendable { let id: UInt64 }

    @MainActor private final class Recorder {
        var requests: [(offset: Int, limit: Int, revision: UInt64?)] = []
    }

    /// The first window a reset cache asks for: `ListModel` resets its cache for each description.
    private func firstWindow() async throws -> (offset: Int, limit: Int, revision: UInt64?) {
        let recorder = Recorder()
        let cache = ListWindowCache<Row> { _, _, _ in throw WindowFetchError.failed("not started") }
        cache.reset { @MainActor offset, limit, revision in
            recorder.requests.append((offset, limit, revision))
            throw WindowFetchError.failed("test")
        }
        for _ in 0..<500 where recorder.requests.isEmpty { await Task.yield() }
        return try #require(recorder.requests.first)
    }

    /// What `ListModel` sends first for Songs: `currentDescription()` with a new list's state
    /// (no picked sort, an empty filter, a new browser selection when the browser is shown).
    private func listModelFirstFetch(group: Grouping, browserShown: Bool) async throws -> [String: JSONValue] {
        let window = try await firstWindow()
        let list = TrackListRequest.description(
            source: .library, browser: browserShown ? BrowserSelection().browser : nil,
            query: "", sort: [], defaultSort: [], group: group)
        return try TrackListRequest.fields(list: list, revision: window.revision, offset: window.offset,
                                           limit: window.limit)
    }

    @Test func defaultDefaultsMatchTheFirstSongsFetch() async throws {
        let launch = try TrackListRequest.launchSongs(defaultSort: [], group: .none)
        #expect(try await listModelFirstFetch(group: .none, browserShown: false) == launch)
        #expect(try await listModelFirstFetch(group: .none, browserShown: true) == launch)
        #expect(launch["offset"] == .int(0))
        #expect(launch["limit"] == .int(200))
        #expect(launch["revision"] == JSONValue.null)
    }

    @Test func songsGroupedByAlbumMatchTheFirstSongsFetch() async throws {
        let launch = try TrackListRequest.launchSongs(defaultSort: [], group: .album)
        #expect(try await listModelFirstFetch(group: .album, browserShown: false) == launch)
        #expect(try await listModelFirstFetch(group: .album, browserShown: true) == launch)
        #expect(launch != (try TrackListRequest.launchSongs(defaultSort: [], group: .none)))
    }

    @Test func aPickedSortOrFilterDoesNotMatch() throws {
        let launch = try TrackListRequest.launchSongs(defaultSort: [], group: .none)
        let sorted = TrackListRequest.description(source: .library, browser: nil, query: "",
                                                  sort: [SortKey(field: .title, descending: false)],
                                                  defaultSort: [], group: .none)
        let filtered = TrackListRequest.description(source: .library, browser: nil, query: "a", sort: [],
                                                    defaultSort: [], group: .none)
        #expect(try TrackListRequest.fields(list: sorted, revision: nil, offset: 0, limit: 200) != launch)
        #expect(try TrackListRequest.fields(list: filtered, revision: nil, offset: 0, limit: 200) != launch)
    }
}
