import Foundation
import Observation
import TyrannyCore

/// The Radio source's saved stations. `StationCatalog` (TyrannyCore) holds the rows and decides
/// every request; this model sends them on the shared connection, plays through `UpNextModel`,
/// reads station files, and forgets the catalog when the connection changes.
///
/// Edits run as user mutations, like playlist edits: one at a time, and the model is busy meanwhile.
@MainActor @Observable
final class StationsModel {
    @ObservationIgnored private weak var player: PlayerModel?
    @ObservationIgnored private var session: UInt64?
    @ObservationIgnored private lazy var catalog = StationCatalog(
        send: { [weak self] request in
            guard let player = self?.player else { throw CancellationError() }
            return try await player.sendRaw(request)
        },
        addStream: { [weak self] url, title, play in
            guard let player = self?.player else { return "Radio closed." }
            return await player.upNext.addStream(url: url, title: title, play: play)
        })

    func attach(_ player: PlayerModel) { self.player = player }

    var stations: [Station] { catalog.stations }
    /// The revision `stations` was read at; 0 until the first fetch.
    var revision: UInt64 { catalog.revision ?? 0 }
    /// The catalog has been fetched on this connection.
    var isLoaded: Bool { catalog.revision != nil }
    var error: String? {
        get { catalog.error }
        set { catalog.error = newValue }
    }

    // MARK: Following state

    /// Follows the connection and `stations_revision`. A new connection forgets the catalog.
    func sync() {
        guard let player else { return }
        let generation = player.playlistConnectionGeneration
        if session != generation {
            session = generation
            catalog.reset()
        }
        guard player.connected, let snapshot = player.snapshot else { return }
        sync(snapshot)
    }

    /// The reply to this model's own edit brings its snapshot first; the catalog does not fetch
    /// for it (`StationCatalog.observe`).
    func sync(_ snapshot: ControlState) {
        guard catalog.observe(snapshot) else { return }
        // A cancelled request would close the shared connection: fetch unstructured.
        Task { @MainActor in await catalog.sync() }
    }

    // MARK: Edits

    func add(name: String, url: String) async -> Bool {
        await run { await self.catalog.add(name: name, url: url) }
    }

    func update(_ id: UInt64, name: String, url: String) async -> Bool {
        await run { await self.catalog.update(id, name: name, url: url) }
    }

    func remove(_ id: UInt64) async {
        _ = await run { await self.catalog.remove(id) }
    }

    /// Reads a `.pls` or `.m3u` file and adds its stations in one batch.
    func importFile(_ url: URL) async {
        let text: String
        do {
            let data = try Data(contentsOf: url)
            // Station files from the wild are often Latin-1.
            text = String(data: data, encoding: .utf8) ?? String(data: data, encoding: .isoLatin1)
                ?? String(decoding: data, as: UTF8.self)
        } catch {
            catalog.error = "\(url.lastPathComponent) could not be read: \(error.localizedDescription)"
            return
        }
        var added: Int?
        _ = await run {
            added = await self.catalog.importFile(text: text, fileName: url.lastPathComponent)
            return added != nil
        }
        if let added {
            player?.message = "Imported \(DisplayFormat.count(added)) new \(added == 1 ? "station" : "stations") from \(url.lastPathComponent)."
        }
    }

    // MARK: Playing

    func play(_ station: Station) async { await catalog.play(station) }

    func enqueue(_ station: Station) async { await catalog.enqueue(station) }

    // MARK: Running

    /// Runs one catalog edit as a user mutation. Its failure is in `error`.
    private func run(_ operation: @escaping @MainActor () async -> Bool) async -> Bool {
        guard let player, player.canMutate else {
            catalog.error = "Wait for the current command to finish."
            return false
        }
        return await withCheckedContinuation { continuation in
            player.mutate { continuation.resume(returning: await operation()) }
        }
    }
}
