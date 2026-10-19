import Testing
@testable import TyrannyCore

private struct TestRow: Identifiable, Sendable, Equatable {
    let id: UInt64
    let label: String
}

/// Manual time for the refresh throttle. Sleeps resume only when the test advances `now`.
/// The cache starts its sleep in a new task, so tests drain before advancing.
@MainActor
private final class ManualClock {
    private(set) var now: Duration = .zero
    private var sleepers: [(deadline: Duration, continuation: CheckedContinuation<Void, any Error>)] = []

    var sleep: ListWindowCache<TestRow>.Sleep {
        { duration in try await self.sleep(for: duration) }
    }

    func sleep(for duration: Duration) async throws {
        try await withCheckedThrowingContinuation { sleepers.append((now + duration, $0)) }
    }

    func advance(to time: Duration) {
        now = time
        let due = sleepers.filter { $0.deadline <= time }
        sleepers.removeAll { $0.deadline <= time }
        for sleeper in due { sleeper.continuation.resume() }
    }
}

/// A server list backed by an array. It logs every request. Replies are built when a request
/// completes, so changes made while a request is in flight show up in its reply, as on the server.
@MainActor
private final class FakeList {
    struct Request: Equatable {
        let offset: Int
        let limit: Int
        let revision: UInt64?
        let at: Duration
    }

    var rows: [TestRow]
    private(set) var revision: UInt64 = 1
    var autoComplete: Bool
    var clock: ManualClock?
    private(set) var log: [Request] = []
    private var pending: [(request: Request, continuation: CheckedContinuation<WindowPage<TestRow>, any Error>)] = []
    private var nextID: UInt64

    init(count: Int, autoComplete: Bool = false) {
        rows = (0..<count).map { TestRow(id: UInt64($0 + 1), label: "\($0 + 1)@1") }
        nextID = UInt64(count + 1)
        self.autoComplete = autoComplete
    }

    var fetch: ListWindowCache<TestRow>.Fetch {
        { offset, limit, revision in try await self.serve(offset, limit, revision) }
    }

    var pendingCount: Int { pending.count }

    func serve(_ offset: Int, _ limit: Int, _ revision: UInt64?) async throws -> WindowPage<TestRow> {
        let request = Request(offset: offset, limit: limit, revision: revision, at: clock?.now ?? .zero)
        log.append(request)
        if autoComplete { return try page(for: request) }
        return try await withCheckedThrowingContinuation { pending.append((request, $0)) }
    }

    func requests(offset: Int) -> [Request] { log.filter { $0.offset == offset } }

    /// Completes every pending request, oldest first.
    func completeAll() {
        let batch = pending
        pending.removeAll()
        for item in batch { resume(item) }
    }

    /// Completes the oldest pending request for `offset`.
    func complete(offset: Int) {
        guard let index = pending.firstIndex(where: { $0.request.offset == offset }) else {
            Issue.record("no pending request at offset \(offset)")
            return
        }
        resume(pending.remove(at: index))
    }

    /// Changes every row's content without moving rows, and bumps the revision.
    func touch() {
        revision += 1
        rows = rows.map { TestRow(id: $0.id, label: "\($0.id)@\(revision)") }
    }

    func insert(_ count: Int, at position: Int) {
        revision += 1
        let added = (0..<count).map { offset in TestRow(id: nextID + UInt64(offset), label: "new@\(revision)") }
        nextID += UInt64(count)
        rows.insert(contentsOf: added, at: position)
    }

    func remove(id: UInt64) {
        revision += 1
        rows.removeAll { $0.id == id }
    }

    private func resume(_ item: (request: Request, continuation: CheckedContinuation<WindowPage<TestRow>, any Error>)) {
        do { item.continuation.resume(returning: try page(for: item.request)) } catch { item.continuation.resume(throwing: error) }
    }

    private func page(for request: Request) throws -> WindowPage<TestRow> {
        if let pinned = request.revision, pinned != revision { throw WindowFetchError.stale }
        let start = min(request.offset, rows.count)
        let end = min(request.offset + request.limit, rows.count)
        return WindowPage(revision: revision, total: rows.count, offset: request.offset, rows: Array(rows[start..<end]))
    }
}

/// Lets main-actor tasks started by the cache run until `condition` holds.
@MainActor
private func until(_ what: String = "condition", _ condition: () -> Bool) async {
    for _ in 0..<10_000 {
        if condition() { return }
        await Task.yield()
    }
    Issue.record("timed out waiting for \(what)")
}

/// Lets every runnable main-actor task finish.
@MainActor
private func drain() async {
    for _ in 0..<500 { await Task.yield() }
}

@MainActor
struct ListWindowCacheTests {
    @Test func firstAccessLoadsOneWindowAndReportsTotal() async {
        let fake = FakeList(count: 1234)
        let cache = ListWindowCache(fetch: fake.fetch)
        #expect(cache.total == 0)
        #expect(cache.revision == nil)
        #expect(cache.row(at: 0) == nil)
        await until("first request") { fake.pendingCount == 1 }
        #expect(fake.log.map { [$0.offset, $0.limit] } == [[0, 200]])
        #expect(fake.log.first?.revision == nil)

        let generation = cache.generation
        fake.completeAll()
        await until("first window") { cache.total == 1234 }
        #expect(cache.revision == 1)
        #expect(cache.generation != generation)
        #expect(cache.row(at: 0)?.id == 1)
        #expect(cache.row(at: 199)?.id == 200)
        await drain()
        #expect(fake.log.count == 1)
        #expect(cache.row(at: 1234) == nil)
        #expect(cache.row(at: -1) == nil)
        await drain()
        #expect(fake.log.count == 1)
    }

    @Test func visibleRangePrefetchesNeighbouringWindowsOnly() async {
        let fake = FakeList(count: 50_000, autoComplete: true)
        let cache = ListWindowCache(fetch: fake.fetch)
        cache.visible(1000..<1050)
        await until("three windows") { fake.log.count == 3 }
        await drain()
        #expect(Set(fake.log.map(\.offset)) == [800, 1000, 1200])
        #expect(fake.log.allSatisfy { $0.limit == 200 })
        #expect(cache.row(at: 800)?.id == 801)
        #expect(cache.row(at: 1399)?.id == 1400)

        cache.visible(0..<50)
        await until("top windows") { fake.log.count == 5 }
        await drain()
        #expect(Set(fake.log.dropFirst(3).map(\.offset)) == [0, 200])

        cache.visible(49_950..<50_000)
        await until("bottom windows") { fake.log.count == 7 }
        await drain()
        #expect(Set(fake.log.dropFirst(5).map(\.offset)) == [49_600, 49_800])
        #expect(cache.row(at: 49_999)?.id == 50_000)
    }

    @Test func evictsLeastRecentlyVisibleBeyondTen() async {
        let fake = FakeList(count: 50_000, autoComplete: true)
        let cache = ListWindowCache(fetch: fake.fetch)
        for step in 0..<15 {
            cache.visible(step * 200..<step * 200 + 50)
            let wanted = Set(max(0, step - 1)...(step + 1))
            await until("windows around \(step)") { wanted.isSubset(of: Set(cache.loadedWindowIndices)) }
            await drain()
            #expect(cache.loadedWindowIndices.count <= 10)
            #expect(wanted.isSubset(of: Set(cache.loadedWindowIndices)))
        }
        #expect(cache.loadedWindowIndices == Array(6...15))
        #expect(cache.row(at: 14 * 200 + 10)?.id == UInt64(14 * 200 + 11))
    }

    @Test func duplicateRequestsForAWindowCoalesce() async {
        let fake = FakeList(count: 1000)
        let cache = ListWindowCache(fetch: fake.fetch)
        #expect(cache.row(at: 5) == nil)
        #expect(cache.row(at: 10) == nil)
        cache.visible(0..<20)
        cache.visible(0..<30)
        #expect(cache.row(at: 199) == nil)
        await until("requests") { fake.pendingCount == 2 }
        await drain()
        #expect(fake.requests(offset: 0).count == 1)
        #expect(fake.requests(offset: 200).count == 1)

        fake.completeAll()
        await until("rows") { cache.row(at: 5) != nil }
        #expect(cache.row(at: 10)?.id == 11)
        await drain()
        #expect(fake.log.count == 2)
    }

    @Test func staleErrorRestartsAtNewRevision() async {
        let fake = FakeList(count: 5000)
        let cache = ListWindowCache(fetch: fake.fetch)
        cache.visible(0..<50)
        await until("top requests") { fake.pendingCount == 2 }
        fake.completeAll()
        await until("top windows") { cache.row(at: 0) != nil && cache.row(at: 200) != nil }
        #expect(cache.revision == 1)

        // The list changes on the server; this client has not heard yet.
        fake.insert(3, at: 0)
        cache.visible(1000..<1050)
        await until("pinned requests") { fake.pendingCount == 3 }
        #expect(fake.log.suffix(3).allSatisfy { $0.revision == 1 })

        let logged = fake.log.count
        fake.completeAll()
        await until("restart requests") { fake.pendingCount == 3 }
        await drain()
        // All windows were dropped, and only the visible windows are refetched, without a revision.
        #expect(cache.loadedWindowIndices.isEmpty)
        let restart = fake.log.dropFirst(logged)
        #expect(restart.count == 3)
        #expect(Set(restart.map(\.offset)) == [800, 1000, 1200])
        #expect(restart.allSatisfy { $0.revision == nil })

        fake.completeAll()
        await until("new revision") { cache.revision == 2 && cache.row(at: 1000) != nil }
        #expect(cache.row(at: 1000)?.id == 998)
        #expect(cache.total == 5003)

        // Later windows pin the adopted revision.
        cache.visible(3000..<3050)
        await until("next requests") { fake.pendingCount == 3 }
        #expect(fake.log.suffix(3).allSatisfy { $0.revision == 2 })
    }

    @Test func anchorPositionFollowsInsertionAboveIt() async {
        let clock = ManualClock()
        let fake = FakeList(count: 1000, autoComplete: true)
        let cache = ListWindowCache(sleep: clock.sleep, fetch: fake.fetch)
        cache.visible(400..<450)
        await until("visible windows") { Set(cache.loadedWindowIndices) == [1, 2, 3] }
        guard let anchor = cache.row(at: 420)?.id else {
            Issue.record("anchor row missing")
            return
        }

        fake.insert(5, at: 10)
        cache.serverRevisionChanged(fake.revision, anchor: anchor)
        await drain()
        // Old rows stay on screen until the refetch lands: no placeholders, no jump.
        #expect(cache.row(at: 420)?.id == anchor)
        #expect(cache.anchorPosition == nil)

        clock.advance(to: .milliseconds(500))
        await until("anchor restored") { cache.anchorPosition != nil }
        #expect(cache.anchorPosition == 425)
        #expect(cache.position(of: anchor) == 425)
        #expect(cache.revision == 2)
        #expect(cache.row(at: 425)?.id == anchor)

        // When the anchor row is gone, the refetch reports nil.
        fake.remove(id: anchor)
        cache.serverRevisionChanged(fake.revision, anchor: anchor)
        await drain()
        clock.advance(to: .milliseconds(1000))
        await until("revision 3") { cache.revision == 3 }
        await drain()
        #expect(cache.anchorPosition == nil)
        #expect(cache.position(of: anchor) == nil)
    }

    @Test func revision_storm_refetches_visible_window_at_most_twice_per_second() async {
        let clock = ManualClock()
        let fake = FakeList(count: 1000, autoComplete: true)
        fake.clock = clock
        let cache = ListWindowCache(sleep: clock.sleep, fetch: fake.fetch)
        cache.visible(0..<50)
        await until("visible windows") { Set(cache.loadedWindowIndices) == [0, 1] }
        let initial = fake.log.count

        // 20 revision changes in one second, 50 ms apart.
        for step in 0..<20 {
            clock.advance(to: .milliseconds(50 * step))
            await drain()
            fake.touch()
            cache.serverRevisionChanged(fake.revision, anchor: nil)
            await drain()
            #expect(cache.row(at: 10) != nil, "placeholder during the storm at step \(step)")
        }
        clock.advance(to: .milliseconds(1000))
        await until("final revision") { cache.row(at: 10)?.label == "11@21" }
        await drain()

        let refetches = fake.log.dropFirst(initial)
        for offset in [0, 200] {
            let times = refetches.filter { $0.offset == offset }.map(\.at)
            #expect(times.count <= 2, "window at \(offset) refetched \(times.count) times")
            #expect(zip(times, times.dropFirst()).allSatisfy { $1 - $0 >= .milliseconds(500) })
        }
        #expect(refetches.allSatisfy { $0.offset == 0 || $0.offset == 200 })
        #expect(cache.revision == 21)
        #expect(cache.row(at: 210)?.label == "211@21")
    }

    @Test func session_change_discards_in_flight_windows() async {
        let fake = FakeList(count: 1000)
        let cache = ListWindowCache(fetch: fake.fetch)
        cache.visible(0..<50)
        await until("top requests") { fake.pendingCount == 2 }
        fake.complete(offset: 0)
        await until("first window") { cache.row(at: 0) != nil }
        // Window 1 is still in flight when the connection drops.

        cache.discard(session: 1)
        #expect(cache.row(at: 0) == nil, "rows from the old session stay visible")
        #expect(cache.loadedWindowIndices.isEmpty)
        await until("refetch in the new session") { fake.pendingCount == 3 }

        // The new server session serves different content.
        fake.touch()
        fake.complete(offset: 200)  // the old-session reply lands late
        await drain()
        #expect(cache.row(at: 200) == nil, "an old-session reply was applied")
        #expect(cache.loadedWindowIndices.isEmpty)

        fake.completeAll()
        await until("new-session rows") { cache.row(at: 0) != nil && cache.row(at: 200) != nil }
        #expect(cache.row(at: 0)?.label == "1@2")
        #expect(cache.row(at: 200)?.label == "201@2")
        #expect(cache.revision == 2)

        // Discarding the current session again changes nothing.
        let generation = cache.generation
        cache.discard(session: 1)
        #expect(cache.generation == generation)
        #expect(cache.row(at: 0) != nil)
    }

    @Test func resetDropsEverythingAndLoadsTheFirstWindowOfTheNewList() async {
        let old = FakeList(count: 1000)
        let cache = ListWindowCache(fetch: old.fetch)
        cache.visible(0..<50)
        await until("old requests") { old.pendingCount == 2 }
        old.complete(offset: 0)
        await until("old rows") { cache.row(at: 0) != nil }

        let new = FakeList(count: 30)
        new.touch()
        cache.reset(fetch: new.fetch)
        #expect(cache.total == 0)
        #expect(cache.revision == nil)
        #expect(cache.loadedWindowIndices.isEmpty)
        await until("new request") { new.pendingCount == 1 }
        old.completeAll()  // a reply for the old list lands late
        await drain()
        #expect(cache.loadedWindowIndices.isEmpty)

        new.completeAll()
        await until("new rows") { cache.total == 30 }
        #expect(cache.row(at: 0)?.label == "1@2")
        #expect(cache.row(at: 30) == nil)
        #expect(new.log.count == 1)
    }

    @Test func offScreenReplyRequestedBeforeARevisionChangeIsDropped() async {
        let clock = ManualClock()
        let fake = FakeList(count: 5000)
        let cache = ListWindowCache(sleep: clock.sleep, fetch: fake.fetch)
        cache.visible(0..<50)
        await until("top requests") { fake.pendingCount == 2 }
        fake.completeAll()
        await until("top rows") { cache.row(at: 200) != nil }

        #expect(cache.row(at: 2000) == nil)  // off screen, requested directly
        await until("off-screen request") { fake.pendingCount == 1 }
        // A history-style change: rows change content, the page revision does not.
        cache.serverRevisionChanged(42, anchor: nil)
        fake.complete(offset: 2000)
        await drain()
        #expect(!cache.loadedWindowIndices.contains(10))
        #expect(cache.row(at: 0)?.label == "1@1", "visible rows stay until the throttled refetch")
    }
}
