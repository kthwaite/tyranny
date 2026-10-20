import Foundation
import Testing
@testable import TyrannyCore

@Test func transportSubtitleShowsStationForStreams() {
    #expect(transportSubtitle(title: "Song", artist: "", album: "", station: "KEXP") == "KEXP")
    #expect(transportSubtitle(title: "KEXP", artist: "", album: "", station: "KEXP") == "")
    #expect(transportSubtitle(title: "Song", artist: "A", album: "B", station: nil) == "A — B")
    #expect(transportSubtitle(title: "Song", artist: "A", album: "", station: nil) == "A")
}

@Test func nowPlayingDecodesAbsentAndPresentStation() throws {
    let base = #"{"entry_id":1,"title":"T","artist":"","album":"","duration_frames":null,"metadata_error":null,"track_id":null,"album_id":null,"album_artist_id":null,"compilation":null,"format":"","gain_db":null"#
    let absent = try JSONDecoder().decode(NowPlaying.self, from: Data((base + "}").utf8))
    #expect(absent.station == nil)
    let present = try JSONDecoder().decode(NowPlaying.self, from: Data((base + #","station":"KEXP"}"#).utf8))
    #expect(present.station == "KEXP")
}
