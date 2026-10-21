import Foundation
import Observation
import TyrannyCore

/// One USB operation at a time, off the main actor. The playback server never owns USB.
@MainActor @Observable
final class DeviceSyncModel {
    private(set) var devices: [MTPDevice] = []
    private(set) var busy = false
    private(set) var status = ""
    private var operationError: String?
    private var discoveryError: String?
    var error: String? {
        get { operationError ?? discoveryError }
        set {
            operationError = newValue
            discoveryError = nil
        }
    }
    var selectedStorage: [String: UInt32] = [:]
    private(set) var generation: UInt64 = 0
    @ObservationIgnored private var working = false
    @ObservationIgnored private var monitor: Task<Void, Never>?
    @ObservationIgnored private var matchTask: Task<Void, Never>?
    @ObservationIgnored private var pending: [String: Set<String>] = [:]
    @ObservationIgnored private var checked: [String: Set<String>] = [:]
    @ObservationIgnored private var matched: [String: [String: MTPMatch]] = [:]
    @ObservationIgnored private var checking: [String: Set<String>] = [:]
    @ObservationIgnored private var matchFailures: Set<String> = []
    @ObservationIgnored private var contentEpoch: UInt64 = 0
    @ObservationIgnored private var inventoried: Set<String> = []

    func hasInventory(_ id: String) -> Bool {
        _ = generation
        return inventoried.contains(id)
    }

    func device(_ id: String) -> MTPDevice? { devices.first { $0.id == id } }

    func storageID(for device: MTPDevice) -> UInt32? {
        if let selected = selectedStorage[device.id], device.storages.contains(where: { $0.id == selected }) {
            return selected
        }
        return device.storages.first?.id
    }

    func start() {
        guard monitor == nil else { return }
        monitor = Task { [weak self] in
            while !Task.isCancelled {
                await self?.refresh()
                do { try await Task.sleep(for: .seconds(3)) } catch { break }
            }
        }
    }

    /// Discovery is cheap: no recursive song scan, and never contends with a transfer.
    func refresh() async {
        guard !working else { return }
        working = true
        defer { working = false }
        do {
            let response = try await MTPHelper.run(MTPRequest(operation: "discover"))
            if let problem = response.error { throw DeviceSyncFailure(problem) }
            // A recovered connection must not keep a stale discovery error, or
            // erase a failed copy/delete that still needs the user's attention.
            discoveryError = nil
            let discovered = response.devices ?? []
            let ids = Set(discovered.map(\.id))
            let previous = Dictionary(uniqueKeysWithValues: devices.map { ($0.id, $0) })
            let next = discovered.map { found -> MTPDevice in
                var found = found
                found.songs = previous[found.id]?.songs ?? []
                return found
            }
            if devices != next {
                devices = next
                generation &+= 1
            }
            for id in previous.keys where !ids.contains(id) {
                matched[id] = nil; checked[id] = nil; pending[id] = nil
                checking[id] = nil; matchFailures.remove(id)
                inventoried.remove(id)
            }
        } catch {
            // Don't keep a stale connected target when USB discovery fails.
            if !devices.isEmpty {
                devices = []; matched = [:]; checked = [:]; pending = [:]; checking = [:]; matchFailures = []
                inventoried = []
                generation &+= 1
            }
            discoveryError = error.localizedDescription
        }
    }

    private func acquire() async throws {
        while working {
            try await Task.sleep(for: .milliseconds(100))
        }
        try Task.checkCancellation()
        working = true
    }

    func inventory(_ id: String, onlyIfNeeded: Bool = false) async {
        do {
            try await acquire()
            defer { working = false; busy = false }
            guard device(id) != nil else { return }
            if onlyIfNeeded && inventoried.contains(id) { return }
            busy = true; error = nil; status = "Reading songs on device…"
            let reply = try await MTPHelper.run(MTPRequest(operation: "inventory", device_id: id))
            apply(reply, id: id, invalidate: true)
            if let problem = reply.error { throw DeviceSyncFailure(problem) }
            status = "Device is up to date."
        } catch is CancellationError {
            // Leaving a view cancels its queued read, not a USB mutation.
        } catch { self.error = error.localizedDescription; status = "Could not read device." }
    }

    func sync(deviceID: String, paths: [String]) async {
        let chosenStorage = device(deviceID).flatMap { storageID(for: $0) }
        do {
            try await acquire()
            defer { working = false; busy = false }
            guard let target = device(deviceID), let storage = chosenStorage,
                  target.storages.contains(where: { $0.id == storage }) else {
                throw DeviceSyncFailure("The device or selected storage disconnected. Nothing was copied.")
            }
            busy = true; error = nil; status = "Checking and copying \(paths.count) songs…"
            let reply = try await MTPHelper.run(MTPRequest(operation: "sync", device_id: deviceID,
                storage_id: storage, files: paths.map { MTPFile(path: $0) }))
            apply(reply, id: deviceID, invalidate: true)
            if let problem = reply.error { throw DeviceSyncFailure(problem) }
            record(reply, paths: paths, id: deviceID)
            status = "Copied \(reply.copied ?? 0); already on device \(reply.skipped ?? 0)."
        } catch { self.error = error.localizedDescription; status = "Sync stopped. Review the error before trying again." }
    }

    func remove(deviceID: String, storageID: UInt32, songs: [MTPSong]) async {
        guard !songs.isEmpty else { return }
        do {
            try await acquire()
            defer { working = false; busy = false }
            guard let target = device(deviceID),
                  songs.allSatisfy({ song in song.storage_id == storageID && target.songs.contains(song) }) else {
                throw DeviceSyncFailure("The device inventory changed. Refresh and select the songs again.")
            }
            busy = true; error = nil; status = "Removing \(songs.count) songs…"
            let reply = try await MTPHelper.run(MTPRequest(operation: "delete", device_id: deviceID,
                storage_id: storageID, object_ids: songs.map(\.id), expected_songs: songs))
            apply(reply, id: deviceID, invalidate: true)
            if let problem = reply.error { throw DeviceSyncFailure(problem) }
            status = "Removed \(songs.count) songs from the device. Library files were not changed."
        } catch { self.error = error.localizedDescription; status = "Removal stopped. Refresh before trying again." }
    }

    private func apply(_ reply: MTPResponse, id: String, invalidate: Bool) {
        if let found = reply.device, let index = devices.firstIndex(where: { $0.id == id }) {
            devices[index] = found
            inventoried.insert(id)
        }
        if invalidate {
            matched[id] = nil; checked[id] = nil; matchFailures.remove(id)
            contentEpoch &+= 1
        }
        for match in reply.matches ?? [] { matched[id, default: [:]][match.path] = match }
        generation &+= 1
    }

    private func record(_ reply: MTPResponse, paths: [String], id: String) {
        checked[id, default: []].formUnion(paths)
        for path in paths { matched[id]?[path] = nil }
        for match in reply.matches ?? [] { matched[id, default: [:]][match.path] = match }
        generation &+= 1
    }

    /// Called only for visible library rows. Never labels a filename/size guess as synced.
    func check(_ tracks: [TrackSummary]) {
        guard !tracks.isEmpty, !devices.isEmpty else { return }
        for target in devices where !matchFailures.contains(target.id) {
            let paths = tracks.map(\.path).filter {
                !(checked[target.id]?.contains($0) ?? false) && !(checking[target.id]?.contains($0) ?? false)
            }
            pending[target.id, default: []].formUnion(paths)
        }
        guard matchTask == nil, pending.values.contains(where: { !$0.isEmpty }) else { return }
        matchTask = Task { [weak self] in
            do { try await Task.sleep(for: .milliseconds(300)) } catch { return }
            await self?.checkPending()
            self?.matchTask = nil
        }
    }

    private func checkPending() async {
        while let job = pending.first(where: { !$0.value.isEmpty }) {
            let id = job.key
            let paths = Array(job.value.prefix(100))
            pending[id]?.subtract(paths)
            checking[id, default: []].formUnion(paths)
            defer { checking[id]?.subtract(paths) }
            do {
                try await acquire()
                defer { working = false }
                guard device(id) != nil else { continue }
                let epoch = contentEpoch
                let reply = try await MTPHelper.run(MTPRequest(operation: "match", device_id: id,
                    files: paths.map { MTPFile(path: $0) }))
                guard epoch == contentEpoch else { continue }
                apply(reply, id: id, invalidate: false)
                if let problem = reply.error { throw DeviceSyncFailure(problem) }
                record(reply, paths: paths, id: id)
            } catch {
                self.error = error.localizedDescription
                matchFailures.insert(id)
                generation &+= 1
                // Keep unknown state; an explicit inventory refresh enables a new check.
                pending = [:]
                return
            }
        }
    }

    func syncStatus(for track: TrackSummary, deviceID: String) -> String {
        _ = generation
        if matchFailures.contains(deviceID) { return "Could not check" }
        if matched[deviceID]?[track.path] != nil { return "Synced" }
        return checked[deviceID]?.contains(track.path) == true ? "Not synced" : "Checking…"
    }

    /// Library tag/file changes invalidate content identity, even when track IDs are stable.
    func libraryChanged() {
        checked = [:]; matched = [:]; matchFailures = []
        contentEpoch &+= 1
        generation &+= 1
    }

    isolated deinit { monitor?.cancel(); matchTask?.cancel() }
}

private enum MTPHelper {
    static func run(_ request: MTPRequest) async throws -> MTPResponse {
        try await Task.detached(priority: .utility) {
            let fm = FileManager.default
            let bundled = Bundle.main.executableURL?.deletingLastPathComponent().appendingPathComponent("tyranny-mtp")
            let development = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
                .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
                .deletingLastPathComponent().appendingPathComponent("target/mtp/tyranny-mtp")
            let override = ProcessInfo.processInfo.environment["TYRANNY_MTP_HELPER"].map { URL(fileURLWithPath: $0) }
            guard let executable = [override, bundled, development].compactMap({ $0 })
                .first(where: { fm.isExecutableFile(atPath: $0.path) }) else {
                throw DeviceSyncFailure("The MTP helper is missing. Rebuild Tyranny with scripts/build_mtp.sh, or reinstall the app.")
            }
            let logURL = fm.temporaryDirectory.appendingPathComponent("tyranny-mtp-\(UUID().uuidString).log")
            guard fm.createFile(atPath: logURL.path, contents: nil, attributes: [.posixPermissions: 0o600]) else {
                throw DeviceSyncFailure("Cannot create the device diagnostic log.")
            }
            defer { try? fm.removeItem(at: logURL) }
            let log = try FileHandle(forWritingTo: logURL)
            defer { try? log.close() }
            let process = Process()
            let input = Pipe(), output = Pipe()
            process.executableURL = executable
            process.standardInput = input
            process.standardOutput = output
            process.standardError = log
            try process.run()
            do {
                try input.fileHandleForWriting.write(contentsOf: JSONEncoder().encode(request))
                try input.fileHandleForWriting.close()
            } catch {
                process.terminate(); process.waitUntilExit()
                throw error
            }
            let data = try output.fileHandleForReading.readToEnd() ?? Data()
            process.waitUntilExit()
            if let reply = try? JSONDecoder().decode(MTPResponse.self, from: data) { return reply }
            let diagnostic = (try? String(contentsOf: logURL, encoding: .utf8)) ?? ""
            throw DeviceSyncFailure("Device helper failed (\(process.terminationStatus)). \(diagnostic.suffix(2000))")
        }.value
    }
}
