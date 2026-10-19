#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in development acceptance driver. Exercises the displayed controls and
/// native picker through an external accessibility driver (no Apple Events).
/// It never substitutes data or commands for the server's implementation.
@MainActor
enum NativeSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var cases: [String] = []
    private static var snapshots: [ControlState] = []
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-ui-smoke")
    private static var started = false

    static func prepareFolderPanel(_ panel: NSOpenPanel) {
        let args = CommandLine.arguments
        guard args.contains("--ui-smoke-output") || args.contains("--shell-smoke-output"),
              let index = args.firstIndex(of: "--ui-smoke-folder"), index + 1 < args.count else { return }
        panel.directoryURL = URL(fileURLWithPath: args[index + 1], isDirectory: true)
    }

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: "--ui-smoke-output"), index + 1 < args.count, !started else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await wait("connection") { model.connected && model.snapshot != nil }
                try await drive("activate")
                try await Task.sleep(for: .milliseconds(250))
                if args.contains("--ui-smoke-relaunch") {
                    try check(model.snapshot?.transport == "playing", "relaunch did not receive playing state")
                    try check((model.snapshot?.position_frames ?? 0) > 0, "relaunch lost playhead")
                    try capture("relaunch")
                    cases.append("native-relaunch-receives-authoritative-playing-state")
                } else {
                    guard let folderIndex = args.firstIndex(of: "--ui-smoke-folder"), folderIndex + 1 < args.count else {
                        throw Failure(message: "--ui-smoke-folder required")
                    }
                    try await exercise(model, navigation, folder: URL(fileURLWithPath: args[folderIndex + 1], isDirectory: true))
                }
                if let state = model.snapshot { snapshots.append(state) }
                try finish(passed: true, error: nil)
            } catch {
                try? capture("failure")
                try? finish(passed: false, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    /// The core acceptance scenario on the library window: onboarding import, a double-click run,
    /// the seek slider, Next and Previous, the live filter beside the playing track, a rescan
    /// during playback, and disconnect and reconnect. The gate then relaunches the app against
    /// the same daemon (`--ui-smoke-relaunch`).
    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel, folder: URL) async throws {
        try capture("empty")
        try await press("choose-folder")
        try await wait("native directory picker") { NSApp.windows.contains { $0 is NSOpenPanel } }
        guard let panel = NSApp.windows.compactMap({ $0 as? NSOpenPanel }).first else {
            throw Failure(message: "native open panel missing")
        }
        try await Task.sleep(for: .seconds(1))
        try check(panel.directoryURL?.standardizedFileURL == folder.standardizedFileURL, "Picker did not navigate to the isolated fixture")
        try await drive("press-title", "Add Music Folder")
        try await wait("native folder import", seconds: 30) {
            model.snapshot?.library.scan.phase == "complete" && model.snapshot?.library.tracks == 105
        }
        try await wait("imported root") { model.roots?.entries.first?.path == folder.path }
        cases.append("native-folder-picker-import")

        // Songs: a double-click plays the whole list from that row (Up Next is empty).
        try await wait("songs list") { NativeSongsSmoke.songsList != nil }
        guard let songs = NativeSongsSmoke.songsList else { throw Failure(message: "Songs list missing") }
        try await wait("songs rows") { songs.cache.total == 105 && songs.cache.loadedRow(at: 0) != nil }
        try capture("library")
        try await drive("double-click-row", songsTable, "0,0.5")
        try await wait("double-click run") { !model.busy && model.snapshot?.queue_length == 105 && model.snapshot?.transport == "playing" }
        cases.append("native-double-click-plays-the-songs-run")
        try capture("playing")
        try await press("transport-play-pause")
        try await wait("pause") { !model.busy && model.snapshot?.transport == "paused" }
        guard let firstEntry = model.snapshot?.current_id, let duration = model.durationSeconds, duration > 10,
              let playingMetadata = model.snapshot?.now_playing else {
            throw Failure(message: "Authoritative current metadata/duration missing")
        }
        try await drive("slider-value", scrubber, "3")
        try await wait("AX slider seek") { !model.busy && model.positionSeconds >= 2.99 && model.positionSeconds < 3.1 }
        try await drive("increment", scrubber)
        try await wait("AX slider increment") { !model.busy && model.positionSeconds >= 7.99 && model.positionSeconds < 8.1 }
        try await drive("decrement", scrubber)
        try await wait("AX slider decrement") { !model.busy && model.positionSeconds >= 2.99 && model.positionSeconds < 3.1 }
        try await drive("key", scrubber, "right")
        try await wait("keyboard slider adjustment") { !model.busy && model.positionSeconds > 3.01 }
        try await drive("slider-value", scrubber, "3")
        try await wait("drag starting position") { !model.busy && abs(model.positionSeconds - 3) < 0.01 }
        async let drag: Void = drive("drag", scrubber, "\(3 / duration),0.25")
        try await Task.sleep(for: .milliseconds(250))
        try check(abs(model.positionSeconds - 3) < 0.01, "Slider committed before mouse release")
        try await drag
        try await wait("mouse-release seek") {
            !model.busy && model.positionSeconds > duration * 0.20 && model.positionSeconds < duration * 0.30
        }
        try await press("transport-previous")
        try await wait("previous restarts after threshold") {
            !model.busy && model.snapshot?.current_id == firstEntry && model.positionSeconds == 0
        }
        try await press("transport-next")
        try await wait("next paused occurrence") {
            !model.busy && model.snapshot?.current_id != firstEntry && model.snapshot?.transport == "paused"
        }
        try await press("transport-previous")
        try await wait("previous paused occurrence") {
            !model.busy && model.snapshot?.current_id == firstEntry && model.snapshot?.transport == "paused"
        }
        try check(model.snapshot?.now_playing == playingMetadata, "Previous restored incorrect metadata")
        try await drive("slider-value", scrubber, String(duration))
        try await wait("seek duration boundary") {
            !model.busy && model.snapshot?.position_frames == playingMetadata.duration_frames.map { $0 - 1 }
        }
        try await drive("slider-value", scrubber, "3")
        try await wait("seek reset") { !model.busy && abs(model.positionSeconds - 3) < 0.01 }
        cases.append("native-slider-AX-keyboard-drag-release-duration-boundary-and-next-previous")
        try await press("transport-play-pause")
        try await wait("resume") { !model.busy && model.snapshot?.transport == "playing" }
        cases.append("native-pause-seek-resume")

        try await value("library-filter", "no-such-track-ui-smoke")
        try await wait("live filter") { songs.cache.total == 0 && navigation.filter == "no-such-track-ui-smoke" }
        try check(model.snapshot?.now_playing == playingMetadata, "Filtering away the current track lost Now Playing metadata")
        try await drive("expect-text", "now-playing-title", playingMetadata.title)
        try capture("filtered-empty")
        try await value("library-filter", "")
        try await wait("filter reset") { songs.cache.total == 105 && songs.cache.loadedRow(at: 0) != nil }
        try await drive("scroll-fraction", songsTable, "1")
        try await wait("scroll away from the playing track") { songs.cache.loadedRow(at: 104) != nil }
        try check(model.snapshot?.now_playing == playingMetadata && model.durationSeconds == duration,
                  "Scrolling away from the current track lost Now Playing metadata")
        try await drive("expect-text", "now-playing-title", playingMetadata.title)
        cases.append("native-now-playing-survives-filter-and-scroll")
        cases.append("native-live-filter-and-empty-state")

        // Add actual files, then exercise filtering against an active real scan.
        try await Task.detached {
            let additions = folder.appendingPathComponent("Added during playback", isDirectory: true)
            try FileManager.default.createDirectory(at: additions, withIntermediateDirectories: true)
            for index in 0..<1024 {
                try FileManager.default.linkItem(at: folder.appendingPathComponent("Track 000.wav"),
                    to: additions.appendingPathComponent("Added \(index).wav"))
            }
        }.value
        try await press("status-connection")
        try await drive("press-title", "Library")
        guard let root = model.roots?.entries.first else { throw Failure(message: "Imported root missing") }
        try await press("rescan-root-\(root.id)")
        try await wait("active rescan") { model.scanActive }
        frontMainWindow()
        let filterStarted = Date()
        try await value("library-filter", "Track 000")
        try await wait("filter during rescan") { navigation.filter == "Track 000" && songs.cache.total == 1 }
        try check(model.scanActive, "Scan finished before concurrent filtering was exercised")
        try check(Date().timeIntervalSince(filterStarted) < 2, "Filter stalled during scan")
        try check(model.snapshot?.transport == "playing", "Rescan interrupted playback")
        try capture("scanning")
        if let state = model.snapshot { snapshots.append(state) }
        try await wait("completed rescan", seconds: 60) { model.snapshot?.library.scan.phase == "complete" }
        try await value("library-filter", "")
        try await wait("expanded library") { songs.cache.total == 1129 }
        cases.append("native-rescan-and-responsive-filter-during-playback")

        try await drive("press-title", "Playback")
        try await press("resume-on-start")
        try await wait("resume setting") { !model.busy && model.snapshot?.settings.resume_on_start == true }
        cases.append("native-persisted-resume-setting")

        try await press("status-connection")
        try await press("connection-disconnect")
        try await wait("disconnected presentation") { model.connection == "disconnected" && model.snapshot == nil }
        frontMainWindow()
        try capture("disconnected")
        try await press("disconnected-retry")
        try await wait("restored connection") { model.connected && model.snapshot?.transport == "playing" }
        cases.append("native-disconnected-presentation-and-recovery")

        let before = model.snapshot!.position_frames
        try await press("status-connection")
        try await press("connection-connect")
        try await wait("reconnect snapshot") { model.connected && (model.snapshot?.position_frames ?? 0) > before && model.snapshot?.queue_length == 105 }
        try check(model.snapshot?.transport == "playing", "reconnect interrupted playback")
        try check(model.snapshot?.now_playing == playingMetadata, "Reconnect lost authoritative metadata")
        try await drive("expect-text", "now-playing-title", playingMetadata.title)
        cases.append("native-reconnect-without-replayed-mutations")
        frontMainWindow()
        try capture("reconnected")
    }

    private static let songsTable = "track-table-songs"
    private static let scrubber = "now-playing-scrubber"

    private static func frontMainWindow() {
        NSApp.windows.first(where: { $0.title == "Tyranny" })?.makeKeyAndOrderFront(nil)
    }

    private static func wait(_ label: String, seconds: Double = 15, until condition: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !condition() {
            if Date() >= deadline { throw Failure(message: "Timed out waiting for \(label)") }
            try await Task.sleep(for: .milliseconds(50))
        }
        try await Task.sleep(for: .milliseconds(150))
    }
    private static func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }
    private static func elements() -> [AnyObject] {
        var result: [AnyObject] = []
        var seen: Set<ObjectIdentifier> = []
        func visit(_ value: Any, depth: Int) {
            guard depth < 50, result.count < 10_000, let object = value as? NSObject,
                  seen.insert(ObjectIdentifier(object)).inserted else { return }
            let accessible: AnyObject = object
            result.append(accessible)
            for child in accessible.accessibilityChildren?() ?? [] {
                visit(child, depth: depth + 1)
            }
            if let view = object as? NSView { for child in view.subviews { visit(child, depth: depth + 1) } }
            if let window = object as? NSWindow, let content = window.contentView { visit(content, depth: depth + 1) }
        }
        for window in NSApp.windows where window.isVisible { visit(window, depth: 0) }
        return result
    }
    static func drive(_ action: String, _ identifier: String = "", _ value: String? = nil) async throws {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: "--ui-driver"), index + 1 < args.count else {
            throw Failure(message: "Native smoke requires --ui-driver")
        }
        let process = Process()
        let errors = Pipe()
        process.executableURL = URL(fileURLWithPath: args[index + 1])
        // Foundation decomposes Unicode in argv on macOS. Preserve exact input
        // bytes so native query tests exercise the requested text, not a variant.
        process.arguments = [String(getpid()), action, identifier]
            + (value.map { ["--value-base64", Data($0.utf8).base64EncodedString()] } ?? [])
        process.standardError = errors
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            process.terminationHandler = { process in
                if process.terminationStatus == 0 {
                    continuation.resume()
                } else {
                    let text = String(decoding: errors.fileHandleForReading.readDataToEndOfFile(), as: UTF8.self)
                    continuation.resume(throwing: Failure(message: text))
                }
            }
            do { try process.run() }
            catch { process.terminationHandler = nil; continuation.resume(throwing: error) }
        }
    }
    private static func press(_ identifier: String) async throws {
        try await drive("press", identifier)
    }
    private static func value(_ identifier: String, _ text: String) async throws {
        try await drive("value", identifier, text)
    }
    private static func capture(_ name: String) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" }) else {
            throw Failure(message: "Native window cannot be captured")
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber),
                             output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        try check(process.terminationStatus == 0, "Native window screenshot failed")
    }
    private static func finish(passed: Bool, error: String?) throws {
        let tree = elements().map { node in
            ["id": node.accessibilityIdentifier?() ?? "", "role": node.accessibilityRole?()?.rawValue ?? "", "label": node.accessibilityLabel?() ?? "", "class": String(describing: type(of: node))]
        }
        var report: [String: Any] = ["passed": passed, "cases": cases, "accessibility": tree,
            "automation": "External public Accessibility actions on real SwiftUI controls, native NSOpenPanel selection, and composited window screenshots. No Apple Events permission required."]
        if let error { report["error"] = error }
        report["snapshots"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(snapshots))
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("UI_SMOKE \(passed ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
