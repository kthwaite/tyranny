import Observation

/// One page of a server list. `offset` is 0-based. `revision` is the list revision the page was read at.
public struct WindowPage<Row: Sendable>: Sendable {
    public let revision: UInt64
    public let total: Int
    public let offset: Int
    public let rows: [Row]

    public init(revision: UInt64, total: Int, offset: Int, rows: [Row]) {
        self.revision = revision
        self.total = total
        self.offset = offset
        self.rows = rows
    }
}

/// `stale`: the request pinned a revision the server no longer has.
public enum WindowFetchError: Error {
    case stale
    case failed(String)
}

/// Sparse, bounded rows of one server list.
///
/// Rows live in fixed windows of `windowSize` rows. Window `i` covers positions
/// `i * windowSize ..< (i + 1) * windowSize`. At most `maxWindows` windows stay loaded.
/// Each window has at most one request in flight.
///
/// Revisions. While the cache is settled, new windows pin the adopted `revision`, so all windows
/// agree on positions. A `.stale` reply drops every window and refetches the visible windows
/// without a revision. After `serverRevisionChanged`, a refresh pass refetches the visible windows
/// unpinned. The visible windows keep their old rows until every reply of the pass is in; then the
/// pass commits in one step: new rows, the newest page revision, `total` and `anchorPosition`.
/// Revisions are assumed to grow.
///
/// Throttle. The first change after a quiet period starts a pass at once. Each pass starts a
/// cooldown of `refreshInterval`. Changes during the cooldown or during a pass join one trailing
/// pass, which starts when both are over. So passes start at least `refreshInterval` apart.
///
/// Sessions. `discard(session:)` drops all rows and every in-flight request. Replies tagged with an
/// older session are ignored.
@MainActor @Observable public final class ListWindowCache<Row: Sendable & Identifiable> where Row.ID == UInt64 {
    public typealias Fetch = @Sendable (_ offset: Int, _ limit: Int, _ revision: UInt64?) async throws -> WindowPage<Row>
    public typealias Sleep = @Sendable (Duration) async throws -> Void

    /// 0 until the first window arrives.
    public private(set) var total: Int = 0
    /// The adopted page revision. nil until the first window arrives, and after `reset` or `discard`.
    public private(set) var revision: UInt64?
    /// Counts committed refresh passes. Rows may move at the same revision, for example when
    /// play counts change a Plays sort, so selection keyed to positions must remap.
    public private(set) var refreshes: UInt64 = 0
    /// Bumps when rows change. Views observe this.
    public private(set) var generation: UInt64 = 0
    /// The anchor row's position after the last `serverRevisionChanged` refresh pass committed.
    /// nil when the anchor is not in the refetched windows, or no anchor was given.
    public var anchorPosition: Int?

    private struct Window {
        var rows: [Row]
        var revision: UInt64
        /// `changes` when the request for these rows was sent.
        var change: UInt64
    }

    private struct Flight {
        var token: UInt64
        var session: UInt64
        var change: UInt64
        var pinned: Bool
        /// The refresh pass this request belongs to.
        var pass: UInt64?
    }

    /// A refresh pass. Replies are held here until every request of the pass is answered.
    private struct Pass {
        let id: UInt64
        var pending: Set<Int>
        var replies: [Int: (page: WindowPage<Row>, change: UInt64)] = [:]
        var failures: Set<Int> = []
    }

    @ObservationIgnored private let windowSize: Int
    @ObservationIgnored private let maxWindows: Int
    @ObservationIgnored private let refreshInterval: Duration
    @ObservationIgnored private let sleep: Sleep
    @ObservationIgnored private var fetch: Fetch

    @ObservationIgnored private var windows: [Int: Window] = [:]
    @ObservationIgnored private var inFlight: [Int: Flight] = [:]
    /// Windows whose last request failed. Not retried until they newly become visible, the server
    /// revision changes, or the cache is discarded or reset.
    @ObservationIgnored private var failed: Set<Int> = []
    /// Windows covering the last visible range ± 1 window. They are never evicted.
    @ObservationIgnored private var wanted: Set<Int> = []
    @ObservationIgnored private var lastVisible: Range<Int>?
    /// Recency per window, for least-recently-visible eviction. `visible` advances `tick`.
    @ObservationIgnored private var ticks: [Int: UInt64] = [:]
    @ObservationIgnored private var tick: UInt64 = 0
    @ObservationIgnored private var hasTotal = false

    @ObservationIgnored private var session: UInt64 = 0
    @ObservationIgnored private var nextToken: UInt64 = 0
    /// Counts `serverRevisionChanged` calls. A window requested before the latest change is outdated.
    @ObservationIgnored private var changes: UInt64 = 0
    /// true while windows may disagree on revision. Requests are unpinned until the cache settles.
    @ObservationIgnored private var unsettled = true
    @ObservationIgnored private var anchorRequested = false
    @ObservationIgnored private var pendingAnchor: Row.ID?

    @ObservationIgnored private var pass: Pass?
    @ObservationIgnored private var nextPassID: UInt64 = 0
    @ObservationIgnored private var cooldown: Task<Void, Never>?
    @ObservationIgnored private var cooldownEpoch: UInt64 = 0

    /// `sleep` is the throttle's timer. Tests inject a manual one.
    public init(
        windowSize: Int = TrackListRequest.windowSize, maxWindows: Int = 10, refreshInterval: Duration = .milliseconds(500),
        sleep: @escaping Sleep = { try await Task.sleep(for: $0) }, fetch: @escaping Fetch
    ) {
        precondition(windowSize > 0 && windowSize <= 500, "window size must fit the server page limit")
        precondition(maxWindows >= 1, "the cache needs at least one window")
        self.windowSize = windowSize
        self.maxWindows = maxWindows
        self.refreshInterval = refreshInterval
        self.sleep = sleep
        self.fetch = fetch
    }

    /// Loaded window indices, ascending. The 50,000-row scale gate reads it to check the cache bound.
    public var loadedWindowIndices: [Int] { windows.keys.sorted() }

    /// true when every visible window agrees on the adopted revision; new windows then pin it.
    var isSettled: Bool { !unsettled }

    // MARK: Reading

    /// nil is a placeholder. A missing window is requested. Rows at or after `total` are nil and
    /// never requested. A failed window is not retried here.
    public func row(at position: Int) -> Row? {
        _ = generation  // register observation for views that only call row(at:)
        guard position >= 0, !hasTotal || position < total else { return nil }
        let index = position / windowSize
        if let window = windows[index] {
            let offset = position - index * windowSize
            return offset < window.rows.count ? window.rows[offset] : nil
        }
        if inFlight[index] == nil, !failed.contains(index) { request(index) }
        return nil
    }

    /// The loaded row at `position`, or nil. Never requests a window.
    public func loadedRow(at position: Int) -> Row? {
        _ = generation
        guard position >= 0, !hasTotal || position < total else { return nil }
        let index = position / windowSize
        guard let window = windows[index] else { return nil }
        let offset = position - index * windowSize
        return offset < window.rows.count ? window.rows[offset] : nil
    }

    /// Every loaded row with its position, ascending. Never requests a window.
    public func loadedRows() -> [(position: Int, row: Row)] {
        _ = generation
        var result: [(position: Int, row: Row)] = []
        for index in windows.keys.sorted() {
            guard let window = windows[index] else { continue }
            for (offset, row) in window.rows.enumerated() {
                let position = index * windowSize + offset
                if hasTotal, position >= total { break }
                result.append((position, row))
            }
        }
        return result
    }

    /// Prefetches the windows covering `range` ± 1 window and marks them most recently visible.
    public func visible(_ range: Range<Int>) {
        lastVisible = range
        tick &+= 1
        updateWanted()
    }

    /// The row's position, searched only in loaded windows. Current-revision windows win.
    public func position(of id: Row.ID) -> Int? {
        var fallback: Int?
        for (index, window) in windows {
            guard let offset = window.rows.firstIndex(where: { $0.id == id }) else { continue }
            let position = index * windowSize + offset
            if window.revision == revision { return position }
            fallback = fallback ?? position
        }
        return fallback
    }

    // MARK: Invalidation

    /// The server reports a change that may move or alter rows: a library, history, queue or
    /// playlist revision. `revision` is that counter's new value. It is informational, because the
    /// counter may differ from the page revision; the refresh pass is unpinned and adopts the page
    /// revision. Visible windows keep their rows until the pass commits. Other windows are dropped.
    public func serverRevisionChanged(_ revision: UInt64, anchor: Row.ID?) {
        changes &+= 1
        unsettled = true
        failed.removeAll()
        anchorRequested = true
        pendingAnchor = anchor
        let keep = refreshTargets
        let before = windows.count
        windows = windows.filter { keep.contains($0.key) }
        if windows.count != before { generation &+= 1 }
        pumpRefresh()
    }

    /// A new list description: drops everything, then loads the first window.
    public func reset(fetch: @escaping Fetch) {
        self.fetch = fetch
        dropAll()
        revision = nil
        total = 0
        hasTotal = false
        wanted = []
        lastVisible = nil
        ticks = [:]
        anchorRequested = false
        pendingAnchor = nil
        anchorPosition = nil
        request(0)
    }

    /// A new connection session. Rows and in-flight replies from older sessions are dropped; the
    /// visible windows are refetched unpinned. The same session again is a no-op.
    public func discard(session: UInt64) {
        guard session != self.session else { return }
        self.session = session
        dropAll()
        revision = nil
        for index in wanted.sorted() { request(index) }
    }

    // MARK: Requests

    private func request(_ index: Int, pass passID: UInt64? = nil) {
        guard inFlight[index] == nil else { return }
        nextToken &+= 1
        let pinnedRevision = unsettled || passID != nil ? nil : revision
        let flight = Flight(
            token: nextToken, session: session, change: changes, pinned: pinnedRevision != nil, pass: passID)
        inFlight[index] = flight
        let fetch = self.fetch
        let offset = index * windowSize
        let limit = windowSize
        Task { [weak self] in
            let result: Result<WindowPage<Row>, any Error>
            do {
                result = .success(try await fetch(offset, limit, pinnedRevision))
            } catch {
                result = .failure(error)
            }
            self?.receive(index: index, flight: flight, result: result)
        }
    }

    private func receive(index: Int, flight: Flight, result: Result<WindowPage<Row>, any Error>) {
        guard flight.session == session, let current = inFlight[index], current.token == flight.token else { return }
        inFlight[index] = nil
        if let passID = flight.pass, var held = pass, held.id == passID {
            held.pending.remove(index)
            switch result {
            case .success(let page): held.replies[index] = (page, flight.change)
            case .failure: held.failures.insert(index)
            }
            pass = held
            if held.pending.isEmpty { commit(held) }
            return
        }
        switch result {
        case .success(let page):
            apply(page, index: index, change: flight.change)
        case .failure(WindowFetchError.stale) where flight.pinned:
            restartAfterStale(index)
        case .failure:
            failed.insert(index)
            settleIfReady()
        }
    }

    /// A reply outside a refresh pass: it fills a placeholder, or loads a newly visible window.
    private func apply(_ page: WindowPage<Row>, index: Int, change: UInt64) {
        // Requested before the latest change, and off screen: nobody waits for it, and it is outdated.
        guard change == changes || isTarget(index) else {
            settleIfReady()
            return
        }
        if let current = revision, page.revision < current {
            // An unpinned reply older than the adopted revision. Keep it only to fill a placeholder.
            if windows[index] == nil, isTarget(index) {
                store(page, index: index, change: change)
                generation &+= 1
            }
        } else {
            let totalChanged = adopt(page)
            store(page, index: index, change: change)
            generation &+= 1
            if totalChanged { updateWanted() }
        }
        evict(keeping: [index])
        pumpRefresh()
        settleIfReady()
    }

    /// Applies a whole refresh pass in one step. A failed window keeps its old rows or placeholder.
    private func commit(_ done: Pass) {
        pass = nil
        refreshes &+= 1
        var totalChanged = false
        if let newest = done.replies.values.max(by: { $0.page.revision < $1.page.revision })?.page,
            newest.revision >= revision ?? 0 {
            totalChanged = adopt(newest)
        }
        for (index, reply) in done.replies {
            if let existing = windows[index], existing.revision > reply.page.revision { continue }
            store(reply.page, index: index, change: reply.change)
        }
        failed.formUnion(done.failures)
        generation &+= 1
        if totalChanged { updateWanted() }
        evict(keeping: Set(done.replies.keys))
        if anchorRequested {
            anchorPosition = pendingAnchor.flatMap { position(of: $0) }
            if refreshCandidates.isEmpty {
                anchorRequested = false
                pendingAnchor = nil
            }
        }
        pumpRefresh()
        settleIfReady()
    }

    /// Adopts a page's revision and total. When the revision moves on, windows at older revisions
    /// have stale positions: those nobody looks at are dropped; visible ones stay on screen until a
    /// refresh pass replaces them. Returns whether `total` changed; the caller then stores its rows
    /// and calls `updateWanted`, so the window being applied is not requested again.
    private func adopt(_ page: WindowPage<Row>) -> Bool {
        if let current = revision, page.revision > current {
            unsettled = true
            let keep = refreshTargets
            windows = windows.filter { $0.value.revision == page.revision || keep.contains($0.key) }
        }
        revision = page.revision
        let totalChanged = !hasTotal || total != page.total
        total = page.total
        hasTotal = true
        return totalChanged
    }

    private func store(_ page: WindowPage<Row>, index: Int, change: UInt64) {
        windows[index] = Window(rows: page.rows, revision: page.revision, change: change)
        ticks[index] = max(ticks[index] ?? 0, tick)
    }

    private func restartAfterStale(_ index: Int) {
        let targets = wanted.isEmpty ? [index] : wanted
        dropAll()
        for target in targets.sorted() { request(target) }
    }

    /// Drops rows, in-flight requests, the refresh pass and the cooldown. Keeps the visible range
    /// and total.
    private func dropAll() {
        windows = [:]
        inFlight = [:]
        failed = []
        unsettled = true
        pass = nil
        cooldown?.cancel()
        cooldown = nil
        cooldownEpoch &+= 1
        generation &+= 1
    }

    // MARK: Refresh passes

    /// Windows a revision change refreshes: the visible ones, or every loaded one before `visible`.
    private var refreshTargets: Set<Int> {
        wanted.isEmpty ? Set(windows.keys).union(inFlight.keys) : wanted
    }

    /// Refresh targets a pass would fetch now: idle, not failed, and missing or outdated.
    private var refreshCandidates: [Int] {
        refreshTargets.filter { index in
            inFlight[index] == nil && !failed.contains(index) && (windows[index].map(isOutdated) ?? true)
        }.sorted()
    }

    private func isTarget(_ index: Int) -> Bool {
        wanted.isEmpty || wanted.contains(index)
    }

    private func isOutdated(_ window: Window) -> Bool {
        window.revision != revision || window.change < changes
    }

    /// Starts a pass when one is needed, no pass is running and the cooldown is over.
    private func pumpRefresh() {
        guard pass == nil, cooldown == nil else { return }
        let targets = refreshCandidates
        guard !targets.isEmpty else { return }
        nextPassID &+= 1
        pass = Pass(id: nextPassID, pending: Set(targets))
        for index in targets { request(index, pass: nextPassID) }
        let epoch = cooldownEpoch
        let sleep = self.sleep
        let interval = refreshInterval
        cooldown = Task { [weak self] in
            try? await sleep(interval)
            guard let self, self.cooldownEpoch == epoch else { return }
            self.cooldown = nil
            self.pumpRefresh()
        }
    }

    /// Settled: no pass runs and every refresh target is loaded at the adopted revision after the
    /// latest change, or failed. Later requests then pin the revision.
    private func settleIfReady() {
        guard unsettled, pass == nil, revision != nil else { return }
        for index in refreshTargets {
            guard inFlight[index] == nil else { return }
            if failed.contains(index) { continue }
            guard let window = windows[index], !isOutdated(window) else { return }
        }
        unsettled = false
        if anchorRequested {
            anchorRequested = false
            anchorPosition = pendingAnchor.flatMap { position(of: $0) }
            pendingAnchor = nil
        }
    }

    // MARK: Bounds

    /// Recomputes the visible windows from the last visible range and the known total, marks them
    /// recent, and requests the missing ones nearest the centre first.
    private func updateWanted() {
        guard let range = lastVisible else { return }
        let next = windows(covering: range)
        for index in next { ticks[index] = tick }
        failed.subtract(next.subtracting(wanted))
        wanted = next
        let center = range.isEmpty ? 0 : (range.lowerBound + range.upperBound) / 2 / windowSize
        for index in next.sorted(by: { abs($0 - center) < abs($1 - center) })
        where windows[index] == nil && inFlight[index] == nil && !failed.contains(index) {
            request(index)
        }
    }

    private func windows(covering range: Range<Int>) -> Set<Int> {
        guard !range.isEmpty else { return [] }
        var low = max(0, range.lowerBound / windowSize - 1)
        var high = max(0, range.upperBound - 1) / windowSize + 1
        if hasTotal {
            high = min(high, max(0, total - 1) / windowSize)
            low = min(low, high)
        }
        high = min(high, low + maxWindows - 1)
        return Set(low...high)
    }

    /// Evicts least-recently-visible windows beyond `maxWindows`. Visible windows and the windows
    /// just stored stay.
    private func evict(keeping stored: Set<Int>) {
        guard windows.count > maxWindows else { return }
        let candidates = windows.keys
            .filter { !wanted.contains($0) && !stored.contains($0) }
            .sorted { (ticks[$0] ?? 0, $0) < (ticks[$1] ?? 0, $1) }
        for index in candidates.prefix(windows.count - maxWindows) {
            windows[index] = nil
            ticks[index] = nil
        }
        ticks = ticks.filter { windows[$0.key] != nil || inFlight[$0.key] != nil || wanted.contains($0.key) }
    }
}
