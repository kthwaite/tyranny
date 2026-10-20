import Foundation
import Observation

/// The saved radio stations, and the commands that read and edit them (protocol v11).
///
/// The catalog has one revision, `ControlState.stations_revision`. `sync` fetches the whole
/// catalog (`stations`) when a snapshot names another revision than the one shown. Edits name
/// the shown revision as `expected_revision`, and a `stations_applied` reply updates the shown
/// rows and revision, so its broadcast does not refetch. A `stale_stations_revision` reply
/// refetches and reports the server's message. The edit is not retried: the user sees what
/// changed first.
///
/// `send` carries one request to the server and returns the reply's result object.
/// `addStream` appends a stream to Up Next, plays it when asked, and returns failure text.
@MainActor @Observable
public final class StationCatalog {
    public typealias Send = @MainActor (CommandRequest) async throws -> Data
    public typealias AddStream = @MainActor (_ url: String, _ title: String, _ play: Bool) async -> String?

    /// In the server's order: by name ignoring case, then by ID.
    public private(set) var stations: [Station] = []
    /// The revision `stations` was read at. Nil until the first fetch.
    public private(set) var revision: UInt64?
    /// The last failure, for the view to show. Each edit clears it.
    public var error: String?

    @ObservationIgnored private let send: Send
    @ObservationIgnored private let addStream: AddStream
    @ObservationIgnored private let decoder = JSONDecoder()
    /// The latest snapshot's revision.
    @ObservationIgnored private var wanted: UInt64?
    @ObservationIgnored private var fetching = false
    /// Bumps on `reset`. A reply that arrives after it belongs to the old connection.
    @ObservationIgnored private var epoch: UInt64 = 0

    public init(send: @escaping Send, addStream: @escaping AddStream) {
        self.send = send
        self.addStream = addStream
    }

    /// Forgets the catalog after a disconnect or a server change. The next `sync` fetches.
    public func reset() {
        epoch &+= 1
        stations = []
        revision = nil
        wanted = nil
        fetching = false
        error = nil
    }

    /// The snapshot names another catalog than the one shown.
    public func needsFetch(_ snapshot: ControlState) -> Bool {
        revision != snapshot.stations_revision
    }

    /// Fetches the catalog when the snapshot's revision differs from the shown one. A snapshot
    /// that arrives during the fetch is checked when the fetch ends.
    public func sync(_ snapshot: ControlState) async {
        wanted = snapshot.stations_revision
        guard !fetching, revision != wanted else { return }
        fetching = true
        let started = epoch
        defer { if epoch == started { fetching = false } }
        var asked: UInt64?
        repeat {
            asked = wanted
            guard await refetch() else { return }
        } while wanted != asked && revision != wanted
    }

    /// Adds one station. The server names a station with a blank name after its URL's host.
    public func add(name: String, url: String) async -> Bool {
        await edit { shown in
            CommandRequest("add_stations", ["expected_revision": .uint(shown),
                                            "stations": try JSONValue.encoding([StationInput(name: name, url: url)])])
        } != nil
    }

    public func update(_ id: UInt64, name: String, url: String) async -> Bool {
        await edit { shown in
            CommandRequest("update_station", ["station_id": .uint(id), "expected_revision": .uint(shown),
                                              "name": .string(name), "url": .string(url)])
        } != nil
    }

    public func remove(_ id: UInt64) async -> Bool {
        await edit(removing: id) { shown in
            CommandRequest("remove_station", ["station_id": .uint(id), "expected_revision": .uint(shown)])
        } != nil
    }

    /// Reads a `.pls` or `.m3u` file and adds its stations in one `add_stations`. The server
    /// skips URLs that are already saved. Returns how many stations were added, or nil on failure.
    public func importFile(text: String, fileName: String) async -> Int? {
        let inputs: [StationInput]
        do {
            inputs = try StationFile.parse(text, fileName: fileName)
        } catch {
            self.error = (error as? StationFileError)?.message ?? error.localizedDescription
            return nil
        }
        return await edit { shown in
            CommandRequest("add_stations", ["expected_revision": .uint(shown),
                                            "stations": try JSONValue.encoding(inputs)])
        }?.stations.count
    }

    /// Appends the station's stream to Up Next under the station's name, and plays it.
    public func play(_ station: Station) async {
        error = await addStream(station.url, station.name, true)
    }

    /// Appends the station's stream to Up Next under the station's name.
    public func enqueue(_ station: Station) async {
        error = await addStream(station.url, station.name, false)
    }

    // MARK: Requests

    /// Fetches the whole catalog. A reply older than the shown catalog is dropped.
    /// Returns false when the fetch failed or the connection changed meanwhile.
    @discardableResult
    private func refetch() async -> Bool {
        let started = epoch
        do {
            let page = try decoder.decode(StationsPage.self, from: await send(CommandRequest("stations")))
            guard epoch == started else { return false }
            if revision.map({ page.revision >= $0 }) ?? true {
                stations = page.stations
                revision = page.revision
            }
            return true
        } catch {
            guard epoch == started else { return false }
            self.error = Self.describe(error)
            return false
        }
    }

    /// Sends one edit built for the shown revision. The reply's rows replace or join the shown
    /// ones (`removing` drops one), unless a fetch moved the shown catalog on meanwhile.
    private func edit(removing: UInt64? = nil,
                      _ request: (UInt64) throws -> CommandRequest) async -> StationsApplied? {
        error = nil
        guard let shown = revision else {
            error = "Wait for the stations to load."
            return nil
        }
        let started = epoch
        do {
            let applied = try decoder.decode(StationsApplied.self, from: await send(try request(shown)))
            guard epoch == started else { return applied }
            if revision == shown {
                apply(applied, removing: removing)
            }
            return applied
        } catch let failure as ServerFailure where failure.code == "stale_stations_revision" {
            await refetch()
            if epoch == started { error = failure.message }
            return nil
        } catch {
            if epoch == started { self.error = Self.describe(error) }
            return nil
        }
    }

    private func apply(_ applied: StationsApplied, removing: UInt64?) {
        var rows = stations.filter { $0.id != removing }
        for station in applied.stations {
            if let index = rows.firstIndex(where: { $0.id == station.id }) {
                rows[index] = station
            } else {
                rows.append(station)
            }
        }
        // The server orders by the lower-cased name's UTF-8 bytes, then by ID.
        stations = rows.sorted { left, right in
            let a = left.name.lowercased(), b = right.name.lowercased()
            return a == b ? left.id < right.id : a.utf8.lexicographicallyPrecedes(b.utf8)
        }
        revision = applied.snapshot.stations_revision
    }

    private static func describe(_ error: any Error) -> String {
        (error as? ServerFailure)?.message ?? error.localizedDescription
    }
}
