import Foundation

public enum ArtworkProvider: String, CaseIterable, Codable, Sendable {
    case coverArtArchive = "cover_art_archive", deezer, discogs

    public var title: String {
        switch self {
        case .coverArtArchive: "Cover Art Archive"
        case .deezer: "Deezer"
        case .discogs: "Discogs"
        }
    }
}

/// Only provider choices are persisted. Credentials never belong in this value.
public struct ArtworkPreferences: Codable, Equatable, Sendable {
    public private(set) var order: [ArtworkProvider]
    public private(set) var enabled: Set<ArtworkProvider>

    public init(order: [ArtworkProvider] = ArtworkProvider.allCases, enabled: Set<ArtworkProvider> = [.coverArtArchive]) {
        var seen = Set<ArtworkProvider>()
        self.order = (order + ArtworkProvider.allCases).filter { seen.insert($0).inserted }
        self.enabled = enabled
    }

    public var providers: [ArtworkProvider] { order.filter { enabled.contains($0) } }

    public mutating func setEnabled(_ provider: ArtworkProvider, _ value: Bool) {
        if value { enabled.insert(provider) } else { enabled.remove(provider) }
    }

    public mutating func move(_ provider: ArtworkProvider, by offset: Int) {
        guard let index = order.firstIndex(of: provider), order.indices.contains(index + offset) else { return }
        order.swapAt(index, index + offset)
    }

    public static func tokenError(_ token: String) -> String? {
        guard !token.isEmpty else { return "Enter a Discogs personal access token before enabling Discogs." }
        guard token.utf8.count <= 512, !token.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) || CharacterSet.whitespacesAndNewlines.contains($0) }) else {
            return "The Discogs token must be at most 512 bytes and contain no whitespace or control characters."
        }
        return nil
    }
}

public enum ArtworkCachePolicy: String, Codable, Sendable { case persistent, displayOnly = "display_only" }

public enum ArtworkRetention {
    public static func mayRedisplay(policy: ArtworkCachePolicy, consumers: Int) -> Bool {
        policy == .persistent || consumers > 0
    }

    public static func permitsAttachment(source: String) -> Bool {
        ["embedded", "folder", "cover_art_archive"].contains(source)
    }

    /// Reject rather than clamp an untrustworthy expiry; never grant six fresh hours on cache reads.
    public static func valid(source: String, policy: ArtworkCachePolicy, sourceURL: URL?, expiresAt: Date?, now: Date) -> Bool {
        if let expiresAt, expiresAt <= now { return false }
        switch source {
        case "deezer":
            return policy == .displayOnly && validURL(sourceURL, host: "www.deezer.com", prefix: "/album/")
        case "discogs":
            guard policy == .persistent, let expiresAt, expiresAt.timeIntervalSince(now) <= 6 * 60 * 60 else { return false }
            return validURL(sourceURL, host: "www.discogs.com", prefix: "/release/")
        case "embedded", "folder", "cover_art_archive": return policy == .persistent
        default: return false
        }
    }

    private static func validURL(_ url: URL?, host: String, prefix: String) -> Bool {
        guard let url, url.scheme == "https", url.host == host, url.user == nil, url.password == nil,
              url.port == nil || url.port == 443, url.path.hasPrefix(prefix) else { return false }
        return url.path.dropFirst(prefix.count).first?.isNumber == true
    }
}
