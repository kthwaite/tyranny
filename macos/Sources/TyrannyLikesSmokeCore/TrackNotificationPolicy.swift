import Foundation

/// Tracks audible queue occurrences, not position ticks. A hello establishes a quiet baseline.
public struct TrackNotificationPolicy {
    private var lastEntry: UInt64?

    public init() {}

    public mutating func shouldNotify(entryID: UInt64?, transport: String, newSession: Bool,
                                      enabled: Bool, frontmost: Bool, showWhileActive: Bool) -> Bool {
        if newSession {
            lastEntry = entryID
            return false
        }
        guard transport == "playing", let entryID else { return false }
        let changed = entryID != lastEntry
        lastEntry = entryID
        return changed && enabled && (!frontmost || showWhileActive)
    }
}
