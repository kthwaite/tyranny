import Foundation

/// Recorded while an import session is live, so a relaunch mid-import can show the screen again.
public struct ImportNote: Codable, Equatable, Sendable {
    public let endpoint: String
    public let rootID: UInt64

    public init(endpoint: String, rootID: UInt64) {
        self.endpoint = endpoint
        self.rootID = rootID
    }
}

public struct ImportNoteStore: Sendable {
    public static let key = "importNote"

    /// The store for a launch with `arguments`; it shares the quit note's defaults suite.
    public static func forLaunch(arguments: [String]) -> ImportNoteStore {
        let defaults = QuitSuspensionStore.suiteName(forArguments: arguments)
            .flatMap(UserDefaults.init(suiteName:)) ?? .standard
        return ImportNoteStore(defaults: defaults)
    }

    // UserDefaults is documented thread-safe, so sharing it across isolation domains is sound.
    nonisolated(unsafe) private let defaults: UserDefaults

    public init(defaults: UserDefaults) {
        self.defaults = defaults
    }

    public func save(_ note: ImportNote) {
        guard let data = try? JSONEncoder().encode(note) else { return }
        defaults.set(data, forKey: Self.key)
    }

    public func clear() {
        defaults.removeObject(forKey: Self.key)
    }

    /// Returns the stored note and removes it. Undecodable data is removed and returns nil.
    public func take() -> ImportNote? {
        guard let object = defaults.object(forKey: Self.key) else { return nil }
        defaults.removeObject(forKey: Self.key)
        guard let data = object as? Data else { return nil }
        return try? JSONDecoder().decode(ImportNote.self, from: data)
    }
}
