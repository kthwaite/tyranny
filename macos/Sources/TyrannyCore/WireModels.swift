import Foundation

/// The local control protocol this client speaks. The server's hello must carry exactly this
/// version (`src/protocol.rs` `VERSION`; `cargo test` checks that the two agree).
public enum WireProtocol {
    public static let version: UInt32 = 22
}

public struct PlayerSettings: Codable, Sendable, Equatable {
    public let gain_mode: String
    public let resume_on_start: Bool
    public let volume: UInt8
    public let `repeat`: RepeatMode
    public let shuffle: Bool
    public let equalizer: EqualizerSettings
    public let equalizer_presets: [EqualizerUserPreset]

    public init(gain_mode: String, resume_on_start: Bool, volume: UInt8, repeat: RepeatMode, shuffle: Bool, equalizer: EqualizerSettings, equalizer_presets: [EqualizerUserPreset] = []) {
        self.gain_mode = gain_mode
        self.resume_on_start = resume_on_start
        self.volume = volume
        self.`repeat` = `repeat`
        self.shuffle = shuffle
        self.equalizer = equalizer
        self.equalizer_presets = equalizer_presets
    }
}

public struct NowPlaying: Codable, Sendable, Equatable {
    public let entry_id: UInt64
    public let title: String
    public let artist: String
    public let album: String
    public let duration_frames: UInt64?
    public let metadata_error: String?
    public let track_id: UInt64?
    public let album_id: UInt64?
    /// The album's artist, not the track artist; `nil` for an unindexed entry.
    public let album_artist_id: UInt64?
    /// Whether the album is a compilation; `nil` for an unindexed entry.
    public let compilation: Bool?
    public let format: String
    public let gain_db: Double?
    /// The saved station name of a live stream; `nil` for a library entry.
    public let station: String?

    public init(entry_id: UInt64, title: String, artist: String, album: String, duration_frames: UInt64?, metadata_error: String?, track_id: UInt64?, album_id: UInt64?, album_artist_id: UInt64?, compilation: Bool?, format: String, gain_db: Double?, station: String? = nil) {
        self.entry_id = entry_id
        self.title = title
        self.artist = artist
        self.album = album
        self.duration_frames = duration_frames
        self.metadata_error = metadata_error
        self.track_id = track_id
        self.album_id = album_id
        self.album_artist_id = album_artist_id
        self.compilation = compilation
        self.format = format
        self.gain_db = gain_db
        self.station = station
    }

    /// The artist the artist text opens, as "Show in Artist" does: the album artist. The
    /// Artists browser lists only album artists of non-compilation albums, so a compilation
    /// or unindexed entry has none.
    public var artistLinkID: UInt64? {
        compilation == true ? nil : album_artist_id
    }
}

public struct ControlState: Codable, Sendable, Equatable {
    public let sequence: UInt64
    public let queue_revision: UInt64
    public let playlists_revision: UInt64
    public let stations_revision: UInt64
    public let queue_length: Int
    public let current_id: UInt64?
    public let can_next: Bool
    public let can_previous: Bool
    public let now_playing: NowPlaying?
    public let position_frames: UInt64
    public let sample_rate: UInt32
    public let transport: String
    public let settings: PlayerSettings
    public let error: String?
    public let underruns: UInt64
    public let library: LibraryState
    public let history_revision: UInt64
    /// Changes on every play history write: a play opening, counting, saving its listened time or closing.
    public let stats_revision: UInt64
    /// The queue position of `current_id`; nil with nothing current.
    public let current_index: Int?
    /// The upcoming entries, by the server's shuffle-aware rule (see `UpNextSections`).
    public let upcoming_count: Int

    public init(
        sequence: UInt64, queue_revision: UInt64, playlists_revision: UInt64, stations_revision: UInt64, queue_length: Int,
        current_id: UInt64?, can_next: Bool, can_previous: Bool, now_playing: NowPlaying?,
        position_frames: UInt64, sample_rate: UInt32, transport: String, settings: PlayerSettings,
        error: String?, underruns: UInt64, library: LibraryState, history_revision: UInt64, stats_revision: UInt64,
        current_index: Int?, upcoming_count: Int
    ) {
        self.sequence = sequence
        self.queue_revision = queue_revision
        self.playlists_revision = playlists_revision
        self.stations_revision = stations_revision
        self.queue_length = queue_length
        self.current_id = current_id
        self.can_next = can_next
        self.can_previous = can_previous
        self.now_playing = now_playing
        self.position_frames = position_frames
        self.sample_rate = sample_rate
        self.transport = transport
        self.settings = settings
        self.error = error
        self.underruns = underruns
        self.library = library
        self.history_revision = history_revision
        self.stats_revision = stats_revision
        self.current_index = current_index
        self.upcoming_count = upcoming_count
    }

    private enum CodingKeys: String, CodingKey {
        case sequence, queue_revision, queue_length, current_id, can_next, can_previous, now_playing
        case playlists_revision, stations_revision
        case position_frames, sample_rate, transport, settings, error, underruns, library
        case history_revision, stats_revision, current_index, upcoming_count
    }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        sequence = try values.decode(UInt64.self, forKey: .sequence)
        queue_revision = try values.decode(UInt64.self, forKey: .queue_revision)
        playlists_revision = try values.decode(UInt64.self, forKey: .playlists_revision)
        stations_revision = try values.decode(UInt64.self, forKey: .stations_revision)
        queue_length = try values.decode(Int.self, forKey: .queue_length)
        current_id = try values.decode(UInt64?.self, forKey: .current_id)
        can_next = try values.decode(Bool.self, forKey: .can_next)
        can_previous = try values.decode(Bool.self, forKey: .can_previous)
        now_playing = try values.decode(NowPlaying?.self, forKey: .now_playing)
        position_frames = try values.decode(UInt64.self, forKey: .position_frames)
        sample_rate = try values.decode(UInt32.self, forKey: .sample_rate)
        transport = try values.decode(String.self, forKey: .transport)
        settings = try values.decode(PlayerSettings.self, forKey: .settings)
        error = try values.decode(String?.self, forKey: .error)
        underruns = try values.decode(UInt64.self, forKey: .underruns)
        library = try values.decode(LibraryState.self, forKey: .library)
        history_revision = try values.decode(UInt64.self, forKey: .history_revision)
        stats_revision = try values.decode(UInt64.self, forKey: .stats_revision)
        current_index = try values.decode(Int?.self, forKey: .current_index)
        upcoming_count = try values.decode(Int.self, forKey: .upcoming_count)
        guard now_playing?.entry_id == current_id else {
            throw DecodingError.dataCorruptedError(
                forKey: .now_playing, in: values,
                debugDescription: "Now Playing must identify the authoritative current occurrence.")
        }
    }
}

public struct LibraryState: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let roots: Int
    public let tracks: Int
    public let duration_ms: UInt64
    public let size_bytes: UInt64
    public let scan: ScanStatus

    public init(revision: UInt64, roots: Int, tracks: Int, duration_ms: UInt64, size_bytes: UInt64, scan: ScanStatus) {
        self.revision = revision
        self.roots = roots
        self.tracks = tracks
        self.duration_ms = duration_ms
        self.size_bytes = size_bytes
        self.scan = scan
    }
}

public struct ScanStatus: Codable, Sendable, Equatable {
    public let root_id: UInt64?
    public let phase: String
    public let discovered: Int
    public let processed: Int
    public let total: Int?
    public let updated: Int
    public let unchanged: Int
    public let failures: Int
    public let skipped: Int
    public let last_error: String?

    public init(root_id: UInt64?, phase: String, discovered: Int, processed: Int, total: Int?, updated: Int, unchanged: Int, failures: Int, skipped: Int, last_error: String?) {
        self.root_id = root_id
        self.phase = phase
        self.discovered = discovered
        self.processed = processed
        self.total = total
        self.updated = updated
        self.unchanged = unchanged
        self.failures = failures
        self.skipped = skipped
        self.last_error = last_error
    }
}

public struct Root: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt64
    public let path: String
    public let last_scan: ScanStatus?

    public init(id: UInt64, path: String, last_scan: ScanStatus?) {
        self.id = id
        self.path = path
        self.last_scan = last_scan
    }
}

public struct TrackMetadata: Codable, Sendable, Equatable {
    public let title: String
    public let artist: String
    public let album_artist: String
    public let album: String
    public let track_number: Int?
    public let track_total: Int?
    public let disc_number: Int?
    public let disc_total: Int?
    public let compilation: Bool
    public let duration_ms: UInt64?
    public let sample_rate: UInt32
    public let channels: UInt16

    public init(title: String, artist: String, album_artist: String, album: String, track_number: Int?, track_total: Int?, disc_number: Int?, disc_total: Int?, compilation: Bool, duration_ms: UInt64?, sample_rate: UInt32, channels: UInt16) {
        self.title = title
        self.artist = artist
        self.album_artist = album_artist
        self.album = album
        self.track_number = track_number
        self.track_total = track_total
        self.disc_number = disc_number
        self.disc_total = disc_total
        self.compilation = compilation
        self.duration_ms = duration_ms
        self.sample_rate = sample_rate
        self.channels = channels
    }
}

public struct StreamSpec: Codable, Sendable, Equatable {
    public let url: String
    public let title: String

    public init(url: String, title: String) {
        self.url = url
        self.title = title
    }
}

public struct QueueItem: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt64
    public let path: String
    public let stream: StreamSpec?
    public let track_id: UInt64?
    public let source: QueueSource?
    public let run_id: UInt64?
    public let played: Bool
    public let title: String
    public let artist: String
    public let album: String
    public let album_id: UInt64?
    public let duration_ms: UInt64?

    public var displayTitle: String {
        if let stream { return stream.title.isEmpty ? stream.url : stream.title }
        return title.isEmpty ? (path as NSString).lastPathComponent : title
    }

    public var displayArtist: String { stream?.url ?? artist }

    public var displayDuration: String {
        if stream != nil { return "Live" }
        return duration_ms.map { DisplayFormat.clock(Double($0) / 1000) } ?? ""
    }

    public init(id: UInt64, path: String, track_id: UInt64?, source: QueueSource?, run_id: UInt64?, played: Bool, title: String, artist: String, album: String, album_id: UInt64?, duration_ms: UInt64?, stream: StreamSpec? = nil) {
        self.id = id
        self.path = path
        self.stream = stream
        self.track_id = track_id
        self.source = source
        self.run_id = run_id
        self.played = played
        self.title = title
        self.artist = artist
        self.album = album
        self.album_id = album_id
        self.duration_ms = duration_ms
    }
}

public struct QueueSource: Codable, Sendable, Equatable {
    /// The server's `MAX_SOURCE_NAME_BYTES`.
    public static let maximumNameBytes = 256

    public let kind: SourceKind
    public let name: String

    /// A name longer than `maximumNameBytes` UTF-8 bytes is cut on a Unicode scalar boundary,
    /// as the server's `QueueSource::stamped` cuts on a `char` boundary. Decoded names are kept.
    public init(kind: SourceKind, name: String) {
        self.kind = kind
        guard name.utf8.count > Self.maximumNameBytes else {
            self.name = name
            return
        }
        var bytes = 0
        var scalars = String.UnicodeScalarView()
        for scalar in name.unicodeScalars {
            bytes += UTF8.width(scalar)
            if bytes > Self.maximumNameBytes { break }
            scalars.append(scalar)
        }
        self.name = String(scalars)
    }
}

public struct Page<T: Codable & Sendable & Equatable>: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let total: Int
    public let offset: Int
    public let entries: [T]

    public init(revision: UInt64, total: Int, offset: Int, entries: [T]) {
        self.revision = revision
        self.total = total
        self.offset = offset
        self.entries = entries
    }
}

public struct Playlist: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt64
    public let name: String
    public let revision: UInt64
    public let kind: String
    public let query: String?
    public let entry_count: Int?

    public init(id: UInt64, name: String, revision: UInt64, kind: String, query: String?, entry_count: Int?) {
        self.id = id
        self.name = name
        self.revision = revision
        self.kind = kind
        self.query = query
        self.entry_count = entry_count
    }
    public var isSmart: Bool { kind == "smart" }
    public var summary: String { isSmart ? "Smart playlist · Live results" : "\(entry_count ?? 0) tracks" }
}

public struct PlaylistEntry: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt64
    public let track_id: UInt64
    public let path: String
    public let metadata: TrackMetadata
    public let available: Bool
    public let error: String?

    public init(id: UInt64, track_id: UInt64, path: String, metadata: TrackMetadata, available: Bool, error: String?) {
        self.id = id
        self.track_id = track_id
        self.path = path
        self.metadata = metadata
        self.available = available
        self.error = error
    }
}

public struct PlaylistEntries: Codable, Sendable, Equatable {
    public let playlist_id: UInt64
    public let library_revision: UInt64
    public let revision: UInt64
    public let total: Int
    public let offset: Int
    public let entries: [PlaylistEntry]

    public init(playlist_id: UInt64, library_revision: UInt64, revision: UInt64, total: Int, offset: Int, entries: [PlaylistEntry]) {
        self.playlist_id = playlist_id
        self.library_revision = library_revision
        self.revision = revision
        self.total = total
        self.offset = offset
        self.entries = entries
    }
}

public struct PlaylistApplied: Codable, Sendable, Equatable {
    public let snapshot: ControlState
    public let playlist: Playlist?

    public init(snapshot: ControlState, playlist: Playlist?) {
        self.snapshot = snapshot
        self.playlist = playlist
    }
}

public struct Applied: Codable, Sendable, Equatable {
    public let snapshot: ControlState
    public let entry_id: UInt64?

    public init(snapshot: ControlState, entry_id: UInt64?) {
        self.snapshot = snapshot
        self.entry_id = entry_id
    }
}

public struct RootAdded: Codable, Sendable, Equatable {
    public let root: Root

    public init(root: Root) {
        self.root = root
    }
}

public struct ScanAccepted: Codable, Sendable, Equatable {
    public let root_id: UInt64

    public init(root_id: UInt64) {
        self.root_id = root_id
    }
}

public struct SnapshotReply: Codable, Sendable, Equatable {
    public let snapshot: ControlState

    public init(snapshot: ControlState) {
        self.snapshot = snapshot
    }
}

public struct ServerFailure: Error, LocalizedError, Codable, Sendable, Equatable {
    public let code: String
    public let message: String
    public let queue_revision: UInt64?

    public init(code: String, message: String, queue_revision: UInt64? = nil) {
        self.code = code
        self.message = message
        self.queue_revision = queue_revision
    }

    public var errorDescription: String? {
        "\(message) [\(code)]"
    }
}

// MARK: - v9 enums

public enum RepeatMode: String, Codable, Sendable, Equatable {
    case off, all, one
}

public enum SourceKind: String, Codable, Sendable, Equatable {
    case album, playlist, smart, songs, genre, artist, recent, compilations, drag, direct, stream
}

public enum Placement: String, Codable, Sendable, Equatable {
    case append, next
    case replaceUpcoming = "replace_upcoming"
}

// MARK: - List description

public enum ListSource: Codable, Sendable, Equatable {
    case library
    case album(id: UInt64)
    case artist(id: UInt64)
    case appearsOn(artistID: UInt64)
    case compilations
    case genre(id: UInt64)
    case recent
    case playlist(id: UInt64, revision: UInt64)
    case smart(id: UInt64, revision: UInt64)

    private enum CodingKeys: String, CodingKey {
        case kind, id, revision
        case artistID = "artist_id"
    }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(String.self, forKey: .kind) {
        case "library": self = .library
        case "album": self = .album(id: try values.decode(UInt64.self, forKey: .id))
        case "artist": self = .artist(id: try values.decode(UInt64.self, forKey: .id))
        case "appears_on": self = .appearsOn(artistID: try values.decode(UInt64.self, forKey: .artistID))
        case "compilations": self = .compilations
        case "genre": self = .genre(id: try values.decode(UInt64.self, forKey: .id))
        case "recent": self = .recent
        case "playlist":
            self = .playlist(id: try values.decode(UInt64.self, forKey: .id), revision: try values.decode(UInt64.self, forKey: .revision))
        case "smart":
            self = .smart(id: try values.decode(UInt64.self, forKey: .id), revision: try values.decode(UInt64.self, forKey: .revision))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: values, debugDescription: "Unknown list source kind \(other)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .library: try values.encode("library", forKey: .kind)
        case .album(let id): try values.encode("album", forKey: .kind); try values.encode(id, forKey: .id)
        case .artist(let id): try values.encode("artist", forKey: .kind); try values.encode(id, forKey: .id)
        case .appearsOn(let artistID):
            try values.encode("appears_on", forKey: .kind); try values.encode(artistID, forKey: .artistID)
        case .compilations: try values.encode("compilations", forKey: .kind)
        case .genre(let id): try values.encode("genre", forKey: .kind); try values.encode(id, forKey: .id)
        case .recent: try values.encode("recent", forKey: .kind)
        case .playlist(let id, let revision):
            try values.encode("playlist", forKey: .kind); try values.encode(id, forKey: .id); try values.encode(revision, forKey: .revision)
        case .smart(let id, let revision):
            try values.encode("smart", forKey: .kind); try values.encode(id, forKey: .id); try values.encode(revision, forKey: .revision)
        }
    }
}

public struct Browser: Codable, Sendable, Equatable {
    public var genre_ids: [UInt64]
    public var artist_ids: [UInt64]
    public var compilations: Bool
    public var album_ids: [UInt64]

    public init(genre_ids: [UInt64] = [], artist_ids: [UInt64] = [], compilations: Bool = false, album_ids: [UInt64] = []) {
        self.genre_ids = genre_ids
        self.artist_ids = artist_ids
        self.compilations = compilations
        self.album_ids = album_ids
    }
}

public enum SortField: String, Codable, Sendable, Equatable {
    case title, artist
    case albumArtist = "album_artist"
    case album, duration, year, genre, format, plays
    case dateAdded = "date_added"
    case lastPlayed = "last_played"
    case albumOrder = "album_order"
    case playlistOrder = "playlist_order"
}

public struct SortKey: Codable, Sendable, Equatable {
    public var field: SortField
    public var descending: Bool

    public init(field: SortField, descending: Bool = false) {
        self.field = field
        self.descending = descending
    }
}

public enum Grouping: String, Codable, Sendable, Equatable {
    case none, artist, album
    case artistAlbum = "artist_album"
}

public struct ListDescription: Codable, Sendable, Equatable {
    public var source: ListSource
    public var browser: Browser
    public var query: String
    public var sort: [SortKey]
    public var group: Grouping

    public init(source: ListSource, browser: Browser = Browser(), query: String = "", sort: [SortKey] = [], group: Grouping = .none) {
        self.source = source
        self.browser = browser
        self.query = query
        self.sort = sort
        self.group = group
    }
}

// MARK: - Selections

public enum TrackSelection: Codable, Sendable, Equatable {
    case ids(libraryRevision: UInt64, trackIDs: [UInt64])
    /// `ranges` are half-open 0-based row positions; an end of `UInt64.max` means the end of the list.
    case list(list: ListDescription, libraryRevision: UInt64, ranges: [[UInt64]], excluded: [UInt64], firstRowID: UInt64?)

    private enum CodingKeys: String, CodingKey {
        case kind, list, ranges, excluded
        case libraryRevision = "library_revision"
        case trackIDs = "track_ids"
        case firstRowID = "first_row_id"
    }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        switch try values.decode(String.self, forKey: .kind) {
        case "ids":
            self = .ids(libraryRevision: try values.decode(UInt64.self, forKey: .libraryRevision),
                        trackIDs: try values.decode([UInt64].self, forKey: .trackIDs))
        case "list":
            self = .list(list: try values.decode(ListDescription.self, forKey: .list),
                         libraryRevision: try values.decode(UInt64.self, forKey: .libraryRevision),
                         ranges: try values.decode([[UInt64]].self, forKey: .ranges),
                         excluded: try values.decode([UInt64].self, forKey: .excluded),
                         firstRowID: try values.decode(UInt64?.self, forKey: .firstRowID))
        case let other:
            throw DecodingError.dataCorruptedError(forKey: .kind, in: values, debugDescription: "Unknown selection kind \(other)")
        }
    }

    public func encode(to encoder: any Encoder) throws {
        var values = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .ids(let revision, let trackIDs):
            try values.encode("ids", forKey: .kind)
            try values.encode(revision, forKey: .libraryRevision)
            try values.encode(trackIDs, forKey: .trackIDs)
        case .list(let list, let revision, let ranges, let excluded, let firstRowID):
            try values.encode("list", forKey: .kind)
            try values.encode(list, forKey: .list)
            try values.encode(revision, forKey: .libraryRevision)
            try values.encode(ranges, forKey: .ranges)
            try values.encode(excluded, forKey: .excluded)
            // The server requires the key; `encode` on an Optional writes null.
            try values.encode(firstRowID, forKey: .firstRowID)
        }
    }
}

// MARK: - List rows and summaries

public struct TrackSummary: Codable, Sendable, Equatable {
    public let id: UInt64
    public let album_id: UInt64
    public let artist_id: UInt64
    public let album_artist_id: UInt64
    public let title: String
    public let artist: String
    public let album: String
    public let album_artist: String
    public let compilation: Bool
    public let disc_number: UInt32
    public let track_number: UInt32
    public let duration_ms: UInt64?
    public let year: Int32?
    public let genre: String
    public let format: String
    public let sample_rate: UInt32
    public let bitrate_kbps: UInt32?
    public let play_count: UInt64
    public let last_played: Int64?
    public let date_added: Int64
    public let path: String
    public let liked: Bool

    public init(
        id: UInt64, album_id: UInt64, artist_id: UInt64, album_artist_id: UInt64, title: String, artist: String,
        album: String, album_artist: String, compilation: Bool, disc_number: UInt32, track_number: UInt32,
        duration_ms: UInt64?, year: Int32?, genre: String, format: String, sample_rate: UInt32,
        bitrate_kbps: UInt32?, play_count: UInt64, last_played: Int64?, date_added: Int64, path: String,
        liked: Bool
    ) {
        self.id = id
        self.album_id = album_id
        self.artist_id = artist_id
        self.album_artist_id = album_artist_id
        self.title = title
        self.artist = artist
        self.album = album
        self.album_artist = album_artist
        self.compilation = compilation
        self.disc_number = disc_number
        self.track_number = track_number
        self.duration_ms = duration_ms
        self.year = year
        self.genre = genre
        self.format = format
        self.sample_rate = sample_rate
        self.bitrate_kbps = bitrate_kbps
        self.play_count = play_count
        self.last_played = last_played
        self.date_added = date_added
        self.path = path
        self.liked = liked
    }
}

public struct TrackLiked: Decodable, Sendable {
    public let track_id: UInt64
    public let liked: Bool
    public let library_revision: UInt64
}

public struct ListRow: Codable, Sendable, Equatable, Identifiable {
    public let row_id: UInt64
    public let artist_key: UInt64
    public let album_key: UInt64
    public let available: Bool
    public let track: TrackSummary
    public var id: UInt64 { row_id }

    public init(row_id: UInt64, artist_key: UInt64, album_key: UInt64, available: Bool, track: TrackSummary) {
        self.row_id = row_id
        self.artist_key = artist_key
        self.album_key = album_key
        self.available = available
        self.track = track
    }
}

public struct ListTotals: Codable, Sendable, Equatable {
    public let tracks: UInt64
    public let duration_ms: UInt64
    public let size_bytes: UInt64

    public init(tracks: UInt64, duration_ms: UInt64, size_bytes: UInt64) {
        self.tracks = tracks
        self.duration_ms = duration_ms
        self.size_bytes = size_bytes
    }
}

public struct AlbumSummary: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt64
    public let title: String
    public let album_artist: String
    public let album_artist_id: UInt64
    public let compilation: Bool
    public let year: Int32?
    public let genre: String
    public let track_count: UInt64
    public let disc_count: UInt64
    public let duration_ms: UInt64
    public let format: String
    public let album_gain_db: Double?
    public let date_added: Int64
    public let musicbrainz_release_id: String?

    public init(
        id: UInt64, title: String, album_artist: String, album_artist_id: UInt64, compilation: Bool, year: Int32?,
        genre: String, track_count: UInt64, disc_count: UInt64, duration_ms: UInt64, format: String,
        album_gain_db: Double?, date_added: Int64, musicbrainz_release_id: String?
    ) {
        self.id = id
        self.title = title
        self.album_artist = album_artist
        self.album_artist_id = album_artist_id
        self.compilation = compilation
        self.year = year
        self.genre = genre
        self.track_count = track_count
        self.disc_count = disc_count
        self.duration_ms = duration_ms
        self.format = format
        self.album_gain_db = album_gain_db
        self.date_added = date_added
        self.musicbrainz_release_id = musicbrainz_release_id
    }
}

public struct NamedCount: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt64
    public let name: String
    public let album_count: UInt64
    public let track_count: UInt64

    public init(id: UInt64, name: String, album_count: UInt64, track_count: UInt64) {
        self.id = id
        self.name = name
        self.album_count = album_count
        self.track_count = track_count
    }
}

public enum AlbumSort: String, Codable, Sendable, Equatable {
    case albumArtist = "album_artist"
    case year
    case dateAdded = "date_added"
}

public struct GroupSpan: Codable, Sendable, Equatable {
    public let artist_key: UInt64
    public let album_key: UInt64?
    public let start: UInt64
    public let count: UInt64

    public init(artist_key: UInt64, album_key: UInt64?, start: UInt64, count: UInt64) {
        self.artist_key = artist_key
        self.album_key = album_key
        self.start = start
        self.count = count
    }
}

public struct HistoryEntry: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt64
    public let played_at: Int64
    public let track_id: UInt64?
    public let title: String
    public let artist: String
    public let album: String
    public let duration_ms: UInt64?
    public let source: QueueSource?

    public init(id: UInt64, played_at: Int64, track_id: UInt64?, title: String, artist: String, album: String, duration_ms: UInt64?, source: QueueSource?) {
        self.id = id
        self.played_at = played_at
        self.track_id = track_id
        self.title = title
        self.artist = artist
        self.album = album
        self.duration_ms = duration_ms
        self.source = source
    }
}

// MARK: - v9 list responses

public struct ListTracksResponse: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let total: UInt64
    public let offset: Int
    public let entries: [ListRow]
    public let totals: ListTotals?

    public init(revision: UInt64, total: UInt64, offset: Int, entries: [ListRow], totals: ListTotals?) {
        self.revision = revision
        self.total = total
        self.offset = offset
        self.entries = entries
        self.totals = totals
    }

    private enum CodingKeys: String, CodingKey { case revision, total, offset, entries, totals }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        revision = try values.decode(UInt64.self, forKey: .revision)
        total = try values.decode(UInt64.self, forKey: .total)
        offset = try values.decode(Int.self, forKey: .offset)
        entries = try values.decode([ListRow].self, forKey: .entries)
        totals = try values.decodeIfPresent(ListTotals.self, forKey: .totals)
    }
}

public struct ListAlbumsResponse: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let total: UInt64
    public let offset: Int
    public let entries: [AlbumSummary]

    public init(revision: UInt64, total: UInt64, offset: Int, entries: [AlbumSummary]) {
        self.revision = revision
        self.total = total
        self.offset = offset
        self.entries = entries
    }
}

public struct ListArtistsResponse: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let total: UInt64
    public let offset: Int
    public let entries: [NamedCount]
    public let compilations: NamedCount?

    public init(revision: UInt64, total: UInt64, offset: Int, entries: [NamedCount], compilations: NamedCount?) {
        self.revision = revision
        self.total = total
        self.offset = offset
        self.entries = entries
        self.compilations = compilations
    }

    private enum CodingKeys: String, CodingKey { case revision, total, offset, entries, compilations }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        revision = try values.decode(UInt64.self, forKey: .revision)
        total = try values.decode(UInt64.self, forKey: .total)
        offset = try values.decode(Int.self, forKey: .offset)
        entries = try values.decode([NamedCount].self, forKey: .entries)
        compilations = try values.decodeIfPresent(NamedCount.self, forKey: .compilations)
    }
}

public struct ListGenresResponse: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let total: UInt64
    public let offset: Int
    public let entries: [NamedCount]

    public init(revision: UInt64, total: UInt64, offset: Int, entries: [NamedCount]) {
        self.revision = revision
        self.total = total
        self.offset = offset
        self.entries = entries
    }
}

public struct AlbumSummariesResponse: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let entries: [AlbumSummary]

    public init(revision: UInt64, entries: [AlbumSummary]) {
        self.revision = revision
        self.entries = entries
    }
}

public struct ListGroupsResponse: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let total: UInt64
    public let groups: [GroupSpan]

    public init(revision: UInt64, total: UInt64, groups: [GroupSpan]) {
        self.revision = revision
        self.total = total
        self.groups = groups
    }
}

public struct HistoryResponse: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let total: UInt64
    public let offset: Int
    public let entries: [HistoryEntry]

    public init(revision: UInt64, total: UInt64, offset: Int, entries: [HistoryEntry]) {
        self.revision = revision
        self.total = total
        self.offset = offset
        self.entries = entries
    }
}

public struct Station: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt64
    public let name: String
    public let url: String

    public init(id: UInt64, name: String, url: String) {
        self.id = id
        self.name = name
        self.url = url
    }
}

public struct StationInput: Codable, Sendable, Equatable {
    public let name: String
    public let url: String

    public init(name: String, url: String) {
        self.name = name
        self.url = url
    }
}

public struct StationsPage: Codable, Sendable, Equatable {
    public let revision: UInt64
    public let stations: [Station]

    public init(revision: UInt64, stations: [Station]) {
        self.revision = revision
        self.stations = stations
    }
}

public struct StationsApplied: Codable, Sendable, Equatable {
    public let snapshot: ControlState
    public let stations: [Station]

    public init(snapshot: ControlState, stations: [Station]) {
        self.snapshot = snapshot
        self.stations = stations
    }
}

/// One station of the radio-browser directory, as `search_directory` returns it.
public struct DirectoryStation: Codable, Sendable, Equatable, Identifiable {
    public let uuid: String
    public let name: String
    public let url: String
    public let codec: String
    public let bitrate: UInt32
    /// Two ASCII letters, or empty.
    public let country_code: String
    public let tags: [String]

    public var id: String { uuid }

    public init(uuid: String, name: String, url: String, codec: String = "", bitrate: UInt32 = 0,
                country_code: String = "", tags: [String] = []) {
        self.uuid = uuid
        self.name = name
        self.url = url
        self.codec = codec
        self.bitrate = bitrate
        self.country_code = country_code
        self.tags = tags
    }
}

public struct DirectoryResults: Codable, Sendable, Equatable {
    public let stations: [DirectoryStation]

    public init(stations: [DirectoryStation]) {
        self.stations = stations
    }
}
