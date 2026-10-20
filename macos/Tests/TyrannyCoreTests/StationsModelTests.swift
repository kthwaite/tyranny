import Foundation
import Testing
@testable import TyrannyCore

/// A server that records each request and answers with the next queued reply.
@MainActor
private final class FakeStationServer {
    var requests: [CommandRequest] = []
    var replies: [Result<String, ServerFailure>] = []
    var streams: [(url: String, title: String, play: Bool)] = []
    var streamFailure: String?

    func send(_ request: CommandRequest) async throws -> Data {
        requests.append(request)
        return Data(try replies.removeFirst().get().utf8)
    }

    func addStream(url: String, title: String, play: Bool) async -> String? {
        streams.append((url, title, play))
        return streamFailure
    }

    func catalog() -> StationCatalog {
        StationCatalog(send: { try await self.send($0) },
                       addStream: { await self.addStream(url: $0, title: $1, play: $2) })
    }
}

private func stateJSON(stations revision: UInt64) -> String {
    """
    {"sequence":1,"queue_revision":4,"playlists_revision":1,"stations_revision":\(revision),"queue_length":0,
     "current_id":null,"can_next":false,"can_previous":false,"now_playing":null,
     "position_frames":0,"sample_rate":48000,"transport":"stopped",
     "settings":{"gain_mode":"off","resume_on_start":false,"volume":100,"repeat":"off","shuffle":false},
     "error":null,"underruns":0,
     "library":{"revision":1,"roots":0,"tracks":0,"duration_ms":0,"size_bytes":0,
                "scan":{"root_id":null,"phase":"idle","discovered":0,"processed":0,"total":null,"updated":0,"unchanged":0,"failures":0,"skipped":0,"last_error":null}},
     "history_revision":0,"current_index":null,"upcoming_count":0}
    """
}

private func state(stations revision: UInt64) throws -> ControlState {
    try JSONDecoder().decode(ControlState.self, from: Data(stateJSON(stations: revision).utf8))
}

private func json(_ stations: [Station]) -> String {
    String(decoding: try! JSONEncoder().encode(stations), as: UTF8.self)
}

private func page(_ revision: UInt64, _ stations: [Station]) -> Result<String, ServerFailure> {
    .success(#"{"kind":"stations","revision":\#(revision),"stations":\#(json(stations))}"#)
}

private func applied(_ revision: UInt64, _ stations: [Station]) -> Result<String, ServerFailure> {
    .success(#"{"kind":"stations_applied","snapshot":\#(stateJSON(stations: revision)),"stations":\#(json(stations))}"#)
}

private let groove = Station(id: 1, name: "Groove Salad", url: "https://ice1.somafm.com/groovesalad-128-mp3")
private let kexp = Station(id: 2, name: "KEXP", url: "https://kexp.streamguys1.com/kexp160.aac")
private let drone = Station(id: 3, name: "drone zone", url: "https://ice1.somafm.com/dronezone-128-mp3")

@MainActor @Suite struct StationsModelTests {
    @Test func syncFetchesOnlyWhenRevisionChanges() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(0, [])]
        await catalog.sync(try state(stations: 0))
        #expect(server.requests == [CommandRequest("stations")])
        #expect(catalog.revision == 0)
        #expect(catalog.stations.isEmpty)

        await catalog.sync(try state(stations: 0))
        #expect(server.requests.count == 1)

        server.replies = [page(2, [groove, kexp])]
        await catalog.sync(try state(stations: 2))
        #expect(server.requests == [CommandRequest("stations"), CommandRequest("stations")])
        #expect(catalog.revision == 2)
        #expect(catalog.stations == [groove, kexp])

        await catalog.sync(try state(stations: 2))
        #expect(server.requests.count == 2)
    }

    @Test func addSendsAddStationsWithExpectedRevision() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(2, [kexp])]
        await catalog.sync(try state(stations: 2))

        server.replies = [applied(3, [drone])]
        let added = await catalog.add(name: drone.name, url: drone.url)
        #expect(added)
        #expect(server.requests.last == CommandRequest("add_stations", [
            "expected_revision": .uint(2),
            "stations": .array([.object(["name": .string(drone.name), "url": .string(drone.url)])]),
        ]))
        #expect(catalog.revision == 3)
        // Sorted by name, ignoring case, as the server sorts.
        #expect(catalog.stations == [drone, kexp])
        #expect(catalog.error == nil)

        // The broadcast of the same revision does not refetch.
        await catalog.sync(try state(stations: 3))
        #expect(server.requests.count == 2)
    }

    @Test func updateAndRemoveApplyTheReply() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(4, [groove, kexp])]
        await catalog.sync(try state(stations: 4))

        let renamed = Station(id: kexp.id, name: "Alpha", url: "https://kexp.example/live")
        server.replies = [applied(5, [renamed])]
        #expect(await catalog.update(kexp.id, name: renamed.name, url: renamed.url))
        #expect(server.requests.last == CommandRequest("update_station", [
            "station_id": .uint(kexp.id), "expected_revision": .uint(4),
            "name": .string(renamed.name), "url": .string(renamed.url),
        ]))
        #expect(catalog.stations == [renamed, groove])

        server.replies = [applied(6, [])]
        #expect(await catalog.remove(groove.id))
        #expect(server.requests.last == CommandRequest("remove_station", [
            "station_id": .uint(groove.id), "expected_revision": .uint(5),
        ]))
        #expect(catalog.stations == [renamed])
        #expect(catalog.revision == 6)
    }

    @Test func importParsesAndSendsOneBatch() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(1, [kexp])]
        await catalog.sync(try state(stations: 1))

        let text = "[playlist]\r\nFile1=\(groove.url)\r\nTitle1=\(groove.name)\r\nFile2=\(kexp.url)\r\n"
        // The server skips KEXP, which is already saved.
        server.replies = [applied(2, [groove])]
        let added = await catalog.importFile(text: text, fileName: "soma.pls")
        #expect(added == 1)
        #expect(server.requests.count == 2)
        #expect(server.requests.last == CommandRequest("add_stations", [
            "expected_revision": .uint(1),
            "stations": .array([
                .object(["name": .string(groove.name), "url": .string(groove.url)]),
                .object(["name": .string(""), "url": .string(kexp.url)]),
            ]),
        ]))
        #expect(catalog.stations == [groove, kexp])
        #expect(catalog.error == nil)
    }

    @Test func importOfEmptyFileSetsErrorAndSendsNothing() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(1, [])]
        await catalog.sync(try state(stations: 1))

        let added = await catalog.importFile(text: "#EXTM3U\n/Music/a.mp3\nrtsp://x/y\n", fileName: "local.m3u")
        #expect(added == nil)
        #expect(server.requests.count == 1)
        #expect(catalog.error?.contains("local.m3u") == true)
    }

    @Test func staleEditRefetchesAndReports() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(1, [groove])]
        await catalog.sync(try state(stations: 1))

        server.replies = [
            .failure(ServerFailure(code: "stale_stations_revision", message: "The stations changed; reload and try again.")),
            page(2, [groove, kexp]),
        ]
        let saved = await catalog.update(groove.id, name: "Groove", url: groove.url)
        #expect(!saved)
        // Refetched once and not retried.
        #expect(server.requests.map(\.name) == ["stations", "update_station", "stations"])
        #expect(catalog.error == "The stations changed; reload and try again.")
        #expect(catalog.stations == [groove, kexp])
        #expect(catalog.revision == 2)
    }

    @Test func playAppendsStreamWithStationNameAndPlays() async {
        let server = FakeStationServer()
        let catalog = server.catalog()
        await catalog.play(groove)
        await catalog.enqueue(kexp)
        #expect(server.streams.map(\.url) == [groove.url, kexp.url])
        #expect(server.streams.map(\.title) == [groove.name, kexp.name])
        #expect(server.streams.map(\.play) == [true, false])
        #expect(catalog.error == nil)
        #expect(server.requests.isEmpty)

        server.streamFailure = "Wait for the current command to finish."
        await catalog.play(groove)
        #expect(catalog.error == "Wait for the current command to finish.")
    }
}
