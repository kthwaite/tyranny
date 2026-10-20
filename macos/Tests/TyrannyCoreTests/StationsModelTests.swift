import Foundation
import Testing
@testable import TyrannyCore

/// A server that records each request and answers with the next queued reply.
@MainActor
private final class FakeStationServer {
    var requests: [CommandRequest] = []
    var replies: [Result<String, ServerFailure>] = []
    /// Answers when `replies` is empty.
    var respond: ((CommandRequest) -> Result<String, ServerFailure>)?
    /// Runs after a request is recorded and before its reply, as WireClient applies a reply's
    /// snapshot before the caller resumes.
    var beforeReply: [String: () async -> Void] = [:]
    var streams: [(url: String, title: String, play: Bool)] = []
    var streamFailure: String?

    func send(_ request: CommandRequest) async throws -> Data {
        requests.append(request)
        if let hook = beforeReply[request.name] { await hook() }
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

/// Delivers a snapshot as PlayerModel does: the catalog notes it and syncs when it asks to.
@MainActor
private func deliver(_ snapshot: ControlState, to catalog: StationCatalog) async {
    if catalog.observe(snapshot) { await catalog.sync() }
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
        // The reply's snapshot reaches the catalog before the edit resumes.
        server.beforeReply["add_stations"] = { await deliver(try! state(stations: 3), to: catalog) }
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
        // Neither the reply's snapshot nor a later broadcast of the same revision refetches.
        await deliver(try state(stations: 3), to: catalog)
        #expect(server.requests.map(\.name) == ["stations", "add_stations"])
    }

    @Test func aNewerRevisionDuringAnEditFetchesWhenTheEditEnds() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(2, [kexp])]
        await catalog.sync(try state(stations: 2))

        // Another client edits right after this one: its broadcast arrives during the edit.
        server.replies = [applied(3, [drone]), page(4, [drone, groove, kexp])]
        server.beforeReply["add_stations"] = { await deliver(try! state(stations: 4), to: catalog) }
        #expect(await catalog.add(name: drone.name, url: drone.url))
        #expect(server.requests.map(\.name) == ["stations", "add_stations", "stations"])
        #expect(catalog.revision == 4)
        #expect(catalog.stations == [drone, groove, kexp])
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

    @Test func importSendsBatchesThatFitTheRequestLimit() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(10, [])]
        await catalog.sync(try state(stations: 10))

        let inputs = manyStations(400)
        var revision: UInt64 = 10
        var nextID: UInt64 = 100
        server.respond = { request in
            revision += 1
            let rows = entries(request).map { input -> Station in
                nextID += 1
                return Station(id: nextID, name: input.name, url: input.url)
            }
            return applied(revision, rows)
        }
        let added = await catalog.importFile(text: m3u(inputs), fileName: "many.m3u")
        let batches = Array(server.requests.dropFirst())
        #expect(batches.count > 1)
        #expect(batches.allSatisfy { $0.name == "add_stations" })
        for (index, batch) in batches.enumerated() {
            let frame = try RequestFrame.encode(batch.name, fields: batch.fields, id: UInt64.max)
            #expect(frame.count <= RequestFrame.limit)
            #expect(entries(batch).count <= 500)
            // Each batch names the revision of the reply before it.
            #expect(batch.fields["expected_revision"] == .uint(10 + UInt64(index)))
        }
        #expect(batches.flatMap(entries) == inputs)
        #expect(added == 400)
        #expect(catalog.stations.count == 400)
        #expect(catalog.revision == 10 + UInt64(batches.count))
        #expect(catalog.error == nil)
    }

    @Test func aFailedLaterBatchReportsWhatEarlierBatchesAdded() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(1, [])]
        await catalog.sync(try state(stations: 1))

        var sent = 0
        server.respond = { request in
            sent += 1
            guard sent == 1 else {
                return .failure(ServerFailure(code: "invalid_command", message: "station 3: stream URL requires HTTP(S)"))
            }
            // The server skips two URLs that were already saved.
            let rows = entries(request).dropFirst(2).enumerated().map {
                Station(id: UInt64($0.offset + 1), name: $0.element.name, url: $0.element.url)
            }
            return applied(2, Array(rows))
        }
        let added = await catalog.importFile(text: m3u(manyStations(400)), fileName: "many.m3u")
        #expect(added == nil)
        #expect(server.requests.count == 3)
        let batch = entries(server.requests[1]).count
        let first = batch - 2
        #expect(catalog.stations.count == first)
        #expect(catalog.error?.contains("Imported \(first) new stations") == true)
        // The server's 0-based index within the batch becomes the 1-based station in the file.
        #expect(catalog.error?.hasSuffix("many.m3u: station \(batch + 4): stream URL requires HTTP(S)") == true)
    }

    @Test func aFailedFirstBatchNamesTheStationInTheFile() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(1, [])]
        await catalog.sync(try state(stations: 1))

        server.replies = [.failure(ServerFailure(code: "invalid_command", message: "station 0: stream URL requires a host"))]
        let added = await catalog.importFile(text: m3u(manyStations(2)), fileName: "two.m3u")
        #expect(added == nil)
        #expect(catalog.error == "two.m3u: station 1: stream URL requires a host")
    }

    @Test func aStationTooLongToSendIsNamedByItsPlaceInTheFile() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(1, [])]
        await catalog.sync(try state(stations: 1))

        var inputs = manyStations(2)
        inputs.append(StationInput(name: "Huge", url: "https://huge.example.com/" + String(repeating: "x", count: RequestFrame.limit)))
        let added = await catalog.importFile(text: m3u(inputs), fileName: "huge.m3u")
        #expect(added == nil)
        #expect(server.requests.count == 1)
        #expect(catalog.error == "huge.m3u: station 3: too long to send")
    }

    @Test func aResetDuringAnImportStopsItWithoutReporting() async throws {
        let server = FakeStationServer()
        let catalog = server.catalog()
        server.replies = [page(1, [])]
        await catalog.sync(try state(stations: 1))

        var resets = 0
        server.beforeReply["add_stations"] = {
            guard resets == 0 else { return }
            resets += 1
            catalog.reset()
        }
        var revision: UInt64 = 1
        server.respond = { request in
            guard revision == 1 else {
                return .failure(ServerFailure(code: "stale_stations_revision", message: "The stations changed."))
            }
            revision += 1
            return applied(revision, entries(request).enumerated().map {
                Station(id: UInt64($0.offset + 1), name: $0.element.name, url: $0.element.url)
            })
        }
        let added = await catalog.importFile(text: m3u(manyStations(400)), fileName: "many.m3u")
        #expect(added == nil)
        // The fetch and the first batch; no batch is sent after the reset.
        #expect(server.requests.map(\.name) == ["stations", "add_stations"])
        #expect(catalog.stations.isEmpty)
        #expect(catalog.revision == nil)
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

/// Stations with ~100-byte URLs.
private func manyStations(_ count: Int) -> [StationInput] {
    (0..<count).map { index in
        StationInput(name: "Station \(index)",
                     url: "https://radio\(index).example.com/" + String(repeating: "x", count: 72) + "/live.mp3")
    }
}

private func m3u(_ inputs: [StationInput]) -> String {
    "#EXTM3U\n" + inputs.map { "#EXTINF:-1,\($0.name)\n\($0.url)\n" }.joined()
}

/// The stations an `add_stations` request carries.
private func entries(_ request: CommandRequest) -> [StationInput] {
    guard case .array(let items)? = request.fields["stations"] else { return [] }
    return items.compactMap { item in
        guard case .object(let fields) = item, case .string(let name)? = fields["name"],
              case .string(let url)? = fields["url"] else { return nil }
        return StationInput(name: name, url: url)
    }
}
