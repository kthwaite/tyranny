import Foundation

public enum UpNext {
    /// The answer from the snapshot alone, or nil when the queue itself must be read.
    /// Nothing queued has nothing upcoming; a lone current entry has nothing after it.
    public static func quickAnswer(queueLength: Int, currentID: UInt64?) -> Bool? {
        if queueLength <= 0 { return false }
        if currentID != nil, queueLength == 1 { return false }
        return nil
    }

    /// Whether the queue has an entry the server treats as upcoming (what `ClearUpcoming` and
    /// `ReplaceUpcoming` remove): an entry after the current one, or, with nothing current,
    /// any unplayed entry. `loaded` is the queue from its first entry; `total` is its length.
    /// nil when the entries loaded so far cannot tell: read the next page and ask again.
    public static func hasUpcoming(total: Int, currentID: UInt64?, loaded: [QueueItem]) -> Bool? {
        guard total > 0 else { return false }
        if let currentID, let index = loaded.firstIndex(where: { $0.id == currentID }) {
            return index < total - 1
        }
        let complete = loaded.count >= total
        // A current entry still to be found decides by position, not by played marks.
        if currentID != nil, !complete { return nil }
        // No current entry in the queue: the server treats every unplayed entry as upcoming.
        if loaded.contains(where: { !$0.played }) { return true }
        return complete ? false : nil
    }
}
