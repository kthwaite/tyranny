import Observation

/// Album summaries for the album blocks in view, from `album_summaries` (spec 4.3).
///
/// `want` names the albums in view. Albums not fetched since the last `invalidate` are
/// requested, at most 500 per request, and an album already in flight is not requested again.
/// `invalidate` (a library revision change) keeps the shown facts and refetches the albums in
/// view. An album the server does not return is not requested again until the next
/// `invalidate`; one whose request failed, until it comes into view again. `reset` (a new
/// session) drops every summary, ignores older replies and requests the albums in view again.
/// At most `maximumEntries` summaries stay; albums out of view go first.
@MainActor @Observable public final class AlbumFactsCache {
    public typealias Fetch = @Sendable (_ albumIDs: [UInt64]) async throws -> [AlbumSummary]

    /// Server batch limit for `album_summaries`.
    public static let batchLimit = 500

    /// Bumps when summaries arrive. Views observe this.
    public private(set) var generation: UInt64 = 0

    @ObservationIgnored private var fetch: Fetch
    @ObservationIgnored private let maximumEntries: Int
    @ObservationIgnored private var entries: [UInt64: AlbumSummary] = [:]
    /// Albums answered since the last `invalidate`.
    @ObservationIgnored private var settled: Set<UInt64> = []
    /// Albums whose last request failed. Retried when they come into view again.
    @ObservationIgnored private var failed: Set<UInt64> = []
    @ObservationIgnored private var inFlight: Set<UInt64> = []
    @ObservationIgnored private var wanted: Set<UInt64> = []
    /// Bumps on `invalidate` and `reset`. A reply from an older epoch does not settle its albums.
    @ObservationIgnored private var epoch: UInt64 = 0
    /// Bumps on `reset`. A reply from an older session is dropped.
    @ObservationIgnored private var session: UInt64 = 0

    public init(maximumEntries: Int = 2_000, fetch: @escaping Fetch) {
        self.maximumEntries = max(maximumEntries, 1)
        self.fetch = fetch
    }

    public var count: Int { entries.count }

    public func summary(_ albumID: UInt64) -> AlbumSummary? { entries[albumID] }

    /// The albums in view now. Requests the ones not settled and not in flight.
    public func want(_ albumIDs: Set<UInt64>) {
        failed.subtract(albumIDs.subtracting(wanted))
        wanted = albumIDs
        request()
    }

    /// The library changed: shown facts stay until fresh ones arrive.
    public func invalidate() {
        epoch &+= 1
        settled.removeAll()
        failed.removeAll()
        request()
    }

    /// A new session or list: drops every summary and ignores replies in flight.
    public func reset(fetch: Fetch? = nil) {
        if let fetch { self.fetch = fetch }
        epoch &+= 1
        session &+= 1
        entries.removeAll()
        settled.removeAll()
        failed.removeAll()
        inFlight.removeAll()
        generation &+= 1
        request()
    }

    private func request() {
        let missing = wanted.subtracting(settled).subtracting(failed).subtracting(inFlight).sorted()
        guard !missing.isEmpty else { return }
        inFlight.formUnion(missing)
        var start = 0
        while start < missing.count {
            let batch = Array(missing[start..<min(start + Self.batchLimit, missing.count)])
            start += batch.count
            send(batch)
        }
    }

    private func send(_ batch: [UInt64]) {
        let fetch = self.fetch, epoch = self.epoch, session = self.session
        Task { @MainActor [weak self] in
            let result: Result<[AlbumSummary], any Error>
            do { result = .success(try await fetch(batch)) } catch { result = .failure(error) }
            self?.receive(result, batch: batch, epoch: epoch, session: session)
        }
    }

    private func receive(_ result: Result<[AlbumSummary], any Error>, batch: [UInt64], epoch: UInt64, session: UInt64) {
        guard session == self.session else { return }
        inFlight.subtract(batch)
        switch result {
        case .success(let summaries):
            for summary in summaries where batch.contains(summary.id) { entries[summary.id] = summary }
            generation &+= 1
            // A reply sent before `invalidate` may be outdated: its albums are requested again.
            if epoch == self.epoch { settled.formUnion(batch) }
        case .failure:
            if epoch == self.epoch { failed.formUnion(batch) }
        }
        evict()
        request()
    }

    private func evict() {
        guard entries.count > maximumEntries else { return }
        for id in entries.keys where !wanted.contains(id) {
            entries.removeValue(forKey: id)
            settled.remove(id)
            if entries.count <= maximumEntries { return }
        }
    }
}
