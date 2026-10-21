import Foundation
import Observation
import TyrannyCore

/// The Stats page's report for one period, the years the Wrapped menu offers, and the
/// December Wrapped banner's state.
///
/// The page fetches on appear and when `period` changes. While the page is visible, a new
/// `stats_revision` refetches at most once every 10 s. `wrappedYears` comes from `stats_years`
/// on connect and, debounced the same way, when `history_revision` changes: a year gains its
/// first play only when a play counts.
@MainActor @Observable
final class StatsModel {
    /// Setting it remembers its kind and fetches its report.
    var period: StatsPeriod {
        didSet {
            guard period != oldValue else { return }
            shownAt = Date()
            defaults.set(period.kind.rawValue, forKey: Self.periodKindKey)
            if visible { fetchReport() }
        }
    }
    private(set) var report: StatsReport?
    /// The period `report` was fetched for.
    private(set) var loadedPeriod: StatsPeriod?
    private(set) var error: String?
    private(set) var wrappedYears: [Int] = []

    static let periodKindKey = "stats.periodKind"
    /// The years whose Wrapped banner was dismissed, as an array of `Int`.
    static let bannerDismissedKey = "stats.wrappedBannerDismissed"
    static let refetchInterval: Duration = .seconds(10)

    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private let defaults: UserDefaults
    /// The page is on screen. Only then does a new `stats_revision` refetch the report.
    @ObservationIgnored private var visible = false
    /// When `period` was last chosen or shown. A period current then follows now on appear.
    @ObservationIgnored private var shownAt = Date()
    /// nil while disconnected: the next snapshot is a new connection.
    @ObservationIgnored private var statsRevision: UInt64?
    @ObservationIgnored private var historyRevision: UInt64?
    @ObservationIgnored private let reportThrottle = Throttle(interval: refetchInterval)
    @ObservationIgnored private let yearsThrottle = Throttle(interval: refetchInterval)
    /// Bumped by every report request; an older reply is dropped.
    @ObservationIgnored private var reportRequest: UInt64 = 0
    @ObservationIgnored private var yearsRequest: UInt64 = 0
    /// The banner shows only on the page's first visit since launch.
    @ObservationIgnored private var bannerOffered = false

    init(defaults: UserDefaults) {
        self.defaults = defaults
        let kind = defaults.string(forKey: Self.periodKindKey).flatMap(StatsPeriodKind.init(rawValue:)) ?? .week
        period = .current(kind, now: Date())
    }

    /// The harness suite for gate, smoke and render launches, so they never touch the user's choice.
    static func forLaunch(arguments: [String]) -> StatsModel {
        let defaults = QuitSuspensionStore.suiteName(forArguments: arguments)
            .flatMap(UserDefaults.init(suiteName:)) ?? .standard
        return StatsModel(defaults: defaults)
    }

    func attach(_ player: PlayerModel) { self.player = player }

    var calendar: Calendar { .statsCalendar() }

    // MARK: Following state

    /// Follows the connection, `stats_revision` and `history_revision`. The player calls it on
    /// every snapshot.
    func sync() {
        guard let player else { return }
        guard player.connected, let snapshot = player.snapshot else {
            if statsRevision != nil {
                statsRevision = nil
                historyRevision = nil
                reportThrottle.cancel()
                yearsThrottle.cancel()
                reportRequest &+= 1
                yearsRequest &+= 1
            }
            return
        }
        let revision = snapshot.stats_revision
        if statsRevision == nil {
            statsRevision = revision
            historyRevision = snapshot.history_revision
            fetchYears()
            if visible { fetchReport() }
            return
        }
        if snapshot.history_revision != historyRevision {
            historyRevision = snapshot.history_revision
            yearsThrottle.request { [weak self] in self?.sendYears() }
        }
        if revision != statsRevision {
            statsRevision = revision
            if visible { reportThrottle.request { [weak self] in self?.sendReport() } }
        }
    }

    /// The page calls these from `onAppear` and `onDisappear`. A page left on the current
    /// period shows the current period again, however long the app has run.
    func appeared() {
        let now = Date()
        period = period.refreshed(shownAt: shownAt, now: now, calendar: calendar)
        shownAt = now
        visible = true
        fetchReport()
    }

    func disappeared() {
        visible = false
        reportThrottle.cancel()
    }

    // MARK: Wrapped banner

    /// Whether this visit shows the Wrapped banner for `year`: only the first visit since
    /// launch, and only until the year is dismissed.
    func takeBannerOffer(year: Int) -> Bool {
        guard !bannerOffered else { return false }
        bannerOffered = true
        let dismissed = Set(defaults.array(forKey: Self.bannerDismissedKey) as? [Int] ?? [])
        return WrappedOffer.showsBanner(year: year, dismissed: dismissed)
    }

    func dismissBanner(year: Int) {
        let dismissed = Set(defaults.array(forKey: Self.bannerDismissedKey) as? [Int] ?? []).union([year])
        defaults.set(dismissed.sorted(), forKey: Self.bannerDismissedKey)
    }

    // MARK: Fetching

    private func fetchReport() { reportThrottle.now { [weak self] in self?.sendReport() } }
    private func fetchYears() { yearsThrottle.now { [weak self] in self?.sendYears() } }

    private func sendReport() {
        guard let player, player.connected else { return }
        reportRequest &+= 1
        let request = reportRequest
        let period = period
        let fields: [String: JSONValue] = ["period": .object([
            "kind": .string(period.kind.rawValue), "anchor": .string(period.anchorString(calendar: calendar))])]
        // A cancelled request would close the shared connection: fetch unstructured.
        Task { @MainActor [weak self] in
            do {
                let reply: StatsResponse = try await player.send("stats", fields)
                guard let self, request == reportRequest else { return }
                report = reply.report
                loadedPeriod = period
                error = nil
            } catch {
                guard let self, request == reportRequest else { return }
                self.error = (error as? ServerFailure)?.message ?? error.localizedDescription
            }
        }
    }

    private func sendYears() {
        guard let player, player.connected else { return }
        yearsRequest &+= 1
        let request = yearsRequest
        Task { @MainActor [weak self] in
            // The menu keeps the years it had; the Stats page shows report errors.
            guard let reply: StatsYearsResponse = try? await player.send("stats_years"),
                  let self, request == yearsRequest else { return }
            wrappedYears = reply.years
        }
    }
}

/// Runs an action at most once per `interval`. A request inside the interval runs once at its end.
@MainActor
private final class Throttle {
    private let interval: Duration
    private var last: ContinuousClock.Instant?
    private var pending: Task<Void, Never>?

    init(interval: Duration) { self.interval = interval }

    /// Runs `action` now and starts a new interval.
    func now(_ action: @escaping @MainActor () -> Void) {
        cancel()
        last = .now
        action()
    }

    func request(_ action: @escaping @MainActor () -> Void) {
        guard pending == nil else { return }
        guard let last, last + interval > .now else { return now(action) }
        let due = last + interval
        pending = Task { @MainActor [weak self] in
            try? await Task.sleep(until: due, clock: .continuous)
            guard !Task.isCancelled, let self else { return }
            pending = nil
            now(action)
        }
    }

    func cancel() {
        pending?.cancel()
        pending = nil
    }
}
