import Foundation
import Testing
@testable import TyrannyCore

private func json(_ text: String) throws -> JSONValue {
    try JSONDecoder().decode(JSONValue.self, from: Data(text.utf8))
}

@Test func listDescriptionEncodesServerShape() throws {
    let description = ListDescription(
        source: .playlist(id: 4, revision: 9), browser: .init(), query: "genre:Jazz",
        sort: [.init(field: .year, descending: true)], group: .artistAlbum)
    let expected = try json(#"""
        {"source":{"kind":"playlist","id":4,"revision":9},
         "browser":{"genre_ids":[],"artist_ids":[],"compilations":false,"album_ids":[]},
         "query":"genre:Jazz","sort":[{"field":"year","descending":true}],"group":"artist_album"}
        """#)
    #expect(try JSONValue.encoding(description) == expected)
    let decoded = try JSONDecoder().decode(ListDescription.self, from: JSONEncoder().encode(description))
    #expect(decoded == description)
}

@Test func listSourceKindsRoundTrip() throws {
    let cases: [(String, ListSource)] = [
        (#"{"kind":"library"}"#, .library),
        (#"{"kind":"album","id":3}"#, .album(id: 3)),
        (#"{"kind":"artist","id":2}"#, .artist(id: 2)),
        (#"{"kind":"appears_on","artist_id":5}"#, .appearsOn(artistID: 5)),
        (#"{"kind":"compilations"}"#, .compilations),
        (#"{"kind":"genre","id":1}"#, .genre(id: 1)),
        (#"{"kind":"recent"}"#, .recent),
        (#"{"kind":"playlist","id":1,"revision":4}"#, .playlist(id: 1, revision: 4)),
        (#"{"kind":"smart","id":2,"revision":2}"#, .smart(id: 2, revision: 2)),
    ]
    for (text, source) in cases {
        #expect(try JSONValue.encoding(source) == json(text))
        #expect(try JSONDecoder().decode(ListSource.self, from: Data(text.utf8)) == source)
    }
}

private let row = #"""
{"row_id":7,"artist_key":2,"album_key":3,"available":true,"track":{
 "id":7,"album_id":3,"artist_id":2,"album_artist_id":2,"title":"T","artist":"A","album":"B","album_artist":"A",
 "compilation":false,"disc_number":1,"track_number":4,"duration_ms":1000,"year":null,"genre":"Jazz","format":"FLAC 24/96",
 "sample_rate":96000,"bitrate_kbps":null,"play_count":0,"last_played":null,"date_added":1700000000,"path":"/x.flac"}}
"""#

@Test func listTracksResponseDecodes() throws {
    let with = #"{"revision":5,"total":1,"offset":0,"entries":[\#(row)],"totals":{"tracks":1,"duration_ms":1000,"size_bytes":42}}"#
    let decoded = try JSONDecoder().decode(ListTracksResponse.self, from: Data(with.utf8))
    #expect(decoded.entries.first?.id == 7)
    #expect(decoded.entries.first?.track.year == nil)
    #expect(decoded.totals == ListTotals(tracks: 1, duration_ms: 1000, size_bytes: 42))
    for absent in [#"{"revision":5,"total":1,"offset":0,"entries":[\#(row)]}"#,
                   #"{"revision":5,"total":1,"offset":0,"entries":[\#(row)],"totals":null}"#] {
        let none = try JSONDecoder().decode(ListTracksResponse.self, from: Data(absent.utf8))
        #expect(none.totals == nil)
        #expect(none.total == 1)
    }
}

@Test func trackSelectionListEncodesHalfOpenRanges() throws {
    let list = ListDescription(source: .library)
    let selection = TrackSelection.list(
        list: list, libraryRevision: 12, ranges: [[3, 10], [20, UInt64.max]], excluded: [5], firstRowID: nil)
    let value = try JSONValue.encoding(selection)
    guard case .object(let fields) = value else { Issue.record("not an object"); return }
    #expect(fields["kind"] == .string("list"))
    #expect(fields["first_row_id"] == .null)
    #expect(fields["library_revision"] == .uint(12))
    #expect(fields["excluded"] == .array([.uint(5)]))
    #expect(fields["ranges"] == .array([.array([.uint(3), .uint(10)]), .array([.uint(20), .uint(UInt64.max)])]))
    let text = String(decoding: try JSONEncoder().encode(selection), as: UTF8.self)
    #expect(text.contains("18446744073709551615"))
    #expect(text.contains(#""first_row_id":null"#))
    let withRow = TrackSelection.list(list: list, libraryRevision: 12, ranges: [[3, 10]], excluded: [], firstRowID: 9)
    #expect(try JSONValue.encoding(withRow).objectField("first_row_id") == .uint(9))
    #expect(try JSONDecoder().decode(TrackSelection.self, from: JSONEncoder().encode(selection)) == selection)
    let ids = TrackSelection.ids(libraryRevision: 1, trackIDs: [4, 5])
    #expect(try JSONValue.encoding(ids) == json(#"{"kind":"ids","library_revision":1,"track_ids":[4,5]}"#))
}

@Test func jsonValueKeepsNullInsideObjects() throws {
    #expect(try JSONValue.encoding(JSONValue.object(["a": .null, "b": .array([.int(-1)])]))
        == .object(["a": .null, "b": .array([.int(-1)])]))
    let text = String(decoding: try JSONEncoder().encode(JSONValue.object(["a": .null])), as: UTF8.self)
    #expect(text == #"{"a":null}"#)
}

@Test func controlStateAndQueueSourceDecode() throws {
    let source = try JSONDecoder().decode(QueueSource.self, from: Data(#"{"kind":"smart","name":"Mix"}"#.utf8))
    #expect(source.kind == .smart)
    let history = #"{"id":1,"played_at":5,"track_id":null,"title":"t","artist":"a","album":"b","duration_ms":null,"source":{"kind":"drag","name":"x"}}"#
    #expect(try JSONDecoder().decode(HistoryEntry.self, from: Data(history.utf8)).source?.kind == .drag)
    let groups = #"{"revision":1,"total":2,"groups":[{"artist_key":0,"album_key":null,"start":0,"count":2}]}"#
    #expect(try JSONDecoder().decode(ListGroupsResponse.self, from: Data(groups.utf8)).groups[0].album_key == nil)
}

extension JSONValue {
    fileprivate func objectField(_ name: String) -> JSONValue? {
        if case .object(let fields) = self { return fields[name] }
        return nil
    }
}

private let nowPlayingBase = #""entry_id":1,"title":"T","artist":"A","album":"B","duration_frames":null,"metadata_error":null,"track_id":3,"album_id":5,"format":"WAV 16/48","gain_db":null"#

@Test func nowPlayingDecodesAlbumArtistAndCompilation() throws {
    let json = "{\(nowPlayingBase),\"album_artist_id\":7,\"compilation\":false}"
    let decoded = try JSONDecoder().decode(NowPlaying.self, from: Data(json.utf8))
    #expect(decoded.album_artist_id == 7)
    #expect(decoded.compilation == false)
    #expect(decoded.artistLinkID == 7)
}

@Test func nowPlayingCompilationHasNoArtistLink() throws {
    let json = "{\(nowPlayingBase),\"album_artist_id\":7,\"compilation\":true}"
    let decoded = try JSONDecoder().decode(NowPlaying.self, from: Data(json.utf8))
    #expect(decoded.compilation == true)
    #expect(decoded.artistLinkID == nil)
}

@Test func nowPlayingWithoutAlbumArtistDecodesNil() throws {
    let json = "{\(nowPlayingBase)}"
    let decoded = try JSONDecoder().decode(NowPlaying.self, from: Data(json.utf8))
    #expect(decoded.album_artist_id == nil)
    #expect(decoded.compilation == nil)
    #expect(decoded.artistLinkID == nil)
}
