#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in acceptance driver for the library window shell (`--shell-smoke-output`).
/// Every control is driven through the external Accessibility driver; model state is
/// observation only. One setup step, the queue fill, is sent directly because the
/// list views that enqueue arrive in later tasks.
@MainActor
enum NativeShellSmoke {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-shell-smoke")
    private static var cases: [String] = []
    private static var observations: [String: Any] = [:]

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--shell-smoke-output"), index + 1 < args.count else { return }
        started = true
        output = URL(fileURLWithPath: args[index + 1], isDirectory: true)
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                guard let folderIndex = args.firstIndex(of: "--ui-smoke-folder"), folderIndex + 1 < args.count else {
                    throw Failure(message: "--ui-smoke-folder required")
                }
                let folder = URL(fileURLWithPath: args[folderIndex + 1], isDirectory: true)
                try await wait("connection", model) { model.connected && model.snapshot != nil }
                try await NativeSmoke.drive("activate")
                try await Task.sleep(for: .milliseconds(300))
                try await exercise(model, navigation, folder: folder)
                try finish(model, error: nil)
            } catch {
                try? capture("shell-failure")
                try? finish(model, error: error.localizedDescription)
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel, folder: URL) async throws {
        let transport = model.transport

        // Onboarding: no roots shows the Choose Music Folder empty state.
        try check(model.snapshot?.library.roots == 0, "Shell fixture must start without music folders")
        try await drive("exists", "choose-folder")
        try await drive("exists", "status-bar")
        try await drive("expect-text", "status-totals", "0 songs · 0 minutes · 0 bytes")
        try capture("shell-onboarding")
        try await press("choose-folder")
        try await wait("native directory picker", model) { NSApp.windows.contains { $0 is NSOpenPanel } }
        try await Task.sleep(for: .seconds(1))
        try await drive("press-title", "Add Music Folder")
        try await drive("exists", "import-progress")
        try await wait("folder import", model, seconds: 30) {
            model.snapshot?.library.scan.phase == "complete" && model.snapshot?.library.tracks == 105
        }
        try await drive("exists", "import-show-songs")
        try capture("shell-import-done")
        try await press("import-show-songs")
        try await wait("import screen closed", model) { model.importProgress == nil }
        try await drive("exists", "main-songs")
        try check(model.roots?.entries.first?.path == folder.path, "Picker imported the wrong folder")
        cases.append("native-onboarding-choose-folder-imports-root")
        cases.append("native-onboarding-import-screen-fills-then-shows-songs")

        // Source list: every LIBRARY and PLAYING destination.
        for destination in Destination.library + Destination.playing {
            try await drive("click-control", destination.identifier)
            try await wait("destination \(destination.key)", model) { navigation.destination == destination }
            try await drive("exists", "main-\(destination.key)")
        }
        try await drive("click-control", Destination.songs.identifier)
        try await wait("back to songs", model) { navigation.destination == .songs }
        cases.append("native-source-list-switches-every-destination")

        // Status bar totals for the whole library.
        try await wait("library totals", model) { model.snapshot?.library.tracks == 105 }
        if let library = model.snapshot?.library {
            let expected = DisplayFormat.totals(ListTotals(tracks: UInt64(library.tracks),
                duration_ms: library.duration_ms, size_bytes: library.size_bytes))
            try await drive("expect-text", "status-totals", expected)
            observations["status_totals"] = expected
        }
        try await drive("expect-text", "status-connection", "Server · local · protocol v10")
        cases.append("native-status-bar-connection-and-library-totals")

        // Setup only: fill the queue (the enqueueing list views arrive in Tasks 6 and 13).
        try await enqueueLibrary(model)
        try await wait("queue filled", model) { model.snapshot?.queue_length == 105 && !model.busy }

        // Transport buttons.
        try await drive("expect-enabled", "transport-play-pause", "true")
        try await press("transport-play-pause")
        try await wait("playing", model) { !model.busy && model.snapshot?.transport == "playing" }
        guard let first = model.snapshot?.current_id, let playing = model.snapshot?.now_playing else {
            throw Failure(message: "Nothing current after Play")
        }
        try await drive("expect-text", "now-playing-title", playing.title)
        try await drive("exists", "now-playing-scrubber")
        try await drive("exists", "now-playing-format")
        try await press("transport-next")
        try await wait("next entry", model) { !model.busy && model.snapshot?.current_id != first && model.snapshot?.current_id != nil }
        try await press("transport-previous")
        try await wait("previous entry", model) { !model.busy && model.snapshot?.current_id == first }
        try await press("transport-play-pause")
        try await wait("paused", model) { !model.busy && model.snapshot?.transport == "paused" }
        try await drive("slider-value", "now-playing-scrubber", "2")
        try await wait("scrubber seek", model) { !model.busy && abs(model.positionSeconds - 2) < 0.05 }
        cases.append("native-player-bar-play-pause-next-previous-and-scrubber")

        // Volume.
        try await drive("slider-value", "transport-volume", "40")
        try await wait("volume 40", model) { model.snapshot?.settings.volume == 40 && transport.pendingVolume == nil }
        try await drive("increment", "transport-volume")
        try await wait("volume 45", model) { model.snapshot?.settings.volume == 45 && transport.pendingVolume == nil }
        cases.append("native-volume-slider-sets-server-volume")

        // Shuffle and repeat toggles.
        let shuffle = model.snapshot?.settings.shuffle ?? false
        try await press("transport-shuffle")
        try await wait("shuffle toggled", model) { !model.busy && model.snapshot?.settings.shuffle == !shuffle }
        try await press("transport-shuffle")
        try await wait("shuffle restored", model) { !model.busy && model.snapshot?.settings.shuffle == shuffle }
        try check(model.snapshot?.settings.repeat == .off, "Repeat fixture must start off")
        try await press("transport-repeat")
        try await wait("repeat all", model) { !model.busy && model.snapshot?.settings.repeat == .all }
        // Carry: under Repeat All, Previous wraps within the context, so it stays enabled at the first entry.
        try await wait("first entry", model) { model.snapshot?.current_id == first }
        try await drive("expect-enabled", "transport-previous", model.snapshot?.can_previous == true ? "true" : "false")
        observations["repeat_all_first_entry_can_previous"] = model.snapshot?.can_previous
        try check(model.snapshot?.can_previous == true, "Repeat All at the first entry did not allow Previous")
        try await press("transport-repeat")
        try await wait("repeat one", model) { !model.busy && model.snapshot?.settings.repeat == .one }
        try await press("transport-repeat")
        try await wait("repeat off", model) { !model.busy && model.snapshot?.settings.repeat == RepeatMode.off }
        cases.append("native-shuffle-and-repeat-toggles-set-server-modes")

        // Filter syntax error: red border and popover.
        try await drive("value", "library-filter", "artist:[")
        try await wait("filter syntax error", model) { navigation.filterError != nil }
        try await drive("exists", "library-filter-error")
        try capture("shell-filter-error")
        try await drive("value", "library-filter", "")
        try await wait("filter cleared", model) { navigation.filterError == nil && navigation.filter.isEmpty }
        cases.append("native-filter-syntax-error-popover")

        // Resume playback for the window screenshot.
        try await press("transport-play-pause")
        try await wait("playing again", model) { !model.busy && model.snapshot?.transport == "playing" }
        try await Task.sleep(for: .milliseconds(500))
        try capture("shell")

        // Status bar scan progress during a rescan from Settings → Library.
        try await Task.detached {
            let additions = folder.appendingPathComponent("Added during shell smoke", isDirectory: true)
            try FileManager.default.createDirectory(at: additions, withIntermediateDirectories: true)
            for index in 0..<1024 {
                try FileManager.default.linkItem(at: folder.appendingPathComponent("Track 000.wav"),
                    to: additions.appendingPathComponent("Added \(index).wav"))
            }
        }.value
        try await press("status-connection")
        try await drive("exists", "connection-url")
        cases.append("native-status-connection-opens-connection-settings")
        try await drive("press-title", "Library")
        guard let root = model.roots?.entries.first else { throw Failure(message: "Imported root missing") }
        try await press("rescan-root-\(root.id)")
        try await wait("active rescan", model) { model.scanActive }
        try await drive("exists", "status-scan")
        if let scan = model.snapshot?.library.scan {
            let label = DisplayFormat.scanLabel(scan, root: DisplayFormat.abbreviatedPath(root.path, home: NSHomeDirectory()))
            observations["scan_label"] = label
            try check(label?.hasPrefix("Scanning ") == true, "Status bar scan label missing")
        }
        try capture("shell-scanning")
        try await wait("rescan complete", model, seconds: 60) {
            model.snapshot?.library.scan.phase == "complete" && model.snapshot?.library.tracks == 1129
        }
        try check(model.snapshot?.transport == "playing", "Rescan interrupted playback")
        cases.append("native-status-bar-shows-scan-progress-during-rescan")
    }

    private static func enqueueLibrary(_ model: PlayerModel) async throws {
        guard let state = model.snapshot else { throw Failure(message: "No snapshot for setup") }
        let selection = SelectionEncoder.source(list: ListDescription(source: .library), revision: state.library.revision)
        let _: Applied = try await model.send("enqueue_tracks", [
            "selection": try JSONValue.encoding(selection),
            "expected_revision": .uint(state.queue_revision),
            "placement": .string(Placement.replaceUpcoming.rawValue), "play": .bool(false),
            "source": try JSONValue.encoding(QueueSource(kind: .songs, name: "Songs"))])
    }

    private static func drive(_ action: String, _ identifier: String, _ value: String? = nil) async throws {
        try await NativeSmoke.drive(action, identifier, value)
    }
    private static func press(_ identifier: String) async throws { try await drive("press", identifier) }
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
        if process.terminationStatus != 0 {
            // Without Screen Recording permission, keep a self-rendered image for review.
            // It is not a composited screenshot and is named so.
            selfRender(window, name: name + "-self-rendered")
        }
        try check(process.terminationStatus == 0, "Native screenshot failed")
    }
    private static func selfRender(_ window: NSWindow, name: String) {
        guard let frame = window.contentView?.superview,
              let bitmap = frame.bitmapImageRepForCachingDisplay(in: frame.bounds) else { return }
        frame.cacheDisplay(in: frame.bounds, to: bitmap)
        try? bitmap.representation(using: .png, properties: [:])?
            .write(to: output.appendingPathComponent(name + ".png"))
    }
    private static func finish(_ model: PlayerModel, error: String?) throws {
        var report: [String: Any] = ["passed": error == nil, "cases": cases, "observations": observations,
            "automation": "External Accessibility presses, values and CGEvent clicks on the real SwiftUI/AppKit shell; native NSOpenPanel import; composited window screenshots. Queue fill is the only direct command (setup)."]
        if let error { report["error"] = error }
        if let state = model.snapshot {
            report["snapshot"] = try JSONSerialization.jsonObject(with: JSONEncoder().encode(state))
        }
        try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
            .write(to: output.appendingPathComponent("report.json"))
        print("SHELL_SMOKE \(error == nil ? "PASS" : "FAIL") \(output.path)")
    }
}
#endif
