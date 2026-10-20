#if DEBUG
import AppKit
import Darwin
import Foundation
import TyrannyCore

/// Opt-in headless acceptance driver for the client's model layer (`--model-gate-output`): the real
/// server, the real `WireClient`, `PlayerModel`, `TransportModel` and `UpNextModel`, with no
/// Accessibility driver. The window opens but nothing is clicked. `scripts/client_gate.py --model-only`
/// starts it. Commands the model does not issue itself (the setup enqueue, a queue move) go over the
/// same connection through `PlayerModel.send`. It replaces the old `scripts/native_model_gate.swift`.
@MainActor
enum NativeModelGate {
    private struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    private static var started = false
    private static var cases: [String] = []

    private static func argument(_ name: String) -> String? {
        let args = CommandLine.arguments
        guard let index = args.firstIndex(of: name), index + 1 < args.count else { return nil }
        return args[index + 1]
    }

    static func startIfRequested(_ model: PlayerModel) {
        guard !started, let path = argument("--model-gate-output") else { return }
        started = true
        Task {
            var failure: String?
            do {
                guard let folder = argument("--model-gate-folder") else { throw Failure(message: "--model-gate-folder required") }
                try await exercise(model, folder: folder, serverPID: argument("--model-gate-server-pid").flatMap(Int32.init))
            } catch {
                failure = error.localizedDescription
            }
            var report: [String: Any] = ["passed": failure == nil, "cases": cases, "visual_verification": false,
                "scope": "Real WireClient, PlayerModel, TransportModel and UpNextModel against the real server; not SwiftUI input or screenshot verification"]
            if let failure { report["failure"] = failure }
            do {
                try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys])
                    .write(to: URL(fileURLWithPath: path))
            } catch { print(error) }
            print("NATIVE_MODEL_GATE \(failure == nil ? "PASS" : "FAIL") \(path)")
            NSApp.terminate(nil)
        }
    }

    // v9: now_playing.gain_db appears once a gain mode is active; every other field must stay identical.
    private static func sameTrack(_ lhs: NowPlaying?, _ rhs: NowPlaying) -> Bool {
        guard let lhs else { return false }
        return lhs.entry_id == rhs.entry_id && lhs.title == rhs.title && lhs.artist == rhs.artist && lhs.album == rhs.album
            && lhs.duration_frames == rhs.duration_frames && lhs.metadata_error == rhs.metadata_error
            && lhs.track_id == rhs.track_id && lhs.album_id == rhs.album_id && lhs.format == rhs.format
    }

    private static func queue(_ model: PlayerModel) async throws -> Page<QueueItem> {
        try await model.send("queue_page", ["revision": .null, "offset": .int(0), "limit": .int(500)])
    }

    private static func exercise(_ model: PlayerModel, folder: String, serverPID: Int32?) async throws {
        let transport = model.transport
        let probe = WireClient()
        for endpoint in ["ws://localhost:9865/ws", "ws://10.0.0.1:9865/ws", "ws://user@127.0.0.1:9865/ws",
                         "ws://127.0.0.1:9865/ws?secret=x", "wss://127.0.0.1:9865/ws"] {
            do {
                try await probe.connect(endpoint: endpoint, tokenPath: "/nonexistent-token-must-not-be-opened")
                throw Failure(message: "Unsafe endpoint was accepted")
            } catch let error as ServerFailure {
                try check(error.code == "invalid_endpoint", "Endpoint validation happened after credential access")
            }
        }
        cases.append("unsafe-destinations-rejected-before-credential-access")
        let badToken = FileManager.default.temporaryDirectory.appendingPathComponent("tyranny-model-gate-wrong-token")
        FileManager.default.createFile(atPath: badToken.path, contents: Data(String(repeating: "0", count: 64).utf8),
                                       attributes: [.posixPermissions: 0o600])
        defer { try? FileManager.default.removeItem(at: badToken) }
        do {
            try await probe.connect(endpoint: model.endpoint, tokenPath: badToken.path)
            throw Failure(message: "Wrong bearer token was accepted")
        } catch is ServerFailure { }
        cases.append("real-server-rejects-wrong-native-bearer-token")

        try await wait("connected empty library") { model.connected && model.snapshot != nil }
        await model.addRoot(folder)
        try await wait("native import", seconds: 30) {
            model.snapshot?.library.scan.phase == "complete" && model.snapshot?.library.tracks == 105
        }
        try await wait("roots page") { model.roots?.total == 1 && model.roots?.entries.first?.path == folder }
        cases.append("real-native-library-import-and-roots-page")

        // Two tracks enqueued and played as one selection.
        guard let state = model.snapshot else { throw Failure(message: "No snapshot") }
        let rows: ListTracksResponse = try await model.send("list_tracks", [
            "list": try JSONValue.encoding(ListDescription(source: .library)), "revision": .null,
            "offset": .int(0), "limit": .int(2)])
        let selection = TrackSelection.ids(libraryRevision: rows.revision, trackIDs: rows.entries.map(\.track.id))
        let _: Applied = try await model.send("enqueue_tracks", [
            "selection": try JSONValue.encoding(selection), "expected_revision": .uint(state.queue_revision),
            "placement": .string(Placement.replaceUpcoming.rawValue), "play": .bool(true),
            "source": try JSONValue.encoding(QueueSource(kind: .songs, name: "Songs"))])
        try await wait("selected playback") { !model.busy && model.snapshot?.queue_length == 2 && model.snapshot?.transport == "playing" }
        try check(try await queue(model).entries.count == 2, "Queue did not hold the two selected tracks")
        cases.append("selection-enqueue-and-authoritative-playback")

        transport.playPause()
        try await wait("pause") { !model.busy && model.snapshot?.transport == "paused" }
        guard let firstEntry = model.snapshot?.current_id, let metadata = model.snapshot?.now_playing,
              let duration = model.durationSeconds, let durationFrames = metadata.duration_frames, durationFrames > 0 else {
            throw Failure(message: "Current occurrence metadata/duration missing")
        }
        let encodedState = try JSONEncoder().encode(model.snapshot!)
        let stateFields = try JSONSerialization.jsonObject(with: encodedState) as! [String: Any]
        for required in ["can_next", "can_previous", "now_playing", "playlists_revision", "stations_revision"] {
            var missingField = stateFields
            missingField.removeValue(forKey: required)
            do {
                _ = try JSONDecoder().decode(ControlState.self, from: JSONSerialization.data(withJSONObject: missingField))
                throw Failure(message: "Protocol v9 accepted missing \(required)")
            } catch is DecodingError { }
        }
        cases.append("protocol-v9-requires-authoritative-metadata-capabilities-and-playlists-revision")
        transport.pause()
        try await wait("idempotent pause") { !model.busy && model.snapshot?.transport == "paused" }
        transport.seek(seconds: .nan, entryID: firstEntry)
        transport.seek(seconds: .infinity, entryID: firstEntry)
        transport.seek(seconds: -1, entryID: firstEntry)
        transport.seek(seconds: 1, entryID: UInt64.max)
        try check(!model.busy, "Invalid seek reached transport")
        transport.seek(seconds: duration, entryID: firstEntry)
        try await wait("last valid seek frame") { !model.busy && model.snapshot?.position_frames == durationFrames - 1 }
        transport.previous()
        try await wait("previous restart") { !model.busy && model.snapshot?.current_id == firstEntry && model.positionSeconds == 0 }
        transport.next()
        try await wait("next paused") { !model.busy && model.snapshot?.current_id != firstEntry && model.snapshot?.transport == "paused" }
        try check(model.snapshot?.can_next == false && model.snapshot?.can_previous == true, "Incorrect last-entry capabilities")
        transport.previous()
        try await wait("previous paused") { !model.busy && model.snapshot?.current_id == firstEntry && model.snapshot?.transport == "paused" }
        try check(model.snapshot?.now_playing == metadata, "Previous returned incorrect metadata")
        cases.append("occurrence-seek-validation-duration-boundary-next-previous-and-idempotent-pause")
        transport.seek(seconds: 3, entryID: firstEntry)
        try await wait("seek") { !model.busy && model.positionSeconds >= 2.99 && model.positionSeconds < 3.1 }
        transport.setGain("album")
        try await wait("gain") { !model.busy && model.snapshot?.settings.gain_mode == "album" }
        transport.setResume(true)
        try await wait("resume setting") { !model.busy && model.snapshot?.settings.resume_on_start == true }
        transport.playPause()
        try await wait("resume") { !model.busy && model.snapshot?.transport == "playing" }
        cases.append("pause-seek-gain-resume-settings-and-play")

        // A queue drag captured before the queue changed is refused, and nothing is replayed.
        let order = try await queue(model).entries.map(\.id)
        guard let staleDrag = model.upNext.beginDrag(ids: [order[0]]) else { throw Failure(message: "Could not capture queue drag") }
        let beforeMove = model.snapshot!.queue_revision
        let _: Applied = try await model.send("move", [
            "expected_revision": .uint(beforeMove), "entry_ids": .uints([order[1]]),
            "before": .uint(order[0])])
        try await wait("move") { !model.busy && model.snapshot?.queue_revision != beforeMove }
        let edited = try await queue(model)
        try check(edited.entries.first?.id == order[1], "The move did not reorder the queue")
        try check(!model.upNext.drop(token: staleDrag, at: 0), "Stale queue drag was accepted")
        try check(try await queue(model) == edited, "Rejected stale drag changed queue contents")
        cases.append("stale-queue-drag-rejected-without-replaying")
        let _: Applied = try await model.send("remove", [
            "expected_revision": .uint(model.snapshot!.queue_revision), "entry_id": .uint(order[1])])
        try await wait("remove") { !model.busy && model.snapshot?.queue_length == 1 && model.snapshot?.current_id == order[0] }
        cases.append("queue-reorder-removal-and-current-identity")

        // Reconnect clears captured drags even when the queue is unchanged.
        let before = model.snapshot!.position_frames
        guard let reconnectDrag = model.upNext.beginDrag(ids: [firstEntry]) else { throw Failure(message: "Could not capture drag before reconnect") }
        model.reconnect()
        try await wait("authoritative reconnect") {
            model.connected && model.snapshot?.queue_length == 1 && (model.snapshot?.position_frames ?? 0) > before
        }
        try check(model.snapshot?.transport == "playing", "Reconnect interrupted playback")
        try check(sameTrack(model.snapshot?.now_playing, metadata), "Reconnect lost current metadata")
        let reconnected = try await queue(model)
        try check(!model.upNext.drop(token: reconnectDrag, at: 0), "Previous-connection drag was accepted")
        try check(try await queue(model) == reconnected, "Rejected reconnect drag changed queue contents")
        cases.append("reconnect-invalidates-captured-drag-even-with-unchanged-queue")
        cases.append("reconnect-clears-cache-without-replaying-queue-mutations")

        model.disconnect()
        try await probe.connect(endpoint: model.endpoint, tokenPath: model.tokenPath)
        let state2 = try JSONDecoder().decode(SnapshotReply.self, from: await probe.request("snapshot")).snapshot
        try check(state2.transport == "playing" && state2.queue_length == 1 && state2.underruns == 0, "Controller disconnect changed server playback")
        cases.append("controller-disconnect-preserves-server-playback")
        let _: Data = try await probe.request("pause")
        let paused = try JSONDecoder().decode(SnapshotReply.self, from: await probe.request("snapshot")).snapshot
        do {
            let _: Data = try await probe.request("seek", fields: ["entry_id": .uint(UInt64.max), "position_frames": .uint(0)])
            throw Failure(message: "Stale occurrence seek succeeded")
        } catch let error as ServerFailure { try check(error.code == "invalid_command", "Wrong stale occurrence seek error") }
        let afterStaleSeek = try JSONDecoder().decode(SnapshotReply.self, from: await probe.request("snapshot")).snapshot
        try check(afterStaleSeek.current_id == paused.current_id && afterStaleSeek.position_frames == paused.position_frames,
                  "Rejected occurrence seek changed playback")
        cases.append("stale-occurrence-seek-rejected-without-changing-playback")
        do {
            let _: Data = try await probe.request("clear", fields: ["expected_revision": .uint(0)])
            throw Failure(message: "Stale queue edit succeeded")
        } catch let error as ServerFailure { try check(error.code == "stale_revision", "Wrong stale error") }
        var after = try JSONDecoder().decode(SnapshotReply.self, from: await probe.request("snapshot")).snapshot
        try check(after.queue_revision == paused.queue_revision && after.queue_length == 1, "Rejected mutation changed queue")
        cases.append("typed-stale-mutation-failure-without-retry")

        // The whole library enqueued as one list selection, atomically, then cleared.
        model.start()
        try await wait("boundary setup") { model.connected && model.snapshot?.queue_length == 1 }
        guard let fresh = model.snapshot else { throw Failure(message: "No snapshot") }
        let everything = SelectionEncoder.source(list: ListDescription(source: .library), revision: fresh.library.revision)
        let _: Applied = try await model.send("enqueue_tracks", [
            "selection": try JSONValue.encoding(everything), "expected_revision": .uint(fresh.queue_revision),
            "placement": .string(Placement.append.rawValue), "play": .bool(false),
            "source": try JSONValue.encoding(QueueSource(kind: .songs, name: "Songs"))])
        try await wait("whole library queued") { !model.busy && model.snapshot?.queue_length == 106 }
        try check(sameTrack(model.snapshot?.now_playing, metadata) && model.snapshot?.can_next == true,
                  "Current metadata or capabilities depend on the queue contents")
        let _: Applied = try await model.send("clear", ["expected_revision": .uint(model.snapshot!.queue_revision)])
        try await wait("clear large queue") { !model.busy && model.snapshot?.queue_length == 0 }
        cases.append("whole-library-selection-enqueues-atomically-and-clears")
        model.disconnect()
        after = try JSONDecoder().decode(SnapshotReply.self, from: await probe.request("snapshot")).snapshot

        if let pid = serverPID {
            try check(kill(pid, SIGSTOP) == 0, "Could not suspend fixture server")
            defer { kill(pid, SIGCONT) }
            do {
                let _: Data = try await probe.request("clear", fields: ["expected_revision": .uint(after.queue_revision)])
                throw Failure(message: "Suspended server unexpectedly replied")
            } catch let error as ServerFailure {
                try check(error.code == "uncertain_outcome", "Timeout did not report uncertain outcome")
            }
            kill(pid, SIGCONT)
            try await probe.connect(endpoint: model.endpoint, tokenPath: model.tokenPath)
            let recovered = try JSONDecoder().decode(SnapshotReply.self, from: await probe.request("snapshot")).snapshot
            try check(recovered.queue_revision <= after.queue_revision + 1, "Unanswered clear was replayed")
            cases.append("real-request-timeout-closes-connection-and-never-replays")
        }
        probe.disconnect()
    }

    private static func check(_ value: Bool, _ message: String) throws { if !value { throw Failure(message: message) } }
    private static func wait(_ label: String, seconds: Double = 15, until predicate: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !predicate() {
            if Date() >= deadline { throw Failure(message: "Timed out: \(label)") }
            try await Task.sleep(for: .milliseconds(30))
        }
        try await Task.sleep(for: .milliseconds(100))
    }
}
#endif
