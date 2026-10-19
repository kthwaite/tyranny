/// Keys in recency order, bounded to `capacity`. `touch` marks a key most recent and names the
/// keys that fell out, oldest first. The key just touched never falls out.
public struct RecentKeys<Key: Hashable>: Sendable where Key: Sendable {
    public let capacity: Int
    /// Oldest first.
    public private(set) var keys: [Key] = []

    public init(capacity: Int) {
        precondition(capacity >= 1, "keep at least the current key")
        self.capacity = capacity
    }

    public mutating func touch(_ key: Key) -> [Key] {
        keys.removeAll { $0 == key }
        keys.append(key)
        let excess = keys.count - capacity
        guard excess > 0 else { return [] }
        let evicted = Array(keys.prefix(excess))
        keys.removeFirst(excess)
        return evicted
    }

    public mutating func remove(_ key: Key) {
        keys.removeAll { $0 == key }
    }
}
