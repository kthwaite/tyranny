#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Opt-in only. Entry IDs must name occurrences in an isolated daemon's prepared queue.
/// AX mode drives the mounted toggle, pause and seek controls. Explicit model-driven mode
/// keeps the real window/server but does not claim accessibility interaction coverage.
@MainActor
enum NativeLyricsSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var mounted = false
    private static var renderedState = ""
    private static var renderedActive: [String] = []
    private static var observedStates: Set<String> = []
    private static var cases: [String] = []
    private static var captures: [String: String] = [:]
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-likes-smoke-lyrics-smoke")
    private static let environment = ProcessInfo.processInfo.environment
    private static var modelDriven: Bool { environment["TYRANNY_LIKES_SMOKE_LYRICS_SMOKE_MODEL_DRIVEN"] == "1" }

    static func observePane(mounted: Bool, state: String, activeText: [String]) {
        guard started else { return }
        self.mounted = mounted
        renderedState = state
        renderedActive = activeText
        if mounted { observedStates.insert(state) }
    }

    static func startIfRequested(_ model: PlayerModel) {
        guard !started, let path = environment["TYRANNY_LIKES_SMOKE_LYRICS_SMOKE_OUTPUT"] else { return }
        started = true
        output = URL(fileURLWithPath: path, isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await exercise(model)
                try finish(model, error: nil)
            } catch {
                capture("failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel) async throws {
        guard let synced = environment["TYRANNY_LIKES_SMOKE_LYRICS_SMOKE_SYNCED_ENTRY_ID"].flatMap(UInt64.init),
              let plain = environment["TYRANNY_LIKES_SMOKE_LYRICS_SMOKE_PLAIN_ENTRY_ID"].flatMap(UInt64.init), synced != plain else {
            throw Failure(message: "Distinct SYNCED_ENTRY_ID and PLAIN_ENTRY_ID environment fixtures are required")
        }
        try await wait("connected isolated daemon") { model.connected && model.snapshot != nil }
        if modelDriven {
            NSApp.activate(ignoringOtherApps: true)
            NSApp.windows.first(where: { $0.title == "tyranny likes smoke" })?.makeKeyAndOrderFront(nil)
        } else {
            try await NativeSmoke.drive("activate")
        }
        try check(!model.lyrics.isVisible && model.lyrics.payload == nil, "Hidden pane created lyrics demand")
        try await play(synced, model)
        try await toggle(model)
        try await wait("mounted synchronized lyrics") { mounted && renderedState == "ready" && model.lyrics.payload?.source == .embedded }
        try check(model.lyrics.timeline.lines.map(\.text) == ["Opening line", "Second line", "Third line"], "Synchronized fixture differs")
        if !modelDriven { try await NativeSmoke.drive("exists", "lyrics-synced-text") }
        try await pause(model)
        try await seek(3, model)
        try await wait("second highlighted line") { renderedActive == ["Second line"] }
        let pausedPosition = model.positionSeconds
        try await Task.sleep(for: .milliseconds(700))
        try check(model.positionSeconds == pausedPosition && renderedActive == ["Second line"], "Paused lyrics advanced")
        capture("paused-second-line")
        try await seek(0, model)
        try await wait("backwards seek highlight") { renderedActive == ["Opening line"] }
        capture("seek-opening-line")
        cases.append("mounted-synchronized-pane-pauses-and-seeks-backwards-with-authoritative-playhead")

        if modelDriven { model.transport.playPause() }
        else { try await NativeSmoke.drive("press", "transport-play-pause") }
        try await wait("playhead and lyric advance") { model.snapshot?.transport == "playing" && renderedActive == ["Second line"] }
        cases.append("resumed-playback-advances-mounted-highlight")
        try await toggle(model)
        try await wait("hidden pane releases demand") { !mounted && !model.lyrics.isVisible && model.lyrics.payload == nil }
        try await play(plain, model)
        try check(model.lyrics.payload == nil, "Track change loaded hidden lyrics")
        try await toggle(model)
        let plainText = environment["TYRANNY_LIKES_SMOKE_LYRICS_SMOKE_PLAIN_TEXT"] ?? "Plain lyrics"
        try await wait("mounted plain lyrics") {
            mounted && renderedState == "ready" && model.lyrics.timeline.lines.isEmpty
                && model.lyrics.payload?.plain_text?.contains(plainText) == true
        }
        if !modelDriven { try await NativeSmoke.drive("expect-text", "lyrics-plain-text", model.lyrics.payload?.plain_text ?? plainText) }
        capture("plain-lyrics")
        cases.append("hidden-pane-releases-demand-and-plain-embedded-fallback-renders")

        // Setup commands select exact fixture occurrences, not titles or client filesystem paths.
        try await play(synced, model)
        try await play(plain, model)
        try await wait("superseded occurrence discarded") {
            model.snapshot?.current_id == plain && renderedState == "ready"
                && model.lyrics.payload?.plain_text?.contains(plainText) == true && model.lyrics.timeline.lines.isEmpty
        }
        cases.append("rapid-current-occurrence-change-keeps-only-latest-lyrics")
        if let root = environment["TYRANNY_LIKES_SMOKE_LYRICS_SMOKE_ROOT_ID"].flatMap(UInt64.init) {
            let generation = model.lyrics.generation
            try await model.rescanForCommand(root)
            try await wait("completed scan invalidates lyrics") {
                model.snapshot?.library.scan.phase == "complete" && model.lyrics.generation > generation && renderedState == "ready"
            }
            cases.append("completed-scan-refreshes-mounted-lyrics")
        }
        model.disconnect()
        try await wait("disconnect clears stale lyrics") { renderedState == "unavailable" && model.lyrics.payload == nil }
        capture("disconnected")
        if modelDriven { model.reconnect() }
        else { try await NativeSmoke.drive("press", "disconnected-retry") }
        try await wait("reconnected pane") {
            model.connected && renderedState == "ready" && model.lyrics.payload?.plain_text?.contains(plainText) == true
        }
        cases.append("disconnect-invalidates-and-reconnect-reloads-current-lyrics")
        for (key, status) in [("MISSING", "missing"), ("INSTRUMENTAL", "instrumental"), ("UNAVAILABLE", "unavailable"), ("ONLINE", "ready")] {
            if let entry = environment["TYRANNY_LIKES_SMOKE_LYRICS_SMOKE_\(key)_ENTRY_ID"].flatMap(UInt64.init) {
                try await play(entry, model)
                try await wait("mounted \(key) fixture") { mounted && renderedState == status }
                if key == "ONLINE" {
                    try check(model.lyrics.payload?.source == .lrclib && model.lyrics.payload?.attributionURL != nil, "Online lyrics have no LRCLIB attribution")
                    if !modelDriven { try await NativeSmoke.drive("exists", "lyrics-attribution") }
                }
                capture(key.lowercased())
                cases.append("mounted-\(key.lowercased())-fixture")
            }
        }
        if modelDriven { model.lyrics.setVisible(false) }
        else { try await NativeSmoke.drive("press", "lyrics-close") }
        try await wait("close control") { !mounted && !model.lyrics.isVisible }
        cases.append("closing-pane-releases-demand")
    }

    private static func play(_ entry: UInt64, _ model: PlayerModel) async throws {
        let _: Applied = try await model.send("play", ["entry_id": .uint(entry)])
        try await wait("fixture occurrence \(entry)") { model.snapshot?.current_id == entry && model.snapshot?.transport == "playing" }
    }
    private static func toggle(_ model: PlayerModel) async throws {
        if modelDriven { model.lyrics.setVisible(!model.lyrics.isVisible) }
        else { try await NativeSmoke.drive("press", "lyrics-toggle") }
    }
    private static func pause(_ model: PlayerModel) async throws {
        if model.snapshot?.transport != "paused" {
            if modelDriven { model.transport.pause() }
            else { try await NativeSmoke.drive("press", "transport-play-pause") }
        }
        try await wait("pause") { !model.busy && model.snapshot?.transport == "paused" }
    }
    private static func seek(_ seconds: Double, _ model: PlayerModel) async throws {
        if modelDriven { model.transport.seek(to: seconds) }
        else { try await NativeSmoke.drive("slider-value", "now-playing-scrubber", String(seconds)) }
        try await wait("seek \(seconds)") { !model.busy && abs(model.positionSeconds - seconds) < 0.05 }
    }
    private static func wait(_ label: String, until predicate: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(35)
        while !predicate() {
            if Date() >= deadline { throw Failure(message: "Timed out: \(label); rendered=\(renderedState); active=\(renderedActive)") }
            try await Task.sleep(for: .milliseconds(40))
        }
        try await Task.sleep(for: .milliseconds(100))
    }
    private static func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }

    private static func capture(_ name: String) {
        guard let window = NSApp.windows.first(where: { $0.title == "tyranny likes smoke" }), let view = window.contentView else {
            captures[name] = "unavailable: no mounted window"
            return
        }
        let destination = output.appendingPathComponent(name + ".png")
        if !modelDriven {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
            process.arguments = ["-x", "-o", "-l", String(window.windowNumber), destination.path]
            if (try? process.run()) != nil {
                process.waitUntilExit()
                if process.terminationStatus == 0 { captures[name] = "screen-window"; return }
            }
        }
        // Captures the actual mounted NSWindow content, not a substitute preview or mock.
        view.layoutSubtreeIfNeeded()
        guard let bitmap = view.bitmapImageRepForCachingDisplay(in: view.bounds) else {
            captures[name] = "unavailable: no backing bitmap"
            return
        }
        view.cacheDisplay(in: view.bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else {
            captures[name] = "unavailable: PNG encoding"
            return
        }
        do {
            try png.write(to: destination)
            captures[name] = "mounted-window-render (not screen capture)"
        } catch { captures[name] = "unavailable: \(error.localizedDescription)" }
    }

    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = [
            "passed": error == nil, "cases": cases, "captures": captures,
            "observed_pane_states": observedStates.sorted(),
            "automation": modelDriven ? "model-driven real mounted window; AX controls NOT exercised" : "actual AX toggle/pause/seek/close controls; fixture selection and disconnect via model",
        ]
        if let error { report["error"] = error }
        if let state = model.snapshot { report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state)) }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("LYRICS_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
