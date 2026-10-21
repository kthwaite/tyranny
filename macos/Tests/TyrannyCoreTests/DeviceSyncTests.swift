import Foundation
import Testing
@testable import TyrannyCore

@Suite struct DeviceSyncTests {
    private func row(_ id: UInt64, available: Bool = true, path: String? = nil) -> ListRow {
        ListRow(row_id: id + 1000, artist_key: 1, album_key: 1, available: available,
            track: TrackSummary(id: id, album_id: 1, artist_id: 1, album_artist_id: 1,
                title: "Song \(id)", artist: "Artist", album: "Album", album_artist: "Artist",
                compilation: false, disc_number: 1, track_number: 1, duration_ms: 1000,
                year: nil, genre: "", format: "FLAC", sample_rate: 44100, bitrate_kbps: nil,
                play_count: 0, last_played: nil, date_added: 0, path: path ?? "/music/\(id).flac",
                liked: false))
    }

    @Test func pagedSelectionExcludesRowsAndDeduplicatesFiles() async throws {
        let rows = (0..<605).map { row(UInt64($0), available: $0 != 501, path: $0 == 603 ? "/music/600.flac" : nil) }
        let selection = TrackSelection.list(list: ListDescription(source: .playlist(id: 3, revision: 8)),
            libraryRevision: 7, ranges: [[1, UInt64.max]], excluded: [1501], firstRowID: 1001)
        let paths = try await DeviceTrackResolver.paths(selection: selection) { _, revision, offset, limit in
            ListTracksResponse(revision: revision, total: UInt64(rows.count), offset: offset,
                entries: Array(rows.dropFirst(offset).prefix(limit)), totals: nil)
        }
        #expect(paths == (1..<605).filter { $0 != 501 && $0 != 603 }.map { "/music/\($0).flac" })
    }

    @Test func idSelectionPreservesOrderAcrossPages() async throws {
        let rows = (0..<650).map { row(UInt64($0)) }
        let paths = try await DeviceTrackResolver.paths(selection: .ids(libraryRevision: 9, trackIDs: [610, 2, 610])) {
            _, revision, offset, limit in
            ListTracksResponse(revision: revision, total: UInt64(rows.count), offset: offset,
                entries: Array(rows.dropFirst(offset).prefix(limit)), totals: nil)
        }
        #expect(paths == ["/music/610.flac", "/music/2.flac"])
    }

    @Test func stalePageCannotResolveToWrongSongs() async {
        await #expect(throws: DeviceSyncFailure.self) {
            try await DeviceTrackResolver.paths(selection: .ids(libraryRevision: 2, trackIDs: [1])) { _, _, offset, _ in
                ListTracksResponse(revision: 3, total: 1, offset: offset, entries: [row(1)], totals: nil)
            }
        }
    }

    @Test func movedPlaylistAnchorRejectsSelection() async {
        await #expect(throws: DeviceSyncFailure.self) {
            try await DeviceTrackResolver.paths(selection: .list(list: ListDescription(source: .playlist(id: 3, revision: 8)),
                libraryRevision: 2, ranges: [[0, 1]], excluded: [], firstRowID: 1001)) { _, revision, offset, _ in
                ListTracksResponse(revision: revision, total: 1, offset: offset, entries: [row(2)], totals: nil)
            }
        }
    }

    @Test func unavailableSelectionFailsBeforeReturningAnyPaths() async {
        await #expect(throws: DeviceSyncFailure.self) {
            try await DeviceTrackResolver.paths(selection: .ids(libraryRevision: 2, trackIDs: [1, 2])) { _, revision, offset, _ in
                ListTracksResponse(revision: revision, total: 2, offset: offset,
                    entries: [row(1), row(2, available: false)], totals: nil)
            }
        }
    }

    @Test func storageBarSeparatesStoragesAndClampsInconsistentReports() {
        let songs = [MTPSong(id: 1, storage_id: 10, path: "/a", name: "a", size: 30),
                     MTPSong(id: 2, storage_id: 11, path: "/b", name: "b", size: 70)]
        let usage = DeviceStorageUsage(storage: MTPStorage(id: 10, name: "Internal", capacity: 100, free: 40), songs: songs)
        #expect(usage.music == 30 && usage.other == 30 && usage.free == 40)
        let overfull = DeviceStorageUsage(storage: MTPStorage(id: 10, name: "Internal", capacity: 20, free: 5), songs: songs)
        #expect(overfull.music == 15 && overfull.other == 0 && overfull.free == 5)
        let invalid = DeviceStorageUsage(storage: MTPStorage(id: 10, name: "Internal", capacity: 20, free: 50), songs: songs)
        #expect(invalid.music == 0 && invalid.other == 0 && invalid.free == 20)
    }
}
