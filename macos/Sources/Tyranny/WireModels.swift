import Foundation

struct PlayerSettings: Codable, Sendable, Equatable {
    let gain_mode: String
    let resume_on_start: Bool
}

struct NowPlaying: Codable, Sendable, Equatable {
    let entry_id: UInt64
    let title: String
    let artist: String
    let album: String
    let duration_frames: UInt64?
    let metadata_error: String?
}

struct ControlState: Codable, Sendable, Equatable {
    let sequence: UInt64
    let queue_revision: UInt64
    let playlists_revision: UInt64
    let queue_length: Int
    let current_id: UInt64?
    let can_next: Bool
    let can_previous: Bool
    let now_playing: NowPlaying?
    let position_frames: UInt64
    let sample_rate: UInt32
    let transport: String
    let settings: PlayerSettings
    let error: String?
    let underruns: UInt64
    let library: LibraryState

    private enum CodingKeys: String, CodingKey {
        case sequence, queue_revision, queue_length, current_id, can_next, can_previous, now_playing
        case playlists_revision
        case position_frames, sample_rate, transport, settings, error, underruns, library
    }

    init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        sequence = try values.decode(UInt64.self, forKey: .sequence)
        queue_revision = try values.decode(UInt64.self, forKey: .queue_revision)
        playlists_revision = try values.decode(UInt64.self, forKey: .playlists_revision)
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
        guard now_playing?.entry_id == current_id else {
            throw DecodingError.dataCorruptedError(
                forKey: .now_playing, in: values,
                debugDescription: "Now Playing must identify the authoritative current occurrence.")
        }
    }
}

struct LibraryState: Codable, Sendable, Equatable {
    let revision: UInt64
    let roots: Int
    let tracks: Int
    let scan: ScanStatus
}

struct ScanStatus: Codable, Sendable, Equatable {
    let root_id: UInt64?
    let phase: String
    let discovered: Int
    let processed: Int
    let total: Int?
    let updated: Int
    let unchanged: Int
    let failures: Int
    let skipped: Int
    let last_error: String?
}

struct Root: Codable, Sendable, Equatable, Identifiable {
    let id: UInt64
    let path: String
    let last_scan: ScanStatus?
}

struct AlbumArtist: Codable, Sendable, Equatable, Identifiable {
    let id: UInt64
    let name: String
    let album_count: Int
}

struct Album: Codable, Sendable, Equatable, Identifiable {
    let id: UInt64
    let artist_id: UInt64
    let title: String
    let album_artist: String
    let musicbrainz_release_id: String?
    let compilation: Bool
    let disc_count: Int
    let track_count: Int
}

struct Track: Codable, Sendable, Equatable, Identifiable {
    let id: UInt64
    let asset_id: UInt64
    let album_id: UInt64
    let path: String
    let metadata: TrackMetadata
    let error: String?
}

struct TrackMetadata: Codable, Sendable, Equatable {
    let title: String
    let artist: String
    let album_artist: String
    let album: String
    let track_number: Int?
    let track_total: Int?
    let disc_number: Int?
    let disc_total: Int?
    let compilation: Bool
    let duration_ms: UInt64?
    let sample_rate: UInt32
    let channels: UInt16
}

struct QueueItem: Codable, Sendable, Equatable, Identifiable {
    let id: UInt64
    let path: String
    let track_id: UInt64?
}

struct Page<T: Codable & Sendable & Equatable>: Codable, Sendable, Equatable {
    let revision: UInt64
    let total: Int
    let offset: Int
    let entries: [T]
}

struct Playlist: Codable, Sendable, Equatable, Identifiable {
    let id: UInt64
    let name: String
    let revision: UInt64
    let kind: String
    let query: String?
    let entry_count: Int?
    var isSmart: Bool { kind == "smart" }
    var summary: String { isSmart ? "Smart playlist · Live results" : "\(entry_count ?? 0) tracks" }
}

struct PlaylistEntry: Codable, Sendable, Equatable, Identifiable {
    let id: UInt64
    let track_id: UInt64
    let path: String
    let metadata: TrackMetadata
    let available: Bool
    let error: String?
}

struct PlaylistEntries: Codable, Sendable, Equatable {
    let playlist_id: UInt64
    let library_revision: UInt64
    let revision: UInt64
    let total: Int
    let offset: Int
    let entries: [PlaylistEntry]
}

struct PlaylistApplied: Codable, Sendable, Equatable {
    let snapshot: ControlState
    let playlist: Playlist?
}

struct Applied: Codable, Sendable, Equatable {
    let snapshot: ControlState
    let entry_id: UInt64?
}

struct RootAdded: Codable, Sendable, Equatable {
    let root: Root
}

struct ScanAccepted: Codable, Sendable, Equatable {
    let root_id: UInt64
}

struct SnapshotReply: Codable, Sendable, Equatable {
    let snapshot: ControlState
}

enum JSONValue: Encodable, Sendable {
    case string(String)
    case uint(UInt64)
    case int(Int)
    case bool(Bool)
    case uints([UInt64])
    case null

    func encode(to encoder: any Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let value): try container.encode(value)
        case .uint(let value): try container.encode(value)
        case .int(let value): try container.encode(value)
        case .uints(let value): try container.encode(value)
        case .bool(let value): try container.encode(value)
        case .null: try container.encodeNil()
        }
    }
}

struct ServerFailure: Error, LocalizedError, Codable, Sendable, Equatable {
    let code: String
    let message: String
    let queue_revision: UInt64?

    init(code: String, message: String, queue_revision: UInt64? = nil) {
        self.code = code
        self.message = message
        self.queue_revision = queue_revision
    }

    var errorDescription: String? {
        "\(message) [\(code)]"
    }
}
