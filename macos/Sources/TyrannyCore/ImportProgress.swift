/// What the onboarding import screen shows for one scan status.
public struct ImportProgress: Equatable, Sendable {
    public enum Stage: Equatable, Sendable { case preparing, discovering, indexing, done, empty, stopped }

    public let stage: Stage
    public let title: String
    /// Nil on Preparing: the view shows the folder path instead.
    public let detail: String?
    /// "12 files couldn't be read". Indexing and Done only.
    public let failureLine: String?
    /// Nil is an indeterminate bar. Set only on Indexing with `total > 0`.
    public let fraction: Double?
}

/// One onboarding import for one root. Holds the state that `ImportProgress` needs between
/// snapshots: the baseline status and the largest fraction shown so far.
public struct ImportSession: Equatable, Sendable {
    public let rootID: UInt64
    /// The status when the session started or retried. While the scan equals it, a terminal
    /// phase is a stale result and reads as Preparing. Cleared for good on the first change.
    private var baseline: ScanStatus?
    private var lastStage: ImportProgress.Stage?
    private var shownFraction: Double?

    public init(rootID: UInt64, baseline: ScanStatus?) {
        self.rootID = rootID
        self.baseline = baseline
    }

    /// Starts a fresh baseline after Try Again; resets the shown fraction.
    public mutating func retry(baseline: ScanStatus) {
        self.baseline = baseline
        lastStage = nil
        shownFraction = nil
    }

    /// The screen for this status. Updates the remembered fraction (monotonic rule).
    public mutating func progress(library: LibraryState) -> ImportProgress {
        let scan = library.scan
        if let baseline, scan != baseline { self.baseline = nil }

        let stage = Self.stage(scan: scan, rootID: rootID, tracks: library.tracks, baseline: baseline)
        if stage != lastStage {
            lastStage = stage
            shownFraction = nil
        }

        var fraction: Double?
        if stage == .indexing, let total = scan.total, total > 0 {
            let raw = Double(min(scan.processed, total)) / Double(total)
            let clamped = min(max(raw, 0), 1)
            fraction = max(shownFraction ?? 0, clamped)
            shownFraction = fraction
        }

        let detail: String?
        switch stage {
        case .preparing: detail = nil
        case .discovering: detail = "Finding music… \(DisplayFormat.count(scan.discovered)) files found"
        case .indexing:
            if let total = scan.total, total > 0 {
                detail = "Reading \(DisplayFormat.count(scan.processed)) of \(DisplayFormat.count(total)) files"
            } else {
                detail = "Reading \(DisplayFormat.count(scan.processed)) files"
            }
        case .done:
            detail = DisplayFormat.totals(ListTotals(
                tracks: UInt64(max(library.tracks, 0)),
                duration_ms: library.duration_ms,
                size_bytes: library.size_bytes))
        case .empty: detail = "Tyranny found no audio files in this folder."
        case .stopped: detail = scan.last_error ?? "The import did not finish."
        }

        return ImportProgress(
            stage: stage,
            title: Self.title(stage),
            detail: detail,
            failureLine: stage == .indexing || stage == .done ? Self.failureLine(scan) : nil,
            fraction: fraction)
    }

    /// Session to start after `add_root` succeeds, or nil. `rootsBefore` is `library.roots`
    /// before the request; `current` is the existing session, if any.
    public static func afterAddRoot(rootID: UInt64, rootsBefore: Int, current: ImportSession?,
                                    baseline: ScanStatus?) -> ImportSession? {
        guard rootsBefore == 0 || current != nil else { return nil }
        return ImportSession(rootID: rootID, baseline: baseline)
    }

    /// Session for the first authoritative snapshot of a launch, or nil.
    public static func atLaunch(library: LibraryState) -> ImportSession? {
        guard library.roots > 0, library.tracks == 0,
              library.scan.phase == "discovering" || library.scan.phase == "indexing",
              let rootID = library.scan.root_id else { return nil }
        return ImportSession(rootID: rootID, baseline: nil)
    }

    private static func stage(scan: ScanStatus, rootID: UInt64, tracks: Int,
                              baseline: ScanStatus?) -> ImportProgress.Stage {
        guard scan.root_id == rootID else { return .preparing }
        switch scan.phase {
        case "discovering": return .discovering
        case "indexing": return .indexing
        case "complete": return baseline != nil ? .preparing : (tracks > 0 ? .done : .empty)
        case "failed", "interrupted": return baseline != nil ? .preparing : .stopped
        default: return .preparing
        }
    }

    private static func title(_ stage: ImportProgress.Stage) -> String {
        switch stage {
        case .preparing, .discovering, .indexing: "Importing Your Music"
        case .done: "Your Music Is Ready"
        case .empty: "No Music Found"
        case .stopped: "Import Stopped"
        }
    }

    private static func failureLine(_ scan: ScanStatus) -> String? {
        let count = scan.failures + scan.skipped
        switch count {
        case ...0: return nil
        case 1: return "1 file couldn't be read"
        default: return "\(DisplayFormat.count(count)) files couldn't be read"
        }
    }
}
