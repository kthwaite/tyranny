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
/// without a revision. After `serverRevisionChanged`, the visible windows keep showing their old
/// rows until a throttled refetch replaces them. The refetch is unpinned, and the cache adopts the
/// newest page revision it sees. Revisions are assumed to grow.
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
    /// Bumps when rows change. Views observe this.
    public private(set) var generation: UInt64 = 0
    /// The anchor row's position after the last `serverRevisionChanged` refetch settled.
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
    }

    @ObservationIgnored private let windowSize: Int
    @ObservationIgnored private let maxWindows: Int
    @ObservationIgnored private let refreshInterval: Duration
    @ObservationIgnored private let sleep: Sleep
    @ObservationIgnored private var fetch: Fetch

    @ObservationIgnored private var windows: [Int: Window] = [:]
    @ObservationIgnored private var inFlight: [Int: Flight] = [:]
    @ObservationIgnored private var failed: Set<Int> = []
    /// Windows covering the last visible range ± 1 window. They are never evicted.
    @ObservationIgnored private var wanted: Set<Int> = []
    @ObservationIgnored private var lastVisible: Range<Int>?
    /// Last `visible` tick per window, for least-recently-visible eviction.
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

    @ObservationIgnored private var refreshTask: Task<Void, Never>?
    @ObservationIgnored private var refreshEpoch: UInt64 = 0

    /// `sleep` is the throttle's timer. Tests inject a manual one.
    public init(
        windowSize: Int = 200, maxWindows: Int = 10, refreshInterval: Duration = .milliseconds(500),
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

    /// Loaded window indices, ascending.
    var loadedWindowIndices: [Int] { windows.keys.sorted() }

    // MARK: Reading

    /// nil is a placeholder. A missing window is requested. A failed window is not retried here; it is
    /// retried when it newly enters the visible range, on a revision change, or after `discard`/`reset`.
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

    /// Prefetches the windows covering `range` ± 1 window and marks them most recently visible.
    public func visible(_ range: Range<Int>) {
        lastVisible = range
        let next = windows(covering: range)
        tick &+= 1
        for index in next { ticks[index] = tick }
        failed.subtract(next.subtracting(wanted))
        wanted = next
        let center = range.isEmpty ? 0 : (range.lowerBound + range.upperBound) / 2 / windowSize
        for index in next.sorted(by: { abs($0 - center) < abs($1 - center) })
        where windows[index] == nil && inFlight[index] == nil && !failed.contains(index) {
            request(index)
        }
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
    /// counter may differ from the page revision; the refetch is unpinned and adopts the page
    /// revision. Visible windows keep their rows until the refetch lands. Refetches run at most
    /// once per `refreshInterval`. Other windows are dropped.
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
        scheduleRefresh()
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

    private func request(_ index: Int) {
        guard inFlight[index] == nil else { return }
        nextToken &+= 1
        let token = nextToken
        let pinnedRevision = unsettled ? nil : revision
        let flight = Flight(token: token, session: session, change: changes, pinned: pinnedRevision != nil)
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
        switch result {
        case .success(let page):
            apply(page, index: index, change: flight.change)
        case .failure(WindowFetchError.stale) where flight.pinned:
            restartAfterStale(index)
        case .failure:
            failed.insert(index)
        }
    }

    private func apply(_ page: WindowPage<Row>, index: Int, change: UInt64) {
        // Requested before the latest change, and off screen: nobody waits for it, and it is outdated.
        guard change == changes || isTarget(index) else {
            settleIfReady()
            return
        }
        if let current = revision, page.revision < current {
            // An unpinned reply older than the adopted revision. Keep it only to fill a placeholder.
            if windows[index] == nil, isTarget(index) {
                windows[index] = Window(rows: page.rows, revision: page.revision, change: change)
                generation &+= 1
            }
            refreshIfOutdated()
            settleIfReady()
            return
        }
        if let current = revision, page.revision > current {
            // The list moved on. Windows at older revisions have stale positions: drop those
            // nobody looks at, and keep visible ones on screen until the refetch replaces them.
            unsettled = true
            let keep = refreshTargets
            windows = windows.filter { $0.value.revision == page.revision || keep.contains($0.key) }
        }
        revision = page.revision
        let totalChanged = !hasTotal || total != page.total
        total = page.total
        hasTotal = true
        windows[index] = Window(rows: page.rows, revision: page.revision, change: change)
        generation &+= 1
        if totalChanged, let lastVisible { wanted = windows(covering: lastVisible) }
        evict()
        refreshIfOutdated()
        settleIfReady()
    }

    private func restartAfterStale(_ index: Int) {
        let targets = wanted.isEmpty ? [index] : wanted
        dropAll()
        for target in targets.sorted() { request(target) }
    }

    /// Drops rows, in-flight requests and the pending refresh. Keeps the visible range and total.
    private func dropAll() {
        windows = [:]
        inFlight = [:]
        failed = []
        unsettled = true
        refreshTask?.cancel()
        refreshTask = nil
        refreshEpoch &+= 1
        generation &+= 1
    }

    // MARK: Refresh throttle

    /// Windows a revision change refreshes: the visible ones, or every loaded one before `visible`.
    private var refreshTargets: Set<Int> {
        wanted.isEmpty ? Set(windows.keys).union(inFlight.keys) : wanted
    }

    private func isTarget(_ index: Int) -> Bool {
        wanted.isEmpty || wanted.contains(index)
    }

    private func isOutdated(_ window: Window) -> Bool {
        window.revision != revision || window.change < changes
    }

    private func refreshIfOutdated() {
        let outdated = refreshTargets.contains { index in
            inFlight[index] == nil && windows[index].map(isOutdated) == true
        }
        if outdated { scheduleRefresh() }
    }

    /// Trailing throttle: one refresh pass `refreshInterval` after the first change of a burst.
    /// Later changes in the burst join that pass, so passes are at least `refreshInterval` apart.
    private func scheduleRefresh() {
        guard refreshTask == nil else { return }
        let epoch = refreshEpoch
        let sleep = self.sleep
        let interval = refreshInterval
        refreshTask = Task { [weak self] in
            try? await sleep(interval)
            guard let self, self.refreshEpoch == epoch else { return }
            self.refreshTask = nil
            self.runRefresh()
        }
    }

    private func runRefresh() {
        for index in refreshTargets.sorted() where inFlight[index] == nil {
            if let window = windows[index], !isOutdated(window) { continue }
            request(index)
        }
    }

    /// Settled: every refresh target is loaded at the adopted revision, after the latest change.
    /// Then later requests pin the revision again and the anchor is reported.
    private func settleIfReady() {
        guard unsettled, revision != nil else { return }
        for index in refreshTargets {
            guard inFlight[index] == nil, let window = windows[index], !isOutdated(window) else { return }
        }
        unsettled = false
        if anchorRequested {
            anchorRequested = false
            anchorPosition = pendingAnchor.flatMap { position(of: $0) }
            pendingAnchor = nil
        }
    }

    // MARK: Bounds

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

    /// Evicts least-recently-visible windows beyond `maxWindows`. Visible windows stay.
    private func evict() {
        guard windows.count > maxWindows else { return }
        let candidates = windows.keys
            .filter { !wanted.contains($0) }
            .sorted { (ticks[$0] ?? 0, $0) < (ticks[$1] ?? 0, $1) }
        for index in candidates.prefix(windows.count - maxWindows) {
            windows[index] = nil
            ticks[index] = nil
        }
        ticks = ticks.filter { windows[$0.key] != nil || inFlight[$0.key] != nil || wanted.contains($0.key) }
    }
}
