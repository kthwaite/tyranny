import Foundation

public enum UpNext {
    /// The answer from the snapshot alone, or nil when the queue itself must be read.
    /// Nothing queued has nothing upcoming; a lone current entry has nothing after it.
    public static func quickAnswer(queueLength: Int, currentID: UInt64?) -> Bool? {
        if queueLength <= 0 { return false }
        if currentID != nil, queueLength == 1 { return false }
        return nil
    }

    /// Reads the queue page by page and keeps only counters, so a long queue costs no memory.
    public struct Scan: Sendable {
        public let currentID: UInt64?
        private(set) var seen = 0
        private var sawUnplayed = false
        private var currentIndex: Int?

        public init(currentID: UInt64?) { self.currentID = currentID }

        /// Feeds the next entries (in queue order, from the first entry). `total` is the queue's
        /// length. Returns the answer once known, nil to read the next page.
        public mutating func feed(_ entries: [QueueItem], total: Int) -> Bool? {
            guard total > 0 else { return false }
            for entry in entries {
                if let currentID, entry.id == currentID, currentIndex == nil { currentIndex = seen }
                if !entry.played { sawUnplayed = true }
                seen += 1
            }
            if let currentIndex { return currentIndex < total - 1 }
            let complete = seen >= total
            // A current entry still to be found decides by position, not by played marks.
            if currentID != nil, !complete { return nil }
            // No current entry in the queue: the server treats every unplayed entry as upcoming.
            if sawUnplayed { return true }
            return complete ? false : nil
        }
    }

    /// Whether the queue has an entry the server treats as upcoming (what `ClearUpcoming` and
    /// `ReplaceUpcoming` remove): an entry after the current one, or, with nothing current,
    /// any unplayed entry. `loaded` is the queue from its first entry; `total` is its length.
    /// nil when the entries loaded so far cannot tell: read the next page and ask again.
    public static func hasUpcoming(total: Int, currentID: UInt64?, loaded: [QueueItem]) -> Bool? {
        var scan = Scan(currentID: currentID)
        return scan.feed(loaded, total: total)
    }
}
