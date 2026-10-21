#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for the Stats page (`--stats-smoke-output`). The gate prepares a
/// 105-track library and no history. Controls are driven through the external Accessibility
/// driver: ⌘8 opens the page, ‹ and › step the period, and every panel shows its empty state.
@MainActor
enum NativeStatsSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    static let tracks = 105

    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-stats-smoke")
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]

    static weak var stats: StatsModel?

    /// The panels that show an empty state when the period, or all history, has no plays.
    static let emptyPanels = ["top-artists", "top-tracks", "over-time", "heatmap", "new-artists",
                              "most-skipped", "stations", "lapsed-loves", "shallow-dives"]

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--stats-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("connection", model) {
                    model.connected && model.snapshot?.library.tracks == tracks
                        && model.snapshot?.library.scan.phase == "complete"
                }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                try await exercise(model, navigation)
                try finish(model, error: nil)
            } catch {
                try? capture("stats-failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        let calendar = Calendar.statsCalendar()

        // ⌘8 opens Stats from the Go menu, even with the filter field focused.
        try await drive("exists", "main-songs")
        try await drive("key", "library-filter", "command-8")
        try await wait("stats destination", model) { navigation.destination == .stats }
        try await drive("exists", "main-stats")
        try await wait("stats page", model) { stats != nil }
        guard let stats else { throw Failure(message: "Stats model missing") }
        try await wait("stats report", model) { stats.report != nil || stats.error != nil }
        try check(stats.error == nil, "Stats shows an error: \(stats.error ?? "")")
        cases.append("native-stats-opens-at-command-8")

        // The header names this week, and › is disabled on the current period.
        try check(stats.period.kind == .week, "Stats does not default to Week")
        let week = StatsPeriod.current(.week, now: Date())
        try await drive("expect-text", "stats-period-label", week.label(calendar: calendar))
        try await drive("expect-enabled", "stats-next", "false")
        try await drive("press", "stats-previous")
        let previous = week.stepped(-1, calendar: calendar)
        try await drive("expect-text", "stats-period-label", previous.label(calendar: calendar))
        try await drive("expect-enabled", "stats-next", "true")
        try await wait("previous week report", model) {
            stats.report != nil && stats.loadedPeriod == stats.period
        }
        try await drive("press", "stats-next")
        try await drive("expect-text", "stats-period-label", week.label(calendar: calendar))
        try await wait("this week report", model) { stats.report != nil && stats.loadedPeriod == stats.period }
        observations["label"] = week.label(calendar: calendar)
        cases.append("native-stats-header-shows-and-steps-the-period")

        // No history: zero tiles and every panel's empty state, without an error.
        guard let report = stats.report else { throw Failure(message: "Stats report missing") }
        try check(report.plays == 0 && report.history_span_days == 0, "The fixture server has history")
        try check(stats.error == nil, "Stats shows an error: \(stats.error ?? "")")
        try await drive("expect-text", "stats-tile-plays", "0")
        try await drive("expect-text", "stats-tile-skip-rate", "—")
        for panel in emptyPanels {
            try await drive("exists", "stats-\(panel)-empty")
        }
        try await drive("expect-text", "stats-lapsed-loves-empty-title", "Not enough history yet — fills in after 90 days")
        // With no plays at all, there is no first play to count 90 days from.
        try await drive("expect-text", "stats-lapsed-loves-empty-detail", "From 90 days after your first play")
        try await Task.sleep(for: .milliseconds(400))
        try capture("stats-empty")
        cases.append("native-stats-empty-history-shows-empty-states")
    }

    // MARK: Helpers

    private static func drive(_ action: String, _ identifier: String, _ value: String? = nil) async throws {
        try await NativeSmoke.drive(action, identifier, value)
    }
    private static func wait(_ label: String, _ model: PlayerModel, seconds: Double = 15,
                             until condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            if Date() >= deadline {
                throw Failure(message: "Timed out waiting for \(label); message=\(model.message ?? "none")")
            }
            try await Task.sleep(for: .milliseconds(40))
        }
        try await Task.sleep(for: .milliseconds(150))
    }
    private static func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }
    private static func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else {
            throw Failure(message: "Native window unavailable")
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        if process.terminationStatus != 0, let frame = window.contentView?.superview,
           let bitmap = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) {
            // Without Screen Recording permission, keep a self-rendered image for review.
            // It is not a composited screenshot and is named so.
            frame.cacheDisplay(in: frame.bounds, to: bitmap)
            try? bitmap.representation(using: .png, properties: [:])?
                .write(to: output.appendingPathComponent(name + "-self-rendered.png"))
        }
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases, "observations": observations,
            "automation": "External Accessibility ⌘8 keystroke and header button presses by identifier; no server writes.",
            "scenario": "stats"]
        if let error { report["error"] = error }
        if let state = model.snapshot {
            report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state))
        }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("STATS_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
