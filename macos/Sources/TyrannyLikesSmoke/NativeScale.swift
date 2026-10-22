#if DEBUG
import AppKit
import Foundation
import Darwin
import TyrannyLikesSmokeCore

/// Opt-in measurements of the Songs list over a 50,000-track library and external Accessibility
/// actions: the first usable window after launch, end-to-end filter latency, and a top-to-bottom
/// scroll that must keep at most 10 windows (2,000 rows) in `ListWindowCache` and the resident
/// memory within 1.5x of its post-launch value. Timing targets are reported separately from
/// functional correctness.
@MainActor
enum NativeScale {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var appLaunchUptime: Double?
    private static var started = false
    private static var output = URL(fileURLWithPath: "/tmp/tyranny-likes-smoke-native-scale")
    private static var report: [String: Any] = [:]
    private static var samples: [[String: Any]] = []
    private static var windows: [[String: Any]] = []
    private static var screenshots: [String] = []

    static func markLaunch() { appLaunchUptime = ProcessInfo.processInfo.systemUptime }

    private static func argument(_ name: String) -> String? {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: name), index + 1 < args.count else { return nil }
        return args[index + 1]
    }

    static func startIfRequested(_ model: PlayerModel, navigation: NavigationModel) {
        guard let path = argument("--scale-output"), !started else { return }
        started = true
        output = URL(fileURLWithPath: path, isDirectory: true)
        report = ["passed": false, "run_label": argument("--scale-run-label") ?? "unspecified",
                  "automation": "External public Accessibility actions on actual SwiftUI controls; actual native window screenshots",
                  "clock": "Monotonic system uptime; parent spawn timestamp precedes process creation",
                  "limitations": ["Process-cold denotes newly started daemon/client, not cold OS/filesystem caches; no cache purge",
                                  "Native filter samples include Accessibility subprocess and 250ms debounce; they are not SQL query times",
                                  "Resident memory is the process footprint sampled between scroll steps, not a heap profile or a server-internal allocation guarantee",
                                  "Screenshot capture occurs after timing; first usable includes successful external row selection"]]
        Task {
            do {
                try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
                try await exercise(model, navigation)
                report["passed"] = true
            } catch {
                report["error"] = error.localizedDescription
                try? await capture("failure")
            }
            do {
                report["filter_samples"] = samples
                report["window_observations"] = windows
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

    private static let table = "track-table-songs"

    private static func exercise(_ model: PlayerModel, _ navigation: NavigationModel) async throws {
        guard let launched = argument("--scale-launch-uptime").flatMap(Double.init),
              launched <= now, let initialized = appLaunchUptime else {
            throw Failure(message: "Scale measurements require parent monotonic --scale-launch-uptime and NativeScale.markLaunch() at app initialization")
        }
        try await wait("initial Songs window", seconds: 45) {
            model.connected && model.snapshot?.queue_length == 10_000
                && NativeSongsSmoke.songsList.map { $0.cache.total == 50_000 && $0.cache.loadedRow(at: 0) != nil } == true
                && NSApp.windows.contains { $0.title == "tyranny likes smoke" && $0.isVisible }
        }
        guard let songs = NativeSongsSmoke.songsList else { throw Failure(message: "Songs list missing") }
        let modelReady = now
        // Kept even when the Accessibility steps below fail (for example without permission).
        report["startup"] = ["process_to_model_ready_ms": (modelReady - launched) * 1000,
                             "note": "first Songs window loaded; external usability not yet confirmed",
                             "cache_windows": songs.cache.loadedWindowIndices.count,
                             "cache_rows": songs.cache.loadedRows().count]
        try await NativeSmoke.drive("activate")
        // Successful external selection proves a usable real native table, rather
        // than merely timing publication of data into the model.
        try await NativeSmoke.drive("select", table, "0")
        try await wait("native row selection") { songs.selectedPositions.count == 1 }
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

        // The memory baseline is the post-launch footprint, with the first window shown.
        let baseline = residentBytes()
        try check(baseline > 0, "Resident memory is unavailable")
        report["memory"] = ["post_launch_resident_bytes": baseline, "limit_factor": 1.5]
        try observeWindows(songs, label: "initial", baseline: baseline)

        // Top to bottom through Accessibility. The cache keeps at most 10 windows (2,000 rows).
        let steps = 40
        for step in 1...steps {
            try await NativeSmoke.drive("scroll-fraction", table, String(Double(step) / Double(steps)))
            try await Task.sleep(for: .milliseconds(150))
            try observeWindows(songs, label: "scroll-\(step)", baseline: baseline)
        }
        try await wait("last window", seconds: 30) { songs.cache.loadedRow(at: 49_999) != nil }
        try observeWindows(songs, label: "scrolled-to-end", baseline: baseline)
        try await capture("scrolled-to-end")
        try await NativeSmoke.drive("scroll-fraction", table, "0")
        try await wait("first window again") { songs.cache.loadedRow(at: 0) != nil }

        for _ in 0..<3 {
            try await filter(model, songs, navigation, text: "Scale Track 00001", expected: 1, label: "selective-during-playback")
            try await filter(model, songs, navigation, text: "scale-no-match", expected: 0, label: "empty-during-playback")
            try await filter(model, songs, navigation, text: "Scale Track", expected: 50_000, label: "broad-during-playback")
        }
        try await capture("filtered-50000")
        if let scanPath = argument("--scale-scan-root") {
            guard let root = model.roots?.entries.first(where: { $0.path == scanPath }) else {
                throw Failure(message: "Real scan root is absent from the roots page")
            }
            try await NativeSmoke.drive("press", "status-connection")
            try await NativeSmoke.drive("press-title", "Library")
            try await NativeSmoke.drive("press", "rescan-root-\(root.id)")
            // Fast scans may finish before the main window is back in front. Record that
            // honestly instead of manufacturing concurrency or failing a timing threshold.
            try await wait("scan accepted", seconds: 30) {
                model.scanActive || model.snapshot?.library.scan.phase == "complete"
                    || model.snapshot?.library.scan.phase == "failed"
            }
            let observedActive = model.scanActive
            NSApp.windows.first(where: { $0.title == "tyranny likes smoke" })?.makeKeyAndOrderFront(nil)
            let firstSample = samples.count
            try await filter(model, songs, navigation, text: "Scale Track 00001", expected: 1, label: "requested-during-scan")
            try await filter(model, songs, navigation, text: "Scale Track", expected: 50_000, label: "requested-during-scan")
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
        try observeWindows(songs, label: "final", baseline: baseline)
        guard let after = model.snapshot else { throw Failure(message: "Lost authoritative snapshot") }
        let advanced = after.current_id != before.current_id || after.position_frames > before.position_frames
        report["playback"] = ["advanced": advanced, "underruns_before": before.underruns,
                              "underruns_after": after.underruns, "transport_after": after.transport]
        try check(after.transport == "playing" && advanced, "Playback did not advance during native scale actions")
        try check(after.underruns == before.underruns, "Playback underruns increased during native scale actions")
    }

    private static func filter(_ model: PlayerModel, _ songs: ListModel, _ navigation: NavigationModel,
                               text: String, expected: Int, label: String) async throws {
        let before = model.snapshot!
        let scanningBefore = model.scanActive
        // Distinct totals prove the next sample is the new filter's window.
        try check(songs.cache.total != expected, "Filter sample must change the observed total")
        let begin = now
        try await NativeSmoke.drive("value", "library-filter", text)
        try await wait("fresh native filter \(text)", seconds: 30) {
            navigation.filter == text && songs.cache.total == expected
                && (expected == 0 || songs.cache.loadedRow(at: 0)?.track.title.hasPrefix("Scale Track") == true)
        }
        let modelReadyMS = (now - begin) * 1000
        if expected > 0 {
            try await NativeSmoke.drive("select", table, "0")
            try await wait("filtered native row selection") { songs.selectedPositions.contains(0) }
        }
        let elapsed = (now - begin) * 1000
        guard let after = model.snapshot else { throw Failure(message: "Disconnected while filtering") }
        samples.append(["label": label, "query": text, "elapsed_ms": elapsed,
                        "model_ready_ms": modelReadyMS,
                        "completion": expected == 0 ? "empty list published; native screenshot recorded separately" : "external native row selection confirmed",
                        "expected_total": expected, "observed_total": songs.cache.total,
                        "returned": songs.cache.loadedRows().count,
                        "library_revision": songs.cache.revision ?? 0,
                        "scan_active_before": scanningBefore, "scan_active_after": model.scanActive,
                        "state_before": try json(before), "state_after": try json(after)])
        if expected == 0 && !screenshots.contains(output.appendingPathComponent("filtered-empty.png").path) {
            try await capture("filtered-empty")
        }
        try check(after.transport == "playing", "Filtering interrupted real WAV playback")
    }

    /// Records the cache's loaded windows and rows and the resident memory, and fails when the
    /// cache holds more than 10 windows (2,000 rows) or memory passes 1.5x of `baseline`.
    private static func observeWindows(_ songs: ListModel, label: String, baseline: UInt64) throws {
        let indices = songs.cache.loadedWindowIndices
        let rows = songs.cache.loadedRows().count
        let resident = residentBytes()
        windows.append(["label": label, "windows": indices.count, "rows": rows, "window_limit": 10,
                        "resident_bytes": resident, "total": songs.cache.total])
        try check(indices.count <= 10 && rows <= 2_000,
                  "ListWindowCache holds \(indices.count) windows and \(rows) rows at \(label); the limit is 10 windows (2,000 rows)")
        try check(Double(resident) <= Double(baseline) * 1.5,
                  "Resident memory \(resident) bytes passed 1.5x of the post-launch \(baseline) at \(label)")
    }

    /// The process's resident memory, from the kernel's resource usage record.
    private static func residentBytes() -> UInt64 {
        var usage = rusage_info_current()
        let status = withUnsafeMutablePointer(to: &usage) { pointer in
            pointer.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) {
                proc_pid_rusage(getpid(), RUSAGE_INFO_CURRENT, $0)
            }
        }
        return status == 0 ? usage.ri_resident_size : 0
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
        guard let window = NSApp.windows.first(where: { $0.title == "tyranny likes smoke" && $0.isVisible }) else {
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
