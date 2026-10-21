#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for the Stats page and Wrapped (`--stats-smoke-output`). The gate
/// prepares a 105-track library and runs one launch per `--stats-smoke-phase`:
/// - `empty` (default): no history. ⌘8 opens the page, ‹ and › step the period, every panel
///   shows its empty state, and Go › Wrapped is disabled.
/// - `few`: fewer than 50 plays this year. Go › Wrapped › "<year> so far" opens the slideshow
///   on the "not enough" slide, and esc closes it.
/// - `full`: a full year. The slideshow auto-advances, → and ← move, space pauses, the last
///   slide holds, and esc closes it. Then, pretending it is December, the Stats page shows the
///   header button and the banner, and the banner's dismissal is remembered.
/// Controls are driven through the external Accessibility driver.
@MainActor
enum NativeStatsSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    static let tracks = 105

    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-stats-smoke")
    private static var phase = "empty"
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]

    static weak var stats: StatsModel?
    /// The date the Stats page offers Wrapped for, in place of today. Set by the `full` phase.
    static var offerDate: Date?

    /// The panels that show an empty state when the period, or all history, has no plays.
    static let emptyPanels = ["top-artists", "top-tracks", "over-time", "heatmap", "new-artists",
                              "most-skipped", "stations", "lapsed-loves", "shallow-dives"]

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--stats-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        if let index = args.firstIndex(of: "--stats-smoke-phase"), index + 1 < args.count { phase = args[index + 1] }
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("connection", model) {
                    model.connected && model.snapshot?.library.tracks == tracks
                        && model.snapshot?.library.scan.phase == "complete"
                }
                try await NativeSmoke.drive("activate")
                // Keystrokes go to the key window: wait until the library window is key.
                NSApp.windows.first { $0.title == "Tyranny" }?.makeKeyAndOrderFront(nil)
                try await wait("key window", model) { NSApp.keyWindow?.title == "Tyranny" && NSApp.isActive }
                try await Task.sleep(for: .milliseconds(300))
                switch phase {
                case "few": try await exerciseFewPlays(model, navigation)
                case "full": try await exerciseFullYear(model, navigation)
                default: try await exercise(model, navigation)
                }
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

        // No year has plays, so there is no Wrapped to offer.
        try check(stats.wrappedYears.isEmpty, "Wrapped years without history: \(stats.wrappedYears)")
        try await drive("expect-menu-enabled", "Go›Wrapped", "false")
        cases.append("native-stats-wrapped-menu-disabled-without-history")
    }

    /// Fewer than 50 plays this year: the slideshow is one "not enough" slide.
    private static func exerciseFewPlays(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        let year = try await openWrapped(model, navigation)
        try await drive("exists", "wrapped-slide-not-enough")
        try await drive("expect-text", "wrapped-progress", "Slide 1 of 1")
        try await Task.sleep(for: .milliseconds(1200))
        try capture("stats-wrapped-not-enough")
        cases.append("native-stats-wrapped-not-enough-under-50-plays")

        try await drive("key", "wrapped-overlay", "escape")
        try await wait("Wrapped to close", model) { navigation.wrappedYear == nil }
        try await drive("exists", "main-stats")
        observations["year"] = year
        cases.append("native-stats-wrapped-esc-closes")
    }

    /// A full year: every slide, the clock and the keys.
    private static func exerciseFullYear(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        let year = try await openWrapped(model, navigation)
        let reply: WrappedResponse = try await model.send("wrapped", ["year": .int(year)])
        let slides = WrappedDeck.slides(reply.report)
        try check(slides.count > 2, "The seeded year has \(slides.count) slides")
        observations["slides"] = slides.count
        func position(_ index: Int, paused: Bool = false) -> String {
            "Slide \(index) of \(slides.count)" + (paused ? ", paused" : "")
        }

        try await drive("exists", "wrapped-slide-year")
        try await waitText("wrapped-progress", position(1), seconds: 2)
        try await Task.sleep(for: .milliseconds(1200))
        try capture("stats-wrapped-slide-1")
        // The slide moves on by itself after 6 s.
        try await waitText("wrapped-progress", position(2), seconds: 9)
        try await drive("exists", "wrapped-slide-top-artist")
        cases.append("native-stats-wrapped-auto-advances")

        try await drive("key", "wrapped-overlay", "space")
        try await waitText("wrapped-progress", position(2, paused: true), seconds: 2)
        try await drive("key", "wrapped-overlay", "left")
        try await waitText("wrapped-progress", position(1, paused: true), seconds: 2)
        try await drive("key", "wrapped-overlay", "right")
        try await waitText("wrapped-progress", position(2, paused: true), seconds: 2)
        try await Task.sleep(for: .milliseconds(1200))
        try capture("stats-wrapped-slide-2")
        // A click also goes forward.
        try await drive("click-control", "wrapped-overlay")
        try await waitText("wrapped-progress", position(3, paused: true), seconds: 2)
        cases.append("native-stats-wrapped-keys-and-click-move-and-pause")

        for _ in 3..<slides.count {
            try await drive("key", "wrapped-overlay", "right")
        }
        try await waitText("wrapped-progress", position(slides.count, paused: true), seconds: 3)
        try await drive("exists", "wrapped-slide-summary")
        // → on the last slide holds it.
        try await drive("key", "wrapped-overlay", "right")
        try await Task.sleep(for: .milliseconds(2000))
        try await drive("expect-text", "wrapped-progress", position(slides.count, paused: true))
        try capture("stats-wrapped-summary")
        cases.append("native-stats-wrapped-holds-on-the-summary")

        try await drive("key", "wrapped-overlay", "escape")
        try await wait("Wrapped to close", model) { navigation.wrappedYear == nil }
        try await drive("exists", "main-stats")
        cases.append("native-stats-wrapped-esc-closes")

        try await exerciseCallToAction(model, navigation, year: year)
    }

    /// In December the header offers the year's Wrapped, and the first visit shows a banner
    /// until the year is dismissed. The smoke cannot wait for December, so it sets `offerDate`.
    private static func exerciseCallToAction(_ model: PlayerModel, _ navigation: NavigationModel,
                                             year: Int) async throws {
        let defaults = QuitSuspensionStore.suiteName(forArguments: CommandLine.arguments)
            .flatMap(UserDefaults.init(suiteName:))
        defaults?.removeObject(forKey: "stats.wrappedBannerDismissed")
        offerDate = Calendar.statsCalendar().date(from: DateComponents(year: year, month: 12, day: 5, hour: 12))
        // Leave and come back, so the page reads the date again.
        try await navigation.submitCommand("go songs")
        try await drive("exists", "main-songs")
        try await navigation.submitCommand("go stats")
        try await drive("exists", "stats-wrapped-banner")
        try await drive("expect-text", "stats-wrapped", "Your \(year) Wrapped ▶")
        try await Task.sleep(for: .milliseconds(400))
        try capture("stats-wrapped-call-to-action")
        try await drive("press", "stats-wrapped")
        try await wait("Wrapped from the header", model) { navigation.wrappedYear == year }
        try await drive("key", "wrapped-overlay", "escape")
        try await wait("Wrapped to close", model) { navigation.wrappedYear == nil }
        cases.append("native-stats-wrapped-header-button-in-december")

        try await drive("press", "stats-wrapped-banner-dismiss")
        try await wait("banner dismissal", model) {
            (defaults?.array(forKey: "stats.wrappedBannerDismissed") as? [Int]) == [year]
        }
        cases.append("native-stats-wrapped-banner-dismissal-is-remembered")
    }

    /// Opens Stats with the command language, then Go › Wrapped › "<year> so far". Returns the year.
    private static func openWrapped(_ model: PlayerModel, _ navigation: NavigationModel) async throws -> Int {
        let year = Calendar.statsCalendar().component(.year, from: Date())
        try await drive("exists", "main-songs")
        try await navigation.submitCommand("go stats")
        try await wait("stats destination", model) { navigation.destination == .stats }
        try await drive("exists", "main-stats")
        try await wait("Wrapped years", model) { model.stats.wrappedYears == [year] }
        try await drive("expect-menu-enabled", "Go›Wrapped", "true")
        try await drive("menu", "Go›Wrapped›\(year) so far")
        try await wait("Wrapped to open", model) { navigation.wrappedYear == year }
        try await drive("exists", "wrapped-overlay")
        cases.append("native-stats-wrapped-opens-from-the-go-menu")
        return year
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
    /// Retries `expect-text` until it passes or `seconds` run out.
    private static func waitText(_ identifier: String, _ text: String, seconds: Double) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while true {
            do { return try await drive("expect-text", identifier, text) } catch {
                if Date() >= deadline { throw error }
            }
            try await Task.sleep(for: .milliseconds(200))
        }
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
            "automation": "External Accessibility keystrokes, clicks, menu and button presses; no server writes.",
            "scenario": "stats", "phase": phase]
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
