#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Reusable native acceptance driver. Requires an explicit non-default loopback daemon,
/// token file and an empty fixture library/queue; never starts or edits the user's daemon.
@MainActor
enum NativeEqualizerSmoke {
    private static var started = false

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel, openWindow: @escaping () -> Void) {
        let args = CommandLine.arguments
        guard !started, let index = args.firstIndex(of: "--equalizer-smoke-output"), index + 1 < args.count else { return }
        started = true
        let kit = NativePlaylistKit(model: model, navigation: navigation,
                                    output: URL(fileURLWithPath: args[index + 1], isDirectory: true), tag: "EQUALIZER_SMOKE")
        Task {
            var failure: String?
            do {
                try FileManager.default.createDirectory(at: kit.output, withIntermediateDirectories: true)
                let url = URLComponents(string: model.endpoint)
                let userToken = FileManager.default.homeDirectoryForCurrentUser
                    .appendingPathComponent("Library/Application Support/tyranny/token").path
                try kit.check(args.contains("--url") && args.contains("--token-file")
                    && url?.host == "127.0.0.1" && url?.port != nil && url?.port != 9865
                    && model.tokenPath != userToken && !model.managesServer,
                    "Equalizer smoke requires an explicit isolated loopback daemon and token file")
                try await kit.wait("isolated server") { model.connected && model.snapshot != nil }
                try kit.check(model.snapshot?.queue_length == 0 && model.snapshot?.library.tracks == 0,
                              "Equalizer smoke requires an empty isolated library and queue")
                openWindow()
                try await kit.wait("equalizer window") { NSApp.windows.contains { $0.title == "Equalizer" && $0.isVisible } }
                NSApp.windows.first { $0.title == "Equalizer" }?.makeKeyAndOrderFront(nil)
                try await kit.drive("activate")
                try await kit.drive("exists", "equalizer-enabled")
                try await kit.drive("exists", "equalizer-buffer-note")
                if args.contains("--equalizer-smoke-restore") {
                    try kit.check(model.equalizer.settings == EqualizerPreset.bassBoost.settings(enabled: true),
                                  "Persisted equalizer curve did not survive daemon restart")
                    kit.passed("native-equalizer-restored-from-server")
                } else {
                    try await exercise(kit)
                }
                try capture(kit)
            } catch { failure = error.localizedDescription }
            try? kit.finish(automation: "Separate native Equalizer window; external Accessibility preset choices, toggle presses and all eleven sliders; a same-actor model edit burst and disconnect checks cancellation and server-authoritative reconnect. Empty isolated daemon only.", error: failure)
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        if !model.equalizer.settings.enabled { try await kit.press("equalizer-enabled") }
        try await kit.wait("enabled") { model.snapshot?.settings.equalizer.enabled == true && model.equalizer.pending == nil }
        for preset in EqualizerPreset.allCases {
            try await kit.drive("choose", "equalizer-preset", preset.rawValue)
            try await kit.wait(preset.rawValue) {
                model.snapshot?.settings.equalizer == preset.settings(enabled: true) && model.equalizer.pending == nil
            }
        }
        kit.passed("native-four-presets-full-configurations")
        var expected = EqualizerPreset.vocal.settings(enabled: true)
        for index in EqualizerSettings.frequencies.indices {
            let gain = Double(index) - 4.5
            expected.bands_db[index] = gain
            try await kit.drive("slider-value", "equalizer-band-\(index)", String(gain))
            try await kit.wait("band \(index)") { model.snapshot?.settings.equalizer == expected && model.equalizer.pending == nil }
        }
        expected.preamp_db = -9.5
        try await kit.drive("slider-value", "equalizer-preamp", "-9.5")
        try await kit.wait("preamp") { model.snapshot?.settings.equalizer == expected && model.equalizer.pending == nil }
        try kit.check(model.equalizer.settings.preset == nil && !model.busy, "Custom curve or nonblocking editing failed")
        kit.passed("native-ten-bands-preamp-custom-without-busy-lock")

        try await kit.press("equalizer-enabled")
        expected.enabled = false
        try await kit.wait("bypass retains curve") { model.snapshot?.settings.equalizer == expected && model.equalizer.pending == nil }
        kit.passed("native-bypass-retains-entire-curve")

        // Lifecycle regression: queue several edits in one actor turn, then disconnect before
        // the sender runs. None may be replayed on the replacement connection.
        for gain in [-8.0, -7.0, -6.0] { model.equalizer.setPreamp(gain) }
        try kit.check(model.equalizer.pending?.preamp_db == -6 && !model.busy,
                      "Coalesced pending edits did not remain interactive")

        model.disconnect()
        try kit.check(model.equalizer.pending == nil, "Disconnected optimistic state survived")
        try await kit.drive("expect-enabled", "equalizer-preamp", "false")
        model.start()
        try await kit.wait("reconnect authoritative curve") {
            model.connected && model.equalizer.settings == expected && model.equalizer.pending == nil
        }
        try await kit.drive("expect-enabled", "equalizer-preamp", "true")
        kit.passed("native-reconnect-discards-queued-edits-and-restores-server-curve")

        // Final deterministic persisted fixture for the separate daemon/app restart pass.
        try await kit.drive("choose", "equalizer-preset", EqualizerPreset.bassBoost.rawValue)
        try await kit.wait("bypassed bass preset") {
            model.snapshot?.settings.equalizer == EqualizerPreset.bassBoost.settings(enabled: false)
                && model.equalizer.pending == nil
        }
        try await kit.press("equalizer-enabled")
        try await kit.wait("final persisted bass preset") {
            model.snapshot?.settings.equalizer == EqualizerPreset.bassBoost.settings(enabled: true)
                && model.equalizer.pending == nil
        }
        kit.passed("native-preset-preserves-bypass-until-enabled")
    }

    private static func capture(_ kit: NativePlaylistKit) throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Equalizer" }) else {
            throw NativePlaylistKit.Failure(message: "Equalizer window unavailable")
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), kit.output.appendingPathComponent("equalizer.png").path]
        try process.run()
        process.waitUntilExit()
        try kit.check(process.terminationStatus == 0, "Equalizer screenshot failed")
    }
}
#endif
