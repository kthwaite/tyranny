/// The cover-art colour for the playing track, with a bounded cache of extraction results.
/// Extraction itself runs elsewhere; this type decides when it is needed and which result applies.
public struct ArtTint: Sendable {
    public enum Artwork: Equatable, Sendable {
        case noContext, nothingPlaying, loading, missing, unavailable, image(key: String)
    }

    /// noContext if !hasContext; nothingPlaying if !playing; then by state "loading" / "missing" /
    /// "unavailable"; any other state is .image(key) when cacheKey is non-nil, else .loading.
    public static func artwork(hasContext: Bool, playing: Bool, state: String, cacheKey: String?) -> Artwork {
        guard hasContext else { return .noContext }
        guard playing else { return .nothingPlaying }
        switch state {
        case "loading": return .loading
        case "missing": return .missing
        case "unavailable": return .unavailable
        default: return cacheKey.map { .image(key: $0) } ?? .loading
        }
    }

    public private(set) var colour: RGB?

    /// A cached nil means the cover has no usable colour.
    private var cache: [String: RGB?] = [:]
    private var recent: RecentKeys<String>
    private var inFlight: Set<String> = []
    private var currentKey: String?

    public init(capacity: Int = 64) {
        recent = RecentKeys(capacity: capacity)
    }

    /// Returns a key to extract when the image is neither cached nor already in flight.
    public mutating func observe(_ artwork: Artwork) -> String? {
        switch artwork {
        case .noContext, .loading:
            currentKey = nil
        case .nothingPlaying, .missing, .unavailable:
            currentKey = nil
            colour = nil
        case .image(let key):
            currentKey = key
            if let hit = cache[key] {
                colour = hit
                remember(key)
                return nil
            }
            return inFlight.insert(key).inserted ? key : nil
        }
        return nil
    }

    /// Caches the result (nil = no usable colour). It sets `colour` only if `key` is still the observed image.
    public mutating func extracted(_ colour: RGB?, for key: String) {
        inFlight.remove(key)
        cache.updateValue(colour, forKey: key)
        remember(key)
        if currentKey == key { self.colour = colour }
    }

    private mutating func remember(_ key: String) {
        for evicted in recent.touch(key) { cache.removeValue(forKey: evicted) }
    }
}
