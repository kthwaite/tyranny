import Testing
@testable import TyrannyCore

@Suite struct UpNextSectionTests {
    private func item(_ id: UInt64, played: Bool, source: QueueSource? = nil) -> QueueItem {
        QueueItem(id: id, path: "/m/\(id).flac", track_id: id, source: source, run_id: nil, played: played,
                  title: "T\(id)", artist: "A", album: "B", album_id: 1, duration_ms: 1000)
    }

    @Test func playedNotCurrentIsHistory() {
        #expect(UpNextSections.section(of: item(1, played: true), currentID: 2) == .history)
    }

    @Test func currentIsNowPlayingEvenWhenPlayed() {
        #expect(UpNextSections.section(of: item(2, played: true), currentID: 2) == .nowPlaying)
        #expect(UpNextSections.section(of: item(2, played: false), currentID: 2) == .nowPlaying)
    }

    @Test func unplayedNotCurrentIsUpNext() {
        #expect(UpNextSections.section(of: item(3, played: false), currentID: 2) == .upNext)
    }

    @Test func noCurrentEntrySplitsOnPlayedOnly() {
        #expect(UpNextSections.section(of: item(1, played: true), currentID: nil) == .history)
        #expect(UpNextSections.section(of: item(1, played: false), currentID: nil) == .upNext)
    }

    /// Shuffle and Previous leave played entries after the current one, and unplayed ones before
    /// it. Sections follow the flags, never the position.
    @Test func sectionsIgnoreQueuePositionUnderShuffle() {
        let queue = [item(7, played: false), item(3, played: true), item(5, played: false),
                     item(9, played: true), item(4, played: false)]
        let sections = queue.map { UpNextSections.section(of: $0, currentID: 5) }
        #expect(sections == [.upNext, .history, .nowPlaying, .history, .upNext])
    }

    @Test func headerStartsWhereTheSectionChanges() {
        let queue = [item(7, played: false), item(3, played: true), item(5, played: false)]
        func starts(_ index: Int) -> Bool {
            UpNextSections.startsSection(item: queue[index], previous: index > 0 ? queue[index - 1] : nil,
                                         position: index, currentID: 5)
        }
        #expect(starts(0))
        #expect(starts(1))
        #expect(starts(2))
        let same = [item(1, played: true), item(2, played: true)]
        #expect(!UpNextSections.startsSection(item: same[1], previous: same[0], position: 1, currentID: 9))
    }

    @Test func headerWaitsForAnUnloadedNeighbour() {
        #expect(!UpNextSections.startsSection(item: item(4, played: false), previous: nil, position: 40, currentID: 2))
        #expect(UpNextSections.startsSection(item: item(4, played: false), previous: nil, position: 0, currentID: 2))
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

    @Test func upcomingCountSubtractsPlayedAndCurrent() {
        #expect(UpNextSections.upcomingCount(total: 10, playedNotCurrent: 3, hasCurrent: true) == 6)
        #expect(UpNextSections.upcomingCount(total: 10, playedNotCurrent: 3, hasCurrent: false) == 7)
        #expect(UpNextSections.upcomingCount(total: 1, playedNotCurrent: 0, hasCurrent: true) == 0)
        #expect(UpNextSections.upcomingCount(total: 0, playedNotCurrent: 0, hasCurrent: false) == 0)
    }
}
