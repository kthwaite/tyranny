#if DEBUG
import AppKit
import Foundation

/// Opt-in observer for real LaunchServices/launchd acceptance runs.
@MainActor
enum NativeLifecycle {
    private static var started = false
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }

    static func startIfRequested(_ model: PlayerModel) {
        let args = CommandLine.arguments
        func argument(_ name: String) -> String? {
            guard let index = args.firstIndex(of: name), index + 1 < args.count else { return nil }
            return args[index + 1]
        }
        guard let path = argument("--lifecycle-output"), !started else { return }
        started = true
        try? String(getpid()).write(toFile: path + ".pid", atomically: true, encoding: .utf8)
        let action = argument("--lifecycle-action") ?? "observe"
        Task {
            var failure: String?
            do {
                try await wait("initial server state") {
                    model.startupFailed || (model.connected && model.tracks != nil && !model.loading)
                }
                if action != "observe" {
                    guard model.connected else { throw Failure(message: model.connectionDetail) }
                    guard ["restart", "uninstall"].contains(action) else {
                        throw Failure(message: "Unsupported lifecycle action")
                    }
                    try await drive("activate", output: path)
                    try await drive("press", "connection-settings", output: path)
                    try await Task.sleep(for: .milliseconds(300))
                    try await drive("press", action == "restart" ? "managed-restart" : "managed-stop", output: path)
                    try await Task.sleep(for: .milliseconds(300))
                    try await drive("press-title", action == "restart" ? "Restart Server" : "Stop Server", output: path)
                    if action == "restart" {
                        try await wait("managed restart") {
                            !model.serverManagementBusy && ((model.connected && model.tracks != nil) || model.message != nil)
                        }
                    } else {
                        try await wait("managed stop") { !model.serverManagementBusy && !model.managedEnabled && !model.connected }
                    }
                }
                try await Task.sleep(for: .milliseconds(350))
                try await capture(output: path)
                if action != "observe" {
                    try await drive("press-title", "Done", output: path)
                    try await wait("connection sheet dismissal") {
                        NSApp.windows.allSatisfy { !$0.isSheet || !$0.isVisible }
                    }
                }
            } catch {
                failure = error.localizedDescription
            }
            let reportedError = failure ?? (model.startupFailed ? model.connectionDetail : model.message)
            var report: [String: Any] = [
                "pid": getpid(), "connected": model.connected, "action": action,
                "endpoint": model.endpoint, "tokenPath": model.tokenPath,
                "managedLabel": model.managedLabel as Any? ?? NSNull(),
                "error": reportedError as Any? ?? NSNull(),
                "startup_error": model.startupFailed ? model.connectionDetail as Any : NSNull()
            ]
            do {
                report["snapshot"] = try model.snapshot.map { try JSONSerialization.jsonObject(with: JSONEncoder().encode($0)) } ?? NSNull()
                try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
                    .write(to: URL(fileURLWithPath: path), options: .atomic)
            } catch {
                print("Native lifecycle report failed: \(error.localizedDescription)")
            }
            NSApp.terminate(nil)
        }
    }

    private static func wait(_ label: String, until condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(45)
        while !condition() {
            guard ContinuousClock.now < deadline else { throw Failure(message: "Timed out: \(label)") }
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    private static func drive(_ action: String, _ identifier: String = "", output: String) async throws {
        try await request(["kind": "drive", "pid": getpid(), "action": action, "identifier": identifier], output: output)
    }

    private static func capture(output: String) async throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" && $0.isVisible }) else {
            throw Failure(message: "Native lifecycle window missing")
        }
        let image = URL(fileURLWithPath: output).deletingPathExtension().appendingPathExtension("png")
        try await request(["kind": "capture", "window_id": window.windowNumber, "path": image.path], output: output)
    }

    // LaunchServices gives the app its own TCC identity. The terminal-owned
    // harness performs public AX/capture operations under its existing grants.
    private static func request(_ payload: [String: Any], output: String) async throws {
        let request = URL(fileURLWithPath: output + ".request.json")
        let response = URL(fileURLWithPath: output + ".response.json")
        try JSONSerialization.data(withJSONObject: payload).write(to: request, options: .atomic)
        let deadline = ContinuousClock.now + .seconds(25)
        while !FileManager.default.fileExists(atPath: response.path) {
            guard ContinuousClock.now < deadline else { throw Failure(message: "External native automation timed out") }
            try await Task.sleep(for: .milliseconds(25))
        }
        let result = try JSONSerialization.jsonObject(with: Data(contentsOf: response)) as? [String: Any]
        try FileManager.default.removeItem(at: response)
        if let error = result?["error"] as? String { throw Failure(message: error) }
    }
}
#endif
