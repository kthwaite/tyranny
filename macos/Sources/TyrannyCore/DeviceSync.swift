import Foundation

/// USB identities are independent of the playback server and library track IDs.
public struct MTPDevice: Codable, Sendable, Equatable, Identifiable {
    public let id: String
    public let name: String
    public let storages: [MTPStorage]
    public var songs: [MTPSong]

    public init(id: String, name: String, storages: [MTPStorage], songs: [MTPSong]) {
        self.id = id; self.name = name; self.storages = storages; self.songs = songs
    }
}

public struct MTPStorage: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt32
    public let name: String
    public let capacity: UInt64
    public let free: UInt64

    public init(id: UInt32, name: String, capacity: UInt64, free: UInt64) {
        self.id = id; self.name = name; self.capacity = capacity; self.free = free
    }
}

public struct MTPSong: Codable, Sendable, Equatable, Identifiable {
    public let id: UInt32
    public let storage_id: UInt32
    public let path: String
    public let name: String
    public let size: UInt64
    public let sha256: String?

    public init(id: UInt32, storage_id: UInt32, path: String, name: String, size: UInt64, sha256: String? = nil) {
        self.id = id; self.storage_id = storage_id; self.path = path
        self.name = name; self.size = size; self.sha256 = sha256
    }
}

/// MTP capacity/free figures may be inconsistent during Android's media scan.
/// Keep all segments within capacity, and never wrap unsigned subtraction.
public struct DeviceStorageUsage: Sendable, Equatable {
    public let music: UInt64
    public let other: UInt64
    public let free: UInt64
    public let capacity: UInt64

    public init(storage: MTPStorage, songs: [MTPSong]) {
        capacity = storage.capacity
        free = min(storage.free, capacity)
        let used = capacity - free
        var bytes: UInt64 = 0
        for song in songs where song.storage_id == storage.id {
            let (sum, overflow) = bytes.addingReportingOverflow(song.size)
            bytes = overflow ? UInt64.max : sum
        }
        music = min(bytes, used)
        other = used - music
    }
}

public struct MTPFile: Codable, Sendable {
    public let path: String
    public init(path: String) { self.path = path }
}

public struct MTPRequest: Codable, Sendable {
    public let operation: String
    public var device_id: String?
    public var storage_id: UInt32?
    public var files: [MTPFile]?
    public var object_ids: [UInt32]?
    public var expected_songs: [MTPSong]?

    public init(operation: String, device_id: String? = nil, storage_id: UInt32? = nil,
                files: [MTPFile]? = nil, object_ids: [UInt32]? = nil, expected_songs: [MTPSong]? = nil) {
        self.operation = operation; self.device_id = device_id; self.storage_id = storage_id
        self.files = files; self.object_ids = object_ids
        self.expected_songs = expected_songs
    }
}

public struct MTPMatch: Codable, Sendable, Equatable {
    public let path: String
    public let object_id: UInt32
    public let storage_id: UInt32
    public let sha256: String
}

public struct MTPResponse: Codable, Sendable {
    public let devices: [MTPDevice]?
    public let device: MTPDevice?
    public let copied: Int?
    public let skipped: Int?
    public let matches: [MTPMatch]?
    public let error: String?
}

public struct DeviceSyncFailure: LocalizedError, Sendable {
    public let errorDescription: String?
    public init(_ message: String) { errorDescription = message }
}
