#if DEBUG
import AppKit
import Foundation
import TyrannyLikesSmokeCore

/// Opt-in end-to-end driver against an explicitly isolated daemon. Uses a real rejected
/// command and the public Window menu and Copy button, never synthesized diagnostic events.
@MainActor
enum NativeDiagnosticsSmoke {
    private static var started = false

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        let arguments = CommandLine.arguments
        guard !started, let index = arguments.firstIndex(of: "--native-diagnostics-smoke"),
              index + 1 < arguments.count else { return }
        started = true
        let kit = NativePlaylistKit(model: model, navigation: navigation,
            output: URL(fileURLWithPath: arguments[index + 1], isDirectory: true), tag: "DIAGNOSTICS_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                try requireIsolation(kit)
                try await kit.wait("isolated server", seconds: 40) { model.connected && model.snapshot != nil }
                guard let state = model.snapshot, state.library.tracks == 0, state.queue_length == 0 else {
                    throw NativePlaylistKit.Failure(message: "Diagnostics smoke requires an empty fixture library and queue")
                }
                let list = ListDescription(source: .library, query: "diagnosticfixture")
                let job = EnqueueJob(action: .play, source: RunSource.queueSource(list: .library, name: "Songs"),
                    request: .source(SelectionEncoder.source(list: list, revision: state.library.revision), player: model))
                let expected: ServerFailure
                do {
                    try await model.performMutation { try await model.activation.execute(job) }
                    throw NativePlaylistKit.Failure(message: "Empty filtered selection unexpectedly played")
                } catch let error as ServerFailure {
                    expected = error
                }
                try kit.check(model.message == expected.localizedDescription, "Filtered playback failure did not reach the status bar")
                try await model.performMutation { try await model.refreshSnapshot() }
                try kit.check(model.message == nil, "Successful command did not clear the transient error")
                let safeMessage = DiagnosticsLog.sanitize(expected.message)
                try kit.check(model.diagnostics.entries.contains {
                    $0.message.contains("command=enqueue_tracks") && $0.message.contains("request_id=")
                        && $0.message.contains("code=\(expected.code)") && $0.message.contains(safeMessage)
                }, "Rejected command was not retained with request ID, command, code and complete server message")
                kit.passed("filtered-playback-failure-outlives-status")
                try await kit.drive("menu", "Window›Diagnostics…")
                try await kit.wait("diagnostics window") {
                    NSApp.windows.contains { $0.title == "Diagnostics" && $0.isVisible }
                }
                NSApp.windows.first { $0.title == "Diagnostics" }?.makeKeyAndOrderFront(nil)
                try await kit.drive("expect-text", "diagnostics-session-log", model.diagnostics.text)
                try await kit.drive("exists", "diagnostics-daemon-log")
                kit.passed("window-menu-opens-selectable-log-view")
                try await copyAndCheck(kit, expected: expected)
                try capture(kit)
                try await kit.press("diagnostics-clear-session")
                try await kit.wait("session clear") { model.diagnostics.entries.isEmpty }
                kit.passed("clear-session-events")
            } catch {
                failure = error.localizedDescription
            }
            try? kit.finish(automation: "Rejected filtered playback followed by a successful command; Window > Diagnostics… and Copy/Clear via Accessibility. Native diagnostics screenshot.", error: failure)
            NSApp.terminate(nil)
        }
    }

    private static func requireIsolation(_ kit: NativePlaylistKit) throws {
        let arguments = CommandLine.arguments
        if !kit.model.managesServer {
            try kit.requireIsolatedDaemon()
            return
        }
        try kit.check(["--managed-state-dir", "--managed-agent-dir", "--managed-label", "--managed-capture"]
            .allSatisfy { arguments.contains($0) }, "Managed smoke requires explicit isolated directories, label and capture output")
        let roots = [FileManager.default.temporaryDirectory, URL(fileURLWithPath: "/private/tmp")]
            .map { $0.resolvingSymlinksInPath().path }
        for flag in ["--managed-state-dir", "--managed-agent-dir", "--managed-capture"] {
            guard let index = arguments.firstIndex(of: flag), index + 1 < arguments.count else {
                throw NativePlaylistKit.Failure(message: "Missing \(flag)")
            }
            let path = URL(fileURLWithPath: arguments[index + 1]).resolvingSymlinksInPath().path
            try kit.check(roots.contains { path.hasPrefix($0 + "/") }, "Managed smoke paths must be inside a private temporary directory")
        }
        try kit.check(kit.model.managedLabel?.hasPrefix("dev.tyranny-likes-smoke.spike.") == true,
                      "Managed smoke requires a private spike label")
    }

    private static func copyAndCheck(_ kit: NativePlaylistKit, expected: ServerFailure) async throws {
        let pasteboard = NSPasteboard.general
        let saved = (pasteboard.pasteboardItems ?? []).map { item in
            let copy = NSPasteboardItem()
            for type in item.types {
                if let data = item.data(forType: type) { copy.setData(data, forType: type) }
            }
            return copy
        }
        var copiedChangeCount: Int?
        defer {
            if let copiedChangeCount, pasteboard.changeCount == copiedChangeCount {
                pasteboard.clearContents()
                if !saved.isEmpty { pasteboard.writeObjects(saved) }
            }
        }
        let changeCount = pasteboard.changeCount
        try await kit.press("diagnostics-copy")
        try await kit.wait("copied diagnostics") { pasteboard.changeCount != changeCount }
        let text = pasteboard.string(forType: .string) ?? ""
        try kit.check(text.hasPrefix("tyranny likes smoke — Diagnostics"), "Copy did not produce diagnostics")
        copiedChangeCount = pasteboard.changeCount
        try kit.check(text.contains("command=enqueue_tracks") && text.contains("request_id=")
            && text.contains("code=\(expected.code)") && text.contains(DiagnosticsLog.sanitize(expected.message)),
            "Copied diagnostics omitted the complete rejected command")
        try kit.check(text.contains("protocol: \(WireProtocol.version)") && text.contains("Connection: connected")
            && text.contains(DiagnosticsLog.sanitize(kit.model.socketPath)) && text.contains("=== Managed server.log ==="),
            "Copied diagnostics omitted version, connection, socket or daemon context")
        if kit.model.managesServer {
            try kit.check(!text.contains("Managed server log unavailable:"), "Managed daemon log could not be read")
            kit.passed("managed-daemon-tail-exported")
        } else {
            try kit.check(text.contains("No managed daemon log is available for this socket"),
                "Unmanaged socket did not explain the unavailable daemon log")
            kit.passed("unmanaged-socket-does-not-read-default-log")
        }
        try text.write(to: kit.output.appendingPathComponent("diagnostics.txt"), atomically: true, encoding: .utf8)
        kit.passed("copy-complete-diagnostics-and-restore-clipboard")
    }

    private static func capture(_ kit: NativePlaylistKit) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Diagnostics" && $0.isVisible }) else {
            throw NativePlaylistKit.Failure(message: "Diagnostics window unavailable")
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), kit.output.appendingPathComponent("diagnostics.png").path]
        try process.run()
        process.waitUntilExit()
        try kit.check(process.terminationStatus == 0, "Diagnostics screenshot failed")
    }
}
#endif
