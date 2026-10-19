import Foundation
import Testing
@testable import TyrannyCore

/// A snapshot as the server sends it, with the queue fields that decide Up Next.
func snapshot(currentID: UInt64?, currentIndex: Int?, upcoming: Int, shuffle: Bool,
              queueLength: Int = 10) throws -> ControlState {
    let current = currentID.map(String.init) ?? "null"
    let index = currentIndex.map(String.init) ?? "null"
    let playing = currentID.map {
        #"{"entry_id":\#($0),"title":"T","artist":"A","album":"B","duration_frames":null,"metadata_error":null,"#
            + #""track_id":null,"album_id":null,"format":"","gain_db":null}"#
    } ?? "null"
    let text = """
        {"sequence":1,"queue_revision":4,"playlists_revision":1,"queue_length":\(queueLength),
         "current_id":\(current),"can_next":true,"can_previous":true,"now_playing":\(playing),
         "position_frames":0,"sample_rate":48000,"transport":"paused",
         "settings":{"gain_mode":"off","resume_on_start":false,"volume":100,"repeat":"off","shuffle":\(shuffle)},
         "error":null,"underruns":0,
         "library":{"revision":1,"roots":0,"tracks":0,"duration_ms":0,"size_bytes":0,
                    "scan":{"root_id":null,"phase":"idle","discovered":0,"processed":0,"total":null,"updated":0,"unchanged":0,"failures":0,"skipped":0,"last_error":null}},
         "history_revision":0,"current_index":\(index),"upcoming_count":\(upcoming)}
        """
    return try JSONDecoder().decode(ControlState.self, from: Data(text.utf8))
}

@Suite struct UpNextSectionTests {
    private func item(_ id: UInt64, played: Bool, source: QueueSource? = nil) -> QueueItem {
        QueueItem(id: id, path: "/m/\(id).flac", track_id: id, source: source, run_id: nil, played: played,
                  title: "T\(id)", artist: "A", album: "B", album_id: 1, duration_ms: 1000)
    }

    private let off = QueueContext(currentID: 2, currentIndex: 1, shuffle: false)
    private let on = QueueContext(currentID: 2, currentIndex: 1, shuffle: true)

    @Test func currentIsNowPlayingEvenWhenPlayed() {
        for context in [off, on] {
            #expect(UpNextSections.section(of: item(2, played: true), at: 1, in: context) == .nowPlaying)
            #expect(UpNextSections.section(of: item(2, played: false), at: 1, in: context) == .nowPlaying)
        }
    }

    /// Shuffle off: the queue order decides. Every entry before the current one is HISTORY,
    /// played or skipped; every entry after it is UP NEXT, played or not.
    @Test func withoutShuffleSectionsFollowTheQueueOrder() {
        let queue = [item(7, played: false), item(2, played: true), item(5, played: true), item(4, played: false)]
        let sections = queue.enumerated().map { UpNextSections.section(of: $1, at: $0, in: off) }
        #expect(sections == [.history, .nowPlaying, .upNext, .upNext])
    }

    /// Shuffle on: the played marks decide. A skipped entry before the current one is still to
    /// play; a played entry after it is history.
    @Test func withShuffleSectionsFollowThePlayedMarks() {
        let queue = [item(7, played: false), item(2, played: true), item(5, played: true), item(4, played: false)]
        let sections = queue.enumerated().map { UpNextSections.section(of: $1, at: $0, in: on) }
        #expect(sections == [.upNext, .nowPlaying, .history, .upNext])
    }

    @Test func noCurrentEntrySplitsOnPlayedOnly() {
        for shuffle in [false, true] {
            let none = QueueContext(currentID: nil, currentIndex: nil, shuffle: shuffle)
            #expect(UpNextSections.section(of: item(1, played: true), at: 3, in: none) == .history)
            #expect(UpNextSections.section(of: item(1, played: false), at: 0, in: none) == .upNext)
        }
    }

    @Test func contextComesFromTheSnapshot() throws {
        let state = try snapshot(currentID: 5, currentIndex: 3, upcoming: 2, shuffle: true)
        #expect(QueueContext(state) == QueueContext(currentID: 5, currentIndex: 3, shuffle: true))
        #expect(QueueContext(nil) == QueueContext(currentID: nil, currentIndex: nil, shuffle: false))
    }

    @Test func headerStartsWhereTheSectionChanges() {
        let queue = [item(7, played: false), item(2, played: true), item(5, played: true), item(4, played: false)]
        func starts(_ index: Int, _ context: QueueContext) -> Bool {
            UpNextSections.startsSection(item: queue[index], previous: index > 0 ? queue[index - 1] : nil,
                                         position: index, in: context)
        }
        #expect([0, 1, 2, 3].map { starts($0, off) } == [true, true, true, false])
        #expect([0, 1, 2, 3].map { starts($0, on) } == [true, true, true, true])
    }

    @Test func headerWaitsForAnUnloadedNeighbour() {
        #expect(!UpNextSections.startsSection(item: item(4, played: false), previous: nil, position: 40, in: off))
        #expect(UpNextSections.startsSection(item: item(4, played: false), previous: nil, position: 0, in: off))
    }

    /// The drawer lists the UP NEXT section among the loaded rows, in queue order.
    @Test func upcomingRowsFollowTheSameRule() {
        let rows = [item(7, played: false), item(2, played: true), item(5, played: true), item(4, played: false)]
            .enumerated().map { (position: $0, row: $1) }
        #expect(UpNextSections.upcoming(rows, in: off, limit: 10).map(\.id) == [5, 4])
        #expect(UpNextSections.upcoming(rows, in: on, limit: 10).map(\.id) == [7, 4])
        #expect(UpNextSections.upcoming(rows, in: off, limit: 1).map(\.id) == [5])
        let none = QueueContext(currentID: nil, currentIndex: nil, shuffle: false)
        #expect(UpNextSections.upcoming(rows, in: none, limit: 10).map(\.id) == [7, 4])
    }

    /// Shuffle off, the drawer reads the rows after the current entry; otherwise from the start.
    @Test func drawerWindowStartsAfterTheCurrentEntryWithoutShuffle() {
        #expect(UpNextSections.drawerRange(in: QueueContext(currentID: 9, currentIndex: 450, shuffle: false),
                                           limit: 100) == 451..<551)
        #expect(UpNextSections.drawerRange(in: QueueContext(currentID: 9, currentIndex: 450, shuffle: true),
                                           limit: 100) == 0..<100)
        #expect(UpNextSections.drawerRange(in: QueueContext(currentID: nil, currentIndex: nil, shuffle: false),
                                           limit: 100) == 0..<100)
    }

    @Test func sectionTitles() {
        #expect(QueueSection.history.title == "HISTORY")
        #expect(QueueSection.nowPlaying.title == "NOW PLAYING")
        #expect(QueueSection.upNext.title == "UP NEXT")
    }

    @Test func addedFromLabels() {
        func label(_ kind: SourceKind, _ name: String) -> String {
            UpNextSections.addedFrom(QueueSource(kind: kind, name: name))
        }
        #expect(label(.album, "Spirit of Eden") == "Album · Spirit of Eden")
        #expect(label(.playlist, "Late Night") == "Playlist · Late Night")
        #expect(label(.smart, "Unplayed Jazz") == "Smart · Unplayed Jazz")
        #expect(label(.songs, "Songs") == "Songs")
        #expect(label(.drag, "Songs") == "Dragged from Songs")
        #expect(label(.direct, "") == "")
        #expect(UpNextSections.addedFrom(nil) == "")
    }
}
