import Foundation
import Observation

/// The saved radio stations, and the commands that read and edit them (protocol v12).
///
/// The catalog has one revision, `ControlState.stations_revision`. `observe` notes each
/// snapshot's revision; when the shown catalog lags it, `sync` fetches the whole catalog
/// (`stations`). Edits name the shown revision as `expected_revision`, and a `stations_applied`
/// reply updates the shown rows and revision. While an edit is in flight no fetch starts: the
/// reply's own snapshot reaches the client before the reply does. When the edit ends, a
/// snapshot newer than the reply fetches. A `stale_stations_revision` reply refetches and
/// reports the server's message. The edit is not retried: the user sees what changed first.
///
/// `send` carries one request to the server and returns the reply's result object.
/// `addStream` appends a stream to Up Next, plays it when asked, and returns failure text.
@MainActor @Observable
public final class StationCatalog {
    public typealias Send = @MainActor (CommandRequest) async throws -> Data
    public typealias AddStream = @MainActor (_ url: String, _ title: String, _ play: Bool) async -> String?

    /// The server's limit on stations in one `add_stations` (`MAX_STATION_BATCH`).
    public static let maximumBatch = 500

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
    @ObservationIgnored private var editing = false
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
        editing = false
        error = nil
    }

    /// Notes the snapshot's catalog revision. True when `sync` should run now: the shown catalog
    /// lags the snapshot, and no fetch or edit is in flight (each checks again when it ends).
    public func observe(_ snapshot: ControlState) -> Bool {
        wanted = snapshot.stations_revision
        return !fetching && !editing && lags
    }

    /// `observe`, then `sync` when it asks.
    public func sync(_ snapshot: ControlState) async {
        if observe(snapshot) { await sync() }
    }

    /// Fetches the catalog while it lags the latest snapshot. A snapshot that arrives during
    /// the fetch is checked when the fetch ends.
    public func sync() async {
        guard !fetching, !editing, lags else { return }
        fetching = true
        let started = epoch
        defer { if epoch == started { fetching = false } }
        var asked: UInt64?
        repeat {
            asked = wanted
            guard await refetch() else { return }
        } while wanted != asked && lags
    }

    /// Nothing is shown yet, or the latest snapshot names a newer catalog. Revisions only grow
    /// on one connection.
    private var lags: Bool {
        guard let wanted else { return false }
        return revision.map { $0 < wanted } ?? true
    }

    /// Adds one station. The server names a station with a blank name after its URL's host.
    public func add(name: String, url: String) async -> Bool {
        await whileEditing {
            await submit { shown in try Self.addRequest([StationInput(name: name, url: url)], expected: shown) }
        } != nil
    }

    public func update(_ id: UInt64, name: String, url: String) async -> Bool {
        await whileEditing {
            await submit { shown in
                CommandRequest("update_station", ["station_id": .uint(id), "expected_revision": .uint(shown),
                                                  "name": .string(name), "url": .string(url)])
            }
        } != nil
    }

    public func remove(_ id: UInt64) async -> Bool {
        await whileEditing {
            await submit(removing: id) { shown in
                CommandRequest("remove_station", ["station_id": .uint(id), "expected_revision": .uint(shown)])
            }
        } != nil
    }

    /// Reads a `.pls` or `.m3u` file and adds its stations in consecutive `add_stations`
    /// batches (`batches`). Each batch names the revision of the reply before it. The server
    /// skips URLs that are already saved. A failed batch stops the import; earlier batches stay
    /// added and the error says how many stations they added. A `reset` also stops the import,
    /// silently. Errors name a station by its 1-based place in the file. Returns how many
    /// stations were added, or nil on failure.
    public func importFile(text: String, fileName: String) async -> Int? {
        error = nil
        let batches: [[StationInput]]
        do {
            batches = try Self.batches(StationFile.parse(text, fileName: fileName), fileName: fileName)
        } catch {
            self.error = (error as? StationFileError)?.message ?? error.localizedDescription
            return nil
        }
        let started = epoch
        return await whileEditing {
            var added = 0
            var sent = 0
            var expected: UInt64?
            for batch in batches {
                let applied = await submit(expected: expected) { shown in
                    try Self.addRequest(batch, expected: shown)
                }
                // A reset forgot the catalog this import was adding to.
                guard epoch == started else { return nil }
                guard let applied else {
                    let failure = Self.renumbered(error ?? "", after: sent, in: fileName)
                    error = expected == nil ? failure
                        : "Imported \(added) new \(added == 1 ? "station" : "stations") from \(fileName), "
                            + "then the batch of entries \(sent + 1)–\(sent + batch.count) failed: \(failure)"
                    return nil
                }
                added += applied.stations.count
                sent += batch.count
                expected = applied.snapshot.stations_revision
            }
            return added
        }
    }

    /// Appends the station's stream to Up Next under the station's name, and plays it.
    public func play(_ station: Station) async {
        error = await addStream(station.url, station.name, true)
    }

    /// Appends the station's stream to Up Next under the station's name.
    public func enqueue(_ station: Station) async {
        error = await addStream(station.url, station.name, false)
    }

    // MARK: Batches

    /// Splits stations, in order, into `add_stations` batches. A batch holds at most
    /// `maximumBatch` stations, and its request frame fits `RequestFrame.limit` with the
    /// largest revision and request ID. Throws when one station alone does not fit.
    private static func batches(_ inputs: [StationInput], fileName: String) throws -> [[StationInput]] {
        let encoder = JSONEncoder()
        // A frame is the empty frame plus each station's JSON, separated by commas.
        let empty = try frameSize(addRequest([], expected: .max))
        var batches: [[StationInput]] = []
        var batch: [StationInput] = []
        var size = empty
        for (index, input) in inputs.enumerated() {
            let entry = try encoder.encode(JSONValue.encoding(input)).count
            guard empty + entry <= RequestFrame.limit else {
                throw StationFileError("\(Self.entry(index + 1, in: fileName)): too long to send")
            }
            if !batch.isEmpty, batch.count == maximumBatch || size + 1 + entry > RequestFrame.limit {
                batches.append(batch)
                batch = []
                size = empty
            }
            size += (batch.isEmpty ? 0 : 1) + entry
            batch.append(input)
        }
        if !batch.isEmpty { batches.append(batch) }
        return batches
    }

    /// How import errors name a station: by its 1-based place among the file's stations.
    private static func entry(_ number: Int, in fileName: String) -> String {
        "\(fileName): station \(number)"
    }

    /// The server names an invalid station by its 0-based index in the batch ("station 2: …").
    /// Names it instead by its place in the file, given how many stations earlier batches sent.
    /// Other messages are unchanged.
    private static func renumbered(_ message: String, after sent: Int, in fileName: String) -> String {
        guard message.hasPrefix("station "),
              let colon = message.firstIndex(of: ":"),
              let index = Int(message[message.index(message.startIndex, offsetBy: 8)..<colon]) else { return message }
        return entry(sent + index + 1, in: fileName) + message[colon...]
    }

    private static func frameSize(_ request: CommandRequest) throws -> Int {
        try RequestFrame.encode(request.name, fields: request.fields, id: .max).count
    }

    private static func addRequest(_ inputs: [StationInput], expected: UInt64) throws -> CommandRequest {
        CommandRequest("add_stations", ["expected_revision": .uint(expected), "stations": try JSONValue.encoding(inputs)])
    }

    // MARK: Requests

    /// Runs one edit. No fetch starts meanwhile; afterwards the catalog catches up with any
    /// snapshot newer than the edit's reply.
    private func whileEditing<T>(_ body: () async -> T) async -> T {
        editing = true
        let started = epoch
        let result = await body()
        if epoch == started {
            editing = false
            await sync()
        }
        return result
    }

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

    /// Sends one edit built for `expected`, or for the shown revision. The reply's rows replace
    /// or join the shown ones (`removing` drops one), unless a fetch moved the shown catalog on
    /// meanwhile.
    private func submit(expected: UInt64? = nil, removing: UInt64? = nil,
                        _ request: (UInt64) throws -> CommandRequest) async -> StationsApplied? {
        error = nil
        guard let shown = expected ?? revision else {
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
