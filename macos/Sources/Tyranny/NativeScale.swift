#if DEBUG
import AppKit
import Foundation
import TyrannyCore

/// Opt-in measurements of real bounded pages and external Accessibility actions.
/// Timing targets are reported separately from functional correctness.
@MainActor
enum NativeScale {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var appLaunchUptime: Double?
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-native-scale")
    private static var report: [String: Any] = [:]
    private static var samples: [[String: Any]] = []
    private static var pages: [[String: Any]] = []
    private static var screenshots: [String] = []

    static func markLaunch() { appLaunchUptime = ProcessInfo.processInfo.systemUptime }

    private static func argument(_ name: String) -> String? {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: name), index + 1 < args.count else { return nil }
        return args[index + 1]
    }

    static func startIfRequested(_ model: PlayerModel) {
        guard let path = argument("--scale-output"), !started else { return }
        started = true
        output = URL(fileURLWithPath: path, isDirectory: true)
        report = ["passed": false, "run_label": argument("--scale-run-label") ?? "unspecified",
                  "automation": "External public Accessibility actions on actual SwiftUI controls; actual native window screenshots",
                  "clock": "Monotonic system uptime; parent spawn timestamp precedes process creation",
                  "limitations": ["Process-cold denotes newly started daemon/client, not cold OS/filesystem caches; no cache purge",
                                  "Native filter samples include Accessibility subprocess and 250ms debounce; they are not SQL query times",
                                  "Observed bounded model pages are not a heap-profile or a server-internal allocation guarantee",
                                  "Screenshot capture occurs after timing; first usable includes successful external row selection"]]
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await exercise(model)
                report["passed"] = true
            } catch {
                report["error"] = error.localizedDescription
                try? await capture("failure")
            }
            do {
                report["filter_samples"] = samples
                report["page_observations"] = pages
                report["screenshots"] = screenshots
                if let state = model.snapshot { report["final_snapshot"] = try json(state) }
                try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
                    .write(to: output.appendingPathComponent("report.json"), options: .atomic)
            } catch {
                FileHandle.standardError.write(Data("Native scale report failed: \(error.localizedDescription)\n".utf8))
            }
            NSApp.terminate(nil)
        }
    }

    private static func exercise(_ model: PlayerModel) async throws {
        guard let launched = argument("--scale-launch-uptime").flatMap(Double.init),
              launched <= now, let initialized = appLaunchUptime else {
            throw Failure(message: "Scale measurements require parent monotonic --scale-launch-uptime and NativeScale.markLaunch() at app initialization")
        }
        try await wait("initial bounded native pages", seconds: 45) {
            model.connected && !model.loading && model.tracks?.total == 50_000
                && model.snapshot?.queue_length == 10_000 && model.artists != nil && model.albums != nil
                && NSApp.windows.contains { $0.title == "Tyranny" && $0.isVisible }
        }
        let modelReady = now
        try observePages(model, label: "initial")
        try await NativeSmoke.drive("activate")
        // Successful external selection proves a usable real native table, rather
        // than merely timing publication of data into the model.
        try await NativeSmoke.drive("select", "track-table", "0")
        try await wait("native row selection") { model.selectedTracks.count == 1 }
        let usable = now
        let milliseconds = (usable - launched) * 1000
        var startup: [String: Any] = ["process_to_model_ready_ms": (modelReady - launched) * 1000,
                             "process_to_first_usable_ms": milliseconds,
                             "app_init_to_first_usable_ms": (usable - initialized) * 1000,
                             "external_usability_confirmation_ms": (usable - modelReady) * 1000,
                             "warm_target_ms": 1000,
                             "warm_target_applicable": (argument("--scale-run-label") ?? "").hasPrefix("warm-"),
                             "within_warm_target": milliseconds <= 1000]
        if let serverArgument = argument("--scale-server-launch-uptime") {
            guard let serverLaunched = Double(serverArgument), serverLaunched > 0, serverLaunched <= launched else {
                throw Failure(message: "--scale-server-launch-uptime must precede the native client launch")
            }
            startup["server_process_to_first_usable_ms"] = (usable - serverLaunched) * 1000
            startup["server_process_to_client_spawn_ms"] = (launched - serverLaunched) * 1000
            startup["process_cold_scope"] = "New daemon and first native client before library query warmup; includes server readiness and real WAV play readiness"
        }
        startup["cache_condition"] = "OS/filesystem caches uncontrolled; no purge"
        report["startup"] = startup
        try await capture("first-usable")
        try check(model.snapshot?.transport == "playing", "Real WAV playback is not active")
        let before = model.snapshot!
        report["initial_snapshot"] = try json(before)

        try await NativeSmoke.drive("press", "tracks-next")
        try await wait("next bounded track page") { model.tracks?.offset == 100 && !model.loading }
        try observePages(model, label: "tracks-next")
        try await NativeSmoke.drive("press", "tracks-previous")
        try await wait("previous bounded track page") { model.tracks?.offset == 0 && !model.loading }
        for _ in 0..<3 {
            try await filter(model, text: "Scale Track 00001", expected: 1, label: "selective-during-playback")
            try await filter(model, text: "scale-no-match", expected: 0, label: "empty-during-playback")
            try await filter(model, text: "Scale Track", expected: 50_000, label: "broad-during-playback")
        }
        try await capture("filtered-50000")
        if let scanPath = argument("--scale-scan-root") {
            guard let root = model.roots?.entries.first(where: { $0.path == scanPath }) else {
                throw Failure(message: "Real scan root is absent from bounded root page")
            }
            try await NativeSmoke.drive("press", "show-roots")
            try await NativeSmoke.drive("press", "rescan-root-\(root.id)")
            // Fast scans may finish before AX sheet dismissal. Record that honestly
            // instead of manufacturing concurrency or failing a timing threshold.
            try await wait("scan accepted", seconds: 30) {
                model.scanActive || model.snapshot?.library.scan.phase == "complete"
                    || model.snapshot?.library.scan.phase == "failed"
            }
            let observedActive = model.scanActive
            try await NativeSmoke.drive("press-title", "Done")
            let firstSample = samples.count
            try await filter(model, text: "Scale Track 00001", expected: 1, label: "requested-during-scan")
            try await filter(model, text: "Scale Track", expected: 50_000, label: "requested-during-scan")
            try await capture("scan-and-filter")
            try await wait("real scan completion", seconds: 120) {
                ["complete", "failed", "interrupted"].contains(model.snapshot?.library.scan.phase ?? "")
            }
            try check(model.snapshot?.library.scan.phase == "complete", "Real WAV scan did not complete")
            let scanSamples = Array(samples.dropFirst(firstSample))
            let overlapped = scanSamples.contains { ($0["scan_active_before"] as? Bool == true) && ($0["scan_active_after"] as? Bool == true) }
            report["concurrent_scan"] = ["requested": true, "active_phase_observed": observedActive,
                                         "filter_spanned_active_scan": overlapped,
                                         "coverage": overlapped ? "observed" : "not observed: scan completed before a whole filter sample overlapped",
                                         "completion": try json(model.snapshot!.library.scan)]
        } else {
            report["concurrent_scan"] = ["requested": false, "coverage": "not requested for this startup run"]
        }
        try observePages(model, label: "final")
        guard let after = model.snapshot else { throw Failure(message: "Lost authoritative snapshot") }
        let advanced = after.current_id != before.current_id || after.position_frames > before.position_frames
        report["playback"] = ["advanced": advanced, "underruns_before": before.underruns,
                              "underruns_after": after.underruns, "transport_after": after.transport]
        try check(after.transport == "playing" && advanced, "Playback did not advance during native scale actions")
        try check(after.underruns == before.underruns, "Playback underruns increased during native scale actions")
    }

    private static func filter(_ model: PlayerModel, text: String, expected: Int, label: String) async throws {
        let before = model.snapshot!
        let scanningBefore = model.scanActive
        // Background refresh keeps `loading` true while published rows remain
        // selectable. Distinct totals prove this is the new filter's page.
        try check(model.tracks?.total != expected, "Filter sample must change the observed total")
        let begin = now
        try await NativeSmoke.drive("value", "library-filter", text)
        try await wait("fresh native filter \(text)", seconds: 30) {
            model.filter == text && model.tracks?.total == expected
                && (expected == 0 || model.tracks?.entries.first?.metadata.title.hasPrefix("Scale Track") == true)
        }
        let modelReadyMS = (now - begin) * 1000
        if let first = model.tracks?.entries.first {
            try await NativeSmoke.drive("select", "track-table", "0")
            try await wait("filtered native row selection") { model.selectedTracks.contains(first.id) }
        }
        let elapsed = (now - begin) * 1000
        guard let after = model.snapshot else { throw Failure(message: "Disconnected while filtering") }
        samples.append(["label": label, "query": text, "elapsed_ms": elapsed,
                        "model_ready_ms": modelReadyMS,
                        "completion": expected == 0 ? "empty model published; native screenshot recorded separately" : "external native row selection confirmed",
                        "expected_total": expected, "observed_total": model.tracks?.total ?? -1,
                        "returned": model.tracks?.entries.count ?? -1,
                        "library_revision": model.tracks?.revision ?? 0,
                        "scan_active_before": scanningBefore, "scan_active_after": model.scanActive,
                        "state_before": try json(before), "state_after": try json(after)])
        if expected == 0 && !screenshots.contains(output.appendingPathComponent("filtered-empty.png").path) {
            try await capture("filtered-empty")
        }
        try observePages(model, label: label)
        try check(after.transport == "playing", "Filtering interrupted real WAV playback")
    }

    private static func observePages(_ model: PlayerModel, label: String) throws {
        let counts = ["artists": model.artists?.entries.count ?? 0, "albums": model.albums?.entries.count ?? 0,
                      "tracks": model.tracks?.entries.count ?? 0,
                      "roots": model.roots?.entries.count ?? 0]
        try check(counts.values.allSatisfy { $0 <= PlayerModel.pageSize }, "Native page exceeded configured bound")
        pages.append(["label": label, "counts": counts, "page_bound": PlayerModel.pageSize,
                      "track_total": model.tracks?.total ?? 0, "track_offset": model.tracks?.offset ?? 0])
    }

    private static var now: Double { ProcessInfo.processInfo.systemUptime }
    private static func wait(_ label: String, seconds: Double = 15, until condition: () -> Bool) async throws {
        let deadline = now + seconds
        while !condition() {
            if now >= deadline { throw Failure(message: "Timed out waiting for \(label)") }
            try await Task.sleep(for: .milliseconds(10))
        }
        // Yield once so SwiftUI can consume the observed state; no arbitrary
        // post-condition sleep added to every latency sample.
        await Task.yield()
    }
    private static func check(_ condition: Bool, _ message: String) throws {
        if !condition { throw Failure(message: message) }
    }
    private static func json<T: Encodable>(_ value: T) throws -> Any {
        try JSONSerialization.jsonObject(with: JSONEncoder().encode(value))
    }
    private static func capture(_ name: String) async throws {
        guard let window = NSApp.windows.first(where: { $0.title == "Tyranny" && $0.isVisible }) else {
            throw Failure(message: "Native window missing for screenshot")
        }
        let destination = output.appendingPathComponent(name + ".png")
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = ["-x", "-o", "-l", String(window.windowNumber), destination.path]
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            process.terminationHandler = { process in
                if process.terminationStatus == 0 { continuation.resume() }
                else { continuation.resume(throwing: Failure(message: "Native screenshot failed (\(process.terminationStatus)); check Screen Recording permission")) }
            }
            do { try process.run() }
            catch { process.terminationHandler = nil; continuation.resume(throwing: error) }
        }
        let attributes = try FileManager.default.attributesOfItem(atPath: destination.path)
        try check(((attributes[.size] as? NSNumber)?.intValue ?? 0) > 0, "Native screenshot was empty")
        screenshots.append(destination.path)
    }
}
#endif
