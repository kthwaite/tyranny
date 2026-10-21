import Foundation
import Testing
@testable import TyrannyCore

/// A server that records each request. With `parkSearches` set, each `search_directory` waits in `parked` until the test resumes it.
@MainActor
private final class FakeDirectoryServer {
    var requests: [CommandRequest] = []
    var replies: [Result<String, ServerFailure>] = []
    var respond: ((CommandRequest) -> Result<String, ServerFailure>)?
    var streams: [(url: String, title: String, play: Bool)] = []
    var streamFailure: String?
    /// Parked `search_directory` requests, in arrival order.
    var parked: [CheckedContinuation<Void, Never>] = []
    var parkSearches = false
    /// With this set, each `add_stations` request also parks in `parked`.
    var parkAdds = false
    var failure: (any Error)?

    func send(_ request: CommandRequest) async throws -> Data {
        requests.append(request)
        if let failure { throw failure }
        if (parkSearches && request.name == "search_directory") || (parkAdds && request.name == "add_stations") {
            await withCheckedContinuation { parked.append($0) }
        }
        let reply = replies.isEmpty ? respond!(request) : replies.removeFirst()
        return Data(try reply.get().utf8)
    }

    func addStream(url: String, title: String, play: Bool) async -> String? {
        streams.append((url, title, play))
        return streamFailure
    }

    func catalog() -> StationCatalog {
        StationCatalog(send: { try await self.send($0) },
                       addStream: { await self.addStream(url: $0, title: $1, play: $2) })
    }

    func directory(_ catalog: StationCatalog) -> StationDirectory {
        StationDirectory(send: { try await self.send($0) }, catalog: catalog,
                         addStream: { await self.addStream(url: $0, title: $1, play: $2) })
    }
}

private func stateJSON(stations revision: UInt64) -> String {
    """
    {"sequence":1,"queue_revision":4,"playlists_revision":1,"stations_revision":\(revision),"queue_length":0,
     "current_id":null,"can_next":false,"can_previous":false,"now_playing":null,
     "position_frames":0,"sample_rate":48000,"transport":"stopped",
     "settings":{"gain_mode":"off","resume_on_start":false,"volume":100,"repeat":"off","shuffle":false,
                    "equalizer":{"enabled":false,"preamp_db":0,"bands_db":[0,0,0,0,0,0,0,0,0,0]}},
     "error":null,"underruns":0,
     "library":{"revision":1,"roots":0,"tracks":0,"duration_ms":0,"size_bytes":0,
                "scan":{"root_id":null,"phase":"idle","discovered":0,"processed":0,"total":null,"updated":0,"unchanged":0,"failures":0,"skipped":0,"last_error":null}},
     "history_revision":0,"stats_revision":0,"current_index":null,"upcoming_count":0}
    """
}

private func json(_ stations: [Station]) -> String {
    String(decoding: try! JSONEncoder().encode(stations), as: UTF8.self)
}

private func json(_ stations: [DirectoryStation]) -> String {
    String(decoding: try! JSONEncoder().encode(stations), as: UTF8.self)
}

private func page(_ revision: UInt64, _ stations: [Station]) -> Result<String, ServerFailure> {
    .success(#"{"kind":"stations","revision":\#(revision),"stations":\#(json(stations))}"#)
}

private func applied(_ revision: UInt64, _ stations: [Station]) -> Result<String, ServerFailure> {
    .success(#"{"kind":"stations_applied","snapshot":\#(stateJSON(stations: revision)),"stations":\#(json(stations))}"#)
}

private func results(_ stations: [DirectoryStation]) -> Result<String, ServerFailure> {
    .success(#"{"kind":"directory_results","stations":\#(json(stations))}"#)
}

private let clicked: Result<String, ServerFailure> = .success(#"{"kind":"directory_click_counted"}"#)

private let groove = DirectoryStation(uuid: "u-groove", name: "Groove Salad", url: "https://ice1.somafm.com/groovesalad-128-mp3",
                                      codec: "MP3", bitrate: 128, country_code: "US", tags: ["ambient"])
private let kexp = DirectoryStation(uuid: "u-kexp", name: "KEXP", url: "https://kexp.streamguys1.com/kexp160.aac")

/// A catalog that has fetched an empty station list (revision 0), so `add` can name a revision.
@MainActor
private func syncedCatalog(_ server: FakeDirectoryServer) async throws -> StationCatalog {
    let catalog = server.catalog()
    server.replies = [page(0, [])]
    let snapshot = try JSONDecoder().decode(ControlState.self, from: Data(stateJSON(stations: 0).utf8))
    await catalog.sync(snapshot)
    server.requests = []
    return catalog
}

@MainActor @Suite struct StationDirectoryTests {
    @Test func searchSendsTrimmedQueryAndShowsResults() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.replies = [results([groove, kexp])]
        directory.query = "  groove  "
        await directory.search()
        #expect(server.requests.map(\.name) == ["search_directory"])
        #expect(server.requests.first?.fields["query"] == .string("groove"))
        #expect(directory.results == [groove, kexp])
        #expect(directory.searching == false)
        #expect(directory.error == nil)
    }

    @Test func searchedMarksACompletedSearchEvenWithNoStations() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        #expect(directory.searched == false)
        server.replies = [results([])]
        directory.query = "zzzz"
        await directory.search()
        #expect(directory.searched)
        #expect(directory.results.isEmpty)
    }

    @Test func searchedClearsOnEmptyQueryAndReset() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.replies = [results([groove]), results([groove])]
        directory.query = "groove"
        await directory.search()
        directory.query = " "
        await directory.search()
        #expect(directory.searched == false)
        directory.query = "groove"
        await directory.search()
        #expect(directory.searched)
        directory.reset()
        #expect(directory.searched == false)
    }

    @Test func failedOrStaleSearchDoesNotMarkSearched() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.replies = [.failure(ServerFailure(code: "unavailable", message: "Directory unreachable."))]
        directory.query = "groove"
        await directory.search()
        #expect(directory.searched == false)
        server.parkSearches = true
        server.replies = [results([groove])]
        let search = Task { await directory.search() }
        while server.parked.isEmpty { await Task.yield() }
        directory.reset()
        server.parked[0].resume()
        await search.value
        #expect(directory.searched == false)
    }

    @Test func emptyQuerySendsNothing() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.replies = [results([groove])]
        directory.query = "groove"
        await directory.search()
        directory.query = "   "
        await directory.search()
        #expect(server.requests.count == 1)
        #expect(directory.results.isEmpty)
        #expect(directory.searching == false)
    }

    @Test func staleSearchReplyIsDropped() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.parkSearches = true
        server.respond = { $0.fields["query"] == .string("first") ? results([groove]) : results([kexp]) }
        directory.query = "first"
        let first = Task { await directory.search() }
        while server.parked.count < 1 { await Task.yield() }
        directory.query = "second"
        let second = Task { await directory.search() }
        while server.parked.count < 2 { await Task.yield() }
        // The second search answers first; the first answers last.
        server.parked[1].resume()
        await second.value
        #expect(directory.results == [kexp])
        #expect(directory.searching == false)
        server.parked[0].resume()
        await first.value
        #expect(directory.results == [kexp])
        #expect(directory.searching == false)
    }

    @Test func replyAfterResetIsDropped() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.parkSearches = true
        server.replies = [results([groove])]
        directory.query = "groove"
        let search = Task { await directory.search() }
        while server.parked.isEmpty { await Task.yield() }
        directory.reset()
        server.parked[0].resume()
        await search.value
        #expect(directory.results.isEmpty)
        #expect(directory.searching == false)
        #expect(directory.error == nil)
    }

    @Test func errorReplyShowsMessage() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.replies = [results([groove]), .failure(ServerFailure(code: "unavailable", message: "Directory unreachable."))]
        directory.query = "groove"
        await directory.search()
        directory.query = "again"
        await directory.search()
        #expect(directory.error == "Directory unreachable.")
        #expect(directory.results == [groove])
        #expect(directory.searching == false)
    }

    @Test func saveAddsThenCountsClick() async throws {
        let server = FakeDirectoryServer()
        let catalog = try await syncedCatalog(server)
        let directory = server.directory(catalog)
        server.replies = [applied(1, [Station(id: 1, name: groove.name, url: groove.url)]), clicked]
        #expect(await directory.save(groove))
        #expect(server.requests.map(\.name) == ["add_stations", "count_directory_click"])
        #expect(server.requests.last?.fields["station_uuid"] == .string("u-groove"))
        #expect(directory.error == nil)
    }

    @Test func saveIsInFlightUntilItReplies() async throws {
        let server = FakeDirectoryServer()
        let catalog = try await syncedCatalog(server)
        let directory = server.directory(catalog)
        server.parkAdds = true
        server.replies = [applied(1, [Station(id: 1, name: groove.name, url: groove.url)]), clicked]
        let first = Task { await directory.save(groove) }
        while server.parked.isEmpty { await Task.yield() }
        #expect(directory.saving == ["u-groove"])
        // A second save of the same station is refused and sends nothing.
        #expect(await directory.save(groove) == false)
        #expect(directory.error == nil)
        #expect(server.requests.map(\.name) == ["add_stations"])
        server.parked[0].resume()
        #expect(await first.value)
        #expect(directory.saving.isEmpty)
    }

    @Test func failedSaveLeavesNothingInFlight() async throws {
        let server = FakeDirectoryServer()
        let catalog = try await syncedCatalog(server)
        let directory = server.directory(catalog)
        server.replies = [.failure(ServerFailure(code: "invalid_command", message: "No."))]
        #expect(await directory.save(groove) == false)
        #expect(directory.saving.isEmpty)
    }

    @Test func resetForgetsSavesInFlight() async throws {
        let server = FakeDirectoryServer()
        let catalog = try await syncedCatalog(server)
        let directory = server.directory(catalog)
        server.parkAdds = true
        server.replies = [.failure(ServerFailure(code: "internal", message: "Dropped."))]
        let save = Task { await directory.save(groove) }
        while server.parked.isEmpty { await Task.yield() }
        directory.reset()
        #expect(directory.saving.isEmpty)
        server.parked[0].resume()
        _ = await save.value
        #expect(directory.saving.isEmpty)
    }

    @Test func failedSaveSendsNoClick() async throws {
        let server = FakeDirectoryServer()
        let catalog = try await syncedCatalog(server)
        let directory = server.directory(catalog)
        server.replies = [.failure(ServerFailure(code: "invalid_command", message: "Station name is too long."))]
        #expect(await directory.save(groove) == false)
        #expect(server.requests.map(\.name) == ["add_stations"])
        #expect(directory.error == "Station name is too long.")
    }

    @Test func failedSaveWithoutCatalogMessageGetsGenericError() async throws {
        let server = FakeDirectoryServer()
        let catalog = try await syncedCatalog(server)
        let directory = server.directory(catalog)
        // A reset during the request leaves the catalog without a message.
        server.respond = { _ in
            catalog.reset()
            return .failure(ServerFailure(code: "internal", message: "Dropped."))
        }
        #expect(await directory.save(groove) == false)
        #expect(directory.error == "The station could not be saved.")
    }

    @Test func cancelledSearchShowsNoError() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.failure = CancellationError()
        directory.query = "groove"
        await directory.search()
        #expect(directory.error == nil)
        #expect(directory.searching == false)
    }

    @Test func playAddsStreamThenCountsClick() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.replies = [clicked]
        await directory.play(groove)
        #expect(server.streams.count == 1)
        #expect(server.streams.first?.url == groove.url)
        #expect(server.streams.first?.title == groove.name)
        #expect(server.streams.first?.play == true)
        #expect(server.requests.map(\.name) == ["count_directory_click"])
        #expect(server.requests.first?.fields["station_uuid"] == .string("u-groove"))
    }

    @Test func failedPlaySendsNoClick() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.streamFailure = "Cannot open the stream."
        await directory.play(groove)
        #expect(server.requests.isEmpty)
        #expect(directory.error == "Cannot open the stream.")
    }

    @Test func clickErrorsAreIgnored() async {
        let server = FakeDirectoryServer()
        let directory = server.directory(server.catalog())
        server.replies = [.failure(ServerFailure(code: "unavailable", message: "Directory unreachable."))]
        await directory.play(groove)
        #expect(directory.error == nil)
    }

    @Test func isSavedMatchesCatalogURL() async throws {
        let server = FakeDirectoryServer()
        let catalog = try await syncedCatalog(server)
        let directory = server.directory(catalog)
        #expect(directory.isSaved(groove) == false)
        server.replies = [applied(1, [Station(id: 1, name: "My name", url: groove.url)]), clicked]
        _ = await directory.save(groove)
        #expect(directory.isSaved(groove))
        #expect(directory.isSaved(kexp) == false)
    }

    @Test func directoryStationDecodesWireForm() throws {
        let wire = #"{"uuid":"u1","name":"Jazz","url":"https://j.example/s","codec":"AAC","bitrate":96,"country_code":"","tags":["jazz","live"]}"#
        let station = try JSONDecoder().decode(DirectoryStation.self, from: Data(wire.utf8))
        #expect(station == DirectoryStation(uuid: "u1", name: "Jazz", url: "https://j.example/s", codec: "AAC",
                                            bitrate: 96, country_code: "", tags: ["jazz", "live"]))
        #expect(station.id == "u1")
    }
}
