import AppKit
import Darwin
import Foundation

@main
@MainActor
struct NativeModelGate {
    struct Failure: Error, LocalizedError {
        let message: String
        var errorDescription: String? { message }
    }
    static func main() async {
        let args = CommandLine.arguments
        func argument(_ name: String) -> String {
            guard let index = args.firstIndex(of: name), index + 1 < args.count else { fatalError("Missing \(name)") }
            return args[index + 1]
        }
        let output = URL(fileURLWithPath: argument("--output"))
        let folder = argument("--folder")
        let model = PlayerModel()
        var cases: [String] = []
        var failure: String?
        do {
            let probe = WireClient()
            for endpoint in ["ws://localhost:9865/ws", "ws://10.0.0.1:9865/ws", "ws://user@127.0.0.1:9865/ws", "ws://127.0.0.1:9865/ws?secret=x", "wss://127.0.0.1:9865/ws"] {
                do {
                    try await probe.connect(endpoint: endpoint, tokenPath: "/nonexistent-token-must-not-be-opened")
                    throw Failure(message: "Unsafe endpoint was accepted")
                } catch let error as ServerFailure {
                    try check(error.code == "invalid_endpoint", "Endpoint validation happened after credential access")
                }
            }
            cases.append("unsafe-destinations-rejected-before-credential-access")
            let badToken = output.deletingLastPathComponent().appendingPathComponent("wrong-token")
            FileManager.default.createFile(atPath: badToken.path, contents: Data(String(repeating: "0", count: 64).utf8), attributes: [.posixPermissions: 0o600])
            do {
                try await probe.connect(endpoint: model.endpoint, tokenPath: badToken.path)
                throw Failure(message: "Wrong bearer token was accepted")
            } catch is ServerFailure { }
            try FileManager.default.removeItem(at: badToken)
            cases.append("real-server-rejects-wrong-native-bearer-token")

            model.start()
            try await wait("connected empty library") { model.connected && model.tracks != nil }
            await model.addRoot(folder)
            try await wait("native import", seconds: 30) { model.snapshot?.library.scan.phase == "complete" && model.tracks?.total == 105 }
            try check(model.tracks?.entries.count == 100, "Page loaded more than 100 rows")
            model.pageTracks(1)
            try await wait("second page") { model.tracks?.offset == 100 && model.tracks?.entries.count == 5 }
            model.pageTracks(-1)
            try await wait("first page") { model.tracks?.offset == 0 }
            cases.append("real-native-library-import-and-bounded-page-replacement")
            model.selectedArtist = model.artists?.entries.first?.id
            model.selectionChanged()
            try await wait("artist selection") { model.tracks == nil && model.albums?.entries.count == 1 }
            model.selectedAlbum = model.albums?.entries.first?.id
            model.selectionChanged()
            try await wait("album tracks") { model.tracks?.entries.count == 100 }
            model.selectedTracks = Set(model.tracks!.entries.prefix(2).map(\.id))
            model.enqueueSelection(play: true)
            try await wait("selected playback") { !model.busy && model.snapshot?.queue_length == 2 && model.snapshot?.transport == "playing" }
            try check(model.queue?.entries.count == 2, "Queue did not refresh")
            cases.append("selection-serial-enqueue-and-authoritative-playback")
            model.playPause()
            try await wait("pause") { !model.busy && model.snapshot?.transport == "paused" }
            guard let firstEntry = model.snapshot?.current_id, let metadata = model.snapshot?.now_playing,
                  let duration = model.durationSeconds, let durationFrames = metadata.duration_frames, durationFrames > 0 else {
                throw Failure(message: "Current occurrence metadata/duration missing")
            }
            let encodedState = try JSONEncoder().encode(model.snapshot!)
            let stateFields = try JSONSerialization.jsonObject(with: encodedState) as! [String: Any]
            for required in ["can_next", "can_previous", "now_playing", "playlists_revision"] {
                var missingField = stateFields
                missingField.removeValue(forKey: required)
                do {
                    _ = try JSONDecoder().decode(ControlState.self, from: JSONSerialization.data(withJSONObject: missingField))
                    throw Failure(message: "Protocol v5 accepted missing \(required)")
                } catch is DecodingError { }
            }
            cases.append("protocol-v9-requires-authoritative-metadata-capabilities-and-playlists-revision")
            model.pause()
            try await wait("idempotent pause") { !model.busy && model.snapshot?.transport == "paused" }
            model.seek(seconds: .nan, entryID: firstEntry)
            model.seek(seconds: .infinity, entryID: firstEntry)
            model.seek(seconds: -1, entryID: firstEntry)
            model.seek(seconds: 1, entryID: UInt64.max)
            try check(!model.busy, "Invalid seek reached transport")
            model.seek(seconds: duration, entryID: firstEntry)
            try await wait("last valid seek frame") { !model.busy && model.snapshot?.position_frames == durationFrames - 1 }
            model.previous()
            try await wait("previous restart") { !model.busy && model.snapshot?.current_id == firstEntry && model.positionSeconds == 0 }
            model.next()
            try await wait("next paused") { !model.busy && model.snapshot?.current_id != firstEntry && model.snapshot?.transport == "paused" }
            try check(model.snapshot?.can_next == false && model.snapshot?.can_previous == true, "Incorrect last-entry capabilities")
            model.previous()
            try await wait("previous paused") { !model.busy && model.snapshot?.current_id == firstEntry && model.snapshot?.transport == "paused" }
            try check(model.snapshot?.now_playing == metadata, "Previous returned incorrect metadata")
            cases.append("occurrence-seek-validation-duration-boundary-next-previous-and-idempotent-pause")
            model.seek(seconds: 3, entryID: firstEntry)
            try await wait("seek") { !model.busy && model.positionSeconds >= 2.99 && model.positionSeconds < 3.1 }
            model.setGain("album")
            try await wait("gain") { !model.busy && model.snapshot?.settings.gain_mode == "album" }
            model.setResume(true)
            try await wait("resume setting") { !model.busy && model.snapshot?.settings.resume_on_start == true }
            model.playPause()
            try await wait("resume") { !model.busy && model.snapshot?.transport == "playing" }
            cases.append("pause-seek-gain-resume-settings-and-play")
            let order = model.queue!.entries.map(\.id)
            guard let staleDrag = model.beginTrackDrag(source: .queue, ids: [order[0]]) else {
                throw Failure(message: "Could not capture queue drag")
            }
            model.selectedQueue = [order[1]]
            model.moveQueueSelection(-1)
            try await wait("move") { !model.busy && model.queue?.entries.first?.id == order[1] }
            let editedQueue = model.queue!
            try check(!model.dropTracks(token: staleDrag, destination: .queue, row: 0), "Stale queue drag was accepted")
            try check(model.queue == editedQueue, "Rejected stale drag changed queue contents")
            cases.append("stale-queue-drag-rejected-without-replaying")
            model.removeQueueSelection()
            try await wait("remove") { !model.busy && model.queue?.entries.count == 1 && model.snapshot?.current_id == order[0] }
            cases.append("queue-reorder-removal-and-current-identity")
            guard let oldLibraryRow = model.tracks?.entries.first,
                  let filteredDrag = model.beginTrackDrag(source: .library, ids: [oldLibraryRow.id]) else {
                throw Failure(message: "Could not capture library drag before changing filter")
            }
            let queueBeforeFilter = model.snapshot!.queue_revision
            model.filter = "no-such-track"
            model.filterChanged()
            try check(!model.libraryReady && model.tracks == nil && model.selectedTracks.isEmpty, "Filter debounce retained actionable old library rows")
            try check(!model.dropTracks(token: filteredDrag, destination: .queue, row: 0), "Changed source filter accepted a captured drag")
            model.enqueueSelection(play: true)
            try check(!model.busy && model.snapshot?.queue_revision == queueBeforeFilter, "Stale library selection started a mutation while filtering")
            cases.append("source-filter-change-immediately-invalidates-drag-and-play-selection")
            try await wait("empty filter") { model.tracks?.total == 0 }
            try check(model.snapshot?.now_playing == metadata && model.durationSeconds == duration, "Filter lost current metadata")
            model.filter = "Track 104"; model.filterChanged()
            model.filter = "Track 103"; model.filterChanged()
            try await wait("latest filter wins") { model.tracks?.entries.first?.metadata.title == "Track 103" && model.tracks?.total == 1 }
            model.filter = ""; model.filterChanged()
            try await wait("reset filter") { model.tracks?.total == 105 }
            model.pageTracks(1)
            try await wait("playing metadata off page") { model.tracks?.offset == 100 }
            try check(model.snapshot?.now_playing == metadata && model.currentName.contains(metadata.title), "Paging lost current metadata")
            model.pageTracks(-1)
            try await wait("restore library page") { model.tracks?.offset == 0 }
            cases.append("authoritative-now-playing-survives-library-filter-and-page-replacement")
            cases.append("debounced-filter-and-superseded-query-isolation")
            let before = model.snapshot!.position_frames
            guard let reconnectDrag = model.beginTrackDrag(source: .queue, ids: [firstEntry]) else {
                throw Failure(message: "Could not capture drag before reconnect")
            }
            model.reconnect()
            try await wait("authoritative reconnect") { model.connected && model.snapshot?.queue_length == 1 && (model.snapshot?.position_frames ?? 0) > before }
            try check(model.snapshot?.transport == "playing", "Reconnect interrupted playback")
            try check(model.snapshot?.now_playing == metadata, "Reconnect lost current metadata")
            try await wait("queue refreshed after reconnect") { model.queue?.entries.count == 1 }
            let reconnectedQueue = model.queue!
            try check(!model.dropTracks(token: reconnectDrag, destination: .queue, row: 0), "Previous-connection drag was accepted")
            try check(model.queue == reconnectedQueue, "Rejected reconnect drag changed queue contents")
            cases.append("reconnect-invalidates-captured-drag-even-with-unchanged-queue")
            cases.append("reconnect-clears-cache-without-replaying-queue-mutations")
            model.disconnect()
            try await probe.connect(endpoint: model.endpoint, tokenPath: model.tokenPath)
            let state = try JSONDecoder().decode(SnapshotReply.self, from: await probe.request("snapshot")).snapshot
            try check(state.transport == "playing" && state.queue_length == 1 && state.underruns == 0, "Controller disconnect changed server playback")
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
            model.start()
            try await wait("boundary setup") { model.connected && model.tracks?.entries.count == 100 && model.queue?.total == 1 }
            model.selectedTracks = Set(model.tracks!.entries.map(\.id))
            model.enqueueSelection(play: false)
            try await wait("bounded large queue") { !model.busy && model.queue?.total == 101 && model.queue?.entries.count == 100 }
            model.pageQueue(1)
            try await wait("queue second page") { model.queue?.offset == 100 && model.queue?.entries.count == 1 }
            try check(model.snapshot?.now_playing == metadata && model.snapshot?.can_next == true,
                      "Off-page current metadata/capabilities depend on loaded queue page")
            let boundaryID = model.queue!.entries[0].id
            model.selectedQueue = [boundaryID]
            model.moveQueueSelection(-1)
            try await wait("move across previous page", seconds: 3) { !model.busy && model.queue?.entries.first?.id != boundaryID }
            model.pageQueue(-1)
            try await wait("moved entry on first page") { model.queue?.offset == 0 && model.queue?.entries.last?.id == boundaryID }
            model.selectedQueue = [boundaryID]
            model.moveQueueSelection(1)
            try await wait("move across next page", seconds: 3) { !model.busy && model.queue?.entries.last?.id != boundaryID }
            model.pageQueue(1)
            try await wait("restored queue boundary") { model.queue?.offset == 100 && model.queue?.entries.first?.id == boundaryID }
            model.clearQueue()
            try await wait("clear large queue") { !model.busy && model.snapshot?.queue_length == 0 && model.queue?.entries.isEmpty == true }
            model.disconnect()
            after = try JSONDecoder().decode(SnapshotReply.self, from: await probe.request("snapshot")).snapshot
            cases.append("queue-moves-cross-page-boundaries-without-hydrating-queue")
            if let pid = Int32(argument("--server-pid")) {
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
        } catch { failure = error.localizedDescription }
        model.shutdown()
        var report: [String: Any] = ["passed": failure == nil, "cases": cases, "visual_verification": false, "scope": "Real native transport and PlayerModel; not SwiftUI input or screenshot verification"]
        if let failure { report["failure"] = failure }
        do { try JSONSerialization.data(withJSONObject: report, options: [.prettyPrinted, .sortedKeys]).write(to: output) }
        catch { print(error); exit(2) }
        print("NATIVE_MODEL_GATE \(failure == nil ? "PASS" : "FAIL") \(output.path)")
        if failure != nil { exit(1) }
    }
    static func check(_ value: Bool, _ message: String) throws { if !value { throw Failure(message: message) } }
    static func wait(_ label: String, seconds: Double = 15, until predicate: () -> Bool) async throws {
        let deadline = Date().addingTimeInterval(seconds)
        while !predicate() {
            if Date() >= deadline { throw Failure(message: "Timed out: \(label)") }
            try await Task.sleep(for: .milliseconds(30))
        }
        try await Task.sleep(for: .milliseconds(100))
    }
}
