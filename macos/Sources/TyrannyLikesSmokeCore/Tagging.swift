import Foundation

public enum TagField: String, Codable, Sendable, CaseIterable {
    case title, artist, album
    case albumArtist = "album_artist"
    case trackNumber = "track_number"
    case trackTotal = "track_total"
    case discNumber = "disc_number"
    case discTotal = "disc_total"
    case date
    case originalDate = "original_date"
    case musicbrainzRecordingID = "musicbrainz_recording_id"
    case musicbrainzTrackID = "musicbrainz_track_id"
    case musicbrainzReleaseID = "musicbrainz_release_id"
    case musicbrainzReleaseGroupID = "musicbrainz_release_group_id"
    case musicbrainzArtistID = "musicbrainz_artist_id"
    case musicbrainzAlbumArtistID = "musicbrainz_album_artist_id"

    public var label: String {
        switch self {
        case .title: "Title"
        case .artist: "Artist"
        case .album: "Album"
        case .albumArtist: "Album Artist"
        case .trackNumber: "Track Number"
        case .trackTotal: "Track Total"
        case .discNumber: "Disc Number"
        case .discTotal: "Disc Total"
        case .date: "Date"
        case .originalDate: "Original Date"
        case .musicbrainzRecordingID: "MusicBrainz Recording ID"
        case .musicbrainzTrackID: "MusicBrainz Track ID"
        case .musicbrainzReleaseID: "MusicBrainz Release ID"
        case .musicbrainzReleaseGroupID: "MusicBrainz Release Group ID"
        case .musicbrainzArtistID: "MusicBrainz Artist ID"
        case .musicbrainzAlbumArtistID: "MusicBrainz Album Artist ID"
        }
    }
}

public struct TagCandidate: Codable, Sendable, Equatable, Identifiable {
    public let release_id: String
    public let title: String
    public let artist: String
    public let date: String?
    public let country: String?
    public let track_count: UInt32
    public let score: UInt32

    public var id: String { release_id }

    /// "Title — Artist · year · country · N tracks". Missing parts drop with their separator.
    public var label: String {
        var parts = ["\(title) — \(artist)"]
        if let date, !date.isEmpty { parts.append(String(date.prefix(4))) }
        if let country, !country.isEmpty { parts.append(country) }
        parts.append(track_count == 1 ? "1 track" : "\(track_count) tracks")
        return parts.joined(separator: " · ")
    }
}

public struct FieldChange: Codable, Sendable, Equatable {
    public let field: TagField
    public let old: String?
    public let new: String

    public var line: String { "\(field.label): \(old ?? "(none)") → \(new)" }
}

public struct TrackProposal: Codable, Sendable, Equatable, Identifiable {
    public let track_id: UInt64
    public let file_name: String
    public let track_number: UInt32?
    public let matched: Bool
    public let writable: Bool
    public let reason: String?
    public let removes_other_copies: Bool
    public let unwritable_fields: [TagField]
    public let changes: [FieldChange]

    public var id: UInt64 { track_id }

    public var notes: [String] {
        var notes: [String] = []
        if !matched {
            notes.append("No matching track on this release")
        } else if !writable {
            if let reason { notes.append(reason) }
        } else if changes.isEmpty {
            notes.append("Already matches")
        }
        for field in unwritable_fields { notes.append("can't write \(field.label) to this format") }
        if removes_other_copies { notes.append("Also removes old copies of these fields from other tags") }
        return notes
    }
}

public struct TagProposal: Codable, Sendable, Equatable {
    public let proposal_id: String
    public let release_id: String
    public let candidates: [TagCandidate]
    public let tracks: [TrackProposal]
}

public struct TagProposalResponse: Codable, Sendable, Equatable {
    public let proposal: TagProposal
}

public enum TagFileOutcome: String, Codable, Sendable {
    case written, skipped, failed
}

public struct TagFileResult: Codable, Sendable, Equatable {
    public let track_id: UInt64
    public let file_name: String
    public let outcome: TagFileOutcome
    public let message: String?

    public var line: String {
        switch outcome {
        case .written: "\(file_name): written"
        case .skipped: "\(file_name): skipped — \(message ?? "")"
        case .failed: "\(file_name): failed — \(message ?? "")"
        }
    }
}

public struct TagApplyResult: Codable, Sendable, Equatable {
    public let edit_id: UInt64?
    public let files: [TagFileResult]
}

public struct TagEditSummary: Codable, Sendable, Equatable, Identifiable {
    public let edit_id: UInt64
    public let created_at: Int64
    public let release_title: String
    public let file_count: UInt32
    public let reverted: Bool

    public var id: UInt64 { edit_id }

    public func rowText(timeZone: TimeZone) -> String {
        let formatter = DateFormatter()
        formatter.locale = Locale(identifier: "en_US_POSIX")
        formatter.timeZone = timeZone
        formatter.dateFormat = "yyyy-MM-dd HH:mm"
        let when = formatter.string(from: Date(timeIntervalSince1970: TimeInterval(created_at)))
        let files = file_count == 1 ? "1 file" : "\(file_count) files"
        return "\(when) · \(release_title) · \(files)" + (reverted ? " · reverted" : "")
    }
}

public struct TagEditsResponse: Codable, Sendable, Equatable {
    public let edits: [TagEditSummary]
}

/// Which tracks of a proposal the user will write. Starts as every enabled track.
public struct TagSelection: Equatable, Sendable {
    private let proposal: TagProposal
    public private(set) var selected: Set<UInt64>

    public init(_ proposal: TagProposal) {
        self.proposal = proposal
        selected = Set(proposal.tracks.filter(Self.isEnabled).map(\.track_id))
    }

    private static func isEnabled(_ track: TrackProposal) -> Bool {
        track.matched && track.writable && !track.changes.isEmpty
    }

    public func isEnabled(_ trackID: UInt64) -> Bool {
        proposal.tracks.first { $0.track_id == trackID }.map(Self.isEnabled) ?? false
    }

    public mutating func toggle(_ trackID: UInt64) {
        guard isEnabled(trackID) else { return }
        if !selected.insert(trackID).inserted { selected.remove(trackID) }
    }

    public var writeButtonTitle: String {
        selected.count == 1 ? "Write Tags to 1 File" : "Write Tags to \(selected.count) Files"
    }

    public func applyRequest() -> CommandRequest? {
        let ids = proposal.tracks.map(\.track_id).filter { selected.contains($0) }
        guard !ids.isEmpty else { return nil }
        return CommandRequest("apply_tags", ["proposal_id": .string(proposal.proposal_id), "track_ids": .uints(ids)])
    }
}

public enum AcoustIDKey {
    /// Trimmed key, or nil when empty or not 1–64 ASCII letters/digits (the server's rule).
    public static func normalized(_ text: String) -> String? {
        let key = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard (1...64).contains(key.utf8.count), key.utf8.allSatisfy({ ($0 | 0x20) >= 0x61 && ($0 | 0x20) <= 0x7a || ($0 >= 0x30 && $0 <= 0x39) }) else { return nil }
        return key
    }
}

public enum TagRequests {
    /// The lookup lane's request deadline. The server gives up on a lookup after 50 s.
    public static let lookupDeadlineSeconds: UInt64 = 60
    /// The edit lane's (apply, revert, tag_edits) request deadline. The server never cancels a
    /// write, so a long apply must not time out in the client while files are still being written.
    public static let editDeadlineSeconds: UInt64 = 30 * 60

    public static func lookup(albumID: UInt64, libraryRevision: UInt64, releaseID: String?, acoustIDKey: String?) -> CommandRequest {
        var fields: [String: JSONValue] = ["album_id": .uint(albumID), "library_revision": .uint(libraryRevision)]
        if let releaseID { fields["release_id"] = .string(releaseID) }
        if let acoustIDKey {
            fields["fingerprint"] = .bool(true)
            fields["acoustid_key"] = .string(acoustIDKey)
        }
        return CommandRequest("musicbrainz_lookup", fields)
    }

    public static let edits = CommandRequest("tag_edits")

    public static func revert(editID: UInt64) -> CommandRequest {
        CommandRequest("revert_tag_edit", ["edit_id": .uint(editID)])
    }
}

public enum TagLookupMessage {
    public static func text(for failure: ServerFailure) -> String {
        switch failure.code {
        case "invalid_command" where failure.message.contains("lookup expired"):
            "This lookup expired. Look the album up again."
        case "stale_library_revision": "The library changed. Look the album up again."
        case "request_timeout": "MusicBrainz took too long to answer."
        default: failure.message
        }
    }
}

/// Failures of an apply or a revert.
public enum TagEditMessage {
    public static func text(for failure: ServerFailure) -> String {
        switch failure.code {
        // The server keeps writing after the client stops waiting.
        case "request_timeout": "Still writing tags. Check Recent Tag Edits in Settings."
        default: TagLookupMessage.text(for: failure)
        }
    }

    /// Failures of reading the Recent Tag Edits list.
    public static func listText(for failure: ServerFailure) -> String {
        switch failure.code {
        case "request_timeout": "Recent tag edits took too long to load. Try again."
        default: TagLookupMessage.text(for: failure)
        }
    }
}
