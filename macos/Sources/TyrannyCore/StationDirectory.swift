import Foundation
import Observation

/// Searches the radio-browser directory through the server, and saves or plays what it finds
/// (protocol v14 `search_directory` and `count_directory_click`).
///
/// Each search has a token. Only the latest search may set `results` and `error`; a reply to an
/// older search, or one that arrives after `reset`, is dropped. After a station is saved or
/// played, the directory is told of the click. A failed click is ignored.
///
/// `send` and `addStream` are the same closures `StationCatalog` takes. Saving goes through
/// `catalog`, so the saved rows and their revision stay in one place.
@MainActor @Observable
public final class StationDirectory {
    /// What the user typed. `search` trims it.
    public var query = ""
    /// The latest search's stations, in the directory's order.
    public private(set) var results: [DirectoryStation] = []
    /// True while the latest search waits for its reply.
    public private(set) var searching = false
    /// True after a search finished with an answer for the current query. A failed, cancelled or
    /// stale search leaves it as it was. `reset` and an empty query clear it.
    public private(set) var searched = false
    /// The uuids of the stations whose save is in flight.
    public private(set) var saving: Set<String> = []
    /// The last failure, for the view to show.
    public var error: String?

    @ObservationIgnored private let send: StationCatalog.Send
    @ObservationIgnored private let catalog: StationCatalog
    @ObservationIgnored private let addStream: StationCatalog.AddStream
    @ObservationIgnored private let decoder = JSONDecoder()
    /// Bumps on each search and on `reset`. A reply with an older value is stale.
    @ObservationIgnored private var token: UInt64 = 0

    public init(send: @escaping StationCatalog.Send, catalog: StationCatalog,
                addStream: @escaping StationCatalog.AddStream) {
        self.send = send
        self.catalog = catalog
        self.addStream = addStream
    }

    /// Forgets the search after a disconnect or a server change.
    public func reset() {
        token &+= 1
        query = ""
        results = []
        searched = false
        searching = false
        saving = []
        error = nil
    }

    /// Searches for the trimmed query. An empty query sends nothing and clears the results.
    /// On a failure `error` holds the server's message and `results` stays as it was.
    public func search() async {
        token &+= 1
        let mine = token
        let text = query.trimmingCharacters(in: .whitespacesAndNewlines)
        error = nil
        guard !text.isEmpty else {
            results = []
            searched = false
            searching = false
            return
        }
        searching = true
        do {
            let data = try await send(CommandRequest("search_directory", ["query": .string(text)]))
            let found = try decoder.decode(DirectoryResults.self, from: data)
            guard token == mine else { return }
            results = found.stations
            searched = true
        } catch is CancellationError {
            if token == mine { searching = false }
            return
        } catch {
            guard token == mine else { return }
            self.error = (error as? ServerFailure)?.message ?? error.localizedDescription
        }
        searching = false
    }

    /// Saves the station to the catalog. Returns false on failure; `error` then says why.
    /// A station whose save is already in flight is not saved twice: this returns false with no error.
    public func save(_ station: DirectoryStation) async -> Bool {
        guard saving.insert(station.uuid).inserted else { return false }
        let started = token
        defer { if token == started { saving.remove(station.uuid) } }
        error = nil
        guard await catalog.add(name: station.name, url: station.url) else {
            if token == started { error = catalog.error ?? "The station could not be saved." }
            return false
        }
        await countClick(station)
        return true
    }

    /// Appends the station's stream to Up Next under the station's name, and plays it.
    public func play(_ station: DirectoryStation) async {
        let started = token
        let failure = await addStream(station.url, station.name, true)
        if token == started { error = failure }
        if failure == nil { await countClick(station) }
    }

    /// The catalog holds a station with this station's URL.
    public func isSaved(_ station: DirectoryStation) -> Bool {
        catalog.stations.contains { $0.url == station.url }
    }

    private func countClick(_ station: DirectoryStation) async {
        _ = try? await send(CommandRequest("count_directory_click", ["station_uuid": .string(station.uuid)]))
    }
}
