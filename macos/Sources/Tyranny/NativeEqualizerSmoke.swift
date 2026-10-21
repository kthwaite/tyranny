#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Reusable native acceptance driver. Requires an explicit private temporary Unix socket
/// and an empty fixture library/queue; never starts or edits the user's daemon.
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
                try kit.requireIsolatedDaemon()
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
                    try kit.check(model.equalizer.userPresets.contains {
                        $0.name == "Native EQ persistent" && $0.equalizer == EqualizerPreset.bassBoost.settings(enabled: true)
                    }, "Named equalizer preset did not survive daemon restart")
                    kit.passed("native-equalizer-restored-from-server")
                } else {
                    try await exercise(kit)
                }
                try capture(kit)
            } catch { failure = error.localizedDescription }
            try? kit.finish(automation: "Separate native Equalizer window; external Accessibility numeric keyboard edits, factory and server preset CRUD, all filter types, band lifecycle/bypass, preamp compensation and actual graph mouse drag. Same-actor coalescing/disconnect cancellation and authoritative reconnect. Empty isolated daemon only.", error: failure)
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ kit: NativePlaylistKit) async throws {
        let model = kit.model
        let eq = model.equalizer
        if !eq.settings.enabled { try await kit.press("equalizer-enabled") }
        try await kit.wait("enabled") { model.snapshot?.settings.equalizer.enabled == true && eq.pending == nil }
        for preset in EqualizerPreset.allCases {
            try await kit.drive("choose", "equalizer-preset", preset.rawValue)
            try await kit.wait(preset.rawValue) {
                model.snapshot?.settings.equalizer == preset.settings(enabled: true) && eq.pending == nil
            }
        }
        kit.passed("native-four-presets-preserve-original-response")
        let preciseFactory = eq.settings
        try await kit.drive("key", "equalizer-band-q", "return")
        try kit.check(eq.settings == preciseFactory && eq.pending == nil, "Unedited numeric Return rounded the retained factory Q")

        try await kit.press("equalizer-reset")
        try await kit.wait("reset") { eq.settings.bands.isEmpty && eq.pending == nil }
        try await kit.press("equalizer-add-band")
        try await kit.wait("add default band") { eq.settings.bands == [EqualizerBand()] && eq.pending == nil }
        try await kit.drive("drag-point", "equalizer-response-graph", "[0.5663233348,0.3333333333,0.65,0.25]")
        try await kit.wait("graph frequency and gain drag") {
            eq.pending == nil && eq.settings.bands.count == 1
                && abs(eq.settings.bands[0].frequency_hz - 20 * pow(1000, 0.65)) < 80
                && abs(eq.settings.bands[0].gain_db - 6) < 1
        }
        kit.passed("native-graph-pointer-edits-frequency-and-gain")

        try await numeric(kit, "equalizer-band-frequency", "1731.25")
        try await numeric(kit, "equalizer-band-gain", "8.5")
        try await numeric(kit, "equalizer-band-q", "12")
        try await kit.wait("numeric bell edits") {
            eq.pending == nil && eq.settings.bands[0].frequency_hz == 1731.25
                && eq.settings.bands[0].gain_db == 8.5 && eq.settings.bands[0].q == 12
        }
        try await numeric(kit, "equalizer-band-frequency", "30000")
        try kit.check(eq.settings.bands[0].frequency_hz == 1731.25, "Out-of-range numeric input changed the filter")
        try await numeric(kit, "equalizer-band-frequency", "1731.25")
        for kind in FilterKind.allCases {
            try await kit.drive("choose", "equalizer-band-kind", kind.label)
            try await kit.wait(kind.label) { eq.pending == nil && eq.settings.bands[0].kind == kind }
            if kind.isShelf {
                try await numeric(kit, "equalizer-band-shelf-slope", "0.5")
                try await kit.wait("shelf slope") { eq.pending == nil && eq.settings.bands[0].shelf_slope == 0.5 }
            } else {
                try await numeric(kit, "equalizer-band-q", "0.7071")
                try await kit.wait("resonance") { eq.pending == nil && eq.settings.bands[0].q == 0.7071 }
            }
            if kind.isPass {
                for slope in [12, 24, 36, 48] {
                    try await kit.drive("choose", "equalizer-band-slope", "\(slope) dB/oct")
                    try await kit.wait("pass slope") { eq.pending == nil && eq.settings.bands[0].slope_db == UInt8(slope) }
                }
            }
        }
        try await kit.press("equalizer-band-enabled")
        try await kit.wait("band bypass") { eq.pending == nil && !eq.settings.bands[0].enabled }
        try await kit.press("equalizer-band-enabled")
        try await kit.wait("band unbypass") { eq.pending == nil && eq.settings.bands[0].enabled }
        kit.passed("native-six-filter-types-numeric-input-slope-and-band-bypass")
        try await kit.drive("choose", "equalizer-band-kind", FilterKind.notch.label)
        try await numeric(kit, "equalizer-band-frequency", "19500")
        try await numeric(kit, "equalizer-band-q", "18")
        try await kit.wait("narrow high-frequency notch") {
            eq.pending == nil && eq.settings.bands[0].kind == .notch
                && eq.settings.bands[0].frequency_hz == 19500 && eq.settings.bands[0].q == 18
        }
        try capture(kit, name: "narrow-notch")

        for count in 2...16 {
            try await kit.press("equalizer-add-band")
            try await kit.wait("band count \(count)") { eq.pending == nil && eq.settings.bands.count == count }
        }
        try await kit.drive("expect-enabled", "equalizer-add-band", "false")
        try await kit.press("equalizer-band-row-0")
        try await kit.wait("select first band row") { eq.selectedBand == 0 }
        try await kit.press("equalizer-band-row-0-enabled")
        try await kit.wait("row bypass") { eq.pending == nil && !eq.settings.bands[0].enabled }
        try await kit.press("equalizer-band-row-0-enabled")
        try await kit.wait("row unbypass") { eq.pending == nil && eq.settings.bands[0].enabled }
        try await kit.press("equalizer-band-row-15")
        try await kit.wait("select last band row") { eq.selectedBand == 15 }
        kit.passed("native-band-rows-selection-and-independent-bypass")
        try await kit.press("equalizer-remove-band")
        try await kit.wait("remove band") { eq.pending == nil && eq.settings.bands.count == 15 }
        try await kit.press("equalizer-reset")
        try await kit.wait("reset all bands") { eq.pending == nil && eq.settings == EqualizerSettings(enabled: true) }
        try await kit.press("equalizer-add-band")
        try await kit.wait("new band") { eq.pending == nil && eq.settings.bands.count == 1 }
        try await numeric(kit, "equalizer-band-gain", "12")
        try await numeric(kit, "equalizer-band-q", "18")
        try await kit.wait("narrow boost") { eq.pending == nil && eq.settings.bands[0].gain_db == 12 && eq.settings.bands[0].q == 18 }
        try await kit.press("equalizer-compensate")
        try await kit.wait("headroom compensation") { eq.pending == nil && abs(eq.settings.preamp_db + 12) < 0.001 }
        try await numeric(kit, "equalizer-preamp-input", "-13.25")
        try await kit.wait("numeric preamp") { eq.pending == nil && eq.settings.preamp_db == -13.25 }
        try await kit.drive("slider-value", "equalizer-preamp", "-14")
        try await kit.wait("slider preamp") { eq.pending == nil && eq.settings.preamp_db == -14 }
        kit.passed("native-sixteen-band-limit-remove-reset-and-headroom")

        var saved = eq.settings
        try await kit.value("equalizer-preset-name", "Native EQ")
        try await kit.press("equalizer-preset-save")
        try await kit.wait("save preset") { !eq.presetBusy && eq.userPresets.contains { $0.name == "Native EQ" && $0.equalizer == saved } }
        try await numeric(kit, "equalizer-preamp-input", "-15")
        try await kit.wait("edit before overwrite") { eq.pending == nil && eq.settings.preamp_db == -15 }
        saved = eq.settings
        try await kit.press("equalizer-preset-save")
        try await kit.wait("overwrite preset") {
            !eq.presetBusy && eq.userPresets.filter { $0.name == "Native EQ" }.count == 1
                && eq.userPresets.contains { $0.name == "Native EQ" && $0.equalizer == saved }
        }
        try await kit.drive("choose", "equalizer-user-preset", "Native EQ")
        try await kit.value("equalizer-preset-name", "Native EQ renamed")
        try await kit.press("equalizer-preset-rename")
        try await kit.wait("rename preset") {
            !eq.presetBusy && eq.userPresets.contains { $0.name == "Native EQ renamed" && $0.equalizer == saved }
                && !eq.userPresets.contains { $0.name == "Native EQ" }
        }
        try kit.check(eq.settings == saved, "Preset CRUD changed the active curve")
        try await kit.press("equalizer-reset")
        try await kit.wait("reset before recall") { eq.pending == nil && eq.settings.bands.isEmpty }
        try await kit.press("equalizer-enabled")
        try await kit.wait("bypass before recall") { eq.pending == nil && !eq.settings.enabled }
        try await kit.drive("choose", "equalizer-user-preset", "Native EQ renamed")
        try await kit.press("equalizer-preset-recall")
        var expected = saved
        expected.enabled = false
        try await kit.wait("recall preserves bypass") { eq.pending == nil && eq.settings == expected }
        try await kit.press("equalizer-preset-delete")
        try await kit.wait("delete preset") { !eq.presetBusy && !eq.userPresets.contains { $0.name == "Native EQ renamed" } }
        try kit.check(eq.settings == expected, "Deleting a preset changed the active curve")
        kit.passed("native-server-backed-preset-save-rename-recall-delete")

        // Canonically equivalent Swift strings are distinct server names and picker identities.
        let composedName = "\u{e9}"
        let decomposedName = "e\u{301}"
        var composedCurve = expected
        composedCurve.preamp_db = -20
        var decomposedCurve = expected
        decomposedCurve.preamp_db = -21
        for (name, curve) in [(composedName, composedCurve), (decomposedName, decomposedCurve)] {
            let _: Applied = try await model.send("save_equalizer_preset", [
                "name": .string(name), "equalizer": try JSONValue.encoding(curve)
            ])
        }
        try await model.refreshSnapshot()
        try kit.check(eq.userPresets.contains { $0.matchesName(composedName) }
            && eq.userPresets.contains { $0.matchesName(decomposedName) }, "Server Unicode preset identities collapsed")
        // NFC is deliberately recalled first while both entries exist; the driver matches UTF-8.
        for (name, curve) in [(composedName, composedCurve), (decomposedName, decomposedCurve)] {
            try await kit.drive("choose", "equalizer-user-preset", name)
            try await kit.press("equalizer-preset-recall")
            try await kit.wait("byte-exact Unicode preset recall") { eq.pending == nil && eq.settings == curve }
            try await kit.press("equalizer-preset-delete")
            try await kit.wait("byte-exact Unicode preset deletion") {
                !eq.presetBusy && !eq.userPresets.contains { $0.matchesName(name) }
            }
        }
        expected = decomposedCurve
        kit.passed("native-byte-exact-unicode-preset-identities")

        // Queue in one actor turn, disconnect before sender runs: never replay on reconnect.
        for gain in [-8.0, -7.0, -6.0] { eq.setPreamp(gain) }
        try kit.check(eq.pending?.preamp_db == -6 && !model.busy, "Coalesced edits blocked interaction")
        model.disconnect()
        try kit.check(eq.pending == nil && !eq.presetBusy, "Disconnected optimistic state survived")
        try await kit.drive("expect-enabled", "equalizer-preamp", "false")
        model.start()
        try await kit.wait("reconnect authoritative curve") { model.connected && eq.settings == expected && eq.pending == nil }
        try await kit.drive("expect-enabled", "equalizer-preamp", "true")
        kit.passed("native-reconnect-discards-queued-edits-and-restores-server-curve")

        // Deterministic persisted fixture for a separate daemon/app restart pass.
        try await kit.drive("choose", "equalizer-preset", EqualizerPreset.bassBoost.rawValue)
        try await kit.wait("bypassed bass preset") { eq.pending == nil && eq.settings == EqualizerPreset.bassBoost.settings(enabled: false) }
        try await kit.press("equalizer-enabled")
        try await kit.wait("final persisted bass preset") { eq.pending == nil && eq.settings == EqualizerPreset.bassBoost.settings(enabled: true) }
        try await kit.value("equalizer-preset-name", "Native EQ persistent")
        try await kit.press("equalizer-preset-save")
        try await kit.wait("persist named preset") {
            !eq.presetBusy && eq.userPresets.contains {
                $0.name == "Native EQ persistent" && $0.equalizer == EqualizerPreset.bassBoost.settings(enabled: true)
            }
        }
        kit.passed("native-preset-preserves-bypass-until-enabled")
    }

    private static func numeric(_ kit: NativePlaylistKit, _ identifier: String, _ text: String) async throws {
        try await kit.value(identifier, text)
        try await kit.drive("key", identifier, "return")
    }

    private static func capture(_ kit: NativePlaylistKit, name: String = "equalizer") throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Equalizer" }) else {
            throw NativePlaylistKit.Failure(message: "Equalizer window unavailable")
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), kit.output.appendingPathComponent(name + ".png").path]
        try process.run()
        process.waitUntilExit()
        try kit.check(process.terminationStatus == 0, "Equalizer screenshot failed")
    }
}
#endif
